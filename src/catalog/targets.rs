use super::Entry;
use crate::{
    correction::Curve,
    domain::Target,
    error::{Result, invalid},
};
use rustfft::num_complex::Complex64;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Form {
    OverEar,
    InEar,
    Earbud,
}
impl Form {
    fn suffix(self) -> &'static str {
        match self {
            Self::OverEar => "over-ear",
            Self::InEar => "in-ear",
            Self::Earbud => "earbud",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
enum MappedTarget {
    Harman(TargetName),
    Reference(TargetName),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MeasurementReference {
    source: String,
    form: Form,
    rig: Option<String>,
    target: MappedTarget,
}
impl MeasurementReference {
    fn for_entry(entry: &Entry, targets: &[ReferenceTarget]) -> Option<Self> {
        let path = percent_encoding::percent_decode_str(&entry.path)
            .decode_utf8()
            .ok()?;
        let mut parts = path.split('/');
        let source = parts.next()?;
        let group = parts.next()?;
        let form = [Form::OverEar, Form::InEar, Form::Earbud]
            .into_iter()
            .find(|form| {
                group
                    .strip_suffix(form.suffix())
                    .is_some_and(|prefix| prefix.is_empty() || prefix.ends_with(' '))
            })?;
        let rig = group.strip_suffix(form.suffix())?.trim();
        let rig = (!rig.is_empty()).then_some(rig);
        let matching = |target: &&ReferenceTarget| {
            target
                .compatible
                .iter()
                .any(|rule| rule.matches(source, form, rig))
        };
        let target = if let Some(target) = targets
            .iter()
            .filter(matching)
            .find(|target| target.kind() == Target::Harman)
        {
            MappedTarget::Harman(target.label.clone())
        } else {
            let mut compatible = targets
                .iter()
                .filter(matching)
                .filter(|target| target.kind() == Target::Reference);
            let target = compatible
                .clone()
                .find(|target| {
                    target
                        .recommended
                        .iter()
                        .any(|rule| rule.matches(source, form, rig))
                })
                .or_else(|| compatible.next())?;
            MappedTarget::Reference(target.label.clone())
        };
        Some(Self {
            source: source.to_owned(),
            form,
            rig: rig.map(str::to_owned),
            target,
        })
    }
    pub fn target(&self, target: Target) -> Option<&TargetName> {
        match (&self.target, target) {
            (MappedTarget::Harman(name), Target::Harman)
            | (MappedTarget::Reference(name), Target::Reference) => Some(name),
            _ => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetName(String);
impl TargetName {
    pub fn label(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Compatibility {
    source: String,
    form: Form,
    rig: Option<String>,
}
impl Compatibility {
    fn matches(&self, source: &str, form: Form, rig: Option<&str>) -> bool {
        self.source == source
            && self.form == form
            && self
                .rig
                .as_deref()
                .is_none_or(|expected| rig == Some(expected))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Response {
    frequency: Vec<f32>,
    raw: Vec<f32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Shelf {
    fc: f64,
    q: f64,
    gain: f64,
}
impl Shelf {
    fn db(&self, hz: f64) -> f32 {
        // AutoEq's RBJ low shelf uses a 44.1 kHz reference when building targets.
        let a = 10_f64.powf(self.gain / 40.0);
        let w = std::f64::consts::TAU * self.fc / 44100.0;
        let c = w.cos();
        let alpha = w.sin() / (2.0 * self.q);
        let t = 2.0 * a.sqrt() * alpha;
        let b = [
            a * ((a + 1.0) - (a - 1.0) * c + t),
            2.0 * a * ((a - 1.0) - (a + 1.0) * c),
            a * ((a + 1.0) - (a - 1.0) * c - t),
        ];
        let d = [
            (a + 1.0) + (a - 1.0) * c + t,
            -2.0 * ((a - 1.0) + (a + 1.0) * c),
            (a + 1.0) + (a - 1.0) * c - t,
        ];
        let z = Complex64::from_polar(1.0, -std::f64::consts::TAU * hz / 44100.0);
        let response = (b[0] + b[1] * z + b[2] * z * z) / (d[0] + d[1] * z + d[2] * z * z);
        (20.0 * response.norm().log10()) as f32
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReferenceTarget {
    label: TargetName,
    compatible: Vec<Compatibility>,
    recommended: Vec<Compatibility>,
    fr: Response,
    #[serde(rename = "bassBoost")]
    bass_boost: Shelf,
}
impl ReferenceTarget {
    pub fn name(&self) -> &TargetName {
        &self.label
    }
    pub fn kind(&self) -> Target {
        if self.label.0.contains("Harman over-ear") || self.label.0.contains("Harman in-ear") {
            Target::Harman
        } else {
            Target::Reference
        }
    }
    pub fn bass_description(&self) -> String {
        format!(
            "bass {:+.1} dB at {} Hz, Q {}",
            self.bass_boost.gain, self.bass_boost.fc, self.bass_boost.q
        )
    }
    pub fn description(&self) -> String {
        format!("{} ({})", self.label.0, self.bass_description())
    }
    pub fn curve(&self) -> Result<Curve> {
        let mut curve = Curve::new(self.fr.frequency.clone(), self.fr.raw.clone())?;
        for (hz, db) in curve.frequency.iter().zip(&mut curve.db) {
            *db += self.bass_boost.db(f64::from(*hz));
        }
        Curve::new(curve.frequency, curve.db)
    }
    pub fn parse(bytes: &[u8]) -> Result<Vec<Self>> {
        let targets: Vec<Self> = serde_json::from_slice(bytes)?;
        if targets.is_empty() || targets.len() > 256 {
            return Err(invalid("Invalid upstream target count"));
        }
        let mut names = std::collections::BTreeSet::new();
        for target in &targets {
            if target.label.0.is_empty()
                || !names.insert(&target.label.0)
                || !target.bass_boost.fc.is_finite()
                || !(1.0..=20000.0).contains(&target.bass_boost.fc)
                || !target.bass_boost.q.is_finite()
                || target.bass_boost.q <= 0.0
                || !target.bass_boost.gain.is_finite()
                || target.bass_boost.gain.abs() > 24.0
            {
                return Err(invalid("Invalid upstream target metadata"));
            }
            target.curve()?;
        }
        Ok(targets)
    }
}
pub(super) fn assign(entries: &mut [Entry], targets: &[ReferenceTarget]) {
    for entry in entries {
        entry.reference = MeasurementReference::for_entry(entry, targets);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shelf_matches_upstream_rbj_reference_points() {
        let shelf = Shelf {
            fc: 105.0,
            q: 0.7,
            gain: 6.0,
        };
        assert!((shelf.db(1.0) - 6.0).abs() < 0.001);
        assert!((shelf.db(105.0) - 3.0).abs() < 0.001);
        assert!(shelf.db(20000.0).abs() < 0.001);
    }
    #[test]
    fn rtings_5128_and_hms_references_select_different_target_families() {
        let catalog = super::super::Catalog::parse(
            include_str!("../../tests/fixtures/autoeq-index.md"),
            include_str!("../../tests/fixtures/autoeq-recommended.md"),
            super::super::UPSTREAM_REVISION,
        )
        .unwrap();
        for (path, target, label) in [
            (
                "Rtings/Bruel%20&%20Kjaer%205128%20over-ear/Aiaiai%20TMA-2%20DJ",
                Target::Reference,
                "LMG 5128 0.6 without bass",
            ),
            (
                "Rtings/Bruel%20&%20Kjaer%205128%20in-ear/Anker%20Soundcore%20C30i",
                Target::Reference,
                "JM-1 with Harman filters",
            ),
            (
                "Rtings/Bruel%20&%20Kjaer%205128%20earbud/Apple%20AirPods%20(3rd%20generation)",
                Target::Reference,
                "LMG 5128 0.6 without bass",
            ),
            (
                "Rtings/HMS%20II.3%20over-ear/AKG%20K240%20MKII",
                Target::Harman,
                "HMS II.3 Harman over-ear 2018",
            ),
        ] {
            let entry = catalog
                .entries
                .iter()
                .find(|entry| entry.path == path)
                .unwrap();
            assert_eq!(catalog.target(entry, target).unwrap().name().label(), label);
            if target == Target::Reference {
                assert!(catalog.target(entry, Target::Harman).is_err());
            }
        }
        let unknown = super::super::Catalog::parse(
            "- [Example](./Rtings/New%20rig%20over-ear/Example) by Rtings",
            "",
            super::super::UPSTREAM_REVISION,
        )
        .unwrap();
        assert!(unknown.entries[0].reference.is_none());
    }
    #[test]
    fn invalid_target_metadata_is_rejected_before_generation() {
        let mut json: serde_json::Value =
            serde_json::from_slice(include_bytes!("../../assets/autoeq-targets.json")).unwrap();
        json[0]["bassBoost"]["q"] = serde_json::json!(0.0);
        assert!(ReferenceTarget::parse(&serde_json::to_vec(&json).unwrap()).is_err());
    }
}
