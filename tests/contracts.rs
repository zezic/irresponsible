use irresponsible::{
    catalog::{Catalog, UPSTREAM_REVISION, parse_csv},
    correction::{Correction, Curve, FirLength, GenerationSettings, correction_curve, spectrum},
    domain::*,
    dsp::{Analysis, Convolver, Engine, PARTITION, Processor},
    storage::{ContentHash, Store},
};
use std::{io::Cursor, sync::Arc};

fn fixture_ir(coefficients: [Vec<f32>; 2]) -> Correction {
    let interleaved: Vec<_> = coefficients[0]
        .iter()
        .zip(&coefficients[1])
        .flat_map(|(&l, &r)| [l, r])
        .collect();
    Correction::decode_wav(&wav(2, 48000, &interleaved), "fixture.wav", Rate::Hz48000).unwrap()
}
fn noise(count: usize) -> Vec<f32> {
    let mut seed = 0xdeadbeef_u32;
    (0..count)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32
        })
        .collect()
}

#[test]
fn fft_convolution_matches_direct_convolution_across_callback_boundaries() {
    let source = noise(2107);
    let impulse: Vec<_> = noise(717).iter().map(|v| v * 0.03).collect();
    let mut convolver = Convolver::new(&impulse);
    let mut padded = source.clone();
    padded.resize(source.len() + impulse.len() + PARTITION, 0.0);
    let mut output = Vec::new();
    let mut offset = 0;
    for size in [1, 7, 193, 32, 256, 511, 3, 104, 1000, 901] {
        let end = (offset + size).min(padded.len());
        output.extend(padded[offset..end].iter().map(|&v| convolver.tick(v)));
        offset = end;
    }
    output.extend(padded[offset..].iter().map(|&v| convolver.tick(v)));
    for (index, &actual) in output.iter().enumerate() {
        let expected = if index < PARTITION {
            0.0
        } else {
            let time = index - PARTITION;
            impulse
                .iter()
                .enumerate()
                .filter_map(|(tap, h)| {
                    time.checked_sub(tap)
                        .and_then(|i| source.get(i))
                        .map(|x| x * h)
                })
                .sum::<f32>()
        };
        assert!(
            (actual - expected).abs() < 3e-5,
            "sample {index}: {actual} vs {expected}"
        );
    }
}

#[test]
fn identity_preserves_stereo_gain_and_has_declared_latency() {
    let controls = Controls {
        volume: VolumeDb::try_from(0.0).unwrap(),
        ..Default::default()
    };
    let mut engine = Engine::new(Arc::new(Correction::identity(Rate::Hz48000)), controls);
    let gain = db_gain(engine.analysis.headroom_db);
    for i in 0..700 {
        let output = engine.tick(
            if i == 0 { [0.7, -0.3] } else { [0.0, 0.0] },
            CorrectionMode::On,
        );
        let expected = if i == PARTITION {
            [0.7 * gain, -0.3 * gain]
        } else {
            [0.0, 0.0]
        };
        for c in 0..2 {
            assert!((output[c] - expected[c]).abs() < 2e-6);
        }
    }
}

#[test]
fn bypass_keeps_headroom_and_aligns_imported_leading_delay() {
    let ir = Arc::new(fixture_ir([vec![0.0, 0.0, 2.0], vec![0.0, 0.0, 2.0]]));
    let controls = Controls {
        correction: CorrectionMode::Bypass,
        ..Default::default()
    };
    let mut engine = Engine::new(ir, controls);
    let headroom = engine.analysis.headroom_db;
    assert!((engine.analysis.wet_db + 6.0206).abs() < 0.001);
    for i in 0..4000 {
        let output = engine.tick(
            if i == 0 { [1.0, 1.0] } else { [0.0, 0.0] },
            if i < 2000 {
                CorrectionMode::Bypass
            } else {
                CorrectionMode::On
            },
        );
        if i == PARTITION + 2 {
            assert!((output[0] - db_gain(headroom)).abs() < 2e-6);
        }
        assert_eq!(headroom, engine.analysis.headroom_db);
    }
}

