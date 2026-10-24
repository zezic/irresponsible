use irresponsible::{
    correction::room::{self, Recipe, Taps, Target},
    domain::Rate,
    measurement::{
        self,
        simulation::{self, Plant, Scenario},
        workflow::{self, Run, Session},
    },
    storage::Store,
};
use std::sync::{Arc, mpsc};

fn setup(store: &Store, mut scenario: Scenario) -> (workflow::Plan, simulation::Simulated) {
    if matches!(scenario.plant, Plant::Identity) {
        scenario.input_gain = 1.0;
    }
    let mut source = simulation::plan(scenario.rate);
    source.acquisition.sweep_seconds = 1.0;
    source.acquisition.noise_seconds = 1.0;
    source.acquisition.tail_seconds = 1.0;
    let mut provider = simulation::Simulated::new(scenario).unwrap();
    let cancel = irresponsible::audio::Cancellation::new();
    let left = measurement::levels::check(
        &source,
        measurement::Speaker::Left,
        &mut provider,
        &cancel,
        store,
    )
    .unwrap();
    let right = measurement::levels::check(
        &source,
        measurement::Speaker::Right,
        &mut provider,
        &cancel,
        store,
    )
    .unwrap();
    let levels = measurement::levels::Checks::new(left, right, &source).unwrap();
    (
        workflow::Plan {
            levels: Some(levels),
            source,
            recipe: Recipe {
                target: Target::Flat,
                maximum_boost_db: Some(3.0),
                maximum_cut_db: Some(12.0),
                taps: Taps::Short,
                ..Recipe::default()
            },
            progression: workflow::Progression::Manual,
            kind: workflow::RunKind::Measure,
            session: None,
        },
        provider,
    )
}
fn verify_with(
    id: irresponsible::storage::ContentHash,
    store: &Store,
    provider: Box<dyn measurement::io::Provider>,
) -> irresponsible::storage::ContentHash {
    let session = Session::load(id, store).unwrap();
    let history = session.history(store).unwrap();
    let pass = session.default_candidate(&history).unwrap();
    let source = measurement::Source::load(session.source, store).unwrap();
    let (tx, controls) = mpsc::sync_channel(8);
    Run {
        plan: workflow::Plan {
            levels: None,
            source: source.plan,
            recipe: Recipe::default(),
            progression: workflow::Progression::Manual,
            kind: workflow::RunKind::Verify { pass },
            session: Some(id),
        },
        store,
        cancel: irresponsible::audio::Cancellation::new(),
        controls,
        progress: Box::new(move |progress| {
            if let workflow::Progress::Move { movement, .. } = progress {
                tx.send(workflow::Control {
                    movement,
                    action: workflow::MoveAction::Ready,
                })
                .unwrap();
            }
        }),
        provider,
    }
    .execute()
    .unwrap()
}

fn scenario_run(
    plant: Plant,
    effort: f64,
) -> (
    tempfile::TempDir,
    Store,
    irresponsible::storage::ContentHash,
) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::at(directory.path()).unwrap();
    let (mut plan, provider) = setup(
        &store,
        Scenario {
            plant: plant.clone(),
            ..Scenario::default()
        },
    );
    plan.recipe.global_effort = effort;
    let (_tx, rx) = mpsc::sync_channel(8);
    let run = Run {
        plan,
        store: &store,
        cancel: irresponsible::audio::Cancellation::new(),
        controls: rx,
        progress: Box::new(|_| {}),
        provider: Box::new(provider),
    };
    let id = run.execute().unwrap();
    let id = verify_with(
        id,
        &store,
        Box::new(
            simulation::Simulated::new(Scenario {
                input_gain: if matches!(plant, Plant::Identity) {
                    1.0
                } else {
                    Scenario::default().input_gain
                },
                plant,
                ..Scenario::default()
            })
            .unwrap(),
        ),
    );
    (directory, store, id)
}

#[test]
fn identity_acquisition_preserves_gain_compensates_drift_and_stops() {
    let (_directory, store, id) = scenario_run(Plant::Identity, 1.0);
    let session = Session::load(id, &store).unwrap();
    let source = measurement::Source::load(session.source, &store).unwrap();
    for t in &source.points[0].full {
        assert!((t.clock.ppm - 100.0).abs() < 3.0, "clock {:?}", t.clock);
        let errors: Vec<_> = t
            .response
            .magnitude
            .frequency
            .iter()
            .zip(&t.response.magnitude.db)
            .filter(|(f, _)| **f > 40.0 && **f < 16000.0)
            .map(|(_, d)| d.abs())
            .collect();
        assert!(
            errors.iter().sum::<f32>() / (errors.len() as f32) < 0.1,
            "gain error {errors:?}"
        );
        assert_eq!(t.raw.load(&store).unwrap().len(), t.raw.frames);
    }
    let history = session.history(&store).unwrap();
    assert!(
        history
            .iter()
            .any(|p| p.decision == workflow::Decision::Prepared)
    );
    assert_eq!(history[0].decision, workflow::Decision::Baseline);
}

#[test]
fn room_pipeline_generates_and_measures_exact_stereo_filter() {
    let (_directory, store, id) = scenario_run(Plant::Room, 0.5);
    let session = Session::load(id, &store).unwrap();
    let history = session.history(&store).unwrap();
    assert!(history.len() >= 2);
    let trial = history
        .iter()
        .find(|p| p.correction().is_some() && p.fully_verified())
        .unwrap();
    assert!(trial.fully_verified());
    let filter: irresponsible::correction::Correction =
        serde_json::from_slice(&store.get(&trial.correction().unwrap()).unwrap()).unwrap();
    assert_ne!(filter.channels[0], filter.channels[1]);
    let irresponsible::correction::Origin::Room { objective, .. } = filter.origin else {
        panic!("room source");
    };
    let frozen: room::Frozen = serde_json::from_slice(&store.get(&objective).unwrap()).unwrap();
    assert_eq!(frozen.recipe.global_effort, 0.5);
    let measured = &trial.measured[0];
    for (c, take) in measured.iter().enumerate() {
        let predicted = filter.channels[c].clone();
        let response =
            measurement::analysis::magnitude(&predicted, Rate::Hz48000, &frozen.target.frequency);
        let baseline = &history[0].measured[0][c].response.magnitude.db;
        let mismatch: Vec<_> = response
            .iter()
            .zip(baseline)
            .zip(&take.response.magnitude.db)
            .enumerate()
            .filter(|(i, _)| {
                take.response
                    .trusted_band
                    .contains(f64::from(frozen.target.frequency[*i]))
            })
            .map(|(_, ((&f, &b), &m))| (f + b - m).abs())
            .collect();
        assert!(
            mismatch.iter().sum::<f32>() / (mismatch.len() as f32) < 0.25,
            "verification must retain filter effect"
        );
    }
}

