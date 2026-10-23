use super::TrebleExtension;
use crate::error::{Result, invalid};
use serde::{Deserialize, Serialize};
use std::ops::RangeInclusive;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FirLength {
    Taps2048,
    Taps4096,
    #[default]
    Taps8192,
    Taps16384,
    Taps32768,
}
impl FirLength {
    pub const ALL: [Self; 5] = [
        Self::Taps2048,
        Self::Taps4096,
        Self::Taps8192,
        Self::Taps16384,
        Self::Taps32768,
    ];
    pub const fn frames(self) -> usize {
        match self {
            Self::Taps2048 => 2048,
            Self::Taps4096 => 4096,
            Self::Taps8192 => 8192,
            Self::Taps16384 => 16384,
            Self::Taps32768 => 32768,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Alignment {
    MidrangeMean,
    #[default]
    OneKhz,
    BroadbandMean,
}
impl Alignment {
    pub const ALL: [Self; 3] = [Self::MidrangeMean, Self::OneKhz, Self::BroadbandMean];
    pub fn label(self) -> &'static str {
        match self {
            Self::MidrangeMean => "500-2k Hz mean",
            Self::OneKhz => "1 kHz",
            Self::BroadbandMean => "100-10k Hz mean",
        }
    }
    pub(super) fn bounds(self) -> (f32, f32) {
        match self {
            Self::MidrangeMean => (500.0, 2000.0),
            Self::OneKhz => (1000.0, 1000.0),
            Self::BroadbandMean => (100.0, 10000.0),
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Normalization {
    Raw,
    #[default]
    Peak,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BassExtension {
    #[default]
    Extended,
    Tapered,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConchaInterference {
    #[default]
    Absent,
    Present,
}
impl Normalization {
    pub const ALL: [Self; 2] = [Self::Raw, Self::Peak];
    pub fn label(self) -> &'static str {
        match self {
            Self::Raw => "Raw gain",
            Self::Peak => "-0.2 dB peak",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerationParameter {
    Smoothing,
    TrebleSmoothing,
    TrebleStart,
    TrebleEnd,
    TrebleGain,
    BoostLimit,
    CutLimit,
    SlopeLimit,
    SlopeDecay,
    LowUnity,
    LowFull,
    HighFull,
    HighUnity,
}
impl GenerationParameter {
    pub const ALL: [Self; 13] = [
        Self::Smoothing,
        Self::TrebleSmoothing,
        Self::TrebleStart,
        Self::TrebleEnd,
        Self::TrebleGain,
        Self::BoostLimit,
        Self::CutLimit,
        Self::SlopeLimit,
        Self::SlopeDecay,
        Self::LowUnity,
        Self::LowFull,
        Self::HighFull,
        Self::HighUnity,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Smoothing => "Smooth (oct)",
            Self::TrebleSmoothing => "Treble (oct)",
            Self::TrebleStart => "Treble from (Hz)",
            Self::TrebleEnd => "Treble to (Hz)",
            Self::TrebleGain => "Treble strength",
            Self::BoostLimit => "Boost limit (dB)",
            Self::CutLimit => "Cut limit (dB)",
            Self::SlopeLimit => "Slope (dB/oct)",
            Self::SlopeDecay => "Slope decay",
            Self::LowUnity => "Low unity (Hz)",
            Self::LowFull => "Low full (Hz)",
            Self::HighFull => "High full (Hz)",
            Self::HighUnity => "High unity (Hz)",
        }
    }
    pub fn bounds(self) -> RangeInclusive<f32> {
        match self {
            Self::Smoothing => 1.0 / 24.0..=4.0,
            Self::TrebleSmoothing => 1.0 / 12.0..=4.0,
            Self::TrebleStart => 500.0..=16000.0,
            Self::TrebleEnd => 1000.0..=20000.0,
            Self::TrebleGain => 0.0..=1.0,
            Self::BoostLimit => 0.0..=24.0,
            Self::CutLimit => 0.0..=60.0,
            Self::SlopeLimit => 1.0..=48.0,
            Self::SlopeDecay => 0.0..=1.0,
            Self::LowUnity => 1.0..=1000.0,
            Self::LowFull => 20.0..=2000.0,
            Self::HighFull => 1000.0..=18000.0,
            Self::HighUnity => 2000.0..=20000.0,
        }
    }
    pub fn step(self) -> f32 {
        match self {
            Self::Smoothing => 1.0 / 12.0,
            Self::TrebleSmoothing => 0.25,
            Self::TrebleStart | Self::TrebleEnd | Self::HighFull | Self::HighUnity => 500.0,
            Self::TrebleGain | Self::SlopeDecay => 0.05,
            Self::BoostLimit => 0.5,
            Self::LowFull => 10.0,
            _ => 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct GenerationDocument {
    #[serde(alias = "treble")]
    treble_extension: TrebleExtension,
    fir_length: FirLength,
    alignment: Alignment,
    normalization: Normalization,
    bass_extension: BassExtension,
    concha_interference: ConchaInterference,
    smoothing_octaves: f32,
    treble_smoothing_octaves: f32,
    treble_start_hz: f32,
    treble_end_hz: f32,
    treble_gain: f32,
    boost_limit_db: f32,
    cut_limit_db: Option<f32>,
    slope_limit_db_per_octave: f32,
    slope_decay: f32,
    low_unity_hz: f32,
    low_full_hz: f32,
    high_full_hz: f32,
    high_unity_hz: f32,
}
impl Default for GenerationDocument {
    fn default() -> Self {
        Self {
            treble_extension: TrebleExtension::AutoEq,
            fir_length: FirLength::default(),
            alignment: Alignment::default(),
            normalization: Normalization::default(),
            bass_extension: BassExtension::default(),
            concha_interference: ConchaInterference::default(),
            smoothing_octaves: 1.0 / 12.0,
            treble_smoothing_octaves: 2.0,
            treble_start_hz: 6000.0,
            treble_end_hz: 8000.0,
            treble_gain: 1.0,
            boost_limit_db: 6.0,
            cut_limit_db: None,
            slope_limit_db_per_octave: 18.0,
            slope_decay: 0.0,
            low_unity_hz: 10.0,
            low_full_hz: 20.0,
            high_full_hz: 10000.0,
            high_unity_hz: 16000.0,
        }
    }
}

/// Validated generation parameters. Every edit validates the complete band,
/// so partial frequency edits cannot enter the FIR preparation pipeline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "GenerationDocument", into = "GenerationDocument")]
pub struct GenerationSettings(GenerationDocument);
impl TryFrom<GenerationDocument> for GenerationSettings {
    type Error = crate::error::Error;
    fn try_from(document: GenerationDocument) -> Result<Self> {
        let settings = Self(document);
        for parameter in GenerationParameter::ALL {
            let value = settings.value(parameter);
            if !value.is_finite() || !parameter.bounds().contains(&value) {
                return Err(invalid(format!(
                    "{parameter:?} must be within {}..{}",
                    parameter.bounds().start(),
                    parameter.bounds().end()
                )));
            }
        }
        let d = &settings.0;
        if d.treble_start_hz >= d.treble_end_hz {
            return Err(invalid("Treble transition requires from < to."));
        }
        if !(d.low_unity_hz < d.low_full_hz
            && d.low_full_hz <= d.high_full_hz
            && d.high_full_hz < d.high_unity_hz)
        {
            return Err(invalid(
                "The correction band requires low unity < low full <= high full < high unity.",
            ));
        }
        Ok(settings)
    }
}
impl From<GenerationSettings> for GenerationDocument {
    fn from(settings: GenerationSettings) -> Self {
        settings.0
    }
}
impl GenerationSettings {
    pub fn value(self, parameter: GenerationParameter) -> f32 {
        let d = &self.0;
        match parameter {
            GenerationParameter::Smoothing => d.smoothing_octaves,
            GenerationParameter::TrebleSmoothing => d.treble_smoothing_octaves,
            GenerationParameter::TrebleStart => d.treble_start_hz,
            GenerationParameter::TrebleEnd => d.treble_end_hz,
            GenerationParameter::TrebleGain => d.treble_gain,
            GenerationParameter::BoostLimit => d.boost_limit_db,
            GenerationParameter::CutLimit => d.cut_limit_db.unwrap_or(18.0),
            GenerationParameter::SlopeLimit => d.slope_limit_db_per_octave,
            GenerationParameter::SlopeDecay => d.slope_decay,
            GenerationParameter::LowUnity => d.low_unity_hz,
            GenerationParameter::LowFull => d.low_full_hz,
            GenerationParameter::HighFull => d.high_full_hz,
            GenerationParameter::HighUnity => d.high_unity_hz,
        }
    }
    pub fn with(self, parameter: GenerationParameter, value: f32) -> Result<Self> {
        let mut d = self.0;
        match parameter {
            GenerationParameter::Smoothing => d.smoothing_octaves = value,
            GenerationParameter::TrebleSmoothing => d.treble_smoothing_octaves = value,
            GenerationParameter::TrebleStart => d.treble_start_hz = value,
            GenerationParameter::TrebleEnd => d.treble_end_hz = value,
            GenerationParameter::TrebleGain => d.treble_gain = value,
            GenerationParameter::BoostLimit => d.boost_limit_db = value,
            GenerationParameter::CutLimit => d.cut_limit_db = Some(value),
            GenerationParameter::SlopeLimit => d.slope_limit_db_per_octave = value,
            GenerationParameter::SlopeDecay => d.slope_decay = value,
            GenerationParameter::LowUnity => d.low_unity_hz = value,
            GenerationParameter::LowFull => d.low_full_hz = value,
            GenerationParameter::HighFull => d.high_full_hz = value,
            GenerationParameter::HighUnity => d.high_unity_hz = value,
        }
        Self::try_from(d)
    }
    pub fn treble(self) -> TrebleExtension {
        self.0.treble_extension
    }
    #[must_use]
    pub fn with_treble(mut self, treble: TrebleExtension) -> Self {
        self.0.treble_extension = treble;
        self
    }
    pub fn length(self) -> FirLength {
        self.0.fir_length
    }
    #[must_use]
    pub fn with_length(mut self, length: FirLength) -> Self {
        self.0.fir_length = length;
        self
    }
    pub fn alignment(self) -> Alignment {
        self.0.alignment
    }
    #[must_use]
    pub fn with_alignment(mut self, alignment: Alignment) -> Self {
        self.0.alignment = alignment;
        self
    }
    pub fn normalization(self) -> Normalization {
        self.0.normalization
    }
    pub fn bass(self) -> BassExtension {
        self.0.bass_extension
    }
    #[must_use]
    pub fn with_bass(mut self, bass: BassExtension) -> Self {
        self.0.bass_extension = bass;
        self
    }
    pub fn concha(self) -> ConchaInterference {
        self.0.concha_interference
    }
    #[must_use]
    pub fn with_concha(mut self, concha: ConchaInterference) -> Self {
        self.0.concha_interference = concha;
        self
    }
    pub fn cut_limit(self) -> Option<f32> {
        self.0.cut_limit_db
    }
    #[must_use]
    pub fn without_cut_limit(mut self) -> Self {
        self.0.cut_limit_db = None;
        self
    }
    #[must_use]
    pub fn with_normalization(mut self, normalization: Normalization) -> Self {
        self.0.normalization = normalization;
        self
    }
    /// Restores generation defaults while retaining the selected treble mode.
    #[must_use]
    pub fn reset_parameters(self) -> Self {
        Self::default().with_treble(self.treble())
    }
    pub fn policy(self) -> String {
        format!(
            "IRresponsible v4: AutoEq 7ae0f56 correction and minimum-phase FIR; {}",
            serde_json::to_string(&self.0).expect("validated generation settings serialize")
        )
    }
}
