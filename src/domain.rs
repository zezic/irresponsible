use crate::error::{Result, invalid};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Rate {
    #[default]
    Hz44100,
    Hz48000,
}

impl Rate {
    pub const fn hz(self) -> u32 {
        match self {
            Self::Hz44100 => 44100,
            Self::Hz48000 => 48000,
        }
    }
}

impl TryFrom<u32> for Rate {
    type Error = crate::error::Error;
    fn try_from(value: u32) -> Result<Self> {
        match value {
            44100 => Ok(Self::Hz44100),
            48000 => Ok(Self::Hz48000),
            _ => Err(invalid(format!(
                "Unsupported processing rate {value} Hz. Choose 44.1 or 48 kHz in Audio MIDI Setup."
            ))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DeviceUid(cpal::DeviceId);

impl DeviceUid {
    pub fn new(id: cpal::DeviceId) -> Self {
        Self(id)
    }
    pub fn id(&self) -> &cpal::DeviceId {
        &self.0
    }
}

impl TryFrom<String> for DeviceUid {
    type Error = cpal::Error;
    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        value.parse().map(Self)
    }
}
impl From<DeviceUid> for String {
    fn from(value: DeviceUid) -> Self {
        value.0.to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "[u16; 2]", into = "[u16; 2]")]
pub struct StereoPair([u16; 2]);

impl Default for StereoPair {
    fn default() -> Self {
        Self([0, 1])
    }
}
impl StereoPair {
    pub fn indices(self) -> [usize; 2] {
        self.0.map(usize::from)
    }
    pub fn validate(self, channels: u16) -> Result<()> {
        if self.0.iter().any(|&ch| ch >= channels) {
            return Err(invalid("Stereo pair exceeds the device's channel count"));
        }
        Ok(())
    }
    pub fn overlaps(self, other: Self) -> bool {
        self.0.iter().any(|ch| other.0.contains(ch))
    }
}
impl TryFrom<[u16; 2]> for StereoPair {
    type Error = crate::error::Error;
    fn try_from(value: [u16; 2]) -> Result<Self> {
        if value[0] == value[1] {
            return Err(invalid("Left and right must use different channels"));
        }
        Ok(Self(value))
    }
}
impl From<StereoPair> for [u16; 2] {
    fn from(value: StereoPair) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub uid: DeviceUid,
    pub label: String,
    pub pair: StereoPair,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    pub capture: Endpoint,
    pub playback: Endpoint,
}
impl Route {
    pub fn validate(&self) -> Result<()> {
        if self.capture.uid == self.playback.uid && self.capture.pair.overlaps(self.playback.pair) {
            return Err(invalid(
                "Feedback route: capture and playback overlap. Use a distinct virtual device or non-overlapping stereo pairs.",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(try_from = "f32", into = "f32")]
pub struct VolumeDb(f32);
impl Default for VolumeDb {
    fn default() -> Self {
        Self(-18.0)
    }
}
impl VolumeDb {
    pub fn db(self) -> f32 {
        self.0
    }
    pub fn gain(self) -> f32 {
        db_gain(self.0)
    }
}
impl TryFrom<f32> for VolumeDb {
    type Error = crate::error::Error;
    fn try_from(db: f32) -> Result<Self> {
        if !db.is_finite() || !(-80.0..=0.0).contains(&db) {
            return Err(invalid("Listening volume must be between -80 and 0 dB"));
        }
        Ok(Self(db))
    }
}
impl From<VolumeDb> for f32 {
    fn from(value: VolumeDb) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CorrectionMode {
    #[default]
    On,
    Bypass,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Comparison {
    #[default]
    PinkNoise,
    Uncompensated,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    AutoEq,
    Reference,
    Harman,
    Flat,
    #[default]
    Custom,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(try_from = "[u32; 2]", into = "[u32; 2]")]
pub struct WindowSize([u32; 2]);
impl Default for WindowSize {
    fn default() -> Self {
        Self([1100, 860])
    }
}
impl WindowSize {
    pub fn dimensions(self) -> [u32; 2] {
        self.0
    }
}
impl TryFrom<[u32; 2]> for WindowSize {
    type Error = crate::error::Error;
    fn try_from(size: [u32; 2]) -> Result<Self> {
        if !(520..=16384).contains(&size[0]) || !(300..=16384).contains(&size[1]) {
            return Err(invalid("Window dimensions are outside supported bounds"));
        }
        Ok(Self(size))
    }
}
impl From<WindowSize> for [u32; 2] {
    fn from(size: WindowSize) -> Self {
        size.0
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(try_from = "f32", into = "f32")]
pub struct ComparisonTrim(f32);
impl ComparisonTrim {
    pub fn db(self) -> f32 {
        self.0
    }
}
impl TryFrom<f32> for ComparisonTrim {
    type Error = crate::error::Error;
    fn try_from(db: f32) -> Result<Self> {
        if !db.is_finite() || !(-12.0..=12.0).contains(&db) {
            return Err(invalid("Comparison trim must be between -12 and 12 dB"));
        }
        Ok(Self(db))
    }
}
impl From<ComparisonTrim> for f32 {
    fn from(value: ComparisonTrim) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Controls {
    pub volume: VolumeDb,
    pub correction: CorrectionMode,
    pub comparison: Comparison,
    pub trim: ComparisonTrim,
}

#[derive(Clone, Debug, Default)]
pub enum Lifecycle {
    #[default]
    Stopped,
    Starting,
    Running {
        rate: Rate,
        estimated_latency_ms: f64,
    },
    Recovering {
        attempt: RecoveryAttempt,
        reason: String,
    },
    Failed(String),
}

pub fn db_gain(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}
pub fn gain_db(gain: f32) -> f32 {
    20.0 * gain.max(1e-10).log10()
}

#[derive(Clone, Copy, Debug)]
pub enum RecoveryAttempt {
    First,
    Second,
    Third,
}
impl RecoveryAttempt {
    pub fn number(self) -> u64 {
        match self {
            Self::First => 1,
            Self::Second => 2,
            Self::Third => 3,
        }
    }
    pub fn next(self) -> Option<Self> {
        match self {
            Self::First => Some(Self::Second),
            Self::Second => Some(Self::Third),
            Self::Third => None,
        }
    }
}
impl std::fmt::Display for RecoveryAttempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.number().fmt(f)
    }
}