#[test]
fn region_overrides_can_be_partial_removed_and_resized() {
    let mut recipe = Recipe::default();
    recipe
        .insert_region(room::Region {
            band: measurement::Band::new(100.0, 300.0).unwrap(),
            effort: 0.5,
            smoothing_octaves: 0.0,
            lower_fade: 0.1,
            upper_fade: 0.2,
        })
        .unwrap();
    assert_eq!(recipe.profile(1000.0).0, 1.0);
    assert_eq!(recipe.profile(180.0).0, 0.5);
    recipe.resize_region(0, true, 120.0);
    recipe.validate().unwrap();
    recipe.regions.clear();
    assert_eq!(recipe.profile(180.0).0, 1.0);
}

#[test]
fn independent_nominal_rates_lf_inverse_and_calibration_sign() {
    use measurement::{Speaker, io::Provider, probe::Probe};
    let directory = tempfile::tempdir().unwrap();
    let store = Store::at(directory.path()).unwrap();
    let mut scenario = Scenario {
        rate: Rate::Hz44100,
        input_rate: Rate::Hz48000,
        drift_ppm: -150.0,
        plant: Plant::Identity,
        input_gain: 1.0,
        ..Scenario::default()
    };
    scenario.noise_peak = 0.0000001;
    let mut provider = simulation::Simulated::new(scenario).unwrap();
    let cancel = irresponsible::audio::Cancellation::new();
    let background = provider.record(&vec![[0.0; 2]; 44100], 0, &cancel).unwrap();
    let noise = measurement::analysis::noise(
        &background.samples,
        background.rate,
        measurement::NoiseLimits::default(),
        &store,
    )
    .unwrap();
    let calibration = measurement::Calibration::import(
        b"* Sens Factor = unsupported metadata\n10 6\n22000 6\n",
        measurement::Orientation::TowardSpeakers,
        &store,
    )
    .unwrap();
    let mut settings = measurement::Acquisition {
        sweep_seconds: 4.0,
        tail_seconds: 1.0,
        noise_seconds: 1.0,
        ..Default::default()
    };
    settings.band = measurement::Band::new(20.0, 300.0).unwrap();
    let probe = Probe::new(
        Rate::Hz44100,
        &settings,
        &measurement::GainContract::default(),
        Speaker::Right,
        None,
    )
    .unwrap();
    let raw = provider.record(&probe.output, 0, &cancel).unwrap();
    let (clock, response) = measurement::analysis::identify(
        &raw.samples,
        raw.rate,
        &probe,
        &noise,
        &calibration,
        &store,
    )
    .unwrap();
    assert!((clock.ppm + 150.0).abs() < 3.0, "clock {:?}", clock);
    for (&hz, &db) in response
        .magnitude
        .frequency
        .iter()
        .zip(&response.magnitude.db)
    {
        if (30.0..=240.0).contains(&hz) {
            assert!(
                (db + 6.0).abs() < 0.15,
                "LF inverse/calibration: {hz} Hz = {db} dB"
            );
        }
    }
    assert_eq!(response.resolution_hz, 1.0);
    let mut uncompensated = clock.clone();
    uncompensated.ratio = 48000.0 / 44100.0;
    assert!((uncompensated.ratio - clock.ratio).abs() * raw.samples.len() as f64 > 20.0);
}

#[test]
fn offline_regeneration_keeps_history_preference_and_verification_identity() {
    let (_directory, store, id) = scenario_run(Plant::Room, 0.5);
    let mut session = Session::load(id, &store).unwrap();
    let original = session.passes.clone();
    let preferred = workflow::PassId(1);
    let id = session.prefer(preferred, &store).unwrap();
    let recipe = Recipe {
        target: Target::Warm,
        global_effort: 0.25,
        taps: Taps::Short,
        ..Recipe::default()
    };
    let (updated, pass, correction) =
        workflow::prepare(id, recipe, Some(preferred), &store).unwrap();
    assert!(!pass.fully_verified());
    assert_eq!(pass.decision, workflow::Decision::Prepared);
    assert!(pass.measured.is_empty());
    assert_eq!(pass.parent, Some(preferred));
    let reopened = Session::load(updated, &Store::at(store.root()).unwrap()).unwrap();
    assert_eq!(reopened.preferred, Some(preferred));
    assert_eq!(&reopened.passes[..original.len()], original.as_slice());
    let converted = correction.at_rate(Rate::Hz44100, &store).unwrap();
    converted.validate().unwrap();
    assert_eq!(converted.rate, Rate::Hz44100);
    assert_ne!(
        pass.correction(),
        session.history(&store).unwrap()[1].correction()
    );
    let destination = tempfile::tempdir().unwrap();
    workflow::export_session(updated, destination.path(), &store).unwrap();
    assert!(destination.path().join("session.json").exists());
    workflow::export_filter(
        pass.correction().unwrap(),
        &destination.path().join("filter.wav"),
        &store,
    )
    .unwrap();
    let reader = hound::WavReader::open(destination.path().join("filter.wav")).unwrap();
    assert_eq!(reader.spec().channels, 2);
}

