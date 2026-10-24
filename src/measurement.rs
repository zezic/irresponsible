//! Immutable acquisition evidence and the owned room-measurement workflow.
pub mod analysis;
pub mod io;
pub mod levels;
pub mod probe;
pub mod simulation;
pub mod validation;
pub mod workflow;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub microphone: Option<Microphone>,
    pub layout: Layout,
    pub calibration: Calibration,
    pub gain: GainContract,
    pub acquisition: Acquisition,
    pub recipe: crate::correction::room::Recipe,
    pub progression: workflow::Progression,
}
impl Preferences {
    pub fn validate(&self) -> Result<()> {
        self.layout.validate()?;
        self.recipe.validate()?;
        self.acquisition.validate(Rate::Hz48000)?;
        if matches!(self.calibration, Calibration::Response { .. }) {
            self.calibration.validate()?;
        }
        if let workflow::Progression::HandsFree { movement_seconds } = self.progression
            && !(5.0..=120.0).contains(&movement_seconds)
        {
            return Err(invalid("Movement interval must be 5-120 seconds"));
        }
        if self.gain.input_setting.trim().is_empty()
            || self.gain.input_setting.len() > 100
            || !self.gain.correction_reserve_db.is_finite()
            || !(0.0..=12.0).contains(&self.gain.correction_reserve_db)
        {
            return Err(invalid("Invalid fixed-gain measurement preferences"));
        }
        Ok(())
    }
}

use crate::{
    correction::Curve,
    domain::{Microphone, Rate, Route},
    error::{Result, invalid},
    storage::{ContentHash, Store},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Speaker {
    Left,
    Right,
}
impl Speaker {
    pub const ALL: [Self; 2] = [Self::Left, Self::Right];
    pub const fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "[f64; 2]", into = "[f64; 2]")]
