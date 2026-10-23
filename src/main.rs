mod render;
mod ui;

fn main() {
    if let Err(error) = run() {
        eprintln!("IRresponsible: {error}");
        std::process::exit(1);
    }
}

fn run() -> irresponsible::error::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("--login-status") => {
            println!("{:?}", irresponsible::platform::login::status()?);
            Ok(())
        }
        Some("--chrome-metrics") => ui::print_chrome_metrics(),
        Some("--catalog-check") => {
            let directory = std::env::args().nth(2).ok_or_else(|| {
                irresponsible::error::invalid("--catalog-check requires a cache directory")
            })?;
            catalog_check(std::path::Path::new(&directory))
        }
        Some("--render") => {
            let path = std::env::args()
                .nth(2)
                .ok_or_else(|| irresponsible::error::invalid("--render requires a PNG path"))?;
            let args: Vec<_> = std::env::args().skip(3).collect();
            let dimensions = match args.as_slice() {
                [] => [1100, 860],
                [width, height] | [width, height, _] | [width, height, _, _] => [
                    width
                        .parse()
                        .map_err(|_| irresponsible::error::invalid("Invalid render width"))?,
                    height
                        .parse()
                        .map_err(|_| irresponsible::error::invalid("Invalid render height"))?,
                ],
                _ => {
                    return Err(irresponsible::error::invalid(
                        "Use --render PATH [WIDTH HEIGHT [SCALE [STATE]]]",
                    ));
                }
            };
            let scale = args
                .get(2)
                .map(|value| value.parse())
                .transpose()
                .map_err(|_| irresponsible::error::invalid("Invalid render scale"))?
                .unwrap_or(1.0);
            let state = args
                .get(3)
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or_default();
            ui::render_preview(std::path::Path::new(&path), dimensions, scale, state)
        }
        Some("--devices") => {
            for device in irresponsible::audio::devices()? {
                println!(
                    "{} | {} | input {} / output {} | {:?}{}",
                    device.name,
                    device.uid.id(),
                    device.input_channels,
                    device.output_channels,
                    device.rate,
                    if device.virtual_device {
                        " | virtual"
                    } else {
                        ""
                    }
                );
            }
            Ok(())
        }
        Some("--probe") => {
            let seconds: u64 = std::env::args()
                .nth(2)
                .unwrap_or_else(|| "5".into())
                .parse()
                .map_err(|_| {
                    irresponsible::error::invalid(
                        "Probe duration must be an integer number of seconds",
                    )
                })?;
            if !(1..=3600).contains(&seconds) {
                return Err(irresponsible::error::invalid(
                    "Probe duration must be 1-3600 seconds",
                ));
            }
            probe(seconds)
        }
        Some("--help") => {
            println!(
                "IRresponsible\n  --login-status   Read the macOS login-item status\n  --devices        List stable device IDs and negotiated formats\n  --probe SECONDS  Exercise BlackHole/Tascam without changing system output\n  --render PATH    Render the window scene to PNG [WIDTH HEIGHT [SCALE [STATE]]]\n  --chrome-metrics  Read the current OS titlebar dimensions\n  --catalog-check DIR  Validate live AutoEq preparation and cache\n  --help           Show this help\n\nRun without arguments to open the listening window."
            );
            Ok(())
        }
        Some(arg) => Err(irresponsible::error::invalid(format!(
            "Unknown option {arg}"
        ))),
        None => ui::run(),
    }
}

