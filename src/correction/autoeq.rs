//! AutoEq's correction algorithm, ported from frequency_response.py at
//! 7ae0f56d53074872b028649617a22bbb4232feb7. FIR design follows SciPy's
//! firwin2 and homomorphic minimum_phase. See assets/AutoEq-LICENSE and
//! assets/SciPy-LICENSE.

use super::{
    BassExtension, ConchaInterference, Curve, FirLength, GenerationParameter::*,
    GenerationSettings, Normalization, TrebleExtension,
};
use crate::domain::Rate;
use rustfft::{FftPlanner, num_complex::Complex64};
use std::ops::Range;

const STEP: f64 = 1.01;

/// Correction in dB before FIR-grid peak normalization, including selected band overrides.
pub fn correction_curve(measured: &Curve, target: &Curve, settings: GenerationSettings) -> Curve {
    let frequency: Vec<_> = std::iter::successors(Some(20.0), |f| Some(f * STEP))
        .take_while(|&f| f <= 20000.0)
        .collect();
    let error: Vec<_> = frequency
        .iter()
        .map(|&f| interpolate(target, f) - interpolate(measured, f))
        .collect();
    let mut gain = equalize(&frequency, &error, settings);
    for (&hz, db) in frequency.iter().zip(&mut gain) {
        if let Some(cut) = settings.cut_limit() {
            *db = db.max(-f64::from(cut));
        }
        let low = if settings.bass() == BassExtension::Tapered {
            ((hz - f64::from(settings.value(LowUnity)))
                / f64::from(settings.value(LowFull) - settings.value(LowUnity)))
            .clamp(0.0, 1.0)
        } else {
            1.0
        };
        let high = if settings.treble() == TrebleExtension::Limited {
            ((f64::from(settings.value(HighUnity)) - hz)
                / f64::from(settings.value(HighUnity) - settings.value(HighFull)))
            .clamp(0.0, 1.0)
        } else {
            1.0
        };
        *db *= low * high;
    }
    let mut curve = Curve {
        frequency: frequency.into_iter().map(|f| f as f32).collect(),
        db: gain.into_iter().map(|db| db as f32).collect(),
    };
    if settings.bass() == BassExtension::Tapered {
        curve.frequency.insert(0, settings.value(LowUnity));
        curve.db.insert(0, 0.0);
        // The taper's explicit boundary must precede every retained grid point.
        let first = curve.frequency[1..].partition_point(|&f| f <= settings.value(LowUnity));
        curve.frequency.drain(1..=first);
        curve.db.drain(1..=first);
    }
    curve
}

/// Linear interpolation/extrapolation in log frequency, as in AutoEq's k=1 spline.
fn interpolate(curve: &Curve, hz: f64) -> f64 {
    let upper = curve.frequency.partition_point(|&f| f64::from(f) < hz);
    let upper = upper.clamp(1, curve.frequency.len() - 1);
    let lower = upper - 1;
    let t = (hz / f64::from(curve.frequency[lower])).ln()
        / (f64::from(curve.frequency[upper]) / f64::from(curve.frequency[lower])).ln();
    f64::from(curve.db[lower]) + t * (f64::from(curve.db[upper]) - f64::from(curve.db[lower]))
}

fn equalize(frequency: &[f64], error: &[f64], settings: GenerationSettings) -> Vec<f64> {
    let gain = smooth(frequency, error, settings);
    let peak_indices: Vec<_> = peaks(&gain).collect();
    let inverted: Vec<_> = gain.iter().map(|db| -db).collect();
    let dips: Vec<_> = peaks(&inverted).collect();
    if peak_indices.is_empty() && dips.is_empty() {
        // AutoEq returns the smoothed inverse error directly for monotonic curves.
        return gain;
    }
    let mut protected = protection_mask(&gain, &peak_indices, &dips);
    if settings.concha() == ConchaInterference::Present {
        for (&hz, protected) in frequency.iter().zip(&mut protected) {
            if (8000.0..=11500.0).contains(&hz) {
                *protected = false;
            }
        }
    }
    let start = rtl_start(&gain, &peak_indices, &dips);
    let forward = limited_slope(frequency, &gain, &peak_indices, &protected, 0, settings);
    let reversed: Vec<_> = gain.iter().rev().copied().collect();
    let reversed_peaks: Vec<_> = peak_indices.iter().map(|i| gain.len() - i - 1).collect();
    let reversed_protection: Vec<_> = protected.iter().rev().copied().collect();
    // AutoEq reverses the gain/protection arrays while retaining the ascending grid.
    let backward = limited_slope(
        frequency,
        &reversed,
        &reversed_peaks,
        &reversed_protection,
        gain.len() - start - 1,
        settings,
    );
    let combined: Vec<_> = forward
        .iter()
        .zip(backward.iter().rev())
        .zip(frequency)
        .map(|((&left, &right), &hz)| {
            let treble = sigmoid(hz, settings);
            let strength = 1.0 + treble * (f64::from(settings.value(TrebleGain)) - 1.0);
            (left.min(right) * strength).min(f64::from(settings.value(BoostLimit)))
        })
        .collect();
    savgol(&combined, 1.0 / 5.0)
}