#[test]
fn spatial_power_average_does_not_cancel_opposite_position_polarities() {
    let (_directory, store, id) = scenario_run(Plant::Identity, 1.0);
    let session = Session::load(id, &store).unwrap();
    let mut source = measurement::Source::load(session.source, &store).unwrap();
    let mut second = source.points[0].clone();
    second.position = 1;
    for take in &mut second.full {
        take.position = 1;
        let mut ir = take.response.impulse.load(&store).unwrap();
        ir.iter_mut().for_each(|v| *v = -*v);
        take.response.impulse =
            measurement::Waveform::save(&ir, take.response.impulse.rate, &store).unwrap();
    }
    source.points.push(second);
    source.plan.layout.positions.push(measurement::Position {
        name: "Opposite phase point".into(),
        coordinates_cm: Some([20.0, 0.0, 0.0]),
        weight: 2.0,
    });
    source.plan.layout.order.push(1);
    // Aggregate operates on measured magnitude, independent of propagation phase.
    let aggregate = room::aggregate(&source);
    for c in 0..2 {
        for (&a, &b) in aggregate.magnitude[c]
            .db
            .iter()
            .zip(&source.points[0].full[c].response.magnitude.db)
        {
            assert!((a - b).abs() < 0.0001);
        }
    }
}

#[test]
fn noise_advisories_retain_evidence_and_transport_faults_stop_acquisition() {
    use measurement::io::Provider;
    let directory = tempfile::tempdir().unwrap();
    let store = Store::at(directory.path()).unwrap();
    let cancel = irresponsible::audio::Cancellation::new();
    let mut provider = simulation::Simulated::new(Scenario {
        fault: simulation::Fault::TransientOnTake(1),
        ..Scenario::default()
    })
    .unwrap();
    let raw = provider.record(&vec![[0.0; 2]; 48000], 0, &cancel).unwrap();
    let noise = measurement::analysis::noise(
        &raw.samples,
        raw.rate,
        measurement::NoiseLimits::default(),
        &store,
    )
    .unwrap();
    let error = noise
        .quality_warning()
        .expect("variable background advisory");
    assert!(error.contains(&format!(
        "level variation {:.1} dB (warning above 8.0 dB)",
        noise.window_variation_db
    )));
    assert!(error.contains(&format!(
        "peak-to-average {:.1} dB (warning above 24.0 dB)",
        noise.crest_db
    )));
    let limits = measurement::NoiseLimits {
        maximum_variation_db: noise.window_variation_db + 1.0,
        maximum_crest_db: noise.crest_db + 1.0,
    };
    let relaxed = measurement::analysis::noise(&raw.samples, raw.rate, limits, &store).unwrap();
    assert!(relaxed.quality_warning().is_none());
    assert_eq!(relaxed.raw.object, noise.raw.object);
    assert_eq!(relaxed.psd_dbfs_per_hz.db, noise.psd_dbfs_per_hz.db);
    let saved = store.put(&serde_json::to_vec(&relaxed).unwrap()).unwrap();
    let restored: measurement::Noise = serde_json::from_slice(&store.get(&saved).unwrap()).unwrap();
    assert!(restored.quality_warning().is_none());
    assert_eq!(
        restored.limits.maximum_variation_db,
        limits.maximum_variation_db
    );
    assert_eq!(restored.limits.maximum_crest_db, limits.maximum_crest_db);
    let mut provider = simulation::Simulated::new(Scenario {
        fault: simulation::Fault::DropOnTake(1),
        ..Scenario::default()
    })
    .unwrap();
    assert!(provider.record(&vec![[0.0; 2]; 48000], 0, &cancel).is_err());
    cancel.cancel();
    assert!(provider.record(&vec![[0.0; 2]; 48000], 0, &cancel).is_err());
}

#[derive(Clone, Copy)]
enum ModelBehavior {
    CaptureGain,
    MarkerLevel,
}
struct ModelError {
    inner: simulation::Simulated,
    probes: usize,
    behavior: ModelBehavior,
    settings: measurement::Acquisition,
}
impl measurement::io::Provider for ModelError {
    fn elapsed(&self) -> std::time::Duration {
        self.inner.elapsed()
    }
    fn set_deadline(&mut self, d: std::time::Duration) {
        self.inner.set_deadline(d);
    }
    fn set_meters(&mut self, m: Arc<irresponsible::audio::Meters>) {
        self.inner.set_meters(m);
    }
    fn record(
        &mut self,
        output: &[irresponsible::dsp::Frame],
        position: usize,
        cancel: &irresponsible::audio::Cancellation,
    ) -> irresponsible::error::Result<measurement::io::Recording> {
        let mut raw = self.inner.record(output, position, cancel)?;
        let recipe = measurement::probe::Recipe::new(
            Rate::Hz48000,
            &self.settings,
            &measurement::GainContract::default(),
        )?;
        if output.len() > recipe.total_frames - recipe.sweep_frames {
            self.probes += 1;
            if matches!(self.behavior, ModelBehavior::CaptureGain) && self.probes > 2 {
                for sample in &mut raw.samples {
                    *sample *= 10.0_f32.powf(3.0 / 20.0);
                }
            } else if matches!(self.behavior, ModelBehavior::MarkerLevel) && self.probes > 4 {
                let ratio = 1.0001;
                let delay = 0.0173 * 48000.0;
                for marker in recipe
                    .marker_frames
                    .into_iter()
                    .chain(std::iter::once(recipe.diagnostic_marker_frame))
                {
                    let start = (delay + (marker - 12000) as f64 * ratio) as usize;
                    let end = (delay + (marker + 2048 + 12000) as f64 * ratio) as usize;
                    for sample in &mut raw.samples[start..end] {
                        *sample *= 10.0_f32.powf(1.24 / 20.0);
                    }
                }
            }
        }
        Ok(raw)
    }
}

#[test]
fn repeated_full_response_exposes_a_real_capture_level_change() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::at(directory.path()).unwrap();
    let (plan, provider) = setup(
        &store,
        Scenario {
            plant: Plant::Identity,
            ..Default::default()
        },
    );
    let settings = plan.source.acquisition.clone();
    let (_tx, controls) = mpsc::sync_channel(8);
    let id = Run {
        plan,
        store: &store,
        cancel: irresponsible::audio::Cancellation::new(),
        controls,
        progress: Box::new(|_| {}),
        provider: Box::new(ModelError {
            inner: provider,
            probes: 0,
            behavior: ModelBehavior::CaptureGain,
            settings,
        }),
    }
    .execute()
    .unwrap();
    let session = Session::load(id, &store).unwrap();
    let source = measurement::Source::load(session.source, &store).unwrap();
    let history = session.history(&store).unwrap();
    assert!((history[0].metrics.repeatability_db - 3.0).abs() < 0.02);
    assert_eq!(history[0].decision, workflow::Decision::Baseline);
    assert!(
        history
            .iter()
            .all(|pass| pass.decision != workflow::Decision::Accepted)
    );
    assert_eq!(session.best, Some(history[0].id));
    assert_ne!(
        source.points[0].full[0].raw.object,
        source.repeat_reference[0].raw.object
    );
}