#[test]
fn pink_reference_power_matches_within_point_two_db() {
    let ir = fixture_ir([vec![1.0, -0.75], vec![1.0, -0.75]]);
    let analysis = Analysis::new(&ir, Controls::default());
    let wet = analysis.pink_rms_gain * db_gain(analysis.wet_db);
    let dry = db_gain(analysis.dry_db);
    assert!(gain_db(wet / dry).abs() < 0.2);
    assert!(analysis.wet_db <= 0.0 && analysis.dry_db <= 0.0);
}

#[test]
fn protection_off_does_not_clip_an_overrange_transient() {
    let ir = Arc::new(fixture_ir([vec![1.0, -1.0], vec![1.0, -1.0]]));
    let controls = Controls {
        comparison: Comparison::Uncompensated,
        ..Default::default()
    };
    let mut engine = Engine::new(ir, controls);
    // A frequency-maximum estimate is deliberately not a hard transient bound.
    let output: Vec<_> = (0..500)
        .map(|i| {
            engine.tick(
                if i == 0 { [4.0, -4.0] } else { [0.0, 0.0] },
                CorrectionMode::On,
            )
        })
        .collect();
    assert!(output.iter().flatten().any(|v| v.abs() > 1.0));
}

#[test]
fn filter_transition_finishes_after_warmup_without_an_instant_gain_jump() {
    let controls = Controls {
        volume: VolumeDb::try_from(0.0).unwrap(),
        comparison: Comparison::Uncompensated,
        ..Default::default()
    };
    let mut processor = Processor::new(
        Box::new(Engine::new(
            Arc::new(Correction::identity(Rate::Hz48000)),
            controls,
        )),
        controls,
    );
    for _ in 0..4000 {
        processor.tick([0.5, -0.5]);
    }
    let ir = Arc::new(fixture_ir([vec![0.2], vec![0.2]]));
    processor.replace(Box::new(Engine::new(ir.clone(), controls)));
    let mut last = processor.tick([0.5, -0.5]).0[0];
    let mut receipts = 0;
    for _ in 0..4000 {
        let (frame, receipt) = processor.tick([0.5, -0.5]);
        assert!((frame[0] - last).abs() < 0.001);
        assert!(frame[0] >= 0.0);
        receipts += usize::from(receipt);
        last = frame[0];
    }
    assert_eq!(receipts, 1);
    assert!(Arc::ptr_eq(&processor.active.correction, &ir));
}

#[test]
fn literal_flat_and_harman_are_distinct_against_upstream_measurements() {
    let mut measured = parse_csv(include_bytes!("fixtures/dt990-pro.csv"), "raw").unwrap();
    measured.center();
    let mut harman = parse_csv(include_bytes!("fixtures/harman-over-ear-2018.csv"), "raw").unwrap();
    harman.center();
    let flat = Curve::new(measured.frequency.clone(), vec![0.0; measured.db.len()]).unwrap();
    let flat_curve = correction_curve(&measured, &flat, Default::default());
    let harman_curve = correction_curve(&measured, &harman, Default::default());
    assert!(flat.db.iter().all(|&db| db == 0.0));
    assert!((harman_curve.at(3000.0) - flat_curve.at(3000.0)).abs() > 3.0);
    for rate in [Rate::Hz44100, Rate::Hz48000] {
        let catalog = Catalog::parse(
            include_str!("fixtures/autoeq-index.md"),
            include_str!("fixtures/autoeq-recommended.md"),
            UPSTREAM_REVISION,
        )
        .unwrap();
        let entry = catalog.search("DT 990 Pro oratory").next().unwrap().1;
        let flat_ir = irresponsible::catalog::generate(
            entry,
            UPSTREAM_REVISION,
            include_bytes!("fixtures/dt990-pro.csv"),
            irresponsible::catalog::TargetData::Flat,
            rate,
            Default::default(),
        )
        .unwrap();
        let harman_ir = irresponsible::catalog::generate(
            entry,
            UPSTREAM_REVISION,
            include_bytes!("fixtures/dt990-pro.csv"),
            irresponsible::catalog::TargetData::Reference(
                catalog.target(entry, Target::Harman).unwrap(),
            ),
            rate,
            Default::default(),
        )
        .unwrap();
        assert_eq!(flat_ir.target, Target::Flat);
        assert_eq!(harman_ir.target, Target::Harman);
        assert!(
            flat_ir
                .target_curve
                .as_ref()
                .unwrap()
                .db
                .iter()
                .all(|&v| v == 0.0)
        );
        let coefficients = &flat_ir.channels[0];
        let bins = spectrum(coefficients, coefficients.len());
        for hz in [30.0_f32, 100.0, 1000.0, 3000.0, 8000.0, 18000.0] {
            let k = (hz * bins.len() as f32 / rate.hz() as f32).round() as usize;
            let actual = gain_db(bins[k].norm());
            let expected = flat_curve.at(k as f32 * rate.hz() as f32 / bins.len() as f32);
            assert!(
                (actual - expected).abs() < 0.02,
                "{rate:?} {hz} Hz: {actual} vs {expected}"
            );
        }
    }
}

