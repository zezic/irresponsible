//! CLI integration driver. Always uses an isolated store and emulated endpoints.
use super::{
    simulation::{self, Scenario},
    workflow::{self, Control, Progress},
};
use crate::{
    audio::{Backend, Command, Event, Runtime, SystemOutput},
    correction::Correction,
    domain::{Controls, Lifecycle},
    error::{Result, invalid},
    storage::{ContentHash, Store},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub fn run(scenario: Scenario, store: &Store) -> Result<ContentHash> {
    let rate = scenario.rate;
    let runtime = Runtime::with_backend(
        Arc::new(Correction::identity(rate)),
        Controls::default(),
        store.clone(),
        Backend::Simulated(scenario),
    )?;
    runtime.send(Command::Start(simulation::route(), SystemOutput::Unchanged))?;
    let started = Instant::now();
    loop {
        match runtime.events.recv_timeout(Duration::from_millis(100)) {
            Ok(Event::State(Lifecycle::Running { .. })) => break,
            Ok(Event::State(Lifecycle::Failed(reason))) => return Err(invalid(reason)),
            _ if started.elapsed() > Duration::from_secs(10) => {
                return Err(invalid("Emulated listening did not start"));
            }
            _ => {}
        }
    }
    let mut source = simulation::plan(rate);
    source.acquisition.sweep_seconds = 1.0;
    source.acquisition.tail_seconds = 1.0;
    source.acquisition.noise_seconds = 1.0;
    let mut reports = Vec::new();
    for speaker in super::Speaker::ALL {
        runtime.send(Command::CheckLevels(Box::new(source.clone()), speaker))?;
        loop {
            match runtime.events.recv_timeout(Duration::from_millis(100)) {
                Ok(Event::Levels(_, report)) => {
                    reports.push(report);
                    runtime.send(Command::CancelMeasurement)?;
                    break;
                }
                Ok(Event::Measurement(_, Progress::Failed(reason))) => return Err(invalid(reason)),
                _ if started.elapsed() > Duration::from_secs(30) => {
                    return Err(invalid("Level check timed out"));
                }
                _ => {}
            }
        }
    }
    let right = reports.pop().expect("right level check");
    let left = reports.pop().expect("left level check");
    let levels = super::levels::Checks::new(left, right, &source)?;
    let plan = workflow::Plan {
        levels: Some(levels),
        source,
        recipe: crate::correction::room::Recipe::default(),
        progression: workflow::Progression::Manual,
        kind: workflow::RunKind::Measure,
        session: None,
    };
    runtime.send(Command::Measure(Box::new(plan)))?;
    loop {
        match runtime.events.recv_timeout(Duration::from_millis(100)) {
            Ok(Event::Measurement(run, progress)) => match progress {
                Progress::Finished { session, reason } => {
                    println!("Finished: {reason:?} / {}", session.as_str());
                    let session_data = workflow::Session::load(session, store)?;
                    for p in session_data.history(store)? {
                        println!(
                            "Pass {} / {:?} / {:.3} dB / {:?}",
                            p.id.0,
                            p.decision,
                            p.metrics.score(),
                            p.scope
                        );
                    }
                    return Ok(session);
                }
                Progress::Levels { speaker } => println!("Manual {speaker:?} levels"),
                Progress::Failed(reason) => return Err(invalid(reason)),
                Progress::Move { movement, .. } => {
                    println!("Explicit emulated mic placement: {}", movement.position());
                    runtime.send(Command::MeasurementControl {
                        run,
                        control: Control {
                            movement,
                            action: workflow::MoveAction::Ready,
                        },
                    })?;
                }
                Progress::Source { session } => {
                    println!("Saved source session {}", session.as_str())
                }
                Progress::Noise { position, noise } => println!(
                    "Noise at {position}: {:.1} dBFS RMS, variation {:.1}/{:.1} dB, peak-to-average {:.1}/{:.1} dB (measured/limit)",
                    noise.rms_dbfs,
                    noise.window_variation_db,
                    noise.limits.maximum_variation_db,
                    noise.crest_db,
                    noise.limits.maximum_crest_db,
                ),
                Progress::Background { position } => println!("Background at {position}"),
                Progress::Sweep {
                    position,
                    speaker,
                    purpose,
                    band,
                } => println!(
                    "Point {position} / {speaker:?} / {purpose:?} / {:.0}-{:.0} Hz",
                    band.low, band.high
                ),
                Progress::Fitting { band } => {
                    println!("Fitting / {:.0}-{:.0} Hz", band.low, band.high)
                }
                Progress::Pass { record, .. } => println!(
                    "Recorded pass {}: {:?} / {:.3} dB",
                    record.id.0,
                    record.decision,
                    record.metrics.score()
                ),
            },
            _ if started.elapsed() > Duration::from_secs(600) => {
                runtime.send(Command::CancelMeasurement)?;
                return Err(invalid("Simulation exceeded ten minutes"));
            }
            _ => {}
        }
    }
}