fn catalog_check(directory: &std::path::Path) -> irresponsible::error::Result<()> {
    use irresponsible::{
        catalog::Source,
        domain::{Rate, Target},
        dsp::Analysis,
        storage::Store,
    };
    let store = Store::at(directory)?;
    let source = Source::new(store.clone())?;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let catalog = if store.root().join("catalog.json").try_exists()? {
        let cached = source.cached_catalog()?;
        if cached.inventory_known() && cached.metadata_known() {
            cached
        } else {
            source.refresh(&cancel)?
        }
    } else {
        source.refresh(&cancel)?
    };
    println!(
        "AutoEq {}: {} entries",
        catalog.revision,
        catalog.entries.len()
    );
    for filter in irresponsible::catalog::CatalogFilter::ALL {
        println!(
            "{}: {}",
            filter.label(),
            catalog.filtered("", filter).count()
        );
    }
    let measured = catalog
        .entries
        .iter()
        .filter(|entry| entry.has_measurements())
        .count();
    let mapped = catalog
        .entries
        .iter()
        .filter(|entry| entry.has_measurements() && entry.reference.is_some())
        .count();
    println!("Documented measurement references: {mapped}/{measured}");
    let entry = catalog
        .entries
        .iter()
        .find(|entry| entry.path == "oratory1990/over-ear/Beyerdynamic%20DT%20990%20Pro")
        .ok_or_else(|| {
            irresponsible::error::invalid(
                "DT 990 Pro reference fixture is absent from this upstream revision",
            )
        })?;
    for target in [Target::Harman, Target::Flat] {
        let correction = source.prepare(
            &catalog,
            entry,
            target,
            Rate::Hz48000,
            Default::default(),
            &cancel,
        )?;
        let analysis = Analysis::new(&correction, Default::default());
        let hash = store.put(&serde_json::to_vec(&correction)?)?;
        let restored: irresponsible::correction::Correction =
            serde_json::from_slice(&store.get(&hash)?)?;
        restored.validate()?;
        println!(
            "{} / {target:?}: {} taps, boost {:+.2} dB, headroom {:+.2} dB, {}",
            entry.name,
            correction.channels[0].len(),
            analysis.max_boost_db,
            analysis.headroom_db,
            hash.as_str()
        );
    }
    let published = catalog
        .entries
        .iter()
        .find(|entry| {
            entry.path
                == "crinacle/GRAS%2043AG-7%20over-ear/Beyerdynamic%20DT%20990%20Pro%20(250%20Ohm)"
        })
        .ok_or_else(|| irresponsible::error::invalid("250 Ohm reference entry missing"))?;
    for rate in [Rate::Hz44100, Rate::Hz48000] {
        let correction = source.prepare(
            &catalog,
            published,
            Target::AutoEq,
            rate,
            Default::default(),
            &cancel,
        )?;
        println!(
            "{} / published AutoEq / {} Hz / {} taps",
            correction.name,
            rate.hz(),
            correction.channels[0].len()
        );
    }
    Ok(())
}

fn probe(seconds: u64) -> irresponsible::error::Result<()> {
    use irresponsible::{
        audio::{self, Command, Event, Runtime, SystemOutput},
        correction::Correction,
        domain::{Controls, Lifecycle, Rate},
    };
    use std::{
        sync::{Arc, atomic::Ordering},
        time::{Duration, Instant},
    };
    let devices = audio::devices()?;
    let route = audio::default_route(&devices)
        .ok_or_else(|| irresponsible::error::invalid("Probe needs BlackHole 2ch and US-4x4HR"))?;
    println!(
        "Probe: {} -> {}, no system-output change",
        route.capture.label, route.playback.label
    );
    let temporary = tempfile::tempdir()?;
    let store = irresponsible::storage::Store::at(temporary.path())?;
    let runtime = Runtime::new(
        Arc::new(Correction::identity(Rate::Hz44100)),
        Controls::default(),
        store,
    )?;
    runtime.send(Command::Start(route, SystemOutput::Unchanged))?;
    let timeout = Instant::now() + Duration::from_secs(15);
    loop {
        match runtime.events.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::State(Lifecycle::Running {
                rate,
                estimated_latency_ms,
            })) => {
                println!(
                    "Running at {} Hz, estimated pipeline {estimated_latency_ms:.1} ms + device latency",
                    rate.hz()
                );
                break;
            }
            Ok(Event::State(Lifecycle::Failed(error))) => {
                return Err(irresponsible::error::invalid(error));
            }
            _ if Instant::now() > timeout => {
                return Err(irresponsible::error::invalid(
                    "Route did not start within 15 seconds",
                ));
            }
            _ => {}
        }
    }
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end {
        if let Ok(event) = runtime.events.recv_timeout(Duration::from_millis(100))
            && let Event::State(state) = event
        {
            println!("{state:?}");
            if !matches!(state, Lifecycle::Running { .. }) {
                return Err(irresponsible::error::invalid("Route failed during probe"));
            }
        }
    }
    runtime.send(Command::Stop)?;
    let (input, output) = runtime.meters.peaks();
    println!(
        "Frames {}, startup silent frames {}, underrun frames {}, overrun frames {}, backend xruns {}, overload frames {}, queues {}:{}, drift {:+.0} ppm, peaks {input:.5}/{output:.5}",
        runtime.meters.output_frames.load(Ordering::Relaxed),
        runtime.meters.startup_silence.load(Ordering::Relaxed),
        runtime.meters.underruns.load(Ordering::Relaxed),
        runtime.meters.overruns.load(Ordering::Relaxed),
        runtime.meters.backend_xruns.load(Ordering::Relaxed),
        runtime.meters.overloads.load(Ordering::Relaxed),
        runtime.meters.capture_queue.load(Ordering::Relaxed),
        runtime.meters.playback_queue.load(Ordering::Relaxed),
        runtime.meters.drift()
    );
    #[cfg(feature = "alloc-audit")]
    println!(
        "Callback allocations {}, deallocations {}",
        irresponsible::audit::ALLOCATIONS.load(Ordering::Relaxed),
        irresponsible::audit::DEALLOCATIONS.load(Ordering::Relaxed)
    );
    Ok(())
}