/// SciPy find_peaks with prominence=1, including plateau midpoint selection.
fn peaks(values: &[f64]) -> impl Iterator<Item = usize> + '_ {
    (1..values.len() - 1).filter_map(|left| {
        if values[left - 1] >= values[left] {
            return None;
        }
        let mut right = left;
        while right + 1 < values.len() && values[right + 1] == values[left] {
            right += 1;
        }
        if right + 1 == values.len() || values[right + 1] >= values[left] {
            return None;
        }
        let height = values[left];
        let left_base = values[..left]
            .iter()
            .rev()
            .take_while(|&&y| y <= height)
            .copied()
            .fold(height, f64::min);
        let right_base = values[right + 1..]
            .iter()
            .take_while(|&&y| y <= height)
            .copied()
            .fold(height, f64::min);
        (height - left_base.max(right_base) >= 1.0).then_some((left + right) / 2)
    })
}

fn protection_mask(gain: &[f64], peaks: &[usize], dips: &[usize]) -> Vec<bool> {
    let mut protected = vec![false; gain.len()];
    let boundary = if let Some(&peak) = peaks
        .last()
        .filter(|&&p| dips.last().is_none_or(|&d| p > d))
    {
        gain[peak..].iter().copied().fold(f64::INFINITY, f64::min)
    } else {
        gain.iter().copied().fold(f64::INFINITY, f64::min)
    };
    let levels: Vec<_> = dips.iter().map(|&i| gain[i]).chain([boundary]).collect();
    for (i, neighbours) in levels.windows(3).enumerate() {
        let dip = dips[i + 1];
        let left = gain[..dip]
            .iter()
            .rposition(|&y| y >= neighbours[0])
            .expect("the previous dip bounds the protected region")
            + 1;
        let right = gain[dip..]
            .iter()
            .position(|&y| y >= neighbours[2])
            .expect("the next dip bounds the protected region")
            + dip;
        if left < right {
            protected[left..right].fill(true);
        }
    }
    protected
}

fn rtl_start(gain: &[f64], peaks: &[usize], dips: &[usize]) -> usize {
    if let Some(&peak) = peaks
        .last()
        .filter(|&&p| dips.last().is_none_or(|&d| p > d))
    {
        let level = dips
            .last()
            .map_or(gain[0].max(gain[gain.len() - 1]), |&d| gain[d]);
        gain[peak..]
            .iter()
            .position(|&y| y <= level)
            .map_or(gain.len() - 1, |i| peak + i)
    } else {
        *dips
            .last()
            .expect("a non-monotonic curve has a peak or dip")
    }
}

fn limited_slope(
    frequency: &[f64],
    gain: &[f64],
    peaks: &[usize],
    protected: &[bool],
    start: usize,
    settings: GenerationSettings,
) -> Vec<f64> {
    let mut limited = gain.to_vec();
    let mut region: Option<Range<usize>> = None;
    for i in start + 1..gain.len() {
        let octaves = (frequency[i] / frequency[i - 1]).log2();
        let slope = (gain[i] - limited[i - 1]) / octaves;
        let mut limit = f64::from(settings.value(SlopeLimit));
        if settings.concha() == ConchaInterference::Present
            && (8000.0..=11500.0).contains(&frequency[i])
        {
            limit /= 4.0;
        }
        if let Some(region) = &region {
            limit *= (1.0 - f64::from(settings.value(SlopeDecay)))
                .powf((frequency[i] / frequency[region.start]).log2());
        }
        if slope > limit && !protected[i] {
            let region = region.get_or_insert(i..i);
            region.end = i + 1;
            limited[i] = limited[i - 1] + limit * octaves;
        } else if let Some(region) = region.take()
            && !peaks.iter().any(|p| region.contains(p))
        {
            limited[region.clone()].copy_from_slice(&gain[region]);
        }
    }
    limited
}

fn sigmoid(hz: f64, settings: GenerationSettings) -> f64 {
    let low = f64::from(settings.value(TrebleStart)).ln();
    let high = f64::from(settings.value(TrebleEnd)).ln();
    1.0 / (1.0 + (-8.0 * (hz.ln() - (low + high) / 2.0) / (high - low)).exp())
}

fn smooth(frequency: &[f64], values: &[f64], settings: GenerationSettings) -> Vec<f64> {
    let normal = savgol(values, f64::from(settings.value(Smoothing)));
    let treble = savgol(values, f64::from(settings.value(TrebleSmoothing)));
    frequency
        .iter()
        .zip(normal.iter().zip(&treble))
        .map(|(&hz, (&normal, &treble))| {
            let blend = sigmoid(hz, settings);
            normal * (1.0 - blend) + treble * blend
        })
        .collect()
}