#[test]
fn marker_level_variation_does_not_discard_a_verified_unchanged_response() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::at(directory.path()).unwrap();
    let (plan, provider) = setup(&store, Scenario::default());
    let settings = plan.source.acquisition.clone();
    let (_tx, controls) = mpsc::sync_channel(8);
    let id = Run {
        plan,
        store: &store,
        cancel: irresponsible::audio::Cancellation::new(),
        controls,
        progress: Box::new(|_| {}),
        provider: Box::new(ModelError {
            inner: provider,
            probes: 0,
            behavior: ModelBehavior::MarkerLevel,
            settings,
        }),
    }
    .execute()
    .unwrap();
    let id = verify_with(
        id,
        &store,
        Box::new(ModelError {
            inner: simulation::Simulated::new(Scenario::default()).unwrap(),
            probes: 4,
            behavior: ModelBehavior::MarkerLevel,
            settings: measurement::Source::load(Session::load(id, &store).unwrap().source, &store)
                .unwrap()
                .plan
                .acquisition,
        }),
    );
    let session = Session::load(id, &store).unwrap();
    let source = measurement::Source::load(session.source, &store).unwrap();
    let history = session.history(&store).unwrap();
    let best = &history[session.best.unwrap().0];
    assert!(best.correction().is_some());
    assert!(best.fully_verified());
    assert!(best.metrics.score() < history[0].metrics.score());
    for pair in &best.measured {
        for (channel, take) in pair.iter().enumerate() {
            let original = source.points[0].full[channel].clock.marker_projection_db;
            assert!((take.clock.marker_projection_db[0] - original[0] - 1.24).abs() < 0.1);
        }
    }
}

#[test]
fn hands_free_countdown_pauses_between_positions() {
    use std::sync::Mutex;
    let directory = tempfile::tempdir().unwrap();
    let store = Store::at(directory.path()).unwrap();
    let (mut plan, provider) = setup(&store, Scenario::default());
    plan.source.layout.positions.push(measurement::Position {
        name: "Left seat".into(),
        coordinates_cm: Some([-25.0, 0.0, 0.0]),
        weight: 2.0,
    });
    plan.source.layout.order.push(1);
    plan.progression = workflow::Progression::HandsFree {
        movement_seconds: 5.0,
    };
    let (tx, rx) = mpsc::sync_channel(8);
    let moves = Arc::new(Mutex::new(Vec::new()));
    let observed = moves.clone();
    let mut_pauses = Arc::new(Mutex::new(Vec::<(usize, f64)>::new()));
    let pauses = mut_pauses.clone();
    let id = Run {
        plan,
        store: &store,
        cancel: irresponsible::audio::Cancellation::new(),
        controls: rx,
        provider: Box::new(provider),
        progress: Box::new(move |p| {
            if let workflow::Progress::Move {
                movement,
                seconds: Some(seconds),
                paused,
            } = p
            {
                let position = movement.position();
                let for_refine = matches!(movement, workflow::Movement::Reference(_));
                if !paused {
                    if !observed.lock().unwrap().contains(&(position, for_refine)) {
                        observed.lock().unwrap().push((position, for_refine));
                        tx.send(workflow::Control {
                            movement,
                            action: workflow::MoveAction::Pause,
                        })
                        .unwrap();
                    }
                } else {
                    let mut pauses = pauses.lock().unwrap();
                    if let Some((previous, remaining)) = pauses.last() {
                        if *previous == position {
                            assert!((remaining - seconds).abs() < 0.001);
                            tx.send(workflow::Control {
                                movement,
                                action: workflow::MoveAction::Ready,
                            })
                            .unwrap();
                            tx.send(workflow::Control {
                                movement,
                                action: workflow::MoveAction::Ready,
                            })
                            .unwrap();
                        } else {
                            pauses.push((position, seconds));
                        }
                    } else {
                        pauses.push((position, seconds));
                    }
                }
            }
        }),
    }
    .execute()
    .unwrap();
    assert_eq!(*moves.lock().unwrap(), [(1, false)]);
    let session = Session::load(id, &store).unwrap();
    let source = measurement::Source::load(session.source, &store).unwrap();
    assert_eq!(source.points.len(), 2);
    for pass in session.history(&store).unwrap() {
        assert!(!matches!(pass.scope, workflow::Scope::SpatialFull { .. }));
    }
}

