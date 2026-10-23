//! Bounded audio telemetry and UI-side stereo spectrum analysis.
use crate::{
    domain::Rate,
    dsp::Frame,
    error::{Result, invalid},
    frame_schedule::FrameRate,
};
use rustfft::{Fft, FftPlanner, num_complex::Complex32};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    Input,
    #[default]
    Output,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
    Samples2048,
    #[default]
    Samples4096,
    Samples8192,
    Samples16384,
}
impl Resolution {
    pub fn frames(self) -> usize {
        match self {
            Self::Samples2048 => 2048,
            Self::Samples4096 => 4096,
            Self::Samples8192 => 8192,
            Self::Samples16384 => 16384,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Smoothing {
    Fast,
    #[default]
    Medium,
    Slow,
}
impl Smoothing {
    fn seconds(self) -> f32 {
        match self {
            Self::Fast => 0.06,
            Self::Medium => 0.25,
            Self::Slow => 1.0,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Range {
    Db60,
    #[default]
    Db90,
    Db120,
}
impl Range {
    pub fn floor(self) -> f32 {
        match self {
            Self::Db60 => -60.0,
            Self::Db90 => -90.0,
            Self::Db120 => -120.0,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "IntensityValue", into = "f32")]
pub struct Intensity(f32);
impl Default for Intensity {
    fn default() -> Self {
        Self(12.0)
    }
}
impl Intensity {
    pub fn percent(self) -> f32 {
        self.0
    }
    pub fn alpha(self) -> f32 {
        self.0 / 100.0
    }
}
impl TryFrom<f32> for Intensity {
    type Error = crate::error::Error;
    fn try_from(value: f32) -> Result<Self> {
        if !value.is_finite() || !(0.0..=100.0).contains(&value) {
            return Err(invalid("Spectrum intensity must be between 0 and 100%"));
        }
        Ok(Self(value))
    }
}
impl From<Intensity> for f32 {
    fn from(value: Intensity) -> Self {
        value.0
    }
}
#[derive(Deserialize)]
#[serde(untagged)]
enum IntensityValue {
    Percent(f32),
    Named(NamedIntensity),
}
#[derive(Deserialize)]
enum NamedIntensity {
    Subtle,
    Medium,
    Strong,
}
impl TryFrom<IntensityValue> for Intensity {
    type Error = crate::error::Error;
    fn try_from(value: IntensityValue) -> Result<Self> {
        Self::try_from(match value {
            IntensityValue::Percent(value) => value,
            IntensityValue::Named(NamedIntensity::Subtle) => 12.0,
            IntensityValue::Named(NamedIntensity::Medium) => 22.0,
            IntensityValue::Named(NamedIntensity::Strong) => 34.0,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    pub source: Source,
    pub resolution: Resolution,
    pub smoothing: Smoothing,
    pub range: Range,
    pub frame_rate: FrameRate,
    #[serde(alias = "strength")]
    pub intensity: Intensity,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            source: Source::default(),
            resolution: Resolution::default(),
            smoothing: Smoothing::default(),
            range: Range::default(),
            frame_rate: FrameRate::default(),
            intensity: Intensity::default(),
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Source,
    Resolution,
    Smoothing,
    Range,
    FrameRate,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Source(Source),
    Resolution(Resolution),
    Smoothing(Smoothing),
    Range(Range),
    FrameRate(FrameRate),
}
impl Setting {
    pub const ALL: [Self; 5] = [
        Self::Source,
        Self::Resolution,
        Self::Smoothing,
        Self::Range,
        Self::FrameRate,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Source => "Signal",
            Self::Resolution => "FFT size",
            Self::Smoothing => "Smoothing",
            Self::Range => "Level range",
            Self::FrameRate => "Refresh rate",
        }
    }
    pub fn choices(self) -> impl Iterator<Item = Choice> {
        let choices: &[Choice] = match self {
            Self::Source => &[
                Choice::Source(Source::Input),
                Choice::Source(Source::Output),
            ],
            Self::Resolution => &[
                Choice::Resolution(Resolution::Samples2048),
                Choice::Resolution(Resolution::Samples4096),
                Choice::Resolution(Resolution::Samples8192),
                Choice::Resolution(Resolution::Samples16384),
            ],
            Self::Smoothing => &[
                Choice::Smoothing(Smoothing::Fast),
                Choice::Smoothing(Smoothing::Medium),
                Choice::Smoothing(Smoothing::Slow),
            ],
            Self::Range => &[
                Choice::Range(Range::Db60),
                Choice::Range(Range::Db90),
                Choice::Range(Range::Db120),
            ],
            Self::FrameRate => &[],
        };
        let frames = if self == Self::FrameRate {
            &FrameRate::ALL[..]
        } else {
            &[]
        };
        choices
            .iter()
            .copied()
            .chain(frames.iter().copied().map(Choice::FrameRate))
    }
}
impl Choice {
    pub fn label(self) -> std::borrow::Cow<'static, str> {
        let label = match self {
            Self::Source(Source::Input) => "Input / before IR",
            Self::Source(Source::Output) => "Output / after IR",
            Self::Resolution(Resolution::Samples2048) => "2048",
            Self::Resolution(Resolution::Samples4096) => "4096",
            Self::Resolution(Resolution::Samples8192) => "8192",
            Self::Resolution(Resolution::Samples16384) => "16384",
            Self::Smoothing(Smoothing::Fast) => "Fast / 60 ms",
            Self::Smoothing(Smoothing::Medium) => "Medium / 250 ms",
            Self::Smoothing(Smoothing::Slow) => "Slow / 1 s",
            Self::Range(Range::Db60) => "-60 to 0 dBFS",
            Self::Range(Range::Db90) => "-90 to 0 dBFS",
            Self::Range(Range::Db120) => "-120 to 0 dBFS",
            Self::FrameRate(rate) => return rate.label().into(),
        };
        label.into()
    }
}
impl Settings {
    pub fn selected(self, setting: Setting) -> Choice {
        match setting {
            Setting::Source => Choice::Source(self.source),
            Setting::Resolution => Choice::Resolution(self.resolution),
            Setting::Smoothing => Choice::Smoothing(self.smoothing),
            Setting::Range => Choice::Range(self.range),
            Setting::FrameRate => Choice::FrameRate(self.frame_rate),
        }
    }
    pub fn choose(&mut self, choice: Choice) {
        match choice {
            Choice::Source(value) => self.source = value,
            Choice::Resolution(value) => self.resolution = value,
            Choice::Smoothing(value) => self.smoothing = value,
            Choice::Range(value) => self.range = value,
            Choice::FrameRate(value) => self.frame_rate = value,
        }
    }
}

/// The display enables telemetry only while its display is visible. No audio locks.
#[derive(Default)]
pub struct Capture(AtomicBool);
impl Capture {
    pub fn set_enabled(&self, enabled: bool) {
        self.0.store(enabled, Ordering::Relaxed);
    }
    pub fn enabled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}
#[derive(Debug)]
struct Sample {
    index: u64,
    input: Frame,
    output: Frame,
}
pub struct Tap {
    samples: rtrb::Producer<Sample>,
    next: u64,
}
#[derive(Debug)]
pub struct Stream {
    samples: rtrb::Consumer<Sample>,
    rate: Rate,
}
pub fn channel(rate: Rate) -> (Tap, Stream) {
    let (samples, receiver) = rtrb::RingBuffer::new(32768);
    (
        Tap { samples, next: 0 },
        Stream {
            samples: receiver,
            rate,
        },
    )
}
impl Tap {
    /// A full visualization queue drops telemetry; playback never waits for it.
    pub fn push(&mut self, input: Frame, output: Frame) {
        let _ = self.samples.push(Sample {
            index: self.next,
            input,
            output,
        });
        self.next += 1;
    }
    pub fn skip(&mut self, frames: usize) {
        self.next += frames as u64;
    }
}

pub struct Analyzer {
    stream: Option<Stream>,
    settings: Settings,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    gain: f32,
    history: Vec<Frame>,
    cursor: usize,
    filled: usize,
    elapsed: usize,
    expected: Option<u64>,
    scratch: Vec<Complex32>,
    buffer: Vec<Complex32>,
    power: Vec<f32>,
    smoothed: Vec<f32>,
    ready: bool,
}
impl Analyzer {
    pub fn new(settings: Settings) -> Self {
        let n = settings.resolution.frames();
        let fft = FftPlanner::new().plan_fft_forward(n);
        let scratch = vec![Complex32::default(); fft.get_inplace_scratch_len()];
        let window: Vec<_> = (0..n)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())
            .collect();
        let gain = 2.0 / window.iter().sum::<f32>();
        Self {
            stream: None,
            settings,
            fft,
            window,
            gain,
            history: vec![[0.0; 2]; n],
            cursor: 0,
            filled: 0,
            elapsed: 0,
            expected: None,
            scratch,
            buffer: vec![Complex32::default(); n],
            power: vec![0.0; n / 2 + 1],
            smoothed: vec![0.0; n / 2 + 1],
            ready: false,
        }
    }
    pub fn configure(&mut self, settings: Settings) {
        if settings.resolution != self.settings.resolution {
            let stream = self.stream.take();
            *self = Self::new(settings);
            self.stream = stream;
        } else {
            if settings.source != self.settings.source || settings.enabled != self.settings.enabled
            {
                self.reset();
            }
            self.settings = settings;
        }
    }
    pub fn attach(&mut self, stream: Stream) {
        self.reset();
        self.stream = Some(stream);
    }
    pub fn detach(&mut self) {
        self.stream = None;
        self.reset();
    }
    fn reset(&mut self) {
        self.cursor = 0;
        self.filled = 0;
        self.elapsed = 0;
        self.expected = None;
        self.ready = false;
    }
    pub fn update(&mut self) -> bool {
        let Some(mut stream) = self.stream.take() else {
            return false;
        };
        let was_ready = self.ready;
        let available = stream.samples.slots();
        for _ in 0..available {
            let sample = stream
                .samples
                .pop()
                .expect("observed spectrum samples remain available to their only consumer");
            if !self.settings.enabled {
                continue;
            }
            if self.expected.is_some_and(|index| index != sample.index) {
                self.reset();
            }
            self.expected = Some(sample.index + 1);
            self.history[self.cursor] = match self.settings.source {
                Source::Input => sample.input,
                Source::Output => sample.output,
            };
            self.cursor = (self.cursor + 1) % self.history.len();
            self.filled = (self.filled + 1).min(self.history.len());
            self.elapsed += 1;
        }
        let changed = self.filled == self.history.len() && self.elapsed > 0;
        if changed {
            self.transform(stream.rate);
        }
        self.stream = Some(stream);
        changed || (was_ready && !self.ready)
    }
    fn transform(&mut self, rate: Rate) {
        self.power.fill(0.0);
        let n = self.history.len();
        for channel in 0..2 {
            for (i, value) in self.buffer.iter_mut().enumerate() {
                *value = Complex32::new(
                    self.history[(self.cursor + i) % n][channel] * self.window[i],
                    0.0,
                );
            }
            self.fft
                .process_with_scratch(&mut self.buffer, &mut self.scratch);
            for (i, (power, value)) in self.power.iter_mut().zip(&self.buffer).enumerate() {
                let gain = if i == 0 || i == n / 2 {
                    self.gain * 0.5
                } else {
                    self.gain
                };
                *power += value.norm_sqr() * gain * gain * 0.5;
            }
        }
        if self.ready {
            let blend = 1.0
                - (-(self.elapsed as f32) / (rate.hz() as f32 * self.settings.smoothing.seconds()))
                    .exp();
            for (smooth, power) in self.smoothed.iter_mut().zip(&self.power) {
                *smooth += blend * (power - *smooth);
            }
        } else {
            self.smoothed.copy_from_slice(&self.power);
        }
        self.elapsed = 0;
        self.ready = true;
    }
    /// Stereo mean power, with a full-scale bin-centered sine at 0 dBFS.
    pub fn bins(&self) -> impl Iterator<Item = (f32, f32)> + '_ {
        self.stream
            .iter()
            .filter(|_| self.ready)
            .flat_map(move |stream| {
                self.smoothed.iter().enumerate().map(move |(i, &power)| {
                    (
                        i as f32 * stream.rate.hz() as f32 / self.history.len() as f32,
                        10.0 * power.max(1e-12).log10(),
                    )
                })
            })
    }
    pub fn ready(&self) -> bool {
        self.ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tones(analyzer: &mut Analyzer, tap: &mut Tap, rate: Rate, amplitude: f32, frames: usize) {
        for i in 0..frames {
            let value = amplitude
                * (std::f32::consts::TAU * 64.0 * i as f32 / analyzer.history.len() as f32).sin();
            tap.push([value, -value], [value * 0.5, -value * 0.5]);
        }
        assert!(analyzer.update(), "complete window at {} Hz", rate.hz());
    }
    #[test]
    fn stereo_levels_frequency_and_source_at_both_route_rates() {
        for rate in [Rate::Hz44100, Rate::Hz48000] {
            let settings = Settings {
                source: Source::Input,
                ..Settings::default()
            };
            let mut analyzer = Analyzer::new(settings);
            let (mut tap, stream) = channel(rate);
            analyzer.attach(stream);
            tones(&mut analyzer, &mut tap, rate, 0.5, 4096);
            let (hz, db) = analyzer.bins().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
            assert!((hz - rate.hz() as f32 / 64.0).abs() < 0.01);
            assert!(
                (db + 6.0206).abs() < 0.01,
                "opposite channel polarity must not cancel: {db}"
            );
            analyzer.configure(Settings::default());
            tones(&mut analyzer, &mut tap, rate, 0.5, 4096);
            let db = analyzer.bins().map(|(_, db)| db).fold(-120.0, f32::max);
            assert!((db + 12.0412).abs() < 0.01);
            let (mut tap, stream) = channel(rate);
            analyzer.attach(stream);
            tones(&mut analyzer, &mut tap, rate, 0.0001, 4096);
            let db = analyzer.bins().map(|(_, db)| db).fold(-120.0, f32::max);
            assert!(
                (db + 86.0206).abs() < 0.01,
                "a fresh quiet stream must not inherit old power: {db}"
            );
        }
    }
    #[test]
    fn smoothing_silence_and_stream_replacement() {
        let mut analyzer = Analyzer::new(Settings::default());
        let (mut tap, stream) = channel(Rate::Hz44100);
        analyzer.attach(stream);
        tones(&mut analyzer, &mut tap, Rate::Hz44100, 1.0, 4096);
        let before = analyzer.bins().nth(64).unwrap().1;
        tones(&mut analyzer, &mut tap, Rate::Hz44100, 0.0, 4096);
        let after = analyzer.bins().nth(64).unwrap().1;
        assert!(after < before && after > -10.0);
        let (mut tap, stream) = channel(Rate::Hz48000);
        analyzer.attach(stream);
        assert!(!analyzer.ready());
        tones(&mut analyzer, &mut tap, Rate::Hz48000, 0.0, 4096);
        assert!(analyzer.bins().all(|(_, db)| db == -120.0));
    }
    #[test]
    fn overflow_and_disabled_gaps_require_a_fresh_window() {
        let mut analyzer = Analyzer::new(Settings::default());
        let (mut tap, stream) = channel(Rate::Hz44100);
        analyzer.attach(stream);
        for _ in 0..40000 {
            tap.push([0.0; 2], [0.0; 2]);
        }
        assert!(analyzer.update());
        tap.push([0.0; 2], [0.0; 2]);
        assert!(
            analyzer.update(),
            "clearing a discontinuous display requests a redraw"
        );
        assert!(!analyzer.ready());
        tones(&mut analyzer, &mut tap, Rate::Hz44100, 0.5, 4096);
        tap.skip(128);
        tap.push([0.0; 2], [0.0; 2]);
        assert!(
            analyzer.update(),
            "clearing a discontinuous display requests a redraw"
        );
        assert!(!analyzer.ready());
    }
    #[test]
    fn resolution_changes_replan_and_use_a_complete_new_window() {
        let mut analyzer = Analyzer::new(Settings::default());
        let (mut tap, stream) = channel(Rate::Hz44100);
        analyzer.attach(stream);
        tones(&mut analyzer, &mut tap, Rate::Hz44100, 1.0, 4096);
        analyzer.configure(Settings {
            resolution: Resolution::Samples16384,
            ..Settings::default()
        });
        assert!(!analyzer.ready());
        for _ in 0..4096 {
            tap.push([0.0; 2], [0.0; 2]);
        }
        assert!(!analyzer.update());
        tones(&mut analyzer, &mut tap, Rate::Hz44100, 0.0, 12288);
        assert_eq!(analyzer.bins().count(), 8193);
        assert!(analyzer.bins().all(|(_, db)| db == -120.0));
    }
    #[cfg(feature = "alloc-audit")]
    #[test]
    fn telemetry_push_and_overflow_allocate_nothing() {
        let (mut tap, _stream) = channel(Rate::Hz44100);
        let before = crate::audit::ALLOCATIONS.load(Ordering::Relaxed);
        let freed = crate::audit::DEALLOCATIONS.load(Ordering::Relaxed);
        {
            let _audit = crate::audit::enter();
            for _ in 0..40000 {
                tap.push([0.0; 2], [0.0; 2]);
            }
            tap.skip(128);
        }
        assert_eq!(crate::audit::ALLOCATIONS.load(Ordering::Relaxed), before);
        assert_eq!(crate::audit::DEALLOCATIONS.load(Ordering::Relaxed), freed);
    }
    #[test]
    fn long_windows_can_refresh_before_a_quarter_window_arrives() {
        let mut analyzer = Analyzer::new(Settings {
            resolution: Resolution::Samples16384,
            ..Settings::default()
        });
        let (mut tap, stream) = channel(Rate::Hz48000);
        analyzer.attach(stream);
        tones(&mut analyzer, &mut tap, Rate::Hz48000, 1.0, 16384);
        for _ in 0..400 {
            tap.push([0.0; 2], [0.0; 2]);
        }
        assert!(
            analyzer.update(),
            "120 FPS at 48 kHz supplies 400 new samples per frame, independent of FFT size"
        );
    }
    #[test]
    fn intensity_rejects_invalid_values_and_reads_saved_named_levels() {
        for value in [-1.0, 100.1, f32::NAN, f32::INFINITY] {
            assert!(Intensity::try_from(value).is_err());
        }
        let settings: Settings = serde_json::from_str(r#"{"strength":"Medium"}"#).unwrap();
        assert_eq!(settings.intensity.percent(), 22.0);
        assert_eq!(settings.frame_rate, FrameRate::Fps60);
        let settings = Settings {
            intensity: Intensity::try_from(37.25).unwrap(),
            frame_rate: FrameRate::Fps120,
            ..settings
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), settings);
        assert!(serde_json::from_str::<Settings>(r#"{"intensity":101}"#).is_err());
        assert!(serde_json::from_str::<Settings>(r#"{"frame_rate":"Fps999"}"#).is_err());
    }
    #[test]
    fn settings_validate_choices_and_roundtrip() {
        let settings = Settings::default();
        assert_eq!(
            serde_json::from_str::<Settings>(&serde_json::to_string(&settings).unwrap()).unwrap(),
            settings
        );
        assert!(serde_json::from_str::<Settings>(r#"{"resolution":"Samples9999"}"#).is_err());
        for setting in Setting::ALL {
            assert!(
                setting
                    .choices()
                    .any(|choice| choice == settings.selected(setting))
            );
        }
    }
}