pub struct Band {
    pub low: f64,
    pub high: f64,
}
impl Band {
    pub fn new(low: f64, high: f64) -> Result<Self> {
        Self::try_from([low, high])
    }
    pub fn contains(self, hz: f64) -> bool {
        hz >= self.low && hz <= self.high
    }
    pub fn octaves(self) -> f64 {
        (self.high / self.low).log2()
    }
}
impl Default for Band {
    fn default() -> Self {
        Self {
            low: 20.0,
            high: 20000.0,
        }
    }
}
impl TryFrom<[f64; 2]> for Band {
    type Error = crate::error::Error;
    fn try_from([low, high]: [f64; 2]) -> Result<Self> {
        if !low.is_finite() || !high.is_finite() || low < 10.0 || high > 22000.0 || low >= high {
            return Err(invalid(
                "Frequency band must be ordered, finite and within 10-22000 Hz",
            ));
        }
        Ok(Self { low, high })
    }
}
impl From<Band> for [f64; 2] {
    fn from(b: Band) -> Self {
        [b.low, b.high]
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct ProbeLevel(f64);
impl ProbeLevel {
    pub fn db(self) -> f64 {
        self.0
    }
    pub fn gain(self) -> f64 {
        10f64.powf(self.0 / 20.0)
    }
}
impl Default for ProbeLevel {
    fn default() -> Self {
        Self(-30.0)
    }
}
impl TryFrom<f64> for ProbeLevel {
    type Error = crate::error::Error;
    fn try_from(db: f64) -> Result<Self> {
        if !db.is_finite() || !(-60.0..=-12.0).contains(&db) {
            return Err(invalid("Probe peak must be between -60 and -12 dBFS"));
        }
        Ok(Self(db))
    }
}
impl From<ProbeLevel> for f64 {
    fn from(v: ProbeLevel) -> Self {
        v.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Position {
    pub name: String,
    pub coordinates_cm: Option<[f64; 3]>,
    pub weight: f64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpatialMeaning {
    ListeningVolume,
    MultipleSeats,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Layout {
    pub positions: Vec<Position>,
    pub order: Vec<usize>,
    pub reference: usize,
    pub meaning: SpatialMeaning,
}
impl Default for Layout {
    fn default() -> Self {
        Self {
            positions: vec![Position {
                name: "Reference".into(),
                coordinates_cm: Some([0.0; 3]),
                weight: 1.0,
            }],
            order: vec![0],
            reference: 0,
            meaning: SpatialMeaning::ListeningVolume,
        }
    }
}
impl Layout {
    pub fn around_listener() -> Self {
        let positions = [
            ("Listening position", [0.0, 0.0, 0.0]),
            ("30 cm left", [-30.0, 0.0, 0.0]),
            ("30 cm right", [30.0, 0.0, 0.0]),
            ("20 cm forward and 10 cm higher", [0.0, 20.0, 10.0]),
            ("20 cm behind and 10 cm lower", [0.0, -20.0, -10.0]),
        ]
        .into_iter()
        .map(|(name, coordinates)| Position {
            name: name.into(),
            coordinates_cm: Some(coordinates),
            weight: 1.0,
        })
        .collect::<Vec<_>>();
        Self {
            order: (0..positions.len()).collect(),
            positions,
            reference: 0,
            meaning: SpatialMeaning::ListeningVolume,
        }
    }
    pub fn validate(&self) -> Result<()> {
        let n = self.positions.len();
        if n == 0
            || n > 64
            || self.reference >= n
            || self.order.len() != n
            || self
                .order
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                != (0..n).collect()
        {
            return Err(invalid(
                "Positions need one reference and a complete, unique acquisition order (1-64 points)",
            ));
        }
        for p in &self.positions {
            if p.name.trim().is_empty()
                || p.name.len() > 100
                || !p.weight.is_finite()
                || !(0.001..=1000.0).contains(&p.weight)
                || p.coordinates_cm
                    .is_some_and(|v| v.iter().any(|x| !x.is_finite() || x.abs() > 100000.0))
            {
                return Err(invalid("Position label, coordinates or weight is invalid"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Orientation {
    TowardSpeakers,
    Upward,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PhaseCurve {
    pub frequency_hz: Vec<f32>,
    pub degrees: Vec<f32>,
}
impl PhaseCurve {
    fn validate(&self) -> Result<()> {
        if self.frequency_hz.len() != self.degrees.len()
            || self.frequency_hz.len() < 2
            || self
                .frequency_hz
                .iter()
                .any(|f| !f.is_finite() || *f <= 0.0)
            || self.frequency_hz.windows(2).any(|w| w[0] >= w[1])
            || self.degrees.iter().any(|d| !d.is_finite())
        {
            return Err(invalid(
                "Microphone phase needs ordered Hz points and finite degrees",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Calibration {
    Uncalibrated {
        acknowledged: bool,
    },
    Response {
        original: ContentHash,
        response: Curve,
        phase_degrees: Option<PhaseCurve>,
        orientation: Orientation,
        metadata: Vec<String>,
    },
}
impl Default for Calibration {
    fn default() -> Self {
        Self::Uncalibrated {
            acknowledged: false,
        }
    }
}
impl Calibration {
    pub fn import(bytes: &[u8], orientation: Orientation, store: &Store) -> Result<Self> {
        if bytes.len() > 2 * 1024 * 1024 {
            return Err(invalid("Calibration file exceeds 2 MiB"));
        }
        let text = std::str::from_utf8(bytes)
            .map_err(|_| invalid("Calibration must be UTF-8 frequency/response dB text"))?;
        let (mut f, mut db, mut phase, mut metadata) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for line in text.lines().map(str::trim).filter(|s| !s.is_empty()) {
            if line.starts_with(['*', '#', ';', '"']) {
                metadata.push(line.to_owned());
                continue;
            }
            let values = line.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty()).map(str::parse::<f32>).collect::<std::result::Result<Vec<_>, _>>().map_err(|_| invalid("Expected calibration frequency Hz, measured response dB, optional phase degrees"))?;
            if !(2..=3).contains(&values.len())
                || values.iter().any(|v| !v.is_finite())
                || !(1.0..=96000.0).contains(&values[0])
                || values[1].abs() > 60.0
            {
                return Err(invalid("Calibration response has invalid units or bounds"));
            }
            f.push(values[0]);
            db.push(values[1]);
            if values.len() == 3 {
                phase.push(values[2]);
            }
        }
        if !phase.is_empty() && phase.len() != f.len() {
            return Err(invalid(
                "Phase must be supplied for every calibration point or none",
            ));
        }
        let phase_degrees = if phase.is_empty() {
            None
        } else {
            Some(PhaseCurve {
                frequency_hz: f.clone(),
                degrees: phase,
            })
        };
        Ok(Self::Response {
            original: store.put(bytes)?,
            response: Curve::new(f, db)?,
            phase_degrees,
            orientation,
            metadata,
        })
    }
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Uncalibrated { acknowledged: true } => Ok(()),
            Self::Uncalibrated {
                acknowledged: false,
            } => Err(invalid(
                "Acknowledge unknown microphone response before exploratory measurement",
            )),
            Self::Response {
                response,
                phase_degrees,
                ..
            } => {
                Curve::new(response.frequency.clone(), response.db.clone())?;
                if let Some(p) = phase_degrees {
                    p.validate()?;
                    if p.frequency_hz != response.frequency {
                        return Err(invalid(
                            "Calibration phase must use the response frequencies",
                        ));
                    }
                }
                Ok(())
            }
        }
    }
    pub fn correction_db(&self, hz: f64) -> Option<f64> {
        match self {
            Self::Uncalibrated { .. } => Some(0.0),
            Self::Response { response, .. } => (hz >= f64::from(response.frequency[0])
                && hz <= f64::from(*response.frequency.last().expect("validated curve")))
            .then(|| f64::from(response.at(hz as f32))),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GainContract {
    pub probe: ProbeLevel,
    /// User supplied identity of the fixed analog/device gain setting.
    pub input_setting: String,
    /// Reserved for the whole run, independent of listening/headroom gain.
    pub correction_reserve_db: f64,
}
impl Default for GainContract {
    fn default() -> Self {
        Self {
            probe: ProbeLevel::default(),
            input_setting: "Fixed hardware gain".into(),
            correction_reserve_db: 6.0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Acquisition {
    pub band: Band,
    pub sweep_seconds: f64,
    pub tail_seconds: f64,
    pub noise_seconds: f64,
    #[serde(default)]
    pub noise_limits: NoiseLimits,
    pub lf_detail: Option<LfDetail>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct NoiseLimits {
    pub maximum_variation_db: f64,
    pub maximum_crest_db: f64,
}
impl Default for NoiseLimits {
    fn default() -> Self {
        Self {
            maximum_variation_db: 8.0,
            maximum_crest_db: 24.0,
        }
    }
}
impl NoiseLimits {
    pub fn validate(&self) -> Result<()> {
        if [self.maximum_variation_db, self.maximum_crest_db]
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
        {
            return Err(invalid(
                "Background noise limits must be finite and nonnegative",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LfDetail {
    pub band: Band,
    pub seconds: f64,
}
impl Default for Acquisition {
    fn default() -> Self {
        Self {
            band: Band::default(),
            sweep_seconds: 8.0,
            tail_seconds: 2.0,
            noise_seconds: 5.0,
            noise_limits: NoiseLimits::default(),
            lf_detail: None,
        }
    }
}
impl Acquisition {
    pub fn validate(&self, rate: Rate) -> Result<()> {
        self.noise_limits.validate()?;
        Band::new(self.band.low, self.band.high)?;
        if !(1.0..=120.0).contains(&self.sweep_seconds)
            || !(0.25..=10.0).contains(&self.tail_seconds)
            || !(1.0..=30.0).contains(&self.noise_seconds)
            || self.band.high >= f64::from(rate.hz()) * 0.49
        {
            return Err(invalid(
                "Invalid measurement duration, noise margin or excitation band",
            ));
        }
        if let Some(lf) = &self.lf_detail {
            Band::new(lf.band.low, lf.band.high)?;
        }
        if let Some(lf) = &self.lf_detail
            && (!(4.0..=120.0).contains(&lf.seconds)
                || lf.band.high > self.band.high
                || lf.band.low < self.band.low)
        {
            return Err(invalid(
                "LF detail needs its own valid ESS duration and overlapping band",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourcePlan {
    pub route: Route,
    pub microphone: Microphone,
    pub rate: Rate,
    pub layout: Layout,
    pub calibration: Calibration,
    pub gain: GainContract,
    pub acquisition: Acquisition,
}
impl SourcePlan {
    /// Compare acquisition conditions independently of spatial planning and labels.
    pub fn same_acquisition_path(&self, other: &Self) -> Result<bool> {
        let endpoints_match = self.route.playback.uid == other.route.playback.uid
            && self.route.playback.channels == other.route.playback.channels
            && self.microphone.uid == other.microphone.uid
            && self.microphone.channels == other.microphone.channels
            && self.rate == other.rate;
        if !endpoints_match {
            return Ok(false);
        }
        let conditions = |plan: &Self| {
            serde_json::to_vec(&(
                &plan.calibration,
                plan.gain.probe,
                &plan.gain.input_setting,
                plan.gain.correction_reserve_db,
            ))
        };
        Ok(conditions(self)? == conditions(other)?)
    }

    pub fn level_path(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(&(
            &self.route,
            &self.microphone,
            self.rate,
            &self.calibration,
            (
                &self.gain.probe,
                &self.gain.input_setting,
                self.gain.correction_reserve_db,
            ),
        ))
        .map_err(Into::into)
    }
    pub fn validate(&self) -> Result<()> {
        self.route.validate()?;
        self.layout.validate()?;
        self.calibration.validate()?;
        self.acquisition.validate(self.rate)?;
        if self.gain.input_setting.trim().is_empty()
            || !self.gain.correction_reserve_db.is_finite()
            || !(0.0..=12.0).contains(&self.gain.correction_reserve_db)
        {
            return Err(invalid("Declare fixed input gain and correction reserve"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Waveform {
    pub object: ContentHash,
    pub frames: usize,
    pub rate: Rate,
}
impl Waveform {
    pub fn save(samples: &[f32], rate: Rate, store: &Store) -> Result<Self> {
        if samples.is_empty()
            || samples.len() > rate.hz() as usize * 180
            || samples.iter().any(|v| !v.is_finite())
        {
            return Err(invalid(
                "Measurement waveform must contain 0-180 seconds of finite mono samples",
            ));
        }
        let bytes: Vec<_> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        Ok(Self {
            object: store.put(&bytes)?,
            frames: samples.len(),
            rate,
        })
    }
    pub fn load(&self, store: &Store) -> Result<Vec<f32>> {
        let bytes = store.get(&self.object)?;
        if bytes.len() != self.frames * 4 || self.frames > self.rate.hz() as usize * 180 {
            return Err(invalid("Measurement waveform length mismatch"));
        }
        let samples: Vec<_> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&v| f32::from_le_bytes(v))
            .collect();
        if samples.iter().any(|s| !s.is_finite()) {
            return Err(invalid("Measurement waveform contains nonfinite samples"));
        }
        Ok(samples)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaptureTiming {
    pub first_frame: u64,
    pub frames: u64,
    pub first_timestamp_ns: Option<u64>,
    pub last_timestamp_ns: Option<u64>,
    pub discontinuities: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClockMap {
    pub intercept: f64,
    pub ratio: f64,
    pub ppm: f64,
    pub marker_correlation: [f64; 2],
    /// Template projection amplitudes; these do not measure hardware gain.
    #[serde(alias = "marker_gain_db")]
    pub marker_projection_db: [f64; 2],
    pub timing_residual_frames: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Noise {
    pub raw: Waveform,
    pub psd_dbfs_per_hz: Curve,
    pub rms_dbfs: f64,
    pub window_variation_db: f64,
    pub crest_db: f64,
    #[serde(default)]
    pub limits: NoiseLimits,
}
impl Noise {
    pub fn quality_warning(&self) -> Option<String> {
        if self.window_variation_db > self.limits.maximum_variation_db
            || self.crest_db > self.limits.maximum_crest_db
        {
            return Some(format!(
                "Background varied: level variation {:.1} dB (warning above {:.1} dB), peak-to-average {:.1} dB (warning above {:.1} dB). Measurement continues; changing background can reduce confidence in affected bands. Adjust warning thresholds in Setup > Advanced acquisition settings.",
                self.window_variation_db,
                self.limits.maximum_variation_db,
                self.crest_db,
                self.limits.maximum_crest_db,
            ));
        }
        None
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Polarity {
    Positive,
    Negative,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Diagnostics {
    pub pre_arrival_energy_ratio: f64,
    pub late_energy_ratio: f64,
    pub polarity: Polarity,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    pub diagnostics: Diagnostics,
    pub magnitude: Curve,
    pub uncertainty_db: Vec<f32>,
    pub impulse: Waveform,
    pub arrival_frames: f64,
    pub resolution_hz: f64,
    pub trusted_band: Band,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Take {
    pub speaker: Speaker,
    pub position: usize,
    pub raw: Waveform,
    pub probe: probe::Recipe,
    pub output_identity: ContentHash,
    pub candidate: Option<ContentHash>,
    pub timing: CaptureTiming,
    pub output_timing: CaptureTiming,
    pub clock: ClockMap,
    pub response: Response,
    pub noise: Noise,
}
impl Take {
    pub fn referenced_objects(&self) -> impl Iterator<Item = ContentHash> {
        [
            self.raw.object,
            self.output_identity,
            self.response.impulse.object,
            self.noise.raw.object,
        ]
        .into_iter()
    }
    pub fn validate(&self) -> Result<()> {
        self.noise.limits.validate()?;
        let n = analysis::grid().len();
        let r = &self.response;
        Curve::new(r.magnitude.frequency.clone(), r.magnitude.db.clone())?;
        Curve::new(
            self.noise.psd_dbfs_per_hz.frequency.clone(),
            self.noise.psd_dbfs_per_hz.db.clone(),
        )?;
        let nominal = f64::from(self.raw.rate.hz()) / f64::from(self.probe.rate.hz());
        if r.magnitude.db.iter().any(|db| db.abs() > 300.0)
            || r.magnitude.frequency != analysis::grid()
            || r.uncertainty_db.len() != n
            || r.uncertainty_db.iter().any(|v| !v.is_finite() || *v < 0.0)
            || !r.arrival_frames.is_finite()
            || !r.resolution_hz.is_finite()
            || r.resolution_hz <= 0.0
            || !self.noise.rms_dbfs.is_finite()
            || !self.noise.window_variation_db.is_finite()
            || !self.noise.crest_db.is_finite()
            || !self.clock.intercept.is_finite()
            || !self.clock.ratio.is_finite()
            || self.clock.ratio <= 0.0
            || self
                .clock
                .marker_projection_db
                .iter()
                .any(|v| !v.is_finite())
            || !self.clock.timing_residual_frames.is_finite()
            || !self.clock.ppm.is_finite()
            || self.clock.ppm.abs() > 2000.0
            || ((self.clock.ratio / nominal - 1.0) * 1e6 - self.clock.ppm).abs() > 0.01
            || self.clock.marker_correlation.iter().any(|v| {
                !v.is_finite() || !(analysis::MINIMUM_MARKER_CORRELATION..=1.01).contains(v)
            })
            || self.timing.frames != self.raw.frames as u64
            || self.timing.discontinuities != 0
            || self.output_timing.discontinuities != 0
            || self.output_timing.frames != self.probe.total_frames as u64
            || self.probe.policy != "ess-discrete-inverse-v1"
            || !self.probe.seconds.is_finite()
            || !(1.0..=120.0).contains(&self.probe.seconds)
            || !self.probe.tail_seconds.is_finite()
            || self.probe.filter_tail_frames >= crate::correction::MAX_IR_FRAMES
            || (self.probe.tail_seconds * f64::from(self.probe.rate.hz())).round()
                <= self.probe.filter_tail_frames as f64
            || self.probe.total_frames > self.probe.rate.hz() as usize * 180
            || self.probe.sweep_frames == 0
            || self
                .probe
                .sweep_start
                .checked_add(self.probe.sweep_frames)
                .is_none_or(|end| end >= self.probe.total_frames)
        {
            return Err(invalid(
                "Invalid measurement response, timing, noise or probe evidence",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PositionEvidence {
    pub position: usize,
    pub noise: Noise,
    pub full: [Take; 2],
    pub lf: Option<[Take; 2]>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Source {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub levels: Option<levels::Checks>,
    pub schema: u32,
    pub plan: SourcePlan,
    pub points: Vec<PositionEvidence>,
    pub repeat_reference: [Take; 2],
}
impl Source {
    pub fn save(&self, store: &Store) -> Result<ContentHash> {
        self.validate()?;
        store.put(&serde_json::to_vec(self)?)
    }
    pub fn load(id: ContentHash, store: &Store) -> Result<Self> {
        let source: Self = serde_json::from_slice(&store.get(&id)?)?;
        source.validate()?;
        for object in source.referenced_objects() {
            store.get(&object)?;
        }
        Ok(source)
    }
    pub fn validate(&self) -> Result<()> {
        if self.schema != 1 {
            return Err(invalid("Unsupported measurement schema"));
        }
        self.plan.validate()?;
        if let Some(levels) = &self.levels {
            levels.validate(&self.plan)?;
        }
        if self.points.len() != self.plan.layout.positions.len()
            || self.points.iter().enumerate().any(|(i, p)| {
                p.position != i
                    || p.full
                        .iter()
                        .any(|t| t.position != i || t.candidate.is_some())
            })
        {
            return Err(invalid("Incomplete or inconsistent source measurements"));
        }
        for point in &self.points {
            for (c, take) in point.full.iter().enumerate() {
                take.validate()?;
                if take.speaker.index() != c {
                    return Err(invalid("Source speaker order is invalid"));
                }
            }
            if let Some(lf) = &point.lf {
                for (c, take) in lf.iter().enumerate() {
                    take.validate()?;
                    if take.position != point.position
                        || take.speaker.index() != c
                        || take.candidate.is_some()
                    {
                        return Err(invalid("Invalid LF source take"));
                    }
                }
            }
        }
        for (c, take) in self.repeat_reference.iter().enumerate() {
            take.validate()?;
            if take.position != self.plan.layout.reference
                || take.speaker.index() != c
                || take.candidate.is_some()
            {
                return Err(invalid("Invalid reference repeat"));
            }
        }
        Ok(())
    }
    pub fn referenced_objects(&self) -> impl Iterator<Item = ContentHash> + '_ {
        let calibration = match self.plan.calibration {
            Calibration::Response { original, .. } => Some(original),
            _ => None,
        };
        calibration
            .into_iter()
            .chain(
                self.levels
                    .iter()
                    .flat_map(levels::Checks::referenced_objects),
            )
            .chain(self.points.iter().flat_map(|p| {
                std::iter::once(p.noise.raw.object)
                    .chain(p.full.iter().flat_map(Take::referenced_objects))
                    .chain(p.lf.iter().flatten().flat_map(Take::referenced_objects))
            }))
            .chain(
                self.repeat_reference
                    .iter()
                    .flat_map(Take::referenced_objects),
            )
    }
}

#[cfg(test)]
mod acquisition_contract_tests {
    use super::*;

    #[test]
    fn saved_acquisition_ignores_planning_metadata_but_keeps_physical_conditions() {
        let source = simulation::plan(Rate::Hz48000);
        let mut planned = source.clone();
        planned.layout.positions[0].name = "New position label".into();
        planned.layout.positions[0].weight = 2.0;
        planned.layout.positions[0].coordinates_cm = Some([25.0, -10.0, 5.0]);
        planned.layout.meaning = SpatialMeaning::MultipleSeats;
        planned.route.playback.label = "Renamed speaker device".into();
        planned.microphone.label = "Renamed microphone".into();

        planned.acquisition.noise_limits.maximum_variation_db = 12.5;
        planned.acquisition.noise_limits.maximum_crest_db = 30.0;
        planned.acquisition.tail_seconds += 1.0;
        planned.acquisition.sweep_seconds += 1.0;
        planned.acquisition.noise_seconds += 1.0;
        planned.route.capture.uid = crate::domain::DeviceUid::Emulated {
            emulated: crate::domain::EmulatedDevice::Microphone,
        };
        assert!(source.same_acquisition_path(&planned).unwrap());

        let mut changed = planned.clone();
        changed.microphone.channels = crate::domain::MonoChannel(1);
        assert!(!source.same_acquisition_path(&changed).unwrap());
        changed = planned.clone();
        changed.gain.probe = ProbeLevel::try_from(-36.0).unwrap();
        assert!(!source.same_acquisition_path(&changed).unwrap());
        changed = planned.clone();
        changed.route.playback.channels = crate::domain::StereoPair::try_from([2, 3]).unwrap();
        assert!(!source.same_acquisition_path(&changed).unwrap());
        changed = planned;
        changed.rate = Rate::Hz44100;
        assert!(!source.same_acquisition_path(&changed).unwrap());
    }

    #[test]
    fn legacy_acquisition_uses_default_noise_limits() {
        let mut document = serde_json::to_value(Acquisition::default()).unwrap();
        document.as_object_mut().unwrap().remove("noise_limits");
        let acquisition: Acquisition = serde_json::from_value(document).unwrap();
        acquisition.validate(Rate::Hz48000).unwrap();
        assert_eq!(acquisition.noise_limits.maximum_variation_db, 8.0);
        assert_eq!(acquisition.noise_limits.maximum_crest_db, 24.0);
    }
}