#[test]
fn interrupted_channel_resumes_from_its_checkpoint_and_stop_cancels_pending_moves() {
    use measurement::io::Provider;
    let directory = tempfile::tempdir().unwrap();
    let store = Store::at(directory.path()).unwrap();
    let (plan, provider) = setup(
        &store,
        Scenario {
            fault: simulation::Fault::DeviceLostOnTake(5),
            ..Scenario::default()
        },
    );
    let (_tx, rx) = mpsc::sync_channel(8);
    assert!(
        Run {
            plan: plan.clone(),
            store: &store,
            cancel: irresponsible::audio::Cancellation::new(),
            controls: rx,
            progress: Box::new(|_| {}),
            provider: Box::new(provider)
        }
        .execute()
        .is_err()
    );
    let Some(workflow::Recovery::Acquisition(checkpoint)) =
        workflow::Recovery::load(&store).unwrap()
    else {
        panic!("acquisition checkpoint");
    };
    let retained = checkpoint.points[0].full[0].as_ref().unwrap().raw.object;
    assert!(checkpoint.points[0].full[1].is_none());
    let mut resume = plan;
    resume.kind = workflow::RunKind::ResumeMeasurement;
    let (tx, rx) = mpsc::sync_channel(8);
    let id = Run {
        plan: resume,
        store: &store,
        cancel: irresponsible::audio::Cancellation::new(),
        controls: rx,
        progress: Box::new(move |p| {
            if let workflow::Progress::Move { movement, .. } = p {
                tx.send(workflow::Control {
                    movement,
                    action: workflow::MoveAction::Ready,
                })
                .unwrap();
            }
        }),
        provider: Box::new(simulation::Simulated::new(Scenario::default()).unwrap()),
    }
    .execute()
    .unwrap();
    let source =
        measurement::Source::load(Session::load(id, &store).unwrap().source, &store).unwrap();
    assert_eq!(source.points[0].full[0].raw.object, retained);
    let (mut plan, provider) = setup(&store, Scenario::default());
    plan.source.layout.positions.push(measurement::Position {
        name: "Next".into(),
        coordinates_cm: None,
        weight: 1.0,
    });
    plan.source.layout.order.push(1);
    let cancel = irresponsible::audio::Cancellation::new();
    let stop = cancel.clone();
    let (_tx, rx) = mpsc::sync_channel(8);
    let result = Run {
        plan,
        store: &store,
        cancel,
        controls: rx,
        provider: Box::new(provider),
        progress: Box::new(move |p| {
            if matches!(p, workflow::Progress::Move { .. }) {
                stop.cancel();
            }
        }),
    }
    .execute();
    assert!(matches!(
        result,
        Err(irresponsible::error::Error::Measurement(
            workflow::Fault::Stopped
        ))
    ));
    // A stale cancellation lease cannot stop a subsequent independent provider/run.
    let mut provider = simulation::Simulated::new(Scenario::default()).unwrap();
    assert!(
        provider
            .record(
                &vec![[0.0; 2]; 48000],
                0,
                &irresponsible::audio::Cancellation::new()
            )
            .is_ok()
    );
}

#[test]
fn default_full_and_slow_lf_probes_have_headroom_and_matching_gain() {
    use measurement::{io::Provider, probe::Probe};
    let directory = tempfile::tempdir().unwrap();
    let store = Store::at(directory.path()).unwrap();
    let mut provider = simulation::Simulated::new(Scenario::default()).unwrap();
    let cancel = irresponsible::audio::Cancellation::new();
    let settings = measurement::Acquisition::default();
    let background = provider
        .record(&vec![[0.0; 2]; 48000 * 5], 0, &cancel)
        .unwrap();
    let noise = measurement::analysis::noise(
        &background.samples,
        background.rate,
        measurement::NoiseLimits::default(),
        &store,
    )
    .unwrap();
    let calibration = measurement::Calibration::Uncalibrated { acknowledged: true };
    let mut responses = Vec::new();
    for acquisition in [
        settings.clone(),
        measurement::Acquisition {
            band: measurement::Band::new(20.0, 300.0).unwrap(),
            sweep_seconds: 16.0,
            ..settings
        },
    ] {
        let probe = Probe::new(
            Rate::Hz48000,
            &acquisition,
            &measurement::GainContract::default(),
            measurement::Speaker::Left,
            None,
        )
        .unwrap();
        let raw = provider.record(&probe.output, 0, &cancel).unwrap();
        assert!(raw.samples.iter().all(|x| x.abs() < 0.5));
        responses.push(
            measurement::analysis::identify(
                &raw.samples,
                raw.rate,
                &probe,
                &noise,
                &calibration,
                &store,
            )
            .unwrap()
            .1,
        );
    }
    let errors: Vec<_> = responses[0]
        .magnitude
        .frequency
        .iter()
        .enumerate()
        .filter(|(_, hz)| **hz >= 30.0 && **hz <= 240.0)
        .map(|(i, _)| (responses[0].magnitude.db[i] - responses[1].magnitude.db[i]).abs())
        .collect();
    assert!(errors.len() > 100);
    assert!(
        errors.iter().sum::<f32>() / (errors.len() as f32) < 0.2,
        "full/LF inverse scaling"
    );
}

#[test]
fn zero_tonal_effort_keeps_requested_alignment_and_cut_only_filters_stay_bounded() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::at(directory.path()).unwrap();
    let (mut plan, provider) = setup(&store, Scenario::default());
    plan.recipe.global_effort = 0.0;
    plan.recipe.alignment = room::Alignment::Time;
    let (_tx, rx) = mpsc::sync_channel(8);
    let id = Run {
        plan,
        store: &store,
        cancel: irresponsible::audio::Cancellation::new(),
        controls: rx,
        progress: Box::new(|_| {}),
        provider: Box::new(provider),
    }
    .execute()
    .unwrap();
    let id = verify_with(
        id,
        &store,
        Box::new(simulation::Simulated::new(Scenario::default()).unwrap()),
    );
    let session = Session::load(id, &store).unwrap();
    let history = session.history(&store).unwrap();
    let best = &history[session.best.unwrap().0];
    assert!(best.correction().is_some());
    assert!(best.metrics.alignment_error_ms < 0.1);
    let source = measurement::Source::load(session.source, &store).unwrap();
    let recipe = Recipe {
        maximum_boost_db: Some(0.0),
        taps: Taps::Long,
        ..Recipe::default()
    };
    let frozen = room::freeze(&source, session.source, recipe).unwrap();
    let (filter, realized) =
        room::render(&frozen, &frozen.designed_db, Rate::Hz48000, "Cut only").unwrap();
    assert!(realized.peak_gain_db.iter().all(|&db| db <= 0.0001));
    assert_ne!(filter.channels[0], filter.channels[1]);
}

