use irresponsible::{
    correction::{
        BassExtension, Curve, FirLength, GenerationParameter, GenerationSettings, Normalization,
        TrebleExtension, correction_curve, minimum_phase,
    },
    domain::Rate,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Reference {
    frequency: Vec<f32>,
    impulse_curve: Curve,
    cases: Vec<CorrectionCase>,
    impulses: Vec<ImpulseCase>,
}

#[derive(Deserialize)]
struct CorrectionCase {
    name: String,
    measured: FixtureCurve,
    target: Option<FixtureCurve>,
    settings: GenerationSettings,
    expected: Vec<f32>,
}

#[derive(Deserialize)]
struct ImpulseCase {
    rate: u32,
    length: FirLength,
    normalization: Normalization,
    coefficients: Vec<f32>,
}

#[derive(Deserialize)]
struct FixtureCurve {
    frequency: Option<Vec<f32>>,
    db: Vec<f32>,
}
impl FixtureCurve {
    fn curve(&self, grid: &[f32]) -> Curve {
        Curve::new(
            self.frequency.as_deref().unwrap_or(grid).to_vec(),
            self.db.clone(),
        )
        .unwrap()
    }
}
impl CorrectionCase {
    fn curves(&self, grid: &[f32]) -> (Curve, Curve) {
        let measured = self.measured.curve(grid);
        let target = self.target.as_ref().map_or_else(
            || Curve::new(grid.to_vec(), vec![0.0; grid.len()]).unwrap(),
            |target| target.curve(grid),
        );
        (measured, target)
    }
}

fn reference() -> Reference {
    serde_json::from_str(include_str!("fixtures/autoeq-parity.json")).unwrap()
}

#[test]
fn correction_matches_pinned_autoeq_on_real_and_synthetic_measurements() {
    let reference = reference();
    for case in reference.cases {
        let (measured, target) = case.curves(&reference.frequency);
        let actual = correction_curve(&measured, &target, case.settings);
        assert_eq!(actual.db.len(), case.expected.len(), "{}", case.name);
        let max_error = actual
            .db
            .iter()
            .zip(&case.expected)
            .map(|(actual, expected)| (actual - expected).abs())
            .fold(0.0_f32, f32::max);
        assert!(max_error < 0.0002, "{}: {max_error} dB", case.name);
    }
}

#[test]
fn minimum_phase_matches_autoeq_scipy_design_at_both_rates_and_gain_modes() {
    let reference = reference();
    for case in reference.impulses {
        let rate = match case.rate {
            44100 => Rate::Hz44100,
            48000 => Rate::Hz48000,
            _ => panic!("unsupported fixture rate"),
        };
        let actual = minimum_phase(
            &reference.impulse_curve,
            rate,
            case.length,
            case.normalization,
        );
        assert_eq!(actual.len(), case.coefficients.len());
        let max_error = actual
            .iter()
            .zip(&case.coefficients)
            .map(|(actual, expected)| (actual - expected).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            max_error < 0.0002,
            "{} {:?}: {max_error}",
            case.rate,
            case.normalization
        );
    }
}

#[test]
fn optional_limits_are_applied_only_when_selected() {
    let reference = reference();
    let case = reference
        .cases
        .into_iter()
        .find(|c| c.name == "protected-cuts")
        .unwrap();
    let settings = case
        .settings
        .with(GenerationParameter::CutLimit, 3.0)
        .unwrap()
        .with_bass(BassExtension::Tapered)
        .with_treble(TrebleExtension::Limited);
    let (measured, target) = case.curves(&reference.frequency);
    let curve = correction_curve(&measured, &target, settings);
    assert!(curve.db.iter().all(|&db| db >= -3.0));
    assert_eq!(curve.at(1.0), 0.0);
    assert_eq!(curve.at(18000.0), 0.0);
    let original = correction_curve(&measured, &target, case.settings);
    assert!(original.db.iter().any(|&db| db < -3.0));
    assert!(original.at(18000.0).abs() > 0.1);
}

#[test]
fn settings_validate_transition_and_optional_limits() {
    let settings = GenerationSettings::default();
    assert!(
        settings
            .with(GenerationParameter::TrebleStart, 8000.0)
            .is_err()
    );
    assert!(
        settings
            .with(GenerationParameter::TrebleEnd, 6000.0)
            .is_err()
    );
    assert!(
        settings
            .with(GenerationParameter::SlopeDecay, 1.01)
            .is_err()
    );
    assert!(
        settings
            .with(GenerationParameter::CutLimit, f32::NAN)
            .is_err()
    );
    assert!(settings.cut_limit().is_none());
    let limited = settings.with(GenerationParameter::CutLimit, 4.0).unwrap();
    let saved = serde_json::to_string(&limited).unwrap();
    assert_eq!(
        serde_json::from_str::<GenerationSettings>(&saved).unwrap(),
        limited
    );
    assert_eq!(limited.without_cut_limit(), settings);
}