#[test]
fn upstream_index_preserves_variants_sources_and_recommendations() {
    let catalog = Catalog::parse(
        include_str!("fixtures/autoeq-index.md"),
        include_str!("fixtures/autoeq-recommended.md"),
        UPSTREAM_REVISION,
    )
    .unwrap();
    let entries: Vec<_> = catalog.search("DT 990").map(|(_, e)| e).collect();
    assert!(entries.iter().any(|e| e.name.contains("250 Ohm")));
    assert!(entries.iter().any(|e| e.name.contains("worn earpads")));
    assert!(entries.iter().any(|e| e.name == "Beyerdynamic DT 990 Pro"
        && e.source == "oratory1990"
        && e.recommended));
    assert!(entries.iter().all(|e| e.name.contains("990")));
}

fn wav(channels: u16, rate: u32, values: &[f32]) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(
            &mut cursor,
            hound::WavSpec {
                channels,
                sample_rate: rate,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        for &value in values {
            writer.write_sample(value).unwrap();
        }
        writer.finalize().unwrap();
    }
    cursor.into_inner()
}

#[test]
fn wav_import_keeps_channel_order_gain_leading_delay_and_tail() {
    let ir = Correction::decode_wav(
        &wav(2, 48000, &[0.0, 0.0, 2.0, -0.4, 0.1, 0.3]),
        "stereo.wav",
        Rate::Hz48000,
    )
    .unwrap();
    assert_eq!(ir.channels, [vec![0.0, 2.0, 0.1], vec![0.0, -0.4, 0.3]]);
    assert_eq!(ir.target, Target::Custom);
    assert!(
        serde_json::to_value(&ir).unwrap()["origin"]["Imported"]
            .get("original_rate")
            .is_some(),
        "cached WAV provenance keeps its compatible flat field layout"
    );
    let mono = Correction::decode_wav(&wav(1, 48000, &[0.0, 0.3, 0.0]), "mono.wav", Rate::Hz48000)
        .unwrap();
    assert_eq!(mono.channels[0], mono.channels[1]);
    assert!(Correction::decode_wav(&wav(1, 48000, &[f32::NAN]), "nan.wav", Rate::Hz48000).is_err());
    assert!(
        Correction::decode_wav(&wav(1, 48000, &[f32::MAX]), "overflow.wav", Rate::Hz48000).is_err()
    );
    assert!(Correction::decode_wav(&wav(3, 48000, &[0.0; 3]), "multi.wav", Rate::Hz48000).is_err());
    assert!(Correction::decode_wav(b"bad file", "bad.wav", Rate::Hz48000).is_err());
}

#[test]
fn integer_wav_encodings_preserve_full_scale() {
    for bits in [16, 24, 32] {
        let mut cursor = Cursor::new(Vec::new());
        let value = 1_i32 << (bits - 2);
        {
            let mut writer = hound::WavWriter::new(
                &mut cursor,
                hound::WavSpec {
                    channels: 1,
                    sample_rate: 44100,
                    bits_per_sample: bits,
                    sample_format: hound::SampleFormat::Int,
                },
            )
            .unwrap();
            writer.write_sample(value).unwrap();
            writer.finalize().unwrap();
        }
        let ir = Correction::decode_wav(&cursor.into_inner(), "pcm.wav", Rate::Hz44100).unwrap();
        assert_eq!(ir.channels[0], [0.5]);
    }
}