#[test]
fn guided_spatial_verification_visits_each_required_point_and_preserves_partial_evidence() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::at(directory.path()).unwrap();
    let (mut plan, provider) = setup(&store, Scenario::default());
    plan.source.layout.positions.push(measurement::Position {
        name: "Second".into(),
        coordinates_cm: Some([25.0, 0.0, 0.0]),
        weight: 1.0,
    });
    plan.source.layout.order.push(1);
    let base_plan = plan.clone();
    let (tx, rx) = mpsc::sync_channel(8);
    let id = Run {
        plan,
        store: &store,
        cancel: irresponsible::audio::Cancellation::new(),
        controls: rx,
        progress: Box::new(move |p| {
            if let workflow::Progress::Move { movement, .. } = p {
                tx.send(workflow::Control {
                    movement,
                    action: workflow::MoveAction::Ready,
                })
                .unwrap();
            }
        }),
        provider: Box::new(provider),
    }
    .execute()
    .unwrap();
    let original = Session::load(id, &store).unwrap();
    let selected = original
        .default_candidate(&original.history(&store).unwrap())
        .unwrap();
    let mut guided = base_plan;
    guided.kind = workflow::RunKind::VerifySpatial { pass: selected };
    guided.session = Some(id);
    let (tx, rx) = mpsc::sync_channel(8);
    let id = Run {
        plan: guided,
        store: &store,
        cancel: irresponsible::audio::Cancellation::new(),
        controls: rx,
        progress: Box::new(move |p| {
            if let workflow::Progress::Move { movement, .. } = p {
                tx.send(workflow::Control {
                    movement,
                    action: workflow::MoveAction::Ready,
                })
                .unwrap();
            }
        }),
        provider: Box::new(simulation::Simulated::new(Scenario::default()).unwrap()),
    }
    .execute()
    .unwrap();
    let session = Session::load(id, &store).unwrap();
    let history = session.history(&store).unwrap();
    let partial: Vec<_> = history
        .iter()
        .filter(|p| matches!(p.scope, workflow::Scope::SpatialPartial { .. }))
        .collect();
    assert_eq!(partial.len(), 2);
    assert!(partial.iter().all(|p| !p.fully_verified()));
    let full = history.last().unwrap();
    assert!(matches!(&full.scope,workflow::Scope::SpatialFull{positions,..}if positions==&[0,1]));
    assert!(full.fully_verified());
    assert_eq!(full.measured.len(), 2);
    assert_eq!(full.metrics.per_position_rms.len(), 2);
}

#[test]
fn spatial_fault_resumes_the_same_filter_from_completed_point_evidence() {
    fn ready(tx: mpsc::SyncSender<workflow::Control>) -> Box<dyn Fn(workflow::Progress) + Send> {
        Box::new(move |p| {
            if let workflow::Progress::Move { movement, .. } = p {
                tx.send(workflow::Control {
                    movement,
                    action: workflow::MoveAction::Ready,
                })
                .unwrap();
            }
        })
    }
    let directory = tempfile::tempdir().unwrap();
    let store = Store::at(directory.path()).unwrap();
    let (mut plan, provider) = setup(&store, Scenario::default());
    plan.source.layout.positions.push(measurement::Position {
        name: "Second".into(),
        coordinates_cm: Some([25.0, 0.0, 0.0]),
        weight: 1.0,
    });
    plan.source.layout.order.push(1);
    let mut verify = plan.clone();
    let (tx, rx) = mpsc::sync_channel(8);
    let id = Run {
        plan,
        store: &store,
        cancel: irresponsible::audio::Cancellation::new(),
        controls: rx,
        progress: ready(tx),
        provider: Box::new(provider),
    }
    .execute()
    .unwrap();
    let original = Session::load(id, &store).unwrap();
    let selected = original
        .default_candidate(&original.history(&store).unwrap())
        .unwrap();
    verify.kind = workflow::RunKind::VerifySpatial { pass: selected };
    verify.session = Some(id);
    let (tx, rx) = mpsc::sync_channel(8);
    assert!(
        Run {
            plan: verify.clone(),
            store: &store,
            cancel: irresponsible::audio::Cancellation::new(),
            controls: rx,
            progress: ready(tx),
            provider: Box::new(
                simulation::Simulated::new(Scenario {
                    fault: simulation::Fault::DeviceLostOnTake(4),
                    ..Scenario::default()
                })
                .unwrap()
            )
        }
        .execute()
        .is_err()
    );
    let Some(workflow::Recovery::Spatial {
        session,
        candidate,
        completed,
        ..
    }) = workflow::Recovery::load(&store).unwrap()
    else {
        panic!("spatial checkpoint");
    };
    assert_eq!(completed.len(), 1);
    let retained = completed[0][0].raw.object;
    let previous = Session::load(session, &store).unwrap().passes;
    verify.session = Some(session);
    let (tx, rx) = mpsc::sync_channel(8);
    let id = Run {
        plan: verify,
        store: &store,
        cancel: irresponsible::audio::Cancellation::new(),
        controls: rx,
        progress: ready(tx),
        provider: Box::new(simulation::Simulated::new(Scenario::default()).unwrap()),
    }
    .execute()
    .unwrap();
    let session = Session::load(id, &store).unwrap();
    assert_eq!(&session.passes[..previous.len()], &previous);
    let history = session.history(&store).unwrap();
    let final_pass = history.last().unwrap();
    assert_eq!(final_pass.correction(), Some(candidate));
    assert!(final_pass.fully_verified());
    assert_eq!(final_pass.measured[0][0].raw.object, retained);
}

#[test]
fn legacy_settings_and_headphone_metadata_migrate_without_changing_coefficients() {
    use irresponsible::{
        correction::{Correction, Curve, GeneratedTarget, GenerationSettings, Origin},
        domain::Target as HeadphoneTarget,
        storage::ContentHash,
    };
    let directory = tempfile::tempdir().unwrap();
    let store = Store::at(directory.path()).unwrap();
    let generation = GenerationSettings::default();
    let curve = Curve::new(vec![20.0, 1000.0, 20000.0], vec![1.0, 0.0, -2.0]).unwrap();
    let correction = Correction {
        name: "Legacy headphone".into(),
        rate: Rate::Hz44100,
        channels: [vec![0.75, 0.25], vec![0.75, 0.25]],
        origin: Origin::AutoEq {
            revision: "fixture".into(),
            path: "fixture/profile".into(),
            reference: "rig reference".into(),
            target: "Harman reference".into(),
            measurement_hash: ContentHash::of(b"measurement").as_str().into(),
            target_hash: ContentHash::of(b"target").as_str().into(),
            policy: generation.policy(),
            generation,
            target_kind: GeneratedTarget::Harman,
            measured: curve.clone(),
            target_curve: curve,
        },
    };
    let mut legacy = serde_json::to_value(&correction).unwrap();
    for (old, new) in [
        ("target_kind", "target"),
        ("measured", "measured"),
        ("target_curve", "target_curve"),
    ] {
        let value = legacy["origin"]["AutoEq"]
            .as_object_mut()
            .unwrap()
            .remove(old)
            .unwrap();
        legacy.as_object_mut().unwrap().insert(new.into(), value);
    }
    let bytes = serde_json::to_vec(&legacy).unwrap();
    let id = store.put(&bytes).unwrap();
    let decoded: Correction = serde_json::from_slice(&store.get(&id).unwrap()).unwrap();
    decoded.validate().unwrap();
    assert_eq!(decoded.channels, correction.channels);
    assert_eq!(decoded.target(), HeadphoneTarget::Harman);
    assert_eq!(
        decoded.measured().unwrap().db,
        correction.measured().unwrap().db
    );
    let legacy_settings =
        serde_json::json!({"version":"1","active":id,"presets":{id.as_str():"Old preset"}});
    store
        .write(
            "settings.json",
            &serde_json::to_vec(&legacy_settings).unwrap(),
        )
        .unwrap();
    let settings = store.load().unwrap();
    assert_eq!(settings.active, Some(id));
    assert_eq!(settings.presets[&id], "Old preset");
    store.save(&settings).unwrap();
    assert_eq!(store.load().unwrap().presets, settings.presets);
}

