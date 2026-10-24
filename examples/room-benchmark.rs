//! No devices or system routing. Measures existing stereo convolution and replacement.
use irresponsible::{
    correction::{
        Correction, Curve,
        room::{self, Taps},
    },
    domain::{Controls, Rate},
    dsp::{Engine, Processor},
};
use std::{hint::black_box, sync::Arc, time::Instant};
fn main() {
    for rate in [Rate::Hz44100, Rate::Hz48000] {
        for taps in [Taps::Short, Taps::Long] {
            let curve = Curve::new(
                vec![20.0, 50.0, 100.0, 1000.0, 10000.0, 20000.0],
                vec![0.0, -6.0, -3.0, 0.0, 1.0, 0.0],
            )
            .unwrap();
            let h = room::synthesize(&curve, rate, taps);
            let mut correction = Correction::identity(rate);
            correction.channels = [h.clone(), h];
            let correction = Arc::new(correction);
            let controls = Controls::default();
            let mut processor = Processor::new(
                Box::new(Engine::new(correction.clone(), controls)),
                controls,
            );
            let replacement = Box::new(Engine::new(correction, controls));
            let frames = rate.hz() as usize * 5;
            let start = Instant::now();
            for i in 0..frames {
                let x = (i as f32 * 0.123).sin() * 0.1;
                black_box(processor.tick([x, -x]));
            }
            let steady = start.elapsed();
            processor.replace(replacement);
            let start = Instant::now();
            for i in 0..taps.frames() + rate.hz() as usize / 10 {
                let x = (i as f32 * 0.123).sin() * 0.1;
                black_box(processor.tick([x, -x]));
            }
            let transition = start.elapsed();
            let transition_audio =
                (taps.frames() + rate.hz() as usize / 10) as f64 / f64::from(rate.hz());
            println!(
                "{} Hz / {} taps / stereo steady {:.2}% of one core / warmup+transition {:.2}% / {:?} steady",
                rate.hz(),
                taps.frames(),
                steady.as_secs_f64() / 5.0 * 100.0,
                transition.as_secs_f64() / transition_audio * 100.0,
                steady
            );
        }
    }
}