#[test]
fn imported_resampling_preserves_transfer_gain() {
    for input_rate in [8000, 44100, 96000, 384000] {
        let impulse =
            Correction::decode_wav(&wav(1, input_rate, &[1.0]), "delta.wav", Rate::Hz48000)
                .unwrap();
        assert!(
            (impulse.channels[0].iter().sum::<f32>() - 1.0).abs() < 0.006,
            "{input_rate} Hz: sum {}",
            impulse.channels[0].iter().sum::<f32>()
        );
    }
    let mut samples = vec![0.0; 3000];
    samples[400] = 0.6;
    samples[600] = 0.2;
    let ir = Correction::decode_wav(&wav(1, 44100, &samples), "rate.wav", Rate::Hz48000).unwrap();
    assert!((ir.channels[0].iter().sum::<f32>() - 0.8).abs() < 0.004);
    assert!(ir.channels[0].len() > (samples.len() as f64 * 48000.0 / 44100.0).ceil() as usize);
    let original = spectrum(&samples, 32768);
    let converted = spectrum(&ir.channels[0], 32768);
    for hz in [100.0_f32, 1000.0, 6000.0] {
        let a = original[(hz * 32768.0 / 44100.0).round() as usize].norm();
        let b = converted[(hz * 32768.0 / 48000.0).round() as usize].norm();
        assert!((gain_db(a) - gain_db(b)).abs() < 0.2);
    }
}

#[test]
fn a_rate_change_reuses_the_managed_original_after_the_import_is_deleted() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("original.wav");
    let store = Store::at(&directory.path().join("managed")).unwrap();
    let original = [0.0, 0.0, 0.8, 0.2];
    std::fs::write(&path, wav(1, 44100, &original)).unwrap();
    let converted = Correction::read_wav(&path, Rate::Hz48000, &store).unwrap();
    std::fs::remove_file(path).unwrap();
    let restored = converted.at_rate(Rate::Hz44100, &store).unwrap();
    assert_eq!(restored.channels[0], original);
    assert_eq!(restored.leading_delays(), [2, 2]);
}

#[test]
fn cache_settings_and_external_validation_keep_invalid_state_out() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::at(dir.path()).unwrap();
    let hash = store.put(b"managed IR").unwrap();
    assert_eq!(store.get(&hash).unwrap(), b"managed IR");
    std::fs::write(dir.path().join("objects").join(hash.as_str()), b"corrupt").unwrap();
    assert!(store.get(&hash).is_err());
    assert!(serde_json::from_str::<ContentHash>("\"../../path\"").is_err());
    assert!(serde_json::from_str::<StereoPair>("[0,0]").is_err());
    assert!(serde_json::from_str::<VolumeDb>("5.0").is_err());
    assert!(serde_json::from_str::<ComparisonTrim>("30.0").is_err());
    let mut settings = store.load().unwrap();
    settings.controls.volume = VolumeDb::try_from(-27.0).unwrap();
    settings
        .presets
        .insert(hash, "Beyerdynamic DT 990 Pro / Harman / 44100 Hz".into());
    store.save(&settings).unwrap();
    assert_eq!(
        store.load().unwrap().presets[&hash],
        "Beyerdynamic DT 990 Pro"
    );
    assert_eq!(store.load().unwrap().controls.volume.db(), -27.0);
    std::fs::write(dir.path().join("settings.json"), b"bad document").unwrap();
    assert!(store.load().is_err());
    assert!(dir.path().join("objects").join(hash.as_str()).exists());
}

#[test]
fn feedback_is_rejected_but_disjoint_virtual_pairs_are_valid() {
    let endpoint = Endpoint {
        uid: DeviceUid::new(cpal::DeviceId::new(cpal::default_host().id(), "test")),
        label: "test".into(),
        pair: StereoPair::default(),
    };
    let mut route = Route {
        capture: endpoint.clone(),
        playback: endpoint,
    };
    assert!(route.validate().is_err());
    route.playback.pair = StereoPair::try_from([2, 3]).unwrap();
    assert!(route.validate().is_ok());
    assert!(route.playback.pair.validate(2).is_err());
}