#[cfg(feature = "alloc-audit")]
#[test]
fn measurement_callbacks_allocate_and_free_nothing() {
    use measurement::io::{CaptureCallback, Direction, FaultLatch, PlaybackCallback};
    use std::sync::atomic::{AtomicU64, Ordering};
    let fault = Arc::new(measurement::io::FaultLatch::default());
    let (tx, mut rx) = rtrb::RingBuffer::new(1024);
    let mut capture = CaptureCallback::new(tx, fault.clone(), Rate::Hz48000, 0, 1);
    let (mut tx, rx_out) = rtrb::RingBuffer::new(1024);
    for _ in 0..128 {
        tx.push([0.1, -0.1]).unwrap();
    }
    let mut playback = PlaybackCallback {
        clock: measurement::io::CallbackClock::new(Rate::Hz48000),
        ring: rx_out,
        fault,
        played: Arc::new(AtomicU64::new(0)),
        total: 128,
        pair: irresponsible::domain::StereoPair::default(),
        channels: 2,
        cancel: irresponsible::audio::Cancellation::new(),
        first_timestamp: Arc::new(AtomicU64::new(0)),
        last_timestamp: Arc::new(AtomicU64::new(0)),
        deadline: None,
        meters: Arc::new(irresponsible::audio::Meters::default()),
    };
    let backend_fault = FaultLatch::default();
    let backend_meters = irresponsible::audio::Meters::default();
    let mut output = [0.0; 256];
    let allocations = irresponsible::audit::ALLOCATIONS.load(Ordering::Relaxed);
    let frees = irresponsible::audit::DEALLOCATIONS.load(Ordering::Relaxed);
    capture.process(&[0.1; 128], 0);
    playback.process(&mut output, 0);
    // Exercise clipped input, capture overflow and an error callback as well as
    // the successful audio path. Formatting stays outside these callbacks.
    capture.process(&[1.0; 1024], 2_666_667);
    backend_fault.backend(Direction::Input, cpal::ErrorKind::Xrun, &backend_meters);
    assert_eq!(
        irresponsible::audit::ALLOCATIONS.load(Ordering::Relaxed),
        allocations
    );
    assert_eq!(
        irresponsible::audit::DEALLOCATIONS.load(Ordering::Relaxed),
        frees
    );
    assert_eq!(rx.pop().unwrap().sample, 0.1);
    assert_eq!(output[0], 0.1);
    assert_eq!(output[1], -0.1);
}

#[test]
fn optional_single_speaker_check_preserves_its_recording_in_the_source() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::at(directory.path()).unwrap();
    let (mut plan, mut provider) = setup(&store, Scenario::default());
    let report = measurement::levels::check(
        &plan.source,
        measurement::Speaker::Left,
        &mut provider,
        &irresponsible::audio::Cancellation::new(),
        &store,
    )
    .unwrap();
    let original = report.raw.object;
    plan.levels =
        measurement::levels::Checks::from_reports([Some(report), None], &plan.source).unwrap();
    let (_tx, controls) = mpsc::sync_channel(8);
    let session = Run {
        plan,
        store: &store,
        cancel: irresponsible::audio::Cancellation::new(),
        controls,
        progress: Box::new(|_| {}),
        provider: Box::new(provider),
    }
    .execute()
    .unwrap();
    let session = Session::load(session, &store).unwrap();
    let source = measurement::Source::load(session.source, &store).unwrap();
    assert!(
        source
            .levels
            .as_ref()
            .unwrap()
            .referenced_objects()
            .any(|object| object == original)
    );
    assert!(source.referenced_objects().any(|object| object == original));
}

struct AmbientChange {
    noise_peak: f32,
    inner: simulation::Simulated,
    backgrounds: usize,
    random: u64,
}
impl measurement::io::Provider for AmbientChange {
    fn elapsed(&self) -> std::time::Duration {
        self.inner.elapsed()
    }
    fn set_deadline(&mut self, deadline: std::time::Duration) {
        self.inner.set_deadline(deadline);
    }
    fn set_meters(&mut self, meters: Arc<irresponsible::audio::Meters>) {
        self.inner.set_meters(meters);
    }
    fn record(
        &mut self,
        output: &[irresponsible::dsp::Frame],
        position: usize,
        cancel: &irresponsible::audio::Cancellation,
    ) -> irresponsible::error::Result<measurement::io::Recording> {
        let mut capture = self.inner.record(output, position, cancel)?;
        if output.iter().flatten().all(|&sample| sample == 0.0) {
            self.backgrounds += 1;
        }
        if self.backgrounds > 1 {
            for sample in &mut capture.samples {
                self.random ^= self.random << 13;
                self.random ^= self.random >> 7;
                self.random ^= self.random << 17;
                *sample += ((self.random >> 40) as f32 / 8_388_608.0 - 1.0) * self.noise_peak;
            }
        }
        Ok(capture)
    }
}

