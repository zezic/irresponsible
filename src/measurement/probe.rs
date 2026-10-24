use super::{Acquisition, Band, GainContract, Speaker};
use crate::{
    correction::Correction,
    domain::Rate,
    dsp::{Convolver, Frame, PARTITION},
    error::{Result, invalid},
    storage::ContentHash,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Recipe {
    pub policy: String,
    pub rate: Rate,
    pub band: Band,
    pub seconds: f64,
    pub tail_seconds: f64,
    /// Known FIR ringout added before the complete acoustic decay interval.
    #[serde(default)]
    pub filter_tail_frames: usize,
    pub gain: GainContract,
    pub marker_frames: [usize; 2],
    pub diagnostic_marker_frame: usize,
    pub sweep_start: usize,
    pub sweep_frames: usize,
    pub total_frames: usize,
}
pub struct Probe {
    pub recipe: Recipe,
    pub reference: Vec<f32>,
    pub output: Vec<Frame>,
    pub marker: Vec<f32>,
    pub output_identity: ContentHash,
}

/// Each ordinary or LF take has its own exact discrete reference and inverse.
pub fn ess(rate: Rate, band: Band, seconds: f64, amplitude: f64) -> Vec<f32> {
    let fs = f64::from(rate.hz());
    let n = (seconds * fs).round() as usize;
    let l = seconds / (band.high / band.low).ln();
    let fade = (fs * 0.01).round() as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / fs;
            let edge = i.min(n - 1 - i).min(fade) as f64 / fade as f64;
            let window = (std::f64::consts::PI * edge / 2.0).sin().powi(2);
            (amplitude * window * (std::f64::consts::TAU * band.low * l * (t / l).exp_m1()).sin())
                as f32
        })
        .collect()
}

/// Deterministic high-frequency coded reference, always on the left speaker.
pub fn marker(rate: Rate, amplitude: f64) -> Vec<f32> {
    coded_marker(rate, amplitude, 0x9e3779b9)
}
pub fn diagnostic_marker(rate: Rate, amplitude: f64) -> Vec<f32> {
    coded_marker(rate, amplitude, 0x6a09e667)
}
fn coded_marker(rate: Rate, amplitude: f64, mut state: u32) -> Vec<f32> {
    use rustfft::num_complex::Complex64;
    let n = 2048;
    let mut bins = vec![Complex64::default(); n];
    for k in 1..n / 2 {
        let hz = k as f64 * f64::from(rate.hz()) / n as f64;
        if !(2000.0..=8000.0).contains(&hz) {
            continue;
        }
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        bins[k] = Complex64::from_polar(
            1.0,
            f64::from(state) / f64::from(u32::MAX) * std::f64::consts::TAU,
        );
        bins[n - k] = bins[k].conj();
    }
    let mut samples = super::analysis::inverse(bins);
    for (i, s) in samples.iter_mut().enumerate() {
        *s *= (std::f64::consts::PI * i as f64 / (n - 1) as f64)
            .sin()
            .powi(2) as f32;
    }
    let peak = samples.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
    samples
        .iter_mut()
        .for_each(|v| *v *= amplitude as f32 / peak);
    samples
}

impl Recipe {
    pub fn new(rate: Rate, settings: &Acquisition, gain: &GainContract) -> Result<Self> {
        Self::with_filter_support(rate, settings, gain, 0)
    }

    pub fn with_filter_support(
        rate: Rate,
        settings: &Acquisition,
        gain: &GainContract,
        filter_tail_frames: usize,
    ) -> Result<Self> {
        settings.validate(rate)?;
        if filter_tail_frames >= crate::correction::MAX_IR_FRAMES {
            return Err(invalid("Probe filter support exceeds the FIR frame limit"));
        }
        let fs = rate.hz() as usize;
        let start_marker = fs / 2;
        let diagnostic_marker_frame = start_marker + 2048 + fs / 2;
        let sweep_start = diagnostic_marker_frame + 2048 + fs;
        let sweep_frames = (settings.sweep_seconds * fs as f64).round() as usize;
        let tail = (settings.tail_seconds * fs as f64).ceil() as usize + filter_tail_frames;
        let end_marker = sweep_start + sweep_frames + tail + fs / 2;
        Ok(Self {
            policy: "ess-discrete-inverse-v1".into(),
            rate,
            band: settings.band,
            seconds: settings.sweep_seconds,
            tail_seconds: tail as f64 / fs as f64,
            filter_tail_frames,
            gain: gain.clone(),
            marker_frames: [start_marker, end_marker],
            diagnostic_marker_frame,
            sweep_start,
            sweep_frames,
            total_frames: end_marker + 2048 + fs,
        })
    }

    pub fn acoustic_tail_seconds(&self) -> f64 {
        self.tail_seconds - self.filter_tail_frames as f64 / f64::from(self.rate.hz())
    }
}

impl Probe {
    pub fn new(
        rate: Rate,
        settings: &Acquisition,
        gain: &GainContract,
        speaker: Speaker,
        candidate: Option<&Correction>,
    ) -> Result<Self> {
        settings.validate(rate)?;
        if let Some(c) = candidate {
            c.validate()?;
            if c.rate != rate {
                return Err(invalid("Candidate and probe rates differ"));
            }
        }
        let fs = rate.hz() as usize;
        let reference = ess(
            rate,
            settings.band,
            settings.sweep_seconds,
            gain.probe.gain(),
        );
        let marker = marker(rate, gain.probe.gain());
        let filter_tail_frames = candidate.map_or(0, |c| {
            c.channels.iter().map(Vec::len).max().expect("stereo FIR") - 1
        });
        let recipe = Recipe::with_filter_support(rate, settings, gain, filter_tail_frames)?;
        let [start_marker, end_marker] = recipe.marker_frames;
        let sweep_start = recipe.sweep_start;
        let tail = (recipe.tail_seconds * fs as f64).round() as usize;
        let total = recipe.total_frames;
        let mut output = vec![[0.0; 2]; total];
        for &at in &[start_marker, end_marker] {
            for (i, &s) in marker.iter().enumerate() {
                output[at + i][0] = s;
            }
        }
        for (i, &s) in diagnostic_marker(rate, gain.probe.gain())
            .iter()
            .enumerate()
        {
            output[recipe.diagnostic_marker_frame + i][0] = s;
        }
        if let Some(c) = candidate {
            let coeff = &c.channels[speaker.index()];
            let mut filter = Convolver::new(coeff);
            // Remove only the known block implementation delay, retaining the
            // complete FIR effect and deliberately encoded channel delays.
            for i in 0..reference.len() + tail + PARTITION {
                let value = filter.tick(reference.get(i).copied().unwrap_or(0.0));
                if i >= PARTITION {
                    output[sweep_start + i - PARTITION][speaker.index()] = value;
                }
            }
        } else {
            for (frame, &s) in output[sweep_start..].iter_mut().zip(&reference) {
                frame[speaker.index()] = s;
            }
        }
        let limit = gain.probe.gain() * 10f64.powf(gain.correction_reserve_db / 20.0);
        if output
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || f64::from(v.abs()) > limit.min(0.95))
        {
            return Err(invalid(
                "Rendered probe exceeds the frozen measurement gain reserve",
            ));
        }
        let bytes: Vec<_> = output
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let output_identity = ContentHash::of(&bytes);
        Ok(Self {
            recipe,
            reference,
            output,
            marker,
            output_identity,
        })
    }
}