#[test]
fn published_profiles_keep_gain_and_provenance_when_prepared_at_another_rate() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::at(directory.path()).unwrap();
    let bytes = include_bytes!("fixtures/dt990-pro-250-44100.wav");
    store.put(bytes).unwrap();
    let decoded = Correction::decode_wav(bytes, "DT 990 Pro (250 Ohm)", Rate::Hz44100).unwrap();
    let channels = decoded.channels.clone();
    let published = decoded.into_published(
        "DT 990 Pro (250 Ohm)".into(),
        UPSTREAM_REVISION.into(),
        "crinacle/GRAS%2043AG-7%20over-ear/DT990".into(),
        "crinacle".into(),
    );
    assert_eq!(published.channels, channels);
    assert_eq!(published.target, Target::AutoEq);
    let converted = published.at_rate(Rate::Hz48000, &store).unwrap();
    assert_eq!(converted.target, Target::AutoEq);
    assert!(matches!(
        converted.origin,
        irresponsible::correction::Origin::PublishedAutoEq { .. }
    ));
    assert_eq!(
        converted.at_rate(Rate::Hz44100, &store).unwrap().channels,
        channels
    );
}

#[test]
fn documented_rig_families_generate_their_own_reference_targets_at_both_rates() {
    use irresponsible::{
        catalog::{TargetData, generate},
        correction::{Origin, TrebleExtension},
    };
    let catalog = Catalog::parse(
        include_str!("fixtures/autoeq-index.md"),
        include_str!("fixtures/autoeq-recommended.md"),
        UPSTREAM_REVISION,
    )
    .unwrap();
    let cases: &[(&str, &[u8], Target, &str)] = &[
        (
            "Auriculares%20Argentina/over-ear/Beyerdynamic%20DT%20990%20Pro%20(250%20Ohm)",
            include_bytes!("fixtures/dt990-argentina.csv"),
            Target::Harman,
            "Harman over-ear 2018",
        ),
        (
            "Innerfidelity/in-ear/1MORE%20Crystal%20Piston",
            include_bytes!("fixtures/1more-hms-inear.csv"),
            Target::Harman,
            "HMS II.3 Harman in-ear 2019",
        ),
        (
            "Innerfidelity/over-ear/1MORE%20MK801",
            include_bytes!("fixtures/1more-hms-overear.csv"),
            Target::Harman,
            "HMS II.3 Harman over-ear 2018",
        ),
        (
            "HypetheSonics/Bruel%20&%20Kjaer%205128%20in-ear/1MORE%20Quad%20Driver",
            include_bytes!("fixtures/1more-jm1.csv"),
            Target::Reference,
            "JM-1 with Harman filters",
        ),
        (
            "HypetheSonics/over-ear/Audeze%20LCD-4",
            include_bytes!("fixtures/audeze-lmg5128.csv"),
            Target::Reference,
            "LMG 5128 0.6 without bass",
        ),
        (
            "HypetheSonics/GRAS%20RA0045%20in-ear/1MORE%20Aero%20(ANC%20Off)",
            include_bytes!("fixtures/1more-711.csv"),
            Target::Harman,
            "Harman in-ear 2019",
        ),
    ];
    for &(path, bytes, kind, label) in cases {
        let entry = catalog
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .unwrap();
        let target = catalog.target(entry, kind).unwrap();
        assert_eq!(target.name().label(), label);
        for rate in [Rate::Hz44100, Rate::Hz48000] {
            for treble in [TrebleExtension::Limited, TrebleExtension::AutoEq] {
                for data in [TargetData::Reference(target), TargetData::Flat] {
                    let ir = generate(
                        entry,
                        &catalog.revision,
                        bytes,
                        data,
                        rate,
                        GenerationSettings::default().with_treble(treble),
                    )
                    .unwrap();
                    ir.validate().unwrap();
                    assert!(ir.response(64)[0].db.iter().all(|db| db.is_finite()));
                    if ir.target != Target::Flat {
                        let Origin::AutoEq { target, .. } = &ir.origin else {
                            panic!("generated origin")
                        };
                        assert_eq!(target, &catalog.target(entry, kind).unwrap().description());
                        assert_eq!(ir.target, kind);
                    }
                }
            }
        }
    }
}