/// Quadratic Savitzky-Golay with SciPy's default polynomial-interpolated edges.
fn savgol(values: &[f64], octaves: f64) -> Vec<f64> {
    let window = ((octaves * 2.0_f64.ln() / STEP.ln()).round_ties_even() as usize) | 1;
    assert!(window >= 3 && window <= values.len());
    let half = window / 2;
    let n = window as f64;
    let second = (half * (half + 1)) as f64 / 3.0;
    let variance = (0..window)
        .map(|i| ((i as f64 - half as f64).powi(2) - second).powi(2))
        .sum::<f64>();
    (0..values.len())
        .map(|i| {
            let start = i.saturating_sub(half).min(values.len() - window);
            let position = (i - start) as f64 - half as f64;
            values[start..start + window]
                .iter()
                .enumerate()
                .map(|(j, &y)| {
                    let x = j as f64 - half as f64;
                    let weight = 1.0 / n
                        + position * x / (n * second)
                        + (position.powi(2) - second) * (x.powi(2) - second) / variance;
                    weight * y
                })
                .sum()
        })
        .collect()
}

/// AutoEq's firwin2(2*n) Hamming design followed by half-length homomorphic
/// conversion. The selected tap count determines its frequency resolution.
pub fn minimum_phase(
    curve: &Curve,
    rate: Rate,
    length: FirLength,
    normalization: Normalization,
) -> Vec<f32> {
    let n = length.frames();
    let nyquist = f64::from(rate.hz()) / 2.0;
    let mut gain: Vec<_> = (0..n)
        .map(|i| {
            let hz = (i as f64 * nyquist / (n - 1) as f64)
                .max(f64::from(curve.frequency[0]).max(nyquist / (n - 1) as f64));
            interpolate(curve, hz)
        })
        .collect();
    let offset = match normalization {
        Normalization::Raw => 0.0,
        Normalization::Peak => gain.iter().copied().fold(f64::NEG_INFINITY, f64::max) + 0.2,
    };
    // AutoEq doubles dB gain because half-length conversion takes the square root.
    gain.iter_mut()
        .for_each(|db| *db = 10.0_f64.powf((*db - offset) / 10.0));
    gain[n - 1] = 0.0;

    let taps = 2 * n;
    let fft_len = 2 * taps;
    let mut bins = vec![Complex64::default(); fft_len];
    for i in 0..=taps {
        let position = i as f64 * (n - 1) as f64 / taps as f64;
        let lower = (position.floor() as usize).min(n - 2);
        let amplitude = gain[lower] + (position - lower as f64) * (gain[lower + 1] - gain[lower]);
        let phase = -((taps - 1) as f64) / 2.0 * std::f64::consts::PI * i as f64 / taps as f64;
        bins[i] = Complex64::from_polar(amplitude, phase);
        if i > 0 && i < taps {
            bins[fft_len - i] = bins[i].conj();
        }
    }
    let mut planner = FftPlanner::<f64>::new();
    planner.plan_fft_inverse(fft_len).process(&mut bins);
    let mut linear: Vec<_> = bins[..taps]
        .iter()
        .enumerate()
        .map(|(i, bin)| {
            let window =
                0.54 - 0.46 * (2.0 * std::f64::consts::PI * i as f64 / (taps - 1) as f64).cos();
            Complex64::new(bin.re / fft_len as f64 * window, 0.0)
        })
        .collect();
    let forward = planner.plan_fft_forward(taps);
    let inverse = planner.plan_fft_inverse(taps);
    forward.process(&mut linear);
    let floor = linear
        .iter()
        .map(|v| v.norm())
        .filter(|&v| v > 0.0)
        .fold(f64::INFINITY, f64::min)
        * 1e-7;
    for bin in &mut linear {
        *bin = Complex64::new(0.5 * (bin.norm() + floor).ln(), 0.0);
    }
    inverse.process(&mut linear);
    // Only the real cepstrum participates in the homomorphic lifter.
    for (i, bin) in linear.iter_mut().enumerate() {
        let weight = if i == 0 || i == n {
            1.0
        } else if i < n {
            2.0
        } else {
            0.0
        };
        *bin = Complex64::new(bin.re / taps as f64 * weight, 0.0);
    }
    forward.process(&mut linear);
    linear.iter_mut().for_each(|bin| *bin = bin.exp());
    inverse.process(&mut linear);
    linear[..n]
        .iter()
        .map(|bin| (bin.re / taps as f64) as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::peaks;

    #[test]
    fn prominence_handles_plateaus_equal_peaks_and_nested_peaks() {
        let values = [
            0.0, 2.0, 2.0, 0.0, 2.0, 2.0, 2.0, 0.0, 0.5, 0.0, 3.0, 2.5, 2.8, 0.0,
        ];
        assert_eq!(peaks(&values).collect::<Vec<_>>(), [1, 5, 10]);
    }
}
