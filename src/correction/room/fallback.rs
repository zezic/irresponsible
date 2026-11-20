//! Reading recipes saved with a global effort, range fades, regional overrides and a fixed
//! 200 Hz-6 kHz smoothing blend.
use super::{Effort, EffortPoint, Recipe, Smoothing};
use crate::measurement::Band;
use serde::{Deserialize, Deserializer, de::Error as _};
use serde_json::{Map, Value};

impl<'de> Deserialize<'de> for Recipe {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut value = Value::deserialize(deserializer)?;
        if let Value::Object(fields) = &mut value
            && fields.contains_key("global_effort")
        {
            upgrade(fields).map_err(D::Error::custom)?;
        }
        Recipe::deserialize(value).map_err(D::Error::custom)
    }
}

#[derive(Deserialize)]
struct Legacy {
    range: Band,
    global_effort: f64,
    lower_fade: f64,
    upper_fade: f64,
    smoothing_bass: f64,
    smoothing_treble: f64,
    #[serde(default)]
    regions: Vec<Region>,
}

#[derive(Deserialize)]
struct Region {
    band: Band,
    effort: f64,
    lower_fade: f64,
    upper_fade: f64,
}
impl Region {
    fn weight(&self, hz: f64) -> f64 {
        if !self.band.contains(hz) {
            return 0.0;
        }
        let smooth = |x: f64| {
            let x = x.clamp(0.0, 1.0);
            x * x * (3.0 - 2.0 * x)
        };
        let ramp = |octaves: f64, fade: f64| {
            if fade == 0.0 {
                1.0
            } else {
                smooth(octaves / fade)
            }
        };
        ramp((hz / self.band.low).log2(), self.lower_fade)
            .min(ramp((self.band.high / hz).log2(), self.upper_fade))
    }
}

const LEGACY_FIELDS: [&str; 6] = [
    "global_effort",
    "lower_fade",
    "upper_fade",
    "smoothing_bass",
    "smoothing_treble",
    "regions",
];

fn upgrade(fields: &mut Map<String, Value>) -> serde_json::Result<()> {
    let legacy: Legacy = serde_json::from_value(Value::Object(fields.clone()))?;
    for key in LEGACY_FIELDS {
        fields.remove(key);
    }
    fields.insert("effort".into(), serde_json::to_value(effort(&legacy))?);
    fields.insert(
        "smoothing".into(),
        serde_json::to_value(Smoothing {
            bass_octaves: legacy.smoothing_bass,
            treble_octaves: legacy.smoothing_treble,
            crossover_hz: (200.0f64 * 6000.0).sqrt(),
            transition_octaves: 30.0f64.log2(),
        })?,
    );
    Ok(())
}

fn effort(legacy: &Legacy) -> Effort {
    let easy = Effort::Easy {
        level: legacy.global_effort,
        low_fade_octaves: legacy.lower_fade,
        high_fade_octaves: legacy.upper_fade,
    };
    if legacy.regions.is_empty() {
        return easy;
    }
    // Regional smoothstep fades become five knots each; the line stays within a few percent.
    let range = legacy.range;
    let mut knots: Vec<f64> = easy.points(range).iter().map(|p| p.hz).collect();
    for r in &legacy.regions {
        for (from, octaves) in [(r.band.low, r.lower_fade), (r.band.high, -r.upper_fade)] {
            knots.extend((0..=4).map(|k| from * (octaves * f64::from(k) / 4.0).exp2()));
        }
    }
    knots.retain(|hz| range.contains(*hz));
    knots.sort_by(f64::total_cmp);
    knots.dedup_by(|b, a| *b <= *a * (1.0 + 1e-6));
    Effort::Advanced(
        knots
            .into_iter()
            .map(|hz| {
                let region = legacy
                    .regions
                    .iter()
                    .find(|r| r.band.contains(hz))
                    .map_or(1.0, |r| 1.0 + r.weight(hz) * (r.effort - 1.0));
                EffortPoint {
                    hz,
                    effort: easy.at(hz, range) * region,
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_recipes_keep_their_effort_and_smoothing() {
        let mut json = serde_json::to_value(Recipe::default()).unwrap();
        let fields = json.as_object_mut().unwrap();
        fields.remove("effort");
        fields.remove("smoothing");
        let legacy = serde_json::json!({
            "global_effort": 0.8, "lower_fade": 0.5, "upper_fade": 0.25,
            "smoothing_bass": 0.1, "smoothing_treble": 0.5,
            "regions": [{"band": [400.0, 1600.0], "effort": 0.25, "smoothing_octaves": 0.3,
                         "lower_fade": 0.5, "upper_fade": 0.5}]
        });
        fields.extend(legacy.as_object().unwrap().clone());
        let recipe: Recipe = serde_json::from_value(json).unwrap();
        recipe.validate().unwrap();
        assert!((recipe.smoothing.width_at(200.0) - 0.1).abs() < 1e-12);
        assert!((recipe.smoothing.width_at(6000.0) - 0.5).abs() < 1e-12);
        for (hz, expected) in [(20.0, 0.0), (100.0, 0.8), (800.0, 0.2), (10000.0, 0.8)] {
            assert!((recipe.effort_at(hz) - expected).abs() < 0.02, "{hz} Hz");
        }
    }
}
