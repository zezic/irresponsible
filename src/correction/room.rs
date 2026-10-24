//! Room targets, constrained stereo magnitude fitting and neutral minimum phase.
use crate::{
    correction::{Correction, Curve, Origin},
    domain::Rate,
    error::{Result, invalid},
    measurement::{
        Band, Response, Source,
        analysis::{self, grid},
    },
    storage::ContentHash,
};
use rustfft::{FftPlanner, num_complex::Complex64};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct TargetParameters {
    pub tilt_db_octave: f64,
    pub bass_db: f64,
    #[serde(default)]
    pub mid_gain_db: f64,
    #[serde(default = "TargetParameters::default_mid_center_hz")]
    pub mid_center_hz: f64,
    /// Full width at half gain in octaves.
    #[serde(default = "TargetParameters::default_mid_width_octaves")]
    pub mid_width_octaves: f64,
}
impl Default for TargetParameters {
    fn default() -> Self {
        Self {
            tilt_db_octave: 0.0,
            bass_db: 0.0,
            mid_gain_db: 0.0,
            mid_center_hz: Self::default_mid_center_hz(),
            mid_width_octaves: Self::default_mid_width_octaves(),
        }
    }
}

impl TargetParameters {
    fn default_mid_center_hz() -> f64 {
        1000.0
    }

    fn default_mid_width_octaves() -> f64 {
        1.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Target {
    Gentle,
    Natural,
    Warm,
    Flat,
    Custom(Curve),
    Parametric(TargetParameters),
}
impl Target {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Gentle => "Gentle monitoring",
            Self::Natural => "Natural",
            Self::Warm => "Warm",
            Self::Flat => "Flat in-room",
            Self::Custom(_) => "Custom room curve",
            Self::Parametric(_) => "Custom room target",
        }
    }
    pub fn parameters(&self) -> Option<TargetParameters> {
        let (tilt_db_octave, bass_db) = match self {
            Self::Gentle => (0.5, 0.0),
            Self::Natural => (1.0, 0.0),
            Self::Warm => (1.0, 3.0),
            Self::Flat => (0.0, 0.0),
            Self::Parametric(parameters) => return Some(*parameters),
            Self::Custom(_) => return None,
        };
        Some(TargetParameters {
            tilt_db_octave,
            bass_db,
            ..TargetParameters::default()
        })
    }

    pub fn covers(&self, hz: f64) -> bool {
        match self {
            Self::Custom(c) => {
                hz >= f64::from(c.frequency[0])
                    && hz <= f64::from(*c.frequency.last().expect("validated target"))
            }
            _ => true,
        }
    }

    pub fn db(&self, hz: f64) -> f64 {
        if let Self::Custom(curve) = self {
            return f64::from(curve.at(hz as f32));
        }
        let parameters = self.parameters().expect("built-in targets have parameters");
        let mid_distance = (hz / parameters.mid_center_hz).log2() / parameters.mid_width_octaves;
        let mid =
            parameters.mid_gain_db * (-4.0 * std::f64::consts::LN_2 * mid_distance.powi(2)).exp();
        -parameters.tilt_db_octave * (hz.clamp(100.0, 10000.0) / 1000.0).log2()
            + parameters.bass_db * (1.0 / (1.0 + (hz / 100.0).powi(4)) - 1.0 / 10001.0)
            + mid
    }

