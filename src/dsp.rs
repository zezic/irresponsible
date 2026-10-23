use crate::{
    correction::{Correction, spectrum},
    domain::{Comparison, Controls, CorrectionMode, db_gain, gain_db},
};
use rustfft::{Fft, FftPlanner, num_complex::Complex32};
use std::sync::Arc;

pub type Frame = [f32; 2];
pub const PARTITION: usize = 128;

pub struct Convolver {
    forward: Arc<dyn Fft<f32>>,
    inverse: Arc<dyn Fft<f32>>,
    filters: Vec<Vec<Complex32>>,
    history: Vec<Vec<Complex32>>,
    transform: Vec<Complex32>,
    sum: Vec<Complex32>,
    scratch: Vec<Complex32>,
    input: [f32; PARTITION],
    output: [f32; PARTITION],
    overlap: [f32; PARTITION],
    cursor: usize,
    partition: usize,
}

impl Convolver {
    pub fn new(coefficients: &[f32]) -> Self {
        assert!(!coefficients.is_empty());
        let n = PARTITION * 2;
        let mut planner = FftPlanner::new();
        let forward = planner.plan_fft_forward(n);
        let inverse = planner.plan_fft_inverse(n);
        let scratch_len = forward
            .get_inplace_scratch_len()
            .max(inverse.get_inplace_scratch_len());
        let filters: Vec<_> = coefficients
            .chunks(PARTITION)
            .map(|p| spectrum(p, n))
            .collect();
        Self {
            history: vec![vec![Complex32::default(); n]; filters.len()],
            filters,
            forward,
            inverse,
            transform: vec![Complex32::default(); n],
            sum: vec![Complex32::default(); n],
            scratch: vec![Complex32::default(); scratch_len],
            input: [0.0; PARTITION],
            output: [0.0; PARTITION],
            overlap: [0.0; PARTITION],
            cursor: 0,
            partition: 0,
        }
    }
    pub fn tick(&mut self, sample: f32) -> f32 {
        let result = self.output[self.cursor];
        self.input[self.cursor] = sample;
        self.cursor += 1;
        if self.cursor == PARTITION {
            self.cursor = 0;
            self.transform.fill(Complex32::default());
            for (out, &input) in self.transform.iter_mut().zip(&self.input) {
                out.re = input;
            }
            self.forward
                .process_with_scratch(&mut self.transform, &mut self.scratch);
            self.history[self.partition].copy_from_slice(&self.transform);
            self.sum.fill(Complex32::default());
            for (p, filter) in self.filters.iter().enumerate() {
                let history =
                    &self.history[(self.partition + self.filters.len() - p) % self.filters.len()];
                for ((sum, &h), &x) in self.sum.iter_mut().zip(filter).zip(history) {
                    *sum += h * x;
                }
            }
            self.inverse
                .process_with_scratch(&mut self.sum, &mut self.scratch);
            for i in 0..PARTITION {
                self.output[i] = self.sum[i].re / (PARTITION * 2) as f32 + self.overlap[i];
                self.overlap[i] = self.sum[i + PARTITION].re / (PARTITION * 2) as f32;
            }
            self.partition = (self.partition + 1) % self.filters.len();
        }
        result
    }
}