#[test]
fn changed_ambient_level_uses_current_uncertainty_and_retains_measurements() {
    for noise_peak in [0.00001, 0.0001] {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::at(directory.path()).unwrap();
        let (mut plan, provider) = setup(&store, Scenario::default());
        plan.recipe.global_effort = 0.5;
        let (_tx, controls) = mpsc::sync_channel(8);
        let id = Run {
            plan,
            store: &store,
            cancel: irresponsible::audio::Cancellation::new(),
            controls,
            progress: Box::new(|_| {}),
            provider: Box::new(AmbientChange {
                noise_peak,
                inner: provider,
                backgrounds: 0,
                random: 0x1234_9876,
            }),
        }
        .execute()
        .unwrap();
        let id = verify_with(
            id,
            &store,
            Box::new(AmbientChange {
                noise_peak,
                inner: simulation::Simulated::new(Scenario::default()).unwrap(),
                backgrounds: 1,
                random: 0x1234_9876,
            }),
        );
        let session = Session::load(id, &store).unwrap();
        let source = measurement::Source::load(session.source, &store).unwrap();
        let history = session.history(&store).unwrap();
        let best = &history[session.best.unwrap().0];
        let trial = history
            .iter()
            .find(|pass| pass.correction().is_some() && !pass.measured.is_empty())
            .unwrap();
        let measured_noise = &trial.measured[0][0].noise;
        assert!(measured_noise.rms_dbfs > source.points[0].noise.rms_dbfs + 10.0);
        assert_ne!(measured_noise.raw.object, source.points[0].noise.raw.object);
        assert!(trial.metrics.noise_uncertainty_db.is_finite());
        if noise_peak < 0.0001 {
            assert_eq!(best.decision, workflow::Decision::Accepted);
            assert!(best.metrics.score() < history[0].metrics.score());
        } else {
            assert!(
                measured_noise.rms_dbfs > -95.0,
                "realistic ADC noise: {}",
                measured_noise.rms_dbfs
            );
            assert!(best.fully_verified());
        }
    }
}

#[test]
fn saved_source_verification_retains_changed_acoustic_level() {
    let (_directory, store, id) = scenario_run(Plant::Room, 0.5);
    let original = Session::load(id, &store).unwrap();
    let original_best = original.best.unwrap();
    let source = measurement::Source::load(original.source, &store).unwrap();
    let (tx, controls) = mpsc::sync_channel(8);
    let id = Run {
        plan: workflow::Plan {
            levels: None,
            source: source.plan.clone(),
            recipe: Recipe {
                target: Target::Flat,
                global_effort: 0.5,
                taps: Taps::Short,
                ..Recipe::default()
            },
            progression: workflow::Progression::Manual,
            kind: workflow::RunKind::Verify {
                pass: original_best,
            },
            session: Some(id),
        },
        store: &store,
        cancel: irresponsible::audio::Cancellation::new(),
        controls,
        progress: Box::new(move |progress| {
            if let workflow::Progress::Move { movement, .. } = progress {
                tx.send(workflow::Control {
                    movement,
                    action: workflow::MoveAction::Ready,
                })
                .unwrap();
            }
        }),
        provider: Box::new(
            simulation::Simulated::new(Scenario {
                input_gain: 0.1 * 10.0_f32.powf(0.6 / 20.0),
                ..Scenario::default()
            })
            .unwrap(),
        ),
    }
    .execute()
    .unwrap();
    let current = Session::load(id, &store).unwrap();
    let history = current.history(&store).unwrap();
    let measured = history.last().unwrap();
    assert_eq!(
        measured.correction(),
        original.history(&store).unwrap()[original_best.0].correction()
    );
    let previous = original.history(&store).unwrap();
    let old = &previous[original_best.0].measured[0][0].response.magnitude;
    let new = &measured.measured[0][0].response.magnitude;
    let delta = old
        .frequency
        .iter()
        .enumerate()
        .filter(|(_, hz)| **hz > 100.0 && **hz < 10000.0)
        .map(|(i, _)| new.db[i] - old.db[i])
        .collect::<Vec<_>>();
    assert!((delta.iter().sum::<f32>() / delta.len() as f32 - 0.6).abs() < 0.03);
    assert_eq!(&current.passes[..original.passes.len()], &original.passes);
    assert!(current.best.is_some());
}

#[test]
fn narrower_reverification_preserves_verified_best_and_records_partial_evidence() {
    let (_directory, store, id) = scenario_run(Plant::Room, 0.5);
    let original = Session::load(id, &store).unwrap();
    let original_best = original.best.unwrap();
    let source = measurement::Source::load(original.source, &store).unwrap();
    let mut acquisition = source.plan.clone();
    acquisition.acquisition.band = measurement::Band::new(100.0, 10000.0).unwrap();
    let (tx, controls) = mpsc::sync_channel(8);
    let id = Run {
        plan: workflow::Plan {
            levels: None,
            source: acquisition,
            recipe: Recipe {
                target: Target::Flat,
                global_effort: 0.5,
                taps: Taps::Short,
                ..Recipe::default()
            },
            progression: workflow::Progression::Manual,
            kind: workflow::RunKind::Verify {
                pass: original_best,
            },
            session: Some(id),
        },
        store: &store,
        cancel: irresponsible::audio::Cancellation::new(),
        controls,
        progress: Box::new(move |progress| {
            if let workflow::Progress::Move { movement, .. } = progress {
                tx.send(workflow::Control {
                    movement,
                    action: workflow::MoveAction::Ready,
                })
                .unwrap();
            }
        }),
        provider: Box::new(simulation::Simulated::new(Scenario::default()).unwrap()),
    }
    .execute()
    .unwrap();
    let current = Session::load(id, &store).unwrap();
    let history = current.history(&store).unwrap();
    let partial = history.last().unwrap();
    assert_eq!(partial.decision, workflow::Decision::Provisional);
    assert!(!partial.fully_verified());
    assert_eq!(current.best, Some(original_best));
    assert_eq!(&current.passes[..original.passes.len()], &original.passes);
    assert!(
        partial
            .measured
            .iter()
            .flatten()
            .all(|take| take.probe.band.low == 100.0)
    );
}