    pub fn import(bytes: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| invalid("Room target must be UTF-8 Hz/dB text"))?;
        let (mut f, mut db) = (Vec::new(), Vec::new());
        for line in text
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.starts_with('#'))
        {
            let p = line
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|s| !s.is_empty())
                .map(str::parse::<f32>)
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|_| invalid("Target format is frequency Hz, acoustic room response dB"))?;
            if p.len() != 2 || p[1].abs() > 60.0 {
                return Err(invalid("Invalid acoustic room target"));
            }
            f.push(p[0]);
            db.push(p[1]);
        }
        Ok(Self::Custom(Curve::new(f, db)?))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Taps {
    Short,
    Long,
}
impl Taps {
    pub const fn frames(self) -> usize {
        match self {
            Self::Short => 16384,
            Self::Long => 32768,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum TargetLevel {
    Automatic,
    Fixed(f64),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Region {
    pub band: Band,
    pub effort: f64,
    pub smoothing_octaves: f64,
    pub lower_fade: f64,
    pub upper_fade: f64,
}
impl Region {
    pub fn validate(&self) -> Result<()> {
        Band::new(self.band.low, self.band.high)?;
        if !(0.0..=1.0).contains(&self.effort)
            || !(0.0..=2.0).contains(&self.smoothing_octaves)
            || !self.lower_fade.is_finite()
            || !self.upper_fade.is_finite()
            || self.lower_fade < 0.0
            || self.upper_fade < 0.0
            || self.lower_fade + self.upper_fade > self.band.octaves() + 1e-9
        {
            return Err(invalid(
                "Region effort, smoothing or independent fades are invalid",
            ));
        }
        Ok(())
    }
    pub fn fit_fades(&mut self) {
        let sum = self.lower_fade + self.upper_fade;
        let span = self.band.octaves();
        if sum > span {
            self.lower_fade *= span / sum;
            self.upper_fade *= span / sum;
        }
    }
    fn weight(&self, f: f64) -> f64 {
        if !self.band.contains(f) {
            return 0.0;
        }
        let smooth = |x: f64| {
            let x = x.clamp(0.0, 1.0);
            x * x * (3.0 - 2.0 * x)
        };
        let low = if self.lower_fade == 0.0 {
            1.0
        } else {
            smooth((f / self.band.low).log2() / self.lower_fade)
        };
        let high = if self.upper_fade == 0.0 {
            1.0
        } else {
            smooth((self.band.high / f).log2() / self.upper_fade)
        };
        low.min(high)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Alignment {
    #[default]
    None,
    Level,
    Time,
    LevelAndTime,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct SpeakerAlignment {
    pub trim_db: [f64; 2],
    pub delay_seconds: [f64; 2],
}
impl SpeakerAlignment {
    pub fn validate(self) -> Result<()> {
        if self
            .trim_db
            .iter()
            .any(|v| !v.is_finite() || !(-6.0..=0.0).contains(v))
            || self
                .delay_seconds
                .iter()
                .any(|v| !v.is_finite() || !(0.0..=0.005).contains(v))
        {
            return Err(invalid("Speaker alignment exceeds 6 dB trim or 5 ms delay"));
        }
        Ok(())
    }
    pub fn frames(self, rate: Rate) -> [usize; 2] {
        self.delay_seconds
            .map(|s| (s * f64::from(rate.hz())).round() as usize)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Contribution {
    Included { weight: f64 },
    Excluded { reason: String },
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub enum SpatialPolicy {
    #[default]
    SourceLayout,
    Reweighted(Vec<Contribution>),
}
impl SpatialPolicy {
    pub fn weights(&self, source: &Source) -> Result<Vec<f64>> {
        match self {
            Self::SourceLayout => Ok(source
                .plan
                .layout
                .positions
                .iter()
                .map(|p| p.weight)
                .collect()),
            Self::Reweighted(points) => {
                if points.len() != source.points.len() {
                    return Err(invalid(
                        "Spatial overrides must identify every retained source point",
                    ));
                }
                Ok(points
                    .iter()
                    .map(|p| match p {
                        Contribution::Included { weight } => *weight,
                        Contribution::Excluded { .. } => 0.0,
                    })
                    .collect())
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Recipe {
    pub policy: String,
    pub alignment: Alignment,
    pub spatial: SpatialPolicy,
    pub lower_fade: f64,
    pub upper_fade: f64,
    pub target: Target,
    pub range: Band,
    pub global_effort: f64,
    pub smoothing_bass: f64,
    pub smoothing_treble: f64,
    pub regions: Vec<Region>,
    pub target_level: TargetLevel,
    pub maximum_boost_db: Option<f64>,
    pub maximum_cut_db: Option<f64>,
    pub taps: Taps,
}
impl Default for Recipe {
    fn default() -> Self {
        Self {
            policy: "room-minphase-power-huber-v1".into(),
            alignment: Alignment::None,
            spatial: SpatialPolicy::SourceLayout,
            lower_fade: 1.0 / 6.0,
            upper_fade: 1.0 / 6.0,
            target: Target::Natural,
            range: Band::default(),
            global_effort: 1.0,
            smoothing_bass: 1.0 / 12.0,
            smoothing_treble: 1.0 / 3.0,
            regions: Vec::new(),
            target_level: TargetLevel::Automatic,
            maximum_boost_db: None,
            maximum_cut_db: None,
            taps: Taps::Long,
        }
    }
}
impl Recipe {
    pub fn validate(&self) -> Result<()> {
        Band::new(self.range.low, self.range.high)?;
        if self.policy != Self::default().policy
            || !(0.0..=1.0).contains(&self.global_effort)
            || !(0.0..=2.0).contains(&self.smoothing_bass)
            || !(0.0..=2.0).contains(&self.smoothing_treble)
            || self
                .maximum_boost_db
                .is_some_and(|db| !(0.0..=24.0).contains(&db))
            || self
                .maximum_cut_db
                .is_some_and(|db| !(0.0..=36.0).contains(&db))
        {
            return Err(invalid("Unsupported room policy or correction limits"));
        }
        if !self.lower_fade.is_finite()
            || !self.upper_fade.is_finite()
            || self.lower_fade < 0.0
            || self.upper_fade < 0.0
            || self.lower_fade + self.upper_fade > self.range.octaves()
        {
            return Err(invalid("Correction range fades exceed the available band"));
        }
        if let SpatialPolicy::Reweighted(points) = &self.spatial {
            if points.is_empty()
                || points.len() > 64
                || !points
                    .iter()
                    .any(|p| matches!(p, Contribution::Included { .. }))
            {
                return Err(invalid("Keep at least one included source point"));
            }
            for p in points {
                match p {
                    Contribution::Included { weight }
                        if !weight.is_finite() || !(0.001..=1000.0).contains(weight) =>
                    {
                        return Err(invalid("Spatial weight must be 0.001-1000"));
                    }
                    Contribution::Excluded { reason }
                        if reason.trim().is_empty() || reason.len() > 500 =>
                    {
                        return Err(invalid("Excluded points require a reason"));
                    }
                    _ => {}
                }
            }
        }
        if let TargetLevel::Fixed(offset) = self.target_level
            && (!offset.is_finite() || offset.abs() > 24.0)
        {
            return Err(invalid("Target offset must be within +/-24 dB"));
        }
        if let Target::Parametric(parameters) = &self.target {
            if !(0.0..=3.0).contains(&parameters.tilt_db_octave)
                || !(-6.0..=9.0).contains(&parameters.bass_db)
            {
                return Err(invalid(
                    "Room target tilt must be 0-3 dB/octave and bass shelf -6 to +9 dB",
                ));
            }
            if !(-12.0..=12.0).contains(&parameters.mid_gain_db)
                || !(0.1..=4.0).contains(&parameters.mid_width_octaves)
                || !(20.0..=20000.0).contains(&parameters.mid_center_hz)
            {
                return Err(invalid(
                    "Room target mid bell gain must be -12 to +12 dB, width 0.1-4 octaves and center 20-20000 Hz",
                ));
            }
        }
        if let Target::Custom(c) = &self.target {
            Curve::new(c.frequency.clone(), c.db.clone())?;
            if c.db.iter().any(|db| db.abs() > 60.0) {
                return Err(invalid(
                    "Room target is a relative acoustic magnitude curve within +/-60 dB",
                ));
            }
        }
        if !self.target.covers(self.range.low) || !self.target.covers(self.range.high) {
            return Err(invalid(
                "Imported room target does not cover the requested correction band; choose its declared range or import a wider target",
            ));
        }
        for r in &self.regions {
            r.validate()?;
            if r.band.low < self.range.low || r.band.high > self.range.high {
                return Err(invalid("Region is outside correction range"));
            }
        }
        if self
            .regions
            .windows(2)
            .any(|w| w[0].band.high > w[1].band.low)
        {
            return Err(invalid(
                "Region overrides must be ordered and nonoverlapping",
            ));
        }
        Ok(())
    }
    pub fn profile(&self, f: f64) -> (f64, f64) {
        let blend = ((f / 200.0).log2() / (6000f64 / 200.0).log2()).clamp(0.0, 1.0);
        let width = self.smoothing_bass * (1.0 - blend) + self.smoothing_treble * blend;
        let (mut effort, mut smoothing) = (1.0, width);
        if let Some(r) = self.regions.iter().find(|r| r.band.contains(f)) {
            let w = r.weight(f);
            effort += w * (r.effort - 1.0);
            smoothing += w * (r.smoothing_octaves - width);
        }
        (
            if self.range.contains(f) {
                self.global_effort * effort
            } else {
                0.0
            },
            smoothing,
        )
    }
    pub fn insert_region(&mut self, region: Region) -> Result<()> {
        region.validate()?;
        let mut next = Vec::new();
        for r in &self.regions {
            if r.band.high <= region.band.low || r.band.low >= region.band.high {
                next.push(r.clone());
            } else {
                if r.band.low < region.band.low {
                    let mut left = r.clone();
                    left.band.high = region.band.low;
                    left.fit_fades();
                    next.push(left);
                }
                if r.band.high > region.band.high {
                    let mut right = r.clone();
                    right.band.low = region.band.high;
                    right.fit_fades();
                    next.push(right);
                }
            }
        }
        next.push(region);
        next.sort_by(|a, b| a.band.low.total_cmp(&b.band.low));
        let old = std::mem::replace(&mut self.regions, next);
        if let Err(e) = self.validate() {
            self.regions = old;
            return Err(e);
        }
        Ok(())
    }
    pub fn resize_region(&mut self, i: usize, lower: bool, f: f64) {
        let previous = i
            .checked_sub(1)
            .map(|j| self.regions[j].band.high)
            .unwrap_or(self.range.low);
        let next = self
            .regions
            .get(i + 1)
            .map_or(self.range.high, |r| r.band.low);
        let shared = if lower {
            i > 0 && previous == self.regions[i].band.low
        } else {
            i + 1 < self.regions.len() && next == self.regions[i].band.high
        };
        if lower {
            let minimum = if shared {
                self.regions[i - 1].band.low * 1.01
            } else {
                previous
            };
            let value = f.clamp(minimum, self.regions[i].band.high / 1.01);
            self.regions[i].band.low = value;
            if shared {
                self.regions[i - 1].band.high = value;
                self.regions[i - 1].fit_fades();
            }
        } else {
            let maximum = if shared {
                self.regions[i + 1].band.high / 1.01
            } else {
                next
            };
            let value = f.clamp(self.regions[i].band.low * 1.01, maximum);
            self.regions[i].band.high = value;
            if shared {
                self.regions[i + 1].band.low = value;
                self.regions[i + 1].fit_fades();
            }
        }
        self.regions[i].fit_fades();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpatialResponse {
    pub magnitude: [Curve; 2],
    pub spread_db: [Vec<f32>; 2],
    pub uncertainty_db: [Vec<f32>; 2],
    pub points: Vec<[Curve; 2]>,
    pub weights: Vec<f64>,
}
pub fn power_average(points: &[[Curve; 2]], weights: &[f64]) -> Result<[Curve; 2]> {
    if points.is_empty()
        || points.len() != weights.len()
        || weights.iter().any(|w| !w.is_finite() || *w < 0.0)
        || weights.iter().sum::<f64>() <= 0.0
    {
        return Err(invalid("Spatial averaging needs weighted point evidence"));
    }
    let f = &points[0][0].frequency;
    for curve in points.iter().flatten() {
        if curve.frequency != *f || curve.db.len() != f.len() {
            return Err(invalid("Spatial evidence uses different grids"));
        }
    }
    let sum: f64 = weights.iter().sum();
    Ok(std::array::from_fn(|c| Curve {
        frequency: f.clone(),
        db: (0..f.len())
            .map(|i| {
                (10.0
                    * (points
                        .iter()
                        .zip(weights)
                        .map(|(p, w)| w * 10f64.powf(f64::from(p[c].db[i]) / 10.0))
                        .sum::<f64>()
                        / sum)
                        .log10()) as f32
            })
            .collect(),
    }))
}
pub fn aggregate(source: &Source) -> SpatialResponse {
    aggregate_weighted(
        source,
        source
            .plan
            .layout
            .positions
            .iter()
            .map(|p| p.weight)
            .collect(),
    )
}
fn aggregate_weighted(source: &Source, weights: Vec<f64>) -> SpatialResponse {
    let f = grid();
    let responses: Vec<[Response; 2]> = source
        .points
        .iter()
        .map(|p| {
            std::array::from_fn(|c| {
                p.lf.as_ref().map_or_else(
                    || p.full[c].response.clone(),
                    |lf| analysis::combine(&p.full[c].response, &lf[c].response),
                )
            })
        })
        .collect();
    let sum: f64 = weights.iter().sum();
    let points: Vec<_> = responses
        .iter()
        .map(|p| p.each_ref().map(|r| r.magnitude.clone()))
        .collect();
    let magnitude = power_average(&points, &weights).expect("validated spatial source");
    let spread_db = std::array::from_fn(|c| {
        (0..f.len())
            .map(|i| {
                let mean = points
                    .iter()
                    .zip(&weights)
                    .map(|(p, w)| w * f64::from(p[c].db[i]))
                    .sum::<f64>()
                    / sum;
                (points
                    .iter()
                    .zip(&weights)
                    .map(|(p, w)| w * (f64::from(p[c].db[i]) - mean).powi(2))
                    .sum::<f64>()
                    / sum)
                    .sqrt() as f32
            })
            .collect()
    });
    let uncertainty_db = std::array::from_fn(|c| {
        (0..f.len())
            .map(|i| {
                responses
                    .iter()
                    .zip(&weights)
                    .filter(|(_, w)| **w > 0.0)
                    .map(|(p, _)| p[c].uncertainty_db[i])
                    .fold(0.0, f32::max)
            })
            .collect()
    });
    SpatialResponse {
        magnitude,
        spread_db,
        uncertainty_db,
        points,
        weights,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Frozen {
    pub rate: Rate,
    pub recipe: Recipe,
    pub alignment: SpeakerAlignment,
    pub source: ContentHash,
    pub target_offset_db: f64,
    pub target_reference: Band,
    pub target: Curve,
    pub baseline: SpatialResponse,
    pub requested_goal: [Curve; 2],
    pub processed_goal: [Curve; 2],
    pub designed_db: [Vec<f32>; 2],
}
impl Frozen {
    pub fn validate(&self) -> Result<()> {
        self.recipe.validate()?;
        self.alignment.validate()?;
        Band::new(self.target_reference.low, self.target_reference.high)?;
        let frequencies = grid();
        let n = frequencies.len();
        if self.target.frequency != frequencies
            || !self.target_offset_db.is_finite()
            || self.baseline.points.is_empty()
            || self.baseline.points.len() > 64
            || self.baseline.weights.len() != self.baseline.points.len()
            || self
                .baseline
                .weights
                .iter()
                .any(|w| !w.is_finite() || !(0.0..=1000.0).contains(w))
            || self.baseline.weights.iter().all(|w| *w == 0.0)
        {
            return Err(invalid("Invalid frozen room objective"));
        }
        for curve in std::iter::once(&self.target)
            .chain(self.baseline.magnitude.iter())
            .chain(self.requested_goal.iter())
            .chain(self.processed_goal.iter())
            .chain(self.baseline.points.iter().flatten())
        {
            Curve::new(curve.frequency.clone(), curve.db.clone())?;
            if curve.frequency != frequencies {
                return Err(invalid(
                    "Frozen room curves use inconsistent frequency grids",
                ));
            }
        }
        for values in self
            .baseline
            .spread_db
            .iter()
            .chain(&self.baseline.uncertainty_db)
            .chain(&self.designed_db)
        {
            if values.len() != n || values.iter().any(|v| !v.is_finite()) {
                return Err(invalid("Invalid frozen room profile dimensions"));
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Realization {
    pub common_gain_db: f64,
    pub design_scale: f64,
    pub peak_gain_db: [f64; 2],
    pub tail_energy_ratio: [f64; 2],
    pub headroom_db: f64,
    pub maximum_design_error_db: f64,
}
impl Realization {
    pub fn validate(&self) -> Result<()> {
        if !self.common_gain_db.is_finite()
            || self.common_gain_db.abs() > 0.15
            || !(0.95..=1.0).contains(&self.design_scale)
            || !self.headroom_db.is_finite()
            || self.headroom_db > -0.999
            || self.peak_gain_db.iter().any(|v| !v.is_finite())
            || self
                .tail_energy_ratio
                .iter()
                .any(|v| !v.is_finite() || !(0.0..=0.001).contains(v))
            || !self.maximum_design_error_db.is_finite()
            || !(0.0..=0.8).contains(&self.maximum_design_error_db)
        {
            return Err(invalid("Invalid realized room filter evidence"));
        }
        Ok(())
    }
}

/// Gaussian dB averaging on the equal-log measurement grid.
fn smooth(values: &[f32], f: &[f32], width: impl Fn(f64) -> f64) -> Vec<f64> {
    assert_eq!(values.len(), f.len());
    let octaves: Vec<_> = f.iter().map(|&hz| f64::from(hz).log2()).collect();
    f.iter()
        .enumerate()
        .map(|(i, &hz)| {
            let width = width(f64::from(hz));
            if width == 0.0 {
                return f64::from(values[i]);
            }
            let sigma = width / (2.0 * (2.0 * std::f64::consts::LN_2).sqrt());
            let center = octaves[i];
            let start = octaves.partition_point(|&x| x < center - 4.0 * sigma);
            let end = octaves.partition_point(|&x| x <= center + 4.0 * sigma);
            let (mut sum, mut weights) = (0.0, 0.0);
            for j in start..end {
                let distance = (octaves[j] - center) / sigma;
                let w = (-0.5 * distance * distance).exp();
                sum += w * f64::from(values[j]);
                weights += w;
            }
            sum / weights
        })
        .collect()
}

fn smoothed_baseline(frozen: &Frozen) -> [Vec<f64>; 2] {
    std::array::from_fn(|c| {
        smooth(
            &frozen.baseline.magnitude[c].db,
            &frozen.target.frequency,
            |hz| frozen.recipe.profile(hz).1,
        )
    })
}

fn project(value: f64, i: usize, c: usize, frozen: &Frozen) -> f32 {
    let f = f64::from(frozen.target.frequency[i]);
    if frozen.recipe.profile(f).0 == 0.0 {
        return 0.0;
    }
    let lower = if frozen.recipe.lower_fade == 0.0 {
        1.0
    } else {
        (f / frozen.recipe.range.low).log2() / frozen.recipe.lower_fade
    };
    let upper = if frozen.recipe.upper_fade == 0.0 {
        1.0
    } else {
        (frozen.recipe.range.high / f).log2() / frozen.recipe.upper_fade
    };
    let edge = lower.min(upper).clamp(0.0, 1.0);
    let mut value = value;
    if let Some(limit) = frozen.recipe.maximum_cut_db {
        value = value.max(-limit + 0.08_f64.min(limit) - frozen.alignment.trim_db[c]);
    }
    if let Some(limit) = frozen.recipe.maximum_boost_db {
        value = value.min((limit - 0.08).max(0.0) - frozen.alignment.trim_db[c]);
    }
    (value * edge) as f32
}
fn design(frozen: &Frozen, baseline: &[Vec<f64>; 2], offset: f64) -> [Vec<f32>; 2] {
    std::array::from_fn(|c| {
        frozen
            .target
            .frequency
            .iter()
            .enumerate()
            .map(|(i, &hz)| {
                let measured = baseline[c][i];
                let hz = f64::from(hz);
                let value =
                    frozen.recipe.profile(hz).0 * (frozen.recipe.target.db(hz) + offset - measured);
                project(value, i, c, frozen)
            })
            .collect()
    })
}
fn huber(e: f64) -> f64 {
    if e.abs() <= 3.0 {
        e * e
    } else {
        6.0 * e.abs() - 9.0
    }
}
fn fit_cost(frozen: &Frozen, offset: f64, designed: &[Vec<f32>; 2]) -> f64 {
    let mut error = 0.0;
    let mut weight = 0.0;
    for (c, channel) in designed.iter().enumerate() {
        for (i, &gain) in channel.iter().enumerate() {
            let f = f64::from(frozen.target.frequency[i]);
            if frozen.recipe.profile(f).0 == 0.0 {
                continue;
            }
            let desired = frozen.recipe.target.db(f) + offset;
            for (p, w) in frozen.baseline.points.iter().zip(&frozen.baseline.weights) {
                let e = f64::from(p[c].db[i] + gain) - desired;
                error += w * (huber(e) + 0.04 * f64::from(gain.max(0.0)).powi(2));
                weight += w;
            }
        }
    }
    if weight == 0.0 { 0.0 } else { error / weight }
}

pub fn freeze(source: &Source, id: ContentHash, recipe: Recipe) -> Result<Frozen> {
    freeze_at_rate(source, id, recipe, source.plan.rate)
}

fn level_alignment(response: [&Response; 2]) -> Result<[f64; 2]> {
    let mut difference = 0.0;
    let mut weight = 0.0;
    for (i, &hz) in response[0].magnitude.frequency.iter().enumerate() {
        if !(300.0..=3000.0).contains(&hz) {
            continue;
        }
        let uncertainty = response
            .iter()
            .map(|r| f64::from(r.uncertainty_db[i]).powi(2))
            .sum::<f64>();
        let w = 1.0 / (1.0 + uncertainty);
        difference += w * f64::from(response[0].magnitude.db[i] - response[1].magnitude.db[i]);
        weight += w;
    }
    if weight == 0.0 {
        return Err(invalid(
            "Speaker level alignment needs a shared band within 300-3000 Hz",
        ));
    }
    let difference = difference / weight;
    Ok([-difference.max(0.0), difference.min(0.0)])
}

/// Select the shortest supported FIR that satisfies the realized response checks.
pub fn freeze_at_rate(
    source: &Source,
    id: ContentHash,
    mut recipe: Recipe,
    rate: Rate,
) -> Result<Frozen> {
    source.validate()?;
    recipe.validate()?;
    recipe.taps = Taps::Short;
    if let Ok(frozen) = freeze_design(source, id, recipe.clone(), rate)
        && realize(&frozen, &frozen.designed_db, rate).is_ok()
    {
        return Ok(frozen);
    }
    recipe.taps = Taps::Long;
    let frozen = freeze_design(source, id, recipe, rate)?;
    realize(&frozen, &frozen.designed_db, rate)?;
    Ok(frozen)
}

fn freeze_design(source: &Source, id: ContentHash, recipe: Recipe, rate: Rate) -> Result<Frozen> {
    source.validate()?;
    recipe.validate()?;
    let mut baseline = aggregate_weighted(source, recipe.spatial.weights(source)?);
    let mut alignment = SpeakerAlignment::default();
    let reference_point = &source.points[source.plan.layout.reference].full;
    if matches!(recipe.alignment, Alignment::Level | Alignment::LevelAndTime) {
        alignment.trim_db = level_alignment(reference_point.each_ref().map(|t| &t.response))?;
    }
    if matches!(recipe.alignment, Alignment::Time | Alignment::LevelAndTime) {
        let arrival = reference_point
            .each_ref()
            .map(|t| t.response.arrival_frames / f64::from(source.plan.rate.hz()));
        let late = arrival.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        alignment.delay_seconds = arrival.map(|s| late - s);
    }
    alignment.validate()?;
    if recipe
        .maximum_cut_db
        .is_some_and(|limit| alignment.trim_db.iter().any(|v| *v < -limit))
    {
        return Err(invalid("Speaker trim exceeds total cut allowance"));
    }
    for (c, trim) in alignment.trim_db.iter().enumerate() {
        baseline.magnitude[c]
            .db
            .iter_mut()
            .for_each(|v| *v += *trim as f32);
        for point in &mut baseline.points {
            point[c].db.iter_mut().for_each(|v| *v += *trim as f32);
        }
    }
    let f = grid();
    let n = f.len();
    let mut reference = Vec::new();
    let mut anchor_frequencies = Vec::new();
    for anchor_midrange in [true, false] {
        for curve in &baseline.magnitude {
            for (i, &hz) in f.iter().enumerate() {
                let hz = f64::from(hz);
                let selected = if anchor_midrange {
                    (300.0..=3000.0).contains(&hz)
                } else {
                    recipe.range.contains(hz)
                };
                if selected && recipe.target.covers(hz) {
                    reference.push(f64::from(curve.db[i]) - recipe.target.db(hz));
                    anchor_frequencies.push(hz);
                }
            }
        }
        if !reference.is_empty() {
            break;
        }
    }
    let anchor_low = anchor_frequencies
        .iter()
        .copied()
        .fold(f64::INFINITY, f64::min);
    let anchor_high = anchor_frequencies.iter().copied().fold(0.0, f64::max);
    if reference.is_empty() || anchor_low >= anchor_high {
        return Err(invalid(
            "No common L/R reference band for the declared target",
        ));
    }
    let target_reference = Band::new(anchor_low, anchor_high)?;
    reference.sort_by(f64::total_cmp);
    let center = reference[reference.len() / 2];
    let empty = std::array::from_fn(|_| Curve {
        frequency: f.clone(),
        db: vec![0.0; n],
    });
    let mut frozen = Frozen {
        rate,
        recipe,
        alignment,
        source: id,
        target_offset_db: center,
        target_reference,
        target: Curve {
            frequency: f.clone(),
            db: vec![0.0; n],
        },
        baseline,
        requested_goal: empty.clone(),
        processed_goal: empty,
        designed_db: std::array::from_fn(|_| vec![0.0; n]),
    };
    let smoothed = smoothed_baseline(&frozen);
    let offset = match frozen.recipe.target_level {
        TargetLevel::Fixed(db) => center + db,
        TargetLevel::Automatic if frozen.recipe.global_effort == 0.0 => center,
        TargetLevel::Automatic => {
            let mut candidates: Vec<_> = (-24..=24)
                .map(|step| {
                    let offset = center + f64::from(step) * 0.5;
                    let d = design(&frozen, &smoothed, offset);
                    (fit_cost(&frozen, offset, &d), offset, d)
                })
                .collect();
            candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
            fit_realized_offset(
                &frozen,
                candidates.into_iter().map(|(_, offset, d)| (offset, d)),
                rate,
            )?
        }
    };
    frozen.target_offset_db = offset;
    frozen.target.db = f
        .iter()
        .map(|&hz| (frozen.recipe.target.db(f64::from(hz)) + offset) as f32)
        .collect();
    frozen.designed_db = design(&frozen, &smoothed, offset);
    for c in 0..2 {
        for (i, &hz) in f.iter().enumerate() {
            let b = frozen.baseline.magnitude[c].db[i];
            frozen.requested_goal[c].db[i] = b
                + (frozen.recipe.profile(f64::from(hz)).0 * f64::from(frozen.target.db[i] - b))
                    as f32;
            frozen.processed_goal[c].db[i] = b + frozen.designed_db[c][i];
        }
    }
    Ok(frozen)
}

fn fit_realized_offset(
    frozen: &Frozen,
    candidates: impl IntoIterator<Item = (f64, [Vec<f32>; 2])>,
    rate: Rate,
) -> Result<f64> {
    let mut best = None;
    let mut feasible = 0;
    let mut first_error = None;
    for (offset, design) in candidates {
        let (channels, _) = match realize(frozen, &design, rate) {
            Ok(realized) => realized,
            Err(error) => {
                first_error.get_or_insert(error);
                continue;
            }
        };
        let actual = std::array::from_fn(|c| {
            analysis::magnitude(&channels[c], rate, &frozen.target.frequency)
                .into_iter()
                .map(|db| db - frozen.alignment.trim_db[c] as f32)
                .collect()
        });
        let cost = fit_cost(frozen, offset, &actual);
        if best.is_none_or(|(previous, _)| cost < previous) {
            best = Some((cost, offset));
        }
        feasible += 1;
        if feasible == 3 {
            break;
        }
    }
    best.map(|(_, offset)| offset)
        .ok_or_else(|| first_error.expect("target-level fitting always supplies candidate offsets"))
}

/// Real-cepstrum minimum phase with unity endpoints; no headphone normalization.
pub fn synthesize(curve: &Curve, rate: Rate, taps: Taps) -> Vec<f32> {
    let length = taps.frames();
    let n = length * 8;
    let mut bins = vec![Complex64::default(); n];
    for k in 0..=n / 2 {
        let hz = k as f64 * f64::from(rate.hz()) / n as f64;
        let db = if hz < 10.0 || hz > 21000.0 {
            0.0
        } else {
            f64::from(curve.at(hz as f32))
        };
        bins[k].re = db * std::f64::consts::LN_10 / 20.0;
        if k > 0 && k < n / 2 {
            bins[n - k] = bins[k];
        }
    }
    let mut planner = FftPlanner::new();
    let forward = planner.plan_fft_forward(n);
    let inverse = planner.plan_fft_inverse(n);
    inverse.process(&mut bins);
    for (i, v) in bins.iter_mut().enumerate() {
        *v = Complex64::new(
            v.re / n as f64
                * if i == 0 || i == n / 2 {
                    1.0
                } else if i < n / 2 {
                    2.0
                } else {
                    0.0
                },
            0.0,
        );
    }
    forward.process(&mut bins);
    bins.iter_mut().for_each(|v| *v = v.exp());
    inverse.process(&mut bins);
    bins[..length]
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let fade = if i > length * 7 / 8 {
                (std::f64::consts::FRAC_PI_2 * (length - i) as f64 / (length / 8) as f64)
                    .sin()
                    .powi(2)
            } else {
                1.0
            };
            (v.re / n as f64 * fade) as f32
        })
        .collect()
}

fn aligned_channels(frozen: &Frozen, design: &[Vec<f32>; 2], rate: Rate) -> [Vec<f32>; 2] {
    std::array::from_fn(|c| {
        let coefficients = synthesize(
            &Curve {
                frequency: frozen.target.frequency.clone(),
                db: design[c].clone(),
            },
            rate,
            frozen.recipe.taps,
        );
        let delay = frozen.alignment.frames(rate)[c];
        let gain = 10f64.powf(frozen.alignment.trim_db[c] / 20.0) as f32;
        std::iter::repeat_n(0.0, delay)
            .chain(coefficients.into_iter().map(|v| v * gain))
            .collect()
    })
}

struct ProjectedFir {
    channels: [Vec<f32>; 2],
    common_gain_db: f64,
    design_scale: f64,
}
fn project_realization(
    frozen: &Frozen,
    design: &[Vec<f32>; 2],
    rate: Rate,
) -> Result<ProjectedFir> {
    for step in 0..=10 {
        let design_scale = 1.0 - f64::from(step) * 0.005;
        let bounded = design
            .each_ref()
            .map(|d| d.iter().map(|v| *v * design_scale as f32).collect());
        let mut channels = aligned_channels(frozen, &bounded, rate);
        let extrema = channels.each_ref().map(|h| {
            let bins = analysis::fft(h, frozen.recipe.taps.frames() * 8);
            let high = bins.iter().map(|v| v.norm()).fold(0.0, f64::max);
            let low = bins.iter().map(|v| v.norm()).fold(f64::INFINITY, f64::min);
            [20.0 * low.max(1e-20).log10(), 20.0 * high.log10()]
        });
        let low = extrema.iter().map(|e| e[0]).fold(f64::INFINITY, f64::min);
        let high = extrema
            .iter()
            .map(|e| e[1])
            .fold(f64::NEG_INFINITY, f64::max);
        let lower = frozen
            .recipe
            .maximum_cut_db
            .map_or(f64::NEG_INFINITY, |limit| -limit - low);
        let upper = frozen
            .recipe
            .maximum_boost_db
            .map_or(f64::INFINITY, |limit| limit - high);
        let common_gain_db = if lower > 0.00001 || upper < -0.00001 {
            if lower + 0.002 > upper {
                continue;
            }
            0.0f64.clamp(lower + 0.001, upper - 0.001)
        } else {
            0.0
        };
        if common_gain_db.abs() > 0.15 {
            continue;
        }
        let common_gain = 10f64.powf(common_gain_db / 20.0) as f32;
        for channel in &mut channels {
            channel.iter_mut().for_each(|v| *v *= common_gain);
        }
        return Ok(ProjectedFir {
            channels,
            common_gain_db,
            design_scale,
        });
    }
    Err(invalid(
        "FIR cannot meet the joint stereo gain limits within 5% design projection; increase smoothing or FIR length",
    ))
}

fn realize(
    frozen: &Frozen,
    design: &[Vec<f32>; 2],
    rate: Rate,
) -> Result<([Vec<f32>; 2], Realization)> {
    frozen.validate()?;
    if frozen.rate != rate {
        return Err(invalid("FIR rate does not match the frozen objective"));
    }
    let f = &frozen.target.frequency;
    if design
        .iter()
        .any(|v| v.len() != f.len() || v.iter().any(|v| !v.is_finite()))
    {
        return Err(invalid("Invalid total correction design"));
    }
    let ProjectedFir {
        channels,
        common_gain_db,
        design_scale,
    } = project_realization(frozen, design, rate)?;
    let mut peak_gain_db = [0.0; 2];
    let mut tail_energy_ratio = [0.0; 2];
    let mut error = 0.0f64;
    let mut error_hz = 0.0;
    for c in 0..2 {
        let bins = analysis::fft(&channels[c], frozen.recipe.taps.frames() * 8);
        let max = bins.iter().map(|b| b.norm()).fold(0.0, f64::max);
        let min = bins.iter().map(|b| b.norm()).fold(f64::INFINITY, f64::min);
        peak_gain_db[c] = 20.0 * max.log10();
        if frozen
            .recipe
            .maximum_boost_db
            .is_some_and(|limit| peak_gain_db[c] > limit + 0.0001)
            || frozen
                .recipe
                .maximum_cut_db
                .is_some_and(|limit| 20.0 * min.max(1e-20).log10() < -limit - 0.0001)
        {
            return Err(invalid("Realized FIR violates cumulative boost/cut limits"));
        }
        let energy: f64 = channels[c].iter().map(|&x| f64::from(x).powi(2)).sum();
        tail_energy_ratio[c] = channels[c][channels[c].len() * 7 / 8..]
            .iter()
            .map(|&x| f64::from(x).powi(2))
            .sum::<f64>()
            / energy.max(1e-24);
        if tail_energy_ratio[c] > 0.001 {
            return Err(invalid(
                "FIR tail is not settled; use a longer filter or smoother correction",
            ));
        }
        let actual = analysis::magnitude(&channels[c], rate, f);
        for ((&a, &d), &hz) in actual.iter().zip(&design[c]).zip(f) {
            if (f64::from(a - d) - frozen.alignment.trim_db[c] - common_gain_db).abs() > error {
                error = (f64::from(a - d) - frozen.alignment.trim_db[c] - common_gain_db).abs();
                error_hz = hz;
            }
        }
    }
    if error > 0.8 {
        return Err(invalid(format!(
            "Truncated FIR differs from design by {error:.2} dB at {error_hz:.1} Hz; increase smoothing"
        )));
    }
    let headroom_db = -peak_gain_db.iter().copied().fold(0.0, f64::max) - 1.0;
    let realization = Realization {
        common_gain_db,
        design_scale,
        peak_gain_db,
        tail_energy_ratio,
        headroom_db,
        maximum_design_error_db: error,
    };
    Ok((channels, realization))
}

pub fn render(
    frozen: &Frozen,
    design: &[Vec<f32>; 2],
    rate: Rate,
    name: &str,
) -> Result<(Correction, Realization)> {
    let (channels, realization) = realize(frozen, design, rate)?;
    let correction = Correction {
        name: name.into(),
        rate,
        channels,
        origin: Origin::Room {
            objective: ContentHash::of(&serde_json::to_vec(frozen)?),
            design_db: design.clone(),
            alignment: frozen.alignment,
            realized: realization.clone(),
        },
    };
    correction.validate()?;
    Ok((correction, realization))
}

#[cfg(test)]
mod realization_tests {
    use super::*;

    fn objective() -> Frozen {
        let frequency = grid();
        let n = frequency.len();
        let flat = Curve {
            frequency,
            db: vec![0.0; n],
        };
        let pair = [flat.clone(), flat.clone()];
        Frozen {
            rate: Rate::Hz48000,
            recipe: Recipe {
                target: Target::Flat,
                maximum_boost_db: Some(3.0),
                maximum_cut_db: Some(12.0),
                taps: Taps::Short,
                ..Recipe::default()
            },
            alignment: SpeakerAlignment::default(),
            source: ContentHash::of(b"realization test"),
            target_offset_db: 0.0,
            target_reference: Band::new(300.0, 3000.0).unwrap(),
            target: flat,
            baseline: SpatialResponse {
                magnitude: pair.clone(),
                spread_db: [vec![0.0; n], vec![0.0; n]],
                uncertainty_db: [vec![0.0; n], vec![0.0; n]],
                points: vec![pair.clone()],
                weights: vec![1.0],
            },
            requested_goal: pair.clone(),
            processed_goal: pair,
            designed_db: [vec![0.0; n], vec![0.0; n]],
        }
    }

    #[test]
    fn mid_bell_has_declared_gain_and_octave_width_in_the_realized_target() {
        let mut frozen = objective();
        frozen.recipe.maximum_boost_db = None;
        frozen.recipe.maximum_cut_db = None;
        for gain in [-6.0, 6.0] {
            frozen.recipe.target = Target::Parametric(TargetParameters {
                mid_gain_db: gain,
                mid_center_hz: 2000.0,
                mid_width_octaves: 2.0,
                ..TargetParameters::default()
            });
            frozen.recipe.validate().unwrap();
            assert!((frozen.recipe.target.db(2000.0) - gain).abs() < 1e-12);
            for hz in [1000.0, 4000.0] {
                assert!((frozen.recipe.target.db(hz) - gain / 2.0).abs() < 1e-12);
            }
            let designed = design(&frozen, &smoothed_baseline(&frozen), 0.0);
            let (channels, _) = realize(&frozen, &designed, frozen.rate).unwrap();
            let actual = analysis::magnitude(&channels[0], frozen.rate, &[1000.0, 2000.0, 4000.0]);
            for (db, expected) in actual.into_iter().zip([gain / 2.0, gain, gain / 2.0]) {
                assert!((f64::from(db) - expected).abs() < 0.05);
            }
        }
    }

    #[test]
    fn saved_parametric_targets_default_to_a_neutral_mid_bell() {
        let target: Target =
            serde_json::from_str(r#"{"Parametric":{"tilt_db_octave":1.0,"bass_db":3.0}}"#).unwrap();
        for hz in [20.0, 100.0, 500.0, 1000.0, 10000.0, 20000.0] {
            assert_eq!(target.db(hz), Target::Warm.db(hz));
        }
        let mut parameters = target.parameters().unwrap();
        assert_eq!(parameters.mid_gain_db, 0.0);
        assert_eq!(parameters.mid_center_hz, 1000.0);
        assert_eq!(parameters.mid_width_octaves, 1.0);
        parameters.mid_gain_db = -3.0;
        parameters.mid_center_hz = 750.0;
        parameters.mid_width_octaves = 1.5;
        let target = Target::Parametric(parameters);
        let restored: Target =
            serde_json::from_slice(&serde_json::to_vec(&target).unwrap()).unwrap();
        assert_eq!(restored.db(1000.0), target.db(1000.0));
        assert_eq!(restored.parameters().unwrap().mid_width_octaves, 1.5);
        assert_eq!(restored.parameters().unwrap().mid_center_hz, 750.0);
    }

    #[test]
    fn optional_gain_limits_preserve_unlimited_design_and_bound_each_direction() {
        let mut frozen = objective();
        frozen.recipe = Recipe {
            target: Target::Flat,
            smoothing_bass: 0.0,
            smoothing_treble: 0.0,
            ..Recipe::default()
        };
        let i = frozen
            .target
            .frequency
            .iter()
            .position(|&f| f > 1000.0)
            .unwrap();
        for channel in &mut frozen.baseline.magnitude {
            channel.db[i] = -30.0;
            channel.db[i + 1] = 42.0;
        }
        let baseline = smoothed_baseline(&frozen);
        let unlimited = design(&frozen, &baseline, 0.0);
        assert_eq!(&unlimited[0][i..=i + 1], &[30.0, -42.0]);
        frozen.recipe.maximum_cut_db = Some(36.0);
        let cut_limited = design(&frozen, &baseline, 0.0);
        assert_eq!(cut_limited[0][i], 30.0);
        assert!((cut_limited[0][i + 1] + 35.92).abs() < 0.001);
        frozen.recipe.maximum_cut_db = None;
        frozen.recipe.maximum_boost_db = Some(24.0);
        let boost_limited = design(&frozen, &baseline, 0.0);
        assert!((boost_limited[0][i] - 23.92).abs() < 0.001);
        assert_eq!(boost_limited[0][i + 1], -42.0);
    }

    #[test]
    fn unlimited_fir_realizes_boost_and_cut_beyond_optional_control_ranges() {
        let mut frozen = objective();
        frozen.recipe.maximum_boost_db = None;
        frozen.recipe.maximum_cut_db = None;
        let design = std::array::from_fn(|c| {
            frozen
                .target
                .frequency
                .iter()
                .map(|&hz| {
                    let shape = (-0.5 * (f64::from(hz) / 1000.0).log2().powi(2) / 0.25).exp();
                    (if c == 0 { 30.0 } else { -42.0 } * shape) as f32
                })
                .collect()
        });
        let (channels, realized) = realize(&frozen, &design, frozen.rate).unwrap();
        assert!(realized.peak_gain_db[0] > 29.0);
        let actual = analysis::magnitude(&channels[1], frozen.rate, &frozen.target.frequency);
        assert!(actual.iter().any(|&db| db < -41.0));
        assert_eq!(realized.common_gain_db, 0.0);
        assert_eq!(realized.design_scale, 1.0);
    }

    #[test]
    fn optional_limits_validate_ranges_and_preserve_saved_numeric_limits() {
        let mut recipe = Recipe::default();
        assert!(recipe.maximum_boost_db.is_none() && recipe.maximum_cut_db.is_none());
        recipe.maximum_boost_db = Some(24.0);
        recipe.maximum_cut_db = Some(36.0);
        recipe.validate().unwrap();
        let json = serde_json::to_value(&recipe).unwrap();
        assert_eq!(json["maximum_boost_db"], 24.0);
        let restored: Recipe = serde_json::from_value(json).unwrap();
        assert_eq!(restored.maximum_cut_db, Some(36.0));
        recipe.maximum_boost_db = Some(24.1);
        assert!(recipe.validate().is_err());
        recipe.maximum_boost_db = None;
        recipe.maximum_cut_db = Some(36.1);
        assert!(recipe.validate().is_err());
    }

    #[test]
    fn smoothing_has_declared_octave_width() {
        let f: Vec<_> = (0..=256)
            .map(|i| (1000.0 * 2f64.powf((f64::from(i) - 128.0) / 64.0)) as f32)
            .collect();
        let mut impulse = vec![0.0; f.len()];
        impulse[128] = 1.0;
        let filtered = smooth(&impulse, &f, |_| 0.5);
        for i in [112, 144] {
            assert!((filtered[i] / filtered[128] - 0.5).abs() < 1e-5);
        }
        assert_eq!(
            smooth(&impulse, &f, |_| 0.0),
            impulse.iter().map(|&v| f64::from(v)).collect::<Vec<_>>()
        );
        assert!(
            smooth(&vec![-6.0; f.len()], &f, |_| 2.0)
                .iter()
                .all(|v| (*v + 6.0).abs() < 1e-12)
        );
    }

    #[test]
    fn regional_smoothing_is_local_and_realizes_a_bounded_filter() {
        let mut frozen = objective();
        let f = frozen.target.frequency.clone();
        for channel in &mut frozen.baseline.magnitude {
            for (db, &hz) in channel.db.iter_mut().zip(&f) {
                *db = (6.0 * (-0.5 * ((f64::from(hz) / 1000.0).log2() / 0.1).powi(2)).exp()) as f32;
            }
        }
        let original = design(&frozen, &smoothed_baseline(&frozen), 0.0);
        frozen.recipe.regions.push(Region {
            band: Band::new(500.0, 2000.0).unwrap(),
            effort: 0.5,
            smoothing_octaves: 0.5,
            lower_fade: 0.5,
            upper_fade: 0.5,
        });
        let changed = design(&frozen, &smoothed_baseline(&frozen), 0.0);
        let peak = f.iter().position(|&hz| hz > 1000.0).unwrap();
        assert!(changed[0][peak] > original[0][peak] * 0.5);
        for (i, &hz) in f.iter().enumerate() {
            if !(500.0..=2000.0).contains(&hz) {
                assert_eq!(changed[0][i], original[0][i]);
            }
        }
        let (_, evidence) = realize(&frozen, &changed, frozen.rate).unwrap();
        evidence.validate().unwrap();
    }

    #[test]
    fn smoothing_applies_regional_effort_once() {
        let mut frozen = objective();
        frozen.recipe.smoothing_bass = 0.0;
        frozen.recipe.smoothing_treble = 0.0;
        frozen.recipe.regions.push(Region {
            band: Band::new(400.0, 1600.0).unwrap(),
            effort: 0.0,
            smoothing_octaves: 0.0,
            lower_fade: 1.0,
            upper_fade: 1.0,
        });
        let designed = design(&frozen, &smoothed_baseline(&frozen), -4.0);
        for (i, &hz) in frozen.target.frequency.iter().enumerate() {
            let expected = -4.0 * frozen.recipe.profile(f64::from(hz)).0;
            if (400.0..=1600.0).contains(&hz) {
                assert!(
                    (f64::from(designed[0][i]) - expected).abs() < 1e-5,
                    "effort at {hz} Hz: {} != {expected}",
                    designed[0][i]
                );
            }
        }
    }

    #[test]
    fn smoothing_preserves_target_shape() {
        let mut frozen = objective();
        let mut target = frozen.target.clone();
        let i = target.frequency.iter().position(|&hz| hz > 1000.0).unwrap();
        target.db[i] = -2.0;
        frozen.recipe.target = Target::Custom(target);
        let designed = design(&frozen, &smoothed_baseline(&frozen), 0.0);
        assert!((designed[0][i] + 2.0).abs() < 1e-5);
        assert!(designed[0][i - 1].abs() < 1e-5);
    }

    #[test]
    fn target_level_search_skips_unrealizable_candidates_and_checks_the_final_fir() {
        let frozen = objective();
        let n = frozen.target.frequency.len();
        let excessive = [vec![30.0; n], vec![30.0; n]];
        let mut unresolved = frozen.designed_db.clone();
        let i = frozen
            .target
            .frequency
            .iter()
            .position(|&f| f > 40.0)
            .unwrap();
        unresolved[0][i] = -12.0;
        assert!(realize(&frozen, &excessive, frozen.rate).is_err());
        assert!(realize(&frozen, &unresolved, frozen.rate).is_err());
        let offset = fit_realized_offset(
            &frozen,
            [
                (2.0, excessive.clone()),
                (1.0, unresolved),
                (0.0, frozen.designed_db.clone()),
            ],
            frozen.rate,
        )
        .unwrap();
        assert_eq!(offset, 0.0);
        assert!(fit_realized_offset(&frozen, [(2.0, excessive)], frozen.rate).is_err());
    }

    #[test]
    fn speaker_level_alignment_uses_the_shared_midrange() {
        let frozen = objective();
        let n = frozen.target.frequency.len();
        let left = Response {
            diagnostics: crate::measurement::Diagnostics {
                pre_arrival_energy_ratio: 0.0,
                late_energy_ratio: 0.0,
                polarity: crate::measurement::Polarity::Positive,
            },
            magnitude: frozen.target.clone(),
            uncertainty_db: vec![0.1; n],
            impulse: crate::measurement::Waveform {
                object: frozen.source,
                frames: 1,
                rate: frozen.rate,
            },
            arrival_frames: 0.0,
            resolution_hz: 1.0,
            trusted_band: Band::default(),
        };
        let mut right = left.clone();
        right.magnitude.db.fill(-2.0);
        let trim = level_alignment([&left, &right]).unwrap();
        assert!((trim[0] + 2.0).abs() < 1e-6);
        assert_eq!(trim[1], 0.0);
    }
}