#[test]
fn treble_extension_preserves_high_frequency_cuts_and_survives_save_and_rate_changes() {
    use irresponsible::correction::{Origin, TrebleExtension, minimum_phase};
    let measured = Curve::new(
        vec![
            20.0, 100.0, 500.0, 1000.0, 2000.0, 4000.0, 6000.0, 8000.0, 10000.0, 12000.0, 16000.0,
            20000.0,
        ],
        vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 4.0, 8.0, 8.0, 8.0, 8.0, 8.0],
    )
    .unwrap();
    let flat = Curve::new(measured.frequency.clone(), vec![0.0; measured.db.len()]).unwrap();
    let limited = correction_curve(&measured, &flat, GenerationSettings::default());
    let extended = correction_curve(
        &measured,
        &flat,
        GenerationSettings::default().with_treble(TrebleExtension::AutoEq),
    );
    assert!(limited.at(18000.0).abs() < 0.001);
    assert!(extended.at(18000.0) < -7.0);
    assert!((limited.at(1000.0) - extended.at(1000.0)).abs() < 0.001);
    for rate in [Rate::Hz44100, Rate::Hz48000] {
        let coefficients = minimum_phase(
            &extended,
            rate,
            FirLength::default(),
            TrebleExtension::AutoEq,
        );
        let bins = spectrum(&coefficients, coefficients.len());
        assert!(
            bins[bins.len() / 2].norm() < 0.00001,
            "Nyquist amplitude must roll off"
        );
    }
    let catalog = Catalog::parse(
        include_str!("fixtures/autoeq-index.md"),
        include_str!("fixtures/autoeq-recommended.md"),
        UPSTREAM_REVISION,
    )
    .unwrap();
    let entry = catalog.search("DT 990 Pro oratory").next().unwrap().1;
    let old = irresponsible::catalog::generate(
        entry,
        &catalog.revision,
        include_bytes!("fixtures/dt990-pro.csv"),
        irresponsible::catalog::TargetData::Reference(
            catalog.target(entry, Target::Harman).unwrap(),
        ),
        Rate::Hz44100,
        GenerationSettings::default(),
    )
    .unwrap();
    let mut old_json = serde_json::to_value(&old).unwrap();
    for key in serde_json::to_value(GenerationSettings::default())
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
    {
        old_json["origin"]["AutoEq"]
            .as_object_mut()
            .unwrap()
            .remove(key);
    }
    let old: Correction = serde_json::from_value(old_json).unwrap();
    old.at_rate(
        Rate::Hz48000,
        &Store::at(tempfile::tempdir().unwrap().path()).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        old.origin,
        Origin::AutoEq {
            generation, ..
        } if generation == GenerationSettings::default()
    ));
    let ir = irresponsible::catalog::generate(
        entry,
        &catalog.revision,
        include_bytes!("fixtures/dt990-pro.csv"),
        irresponsible::catalog::TargetData::Reference(
            catalog.target(entry, Target::Harman).unwrap(),
        ),
        Rate::Hz44100,
        GenerationSettings::default().with_treble(TrebleExtension::AutoEq),
    )
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let store = Store::at(directory.path()).unwrap();
    let hash = store.put(&serde_json::to_vec(&ir).unwrap()).unwrap();
    let saved: Correction = serde_json::from_slice(&store.get(&hash).unwrap()).unwrap();
    let converted = saved.at_rate(Rate::Hz48000, &store).unwrap();
    assert!(matches!(
        converted.origin,
        Origin::AutoEq {
            generation, ..
        } if generation.treble() == TrebleExtension::AutoEq
    ));
    assert!(converted.response(512)[0].at(18000.0) < -1.0);
    assert!((ir.response(512)[0].at(18000.0) - converted.response(512)[0].at(18000.0)).abs() < 0.1);
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires the installed BlackHole 2ch driver"]
fn blackhole_is_recognized_as_a_virtual_capture_device() {
    let devices = irresponsible::audio::devices().unwrap();
    let blackhole = devices
        .iter()
        .find(|device| device.uid.id().id() == "BlackHole2ch_UID")
        .expect("BlackHole 2ch is installed");
    assert!(
        blackhole.virtual_device,
        "BlackHole must be accepted by system-output routing"
    );
}