pub struct Delay {
    samples: Vec<f32>,
    cursor: usize,
}
impl Delay {
    pub fn new(frames: usize) -> Self {
        Self {
            samples: vec![0.0; frames],
            cursor: 0,
        }
    }
    pub fn tick(&mut self, input: f32) -> f32 {
        if self.samples.is_empty() {
            return input;
        }
        let result = self.samples[self.cursor];
        self.samples[self.cursor] = input;
        self.cursor = (self.cursor + 1) % self.samples.len();
        result
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Analysis {
    pub max_boost_db: f32,
    pub pink_rms_gain: f32,
    pub headroom_db: f32,
    pub wet_db: f32,
    pub dry_db: f32,
}
impl Analysis {
    pub fn new(ir: &Correction, controls: Controls) -> Self {
        let n = ir
            .channels
            .iter()
            .map(Vec::len)
            .max()
            .expect("stereo")
            .max(32768)
            .next_power_of_two()
            * 2;
        let mut maximum = 0.0_f32;
        let mut power = 0.0;
        let mut weights = 0.0;
        for channel in &ir.channels {
            let bins = spectrum(channel, n);
            for (k, bin) in bins[..n / 2 + 1].iter().enumerate() {
                maximum = maximum.max(bin.norm());
                let hz = k as f32 * ir.rate.hz() as f32 / n as f32;
                if (20.0..=20000.0_f32.min(ir.rate.hz() as f32 * 0.49)).contains(&hz) {
                    let weight = 1.0 / k as f32;
                    power += weight * bin.norm_sqr();
                    weights += weight;
                }
            }
        }
        let mut result = Self {
            max_boost_db: gain_db(maximum),
            pink_rms_gain: (power / weights).sqrt(),
            headroom_db: 0.0,
            wet_db: 0.0,
            dry_db: 0.0,
        };
        result.configure(controls);
        result
    }
    pub fn configure(&mut self, controls: Controls) {
        let difference = if controls.comparison == Comparison::PinkNoise {
            gain_db(self.pink_rms_gain)
        } else {
            0.0
        };
        self.wet_db = -difference.max(0.0) + controls.trim.db();
        self.dry_db = difference.min(0.0);
        self.headroom_db = -(self.max_boost_db + self.wet_db).max(self.dry_db).max(0.0) - 1.0;
    }
}

pub struct Engine {
    pub correction: Arc<Correction>,
    pub analysis: Analysis,
    convolvers: [Convolver; 2],
    dry: [Delay; 2],
    blend: f32,
    wet_gain: f32,
    dry_gain: f32,
    wet_target: f32,
    dry_target: f32,
    pub warmup: usize,
}
impl Engine {
    pub fn new(correction: Arc<Correction>, controls: Controls) -> Self {
        let analysis = Analysis::new(&correction, controls);
        let delays = correction.leading_delays();
        let convolvers = correction.channels.each_ref().map(|c| Convolver::new(c));
        let dry = delays.map(|n| Delay::new(PARTITION + n));
        let wet_gain = db_gain(analysis.headroom_db + analysis.wet_db);
        let dry_gain = db_gain(analysis.headroom_db + analysis.dry_db);
        let warmup = correction
            .channels
            .iter()
            .map(Vec::len)
            .max()
            .expect("stereo")
            + PARTITION;
        Self {
            correction,
            analysis,
            convolvers,
            dry,
            blend: if controls.correction == CorrectionMode::On {
                1.0
            } else {
                0.0
            },
            wet_gain,
            dry_gain,
            wet_target: wet_gain,
            dry_target: dry_gain,
            warmup,
        }
    }
    pub fn configure(&mut self, controls: Controls) {
        self.analysis.configure(controls);
        self.wet_target = db_gain(self.analysis.headroom_db + self.analysis.wet_db);
        self.dry_target = db_gain(self.analysis.headroom_db + self.analysis.dry_db);
    }
    pub fn tick(&mut self, input: Frame, mode: CorrectionMode) -> Frame {
        let step = 1.0 / (self.correction.rate.hz() as f32 * 0.03);
        self.blend = approach(
            self.blend,
            if mode == CorrectionMode::On { 1.0 } else { 0.0 },
            step,
        );
        self.wet_gain += (self.wet_target - self.wet_gain) * 0.002;
        self.dry_gain += (self.dry_target - self.dry_gain) * 0.002;
        std::array::from_fn(|c| {
            let wet = self.convolvers[c].tick(input[c]);
            let dry = self.dry[c].tick(input[c]);
            dry * self.dry_gain * (1.0 - self.blend) + wet * self.wet_gain * self.blend
        })
    }
}

struct Transition {
    next: Box<Engine>,
    warmup: usize,
    position: usize,
    length: usize,
}

/// Owned by the DSP worker. Callback threads exchange only stereo frames.
pub struct Processor {
    pub active: Box<Engine>,
    transition: Option<Transition>,
    controls: Controls,
    volume: f32,
}
impl Processor {
    pub fn new(engine: Box<Engine>, controls: Controls) -> Self {
        Self {
            active: engine,
            transition: None,
            controls,
            volume: 0.0,
        }
    }
    pub fn controls(&mut self, controls: Controls) {
        self.controls = controls;
        self.active.configure(controls);
        if let Some(transition) = &mut self.transition {
            transition.next.configure(controls);
        }
    }
    pub fn replace(&mut self, mut next: Box<Engine>) {
        assert!(
            self.transition.is_none(),
            "only one admitted filter transition at a time"
        );
        assert_eq!(next.correction.rate, self.active.correction.rate);
        next.configure(self.controls);
        self.transition = Some(Transition {
            warmup: next.warmup,
            length: (next.correction.rate.hz() as f32 * 0.03) as usize,
            position: 0,
            next,
        });
    }
    pub fn transitioning(&self) -> bool {
        self.transition.is_some()
    }
    /// Returns true only after the replacement has finished warming and fading.
    pub fn tick(&mut self, input: Frame) -> (Frame, bool) {
        let mut output = self.active.tick(input, self.controls.correction);
        let mut adopted = false;
        if let Some(t) = &mut self.transition {
            let next = t.next.tick(input, self.controls.correction);
            if t.warmup > 0 {
                t.warmup -= 1;
            } else {
                t.position += 1;
                let blend = t.position as f32 / t.length as f32;
                for c in 0..2 {
                    output[c] = output[c] * (1.0 - blend) + next[c] * blend;
                }
                adopted = t.position == t.length;
            }
        }
        if adopted {
            self.active = self.transition.take().expect("finished transition").next;
        }
        let target = self.controls.volume.gain();
        self.volume = approach(
            self.volume,
            target,
            1.0 / (self.active.correction.rate.hz() as f32 * 0.015),
        );
        output.iter_mut().for_each(|v| *v *= self.volume);
        (output, adopted)
    }
}

fn approach(current: f32, target: f32, step: f32) -> f32 {
    current + (target - current).clamp(-step, step)
}
