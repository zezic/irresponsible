use super::{SourcePlan, Speaker, Waveform, io::Provider};
use crate::{
    error::{Result, invalid},
    storage::{ContentHash, Store},
};
use rustfft::num_complex::Complex64;
use serde::{Deserialize, Serialize};

pub const RECOMMENDED_CHECK_PEAK: f32 = 0.5;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub signal_path: ContentHash,
    pub speaker: Speaker,
    pub input_peak: f32,
    pub rms_dbfs: f64,
    pub raw: Waveform,
    pub timing: super::CaptureTiming,
}
impl Report {
    pub fn matches(&self, speaker: Speaker, signal_path: ContentHash) -> bool {
        self.speaker == speaker
            && self.signal_path == signal_path
            && self.input_peak.is_finite()
            && self.input_peak > 0.0
            && self.input_peak < super::io::INPUT_CLIPPING_PEAK
            && self.rms_dbfs.is_finite()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checks {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    left: Option<Report>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    right: Option<Report>,
}
impl Checks {
    pub fn referenced_objects(&self) -> impl Iterator<Item = ContentHash> {
        self.left
            .iter()
            .chain(self.right.iter())
            .flat_map(|report| [report.signal_path, report.raw.object])
    }
    pub fn from_reports(reports: [Option<Report>; 2], source: &SourcePlan) -> Result<Option<Self>> {
        let hash = ContentHash::of(&source.level_path()?);
        let [left, right] = reports;
        let left = left.filter(|r| r.matches(Speaker::Left, hash));
        let right = right.filter(|r| r.matches(Speaker::Right, hash));
        if left.is_none() && right.is_none() {
            return Ok(None);
        }
        Ok(Some(Self { left, right }))
    }
    pub fn new(left: Report, right: Report, source: &SourcePlan) -> Result<Self> {
        let checks = Self {
            left: Some(left),
            right: Some(right),
        };
        checks.validate(source)?;
        Ok(checks)
    }
    pub fn validate(&self, source: &SourcePlan) -> Result<()> {
        let hash = ContentHash::of(&source.level_path()?);
        if self.left.is_none() && self.right.is_none() {
            return Err(invalid("Saved speaker checks contain no recordings"));
        }
        for (report, speaker) in [(&self.left, Speaker::Left), (&self.right, Speaker::Right)] {
            if let Some(report) = report
                && !report.matches(speaker, hash)
            {
                return Err(invalid(
                    "Saved speaker level check does not match the selected signal path or has invalid headroom",
                ));
            }
        }
        Ok(())
    }
}

pub fn check(
    source: &SourcePlan,
    speaker: Speaker,
    provider: &mut dyn Provider,
    cancel: &crate::audio::Cancellation,
    store: &Store,
) -> Result<Report> {
    source.validate()?;
    let frames = source.rate.hz() as usize * 3;
    let n = frames.next_power_of_two();
    let mut bins = vec![Complex64::default(); n];
    let mut seed = 0x9e3779b9u64;
    for k in 1..n / 2 {
        let hz = k as f64 * f64::from(source.rate.hz()) / n as f64;
        if !(300.0..=3000.0).contains(&hz) {
            continue;
        }
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        bins[k] = Complex64::from_polar(
            1.0 / hz.sqrt(),
            seed as f64 / u64::MAX as f64 * std::f64::consts::TAU,
        );
        bins[n - k] = bins[k].conj();
    }
    let pink = super::analysis::inverse(bins);
    let peak = pink[..frames]
        .iter()
        .map(|s| s.abs())
        .fold(0.0f32, f32::max);
    let scale = source.gain.probe.gain() as f32 / peak;
    let guard = source.rate.hz() as usize / 2;
    let mut output = vec![[0.0; 2]; frames + guard * 2];
    let fade = source.rate.hz() as usize / 20;
    for (i, frame) in output[guard..guard + frames].iter_mut().enumerate() {
        let edge = i.min(frames - 1 - i).min(fade) as f64 / fade as f64;
        frame[speaker.index()] =
            pink[i] * scale * (std::f64::consts::FRAC_PI_2 * edge).sin().powi(2) as f32;
    }
    let recording = provider.record(&output, source.layout.reference, cancel)?;
    recording.validate(output.len())?;
    let input_peak = recording
        .samples
        .iter()
        .map(|s| s.abs())
        .fold(0.0f32, f32::max);
    let input_fs = recording.rate.hz() as usize;
    let active = &recording.samples[input_fs..input_fs * 3];
    let power = active.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / active.len() as f64;
    let quiet = &recording.samples[..input_fs / 4];
    let noise = quiet.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / quiet.len() as f64;
    let margin_db = 10.0 * (power.max(1e-24) / noise.max(1e-24)).log10();
    if margin_db < 10.0 {
        return Err(invalid(format!(
            "The speaker check could not distinguish the signal from background: active recording {:.1} dBFS, preceding background {:.1} dBFS, difference {margin_db:.1} dB (required 10 dB). Check the selected microphone and speaker. The optional check does not prevent a sweep measurement.",
            10.0 * power.max(1e-24).log10(),
            10.0 * noise.max(1e-24).log10(),
        )));
    }
    Ok(Report {
        signal_path: store.put(&source.level_path()?)?,
        speaker,
        input_peak,
        rms_dbfs: 10.0 * power.max(1e-24).log10(),
        raw: Waveform::save(&recording.samples, recording.rate, store)?,
        timing: recording.input,
    })
}