#[test]
fn generation_settings_reject_invalid_external_values_and_preserve_legacy_preferences() {
    use irresponsible::correction::GenerationParameter;
    let settings = GenerationSettings::default();
    assert!(settings.with(GenerationParameter::Smoothing, 0.0).is_err());
    assert!(
        settings
            .with(GenerationParameter::BoostLimit, f32::NAN)
            .is_err()
    );
    assert!(settings.with(GenerationParameter::LowUnity, 20.0).is_err());
    let mut document = serde_json::to_value(settings).unwrap();
    document["high_full_hz"] = serde_json::json!(17000.0);
    assert!(serde_json::from_value::<GenerationSettings>(document).is_err());
    let legacy: irresponsible::storage::Settings =
        serde_json::from_str(r#"{"treble_extension":"AutoEq"}"#).unwrap();
    assert_eq!(
        legacy.generation,
        settings.with_treble(irresponsible::correction::TrebleExtension::AutoEq)
    );
}

#[test]
fn custom_generation_changes_the_impulse_and_keeps_its_recipe_across_rates() {
    use irresponsible::correction::{Alignment, GenerationParameter, Normalization, Origin};
    let catalog = Catalog::parse(
        include_str!("fixtures/autoeq-index.md"),
        include_str!("fixtures/autoeq-recommended.md"),
        UPSTREAM_REVISION,
    )
    .unwrap();
    let entry = catalog.search("DT 990 Pro oratory").next().unwrap().1;
    let settings = GenerationSettings::default()
        .with_length(FirLength::Taps4096)
        .with_alignment(Alignment::OneKhz)
        .with_normalization(Normalization::Peak)
        .with(GenerationParameter::Smoothing, 1.0 / 12.0)
        .unwrap()
        .with(GenerationParameter::BoostLimit, 3.0)
        .unwrap();
    let prepare = |settings| {
        irresponsible::catalog::generate(
            entry,
            &catalog.revision,
            include_bytes!("fixtures/dt990-pro.csv"),
            irresponsible::catalog::TargetData::Reference(
                catalog.target(entry, Target::Harman).unwrap(),
            ),
            Rate::Hz44100,
            settings,
        )
        .unwrap()
    };
    let ir = prepare(settings);
    assert_eq!(ir.channels[0].len(), 4096);
    assert!(ir.measured.as_ref().unwrap().at(1000.0).abs() < 0.001);
    assert!(ir.target_curve.as_ref().unwrap().at(1000.0).abs() < 0.001);
    let peak = spectrum(&ir.channels[0], ir.channels[0].len())
        .into_iter()
        .map(|bin| gain_db(bin.norm()))
        .fold(f32::NEG_INFINITY, f32::max);
    assert!((peak + 0.2).abs() < 0.05, "peak {peak}");
    assert!(
        (ir.response(256)[0].at(500.0)
            - prepare(GenerationSettings::default()).response(256)[0].at(500.0))
        .abs()
            > 0.5
    );
    let directory = tempfile::tempdir().unwrap();
    let store = Store::at(directory.path()).unwrap();
    let hash = store.put(&serde_json::to_vec(&ir).unwrap()).unwrap();
    let saved: Correction = serde_json::from_slice(&store.get(&hash).unwrap()).unwrap();
    let converted = saved.at_rate(Rate::Hz48000, &store).unwrap();
    assert_eq!(converted.channels[0].len(), 4096);
    assert!(matches!(converted.origin, Origin::AutoEq {generation, ..} if generation == settings));
    assert!((converted.response(256)[0].at(1000.0) - ir.response(256)[0].at(1000.0)).abs() < 0.1);
}
