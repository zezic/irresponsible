//! Offline f64 identification. Original ADC samples never enter the listening servo.
use super::{Band, Calibration, ClockMap, Noise, NoiseLimits, Response, Waveform, probe::Probe};
use super::{Diagnostics, Polarity, workflow::Fault};
use crate::{correction::Curve, domain::Rate, error::Result, storage::Store};
use rustfft::{FftPlanner, num_complex::Complex64};

pub(crate) const MINIMUM_MARKER_CORRELATION: f64 = 0.35;

fn quality(message: impl Into<String>) -> crate::error::Error {
    Fault::Quality(message.into()).into()
}
fn timing(message: impl Into<String>) -> crate::error::Error {
    Fault::Timing(message.into()).into()
}

pub fn grid() -> Vec<f32> {
    (0..=480)
        .map(|i| (20.0 * 1000f64.powf(i as f64 / 480.0)) as f32)
        .collect()
}
pub fn fft(samples: &[f32], n: usize) -> Vec<Complex64> {
    let mut bins = vec![Complex64::default(); n];
    for (bin, &s) in bins.iter_mut().zip(samples) {
        bin.re = f64::from(s);
    }
    FftPlanner::new().plan_fft_forward(n).process(&mut bins);
    bins
}
pub(crate) fn inverse(mut bins: Vec<Complex64>) -> Vec<f32> {
    let n = bins.len();
    FftPlanner::new().plan_fft_inverse(n).process(&mut bins);
    bins.into_iter().map(|v| (v.re / n as f64) as f32).collect()
}
pub fn magnitude(samples: &[f32], rate: Rate, frequencies: &[f32]) -> Vec<f32> {
    let n = (samples.len() * 4).max(65536).next_power_of_two();
    let bins = fft(samples, n);
    frequencies
        .iter()
        .map(|&f| {
            let pos = f64::from(f) * n as f64 / f64::from(rate.hz());
            let k = pos.floor() as usize;
            let a = bins[k].norm() * (1.0 - pos.fract()) + bins[k + 1].norm() * pos.fract();
            (20.0 * a.max(1e-12).log10()) as f32
        })
        .collect()
}

/// Analyze background evidence; the workflow reports it before checking its limits.
pub fn noise(samples: &[f32], rate: Rate, limits: NoiseLimits, store: &Store) -> Result<Noise> {
    limits.validate()?;
    if samples.len() < rate.hz() as usize {
        return Err(quality("Background recording is shorter than one second"));
    }
    if samples.iter().any(|v| !v.is_finite()) {
        return Err(quality(super::io::CallbackFault::NonFinite.to_string()));
    }
    if samples
        .iter()
        .any(|v| v.abs() >= super::io::INPUT_CLIPPING_PEAK)
    {
        return Err(quality(super::io::CallbackFault::Clipping.to_string()));
    }
    let n = 4096;
    let fs = f64::from(rate.hz());
    let window: Vec<_> = (0..n)
        .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos())
        .collect();
    let scale = fs * window.iter().map(|v| v * v).sum::<f64>();
    let mut bins = vec![Complex64::default(); n];
    let mut power = vec![0.0; n / 2 + 1];
    let mut energies = Vec::new();
    let mut count = 0;
    let forward = FftPlanner::new().plan_fft_forward(n);
    for block in samples.windows(n).step_by(n / 2) {
        let mean = block.iter().map(|&x| f64::from(x)).sum::<f64>() / n as f64;
        energies.push(
            block
                .iter()
                .map(|&x| (f64::from(x) - mean).powi(2))
                .sum::<f64>()
                / n as f64,
        );
        for ((bin, &s), &w) in bins.iter_mut().zip(block).zip(&window) {
            *bin = Complex64::new((f64::from(s) - mean) * w, 0.0);
        }
        forward.process(&mut bins);
        for (k, p) in power.iter_mut().enumerate() {
            *p += bins[k].norm_sqr() * if k == 0 || k == n / 2 { 1.0 } else { 2.0 } / scale;
        }
        count += 1;
    }
    energies.sort_by(f64::total_cmp);
    let variation = 10.0
        * (energies[energies.len() * 95 / 100].max(1e-20)
            / energies[energies.len() / 20].max(1e-20))
        .log10();
    let rms_power =
        samples.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / samples.len() as f64;
    let peak = samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    let frequencies = grid();
    let db = frequencies
        .iter()
        .map(|&f| {
            let pos = f64::from(f) * n as f64 / fs;
            let k = pos.floor() as usize;
            (10.0
                * ((power[k] * (1.0 - pos.fract()) + power[k + 1] * pos.fract()) / count as f64)
                    .max(1e-24)
                    .log10()) as f32
        })
        .collect();
    Ok(Noise {
        raw: Waveform::save(samples, rate, store)?,
        psd_dbfs_per_hz: Curve::new(frequencies, db)?,
        rms_dbfs: 10.0 * rms_power.max(1e-24).log10(),
        window_variation_db: variation,
        crest_db: 20.0 * f64::from(peak).max(1e-12).log10() - 10.0 * rms_power.max(1e-24).log10(),
        limits,
    })
}

/// A band-limited affine resample, performed once on a derived copy.
pub fn resample(samples: &[f32], intercept: f64, ratio: f64, frames: usize) -> Vec<f32> {
    const RADIUS: isize = 48;
    const PHASES: usize = 2048;
    let cutoff = (1.0 / ratio).min(1.0) * 0.98;
    let table: Vec<Vec<f64>> = (0..=PHASES)
        .map(|p| {
            let phase = p as f64 / PHASES as f64;
            let mut weights: Vec<_> = (-RADIUS + 1..=RADIUS)
                .map(|j| {
                    let x = j as f64 - phase;
                    let sinc = if x.abs() < 1e-12 {
                        cutoff
                    } else {
                        (std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
                    };
                    let w = 0.42
                        + 0.5 * (std::f64::consts::PI * x / RADIUS as f64).cos()
                        + 0.08 * (std::f64::consts::TAU * x / RADIUS as f64).cos();
                    sinc * w
                })
                .collect();
            let sum: f64 = weights.iter().sum();
            weights.iter_mut().for_each(|w| *w /= sum);
            weights
        })
        .collect();
    (0..frames)
        .map(|i| {
            let pos = intercept + ratio * i as f64;
            let base = pos.floor() as isize;
            let p = ((pos - pos.floor()) * PHASES as f64).round() as usize;
            (-RADIUS + 1..=RADIUS)
                .zip(&table[p])
                .map(|(j, w)| {
                    let index = base + j;
                    if index >= 0 {
                        f64::from(samples.get(index as usize).copied().unwrap_or(0.0)) * w
                    } else {
                        0.0
                    }
                })
                .sum::<f64>() as f32
        })
        .collect()
}

pub fn clock(samples: &[f32], input_rate: Rate, probe: &Probe) -> Result<ClockMap> {
    let nominal = f64::from(input_rate.hz()) / f64::from(probe.recipe.rate.hz());
    let template = resample(
        &probe.marker,
        0.0,
        1.0 / nominal,
        (probe.marker.len() as f64 * nominal).round() as usize,
    );
    let n = (samples.len() + template.len()).next_power_of_two();
    let mut correlation = fft(samples, n);
    let reference = fft(&template, n);
    for (y, x) in correlation.iter_mut().zip(reference) {
        *y *= x.conj();
    }
    let correlation = inverse(correlation);
    let energy = template.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>();
    // Prefixes of a whole take lose a quiet marker window after a loud sweep.
    // Sum each window from two local positive sums, without subtracting energies.
    let width = template.len();
    let mut prefix = vec![0.0; samples.len() + 1];
    let mut suffix = vec![0.0; samples.len() + 1];
    for (i, &x) in samples.iter().enumerate() {
        prefix[i + 1] = if (i + 1) % width == 0 {
            0.0
        } else {
            prefix[i] + f64::from(x).powi(2)
        };
    }
    for i in (0..samples.len()).rev() {
        suffix[i] = f64::from(samples[i]).powi(2)
            + if (i + 1) % width == 0 {
                0.0
            } else {
                suffix[i + 1]
            };
    }
    let mut positions = [0.0; 2];
    let mut scores = [0.0; 2];
    for (which, &marker) in probe.recipe.marker_frames.iter().enumerate() {
        let expected = marker as f64 * nominal;
        let start = (expected - f64::from(input_rate.hz()) * 0.4).max(1.0) as usize;
        let end = ((expected + f64::from(input_rate.hz()) * 2.0) as usize)
            .min(samples.len().saturating_sub(template.len() + 1));
        if start >= end {
            return Err(timing("Incomplete timing-reference capture"));
        }
        let score = |i: usize| {
            f64::from(correlation[i]).abs()
                / ((suffix[i] + prefix[i + width]) * energy).max(1e-24).sqrt()
        };
        let at = (start..end)
            .max_by(|&a, &b| correlation[a].abs().total_cmp(&correlation[b].abs()))
            .expect("nonempty search");
        let best = score(at);
        if best < MINIMUM_MARKER_CORRELATION {
            return Err(timing(format!(
                "Timing marker correlation {best:.2} is below the required {MINIMUM_MARKER_CORRELATION:.2}"
            )));
        }
        let (a, b, c) = (
            f64::from(correlation[at - 1]).abs(),
            f64::from(correlation[at]).abs(),
            f64::from(correlation[at + 1]).abs(),
        );
        let offset = 0.5 * (a - c) / (a - 2.0 * b + c);
        positions[which] = at as f64 + offset.clamp(-0.5, 0.5);
        scores[which] = best;
    }
    let ratio = (positions[1] - positions[0])
        / (probe.recipe.marker_frames[1] - probe.recipe.marker_frames[0]) as f64;
    let ppm = (ratio / nominal - 1.0) * 1e6;
    if !ppm.is_finite() || ppm.abs() > 2000.0 {
        return Err(timing(
            "Input/output clock mismatch exceeds the validated +/-2000 ppm range",
        ));
    }
    let intercept = positions[0] - ratio * probe.recipe.marker_frames[0] as f64;
    let marker_projection_db = probe.recipe.marker_frames.map(|m| {
        let aligned = resample(
            samples,
            intercept + ratio * m as f64,
            ratio,
            probe.marker.len(),
        );
        let dot: f64 = aligned
            .iter()
            .zip(&probe.marker)
            .map(|(&a, &b)| f64::from(a) * f64::from(b))
            .sum();
        let energy: f64 = probe.marker.iter().map(|&x| f64::from(x).powi(2)).sum();
        20.0 * (dot.abs() / energy).max(1e-12).log10()
    });
    let diagnostic =
        super::probe::diagnostic_marker(probe.recipe.rate, probe.recipe.gain.probe.gain());
    // The speaker, room and mic filter each code. Predict the independent code
    // through the first marker's measured transfer, retaining reflections and
    // group delay instead of comparing two unrelated raw-template peak lags.
    let pre = probe.marker.len();
    let window = pre + probe.marker.len() + pre;
    let first = resample(
        samples,
        intercept + ratio * (probe.recipe.marker_frames[0] as f64 - pre as f64),
        ratio,
        window,
    );
    let mut reference = vec![0.0; window];
    reference[pre..pre + probe.marker.len()].copy_from_slice(&probe.marker);
    let mut predicted = vec![0.0; window];
    predicted[pre..pre + diagnostic.len()].copy_from_slice(&diagnostic);
    let n = (window * 2).next_power_of_two();
    let reference = fft(&reference, n);
    let first = fft(&first, n);
    let mut predicted = fft(&predicted, n);
    let floor = reference.iter().map(|v| v.norm_sqr()).fold(0.0, f64::max) * 1e-10;
    for ((predicted, observed), reference) in predicted.iter_mut().zip(first).zip(reference) {
        *predicted = *predicted * observed * reference.conj() / (reference.norm_sqr() + floor);
    }
    let predicted = inverse(predicted);
    let diagnostic = &predicted[..window];
    let aligned = resample(
        samples,
        intercept + ratio * (probe.recipe.diagnostic_marker_frame as f64 - pre as f64),
        ratio,
        window + 32,
    );
    let energy = diagnostic
        .iter()
        .map(|&x| f64::from(x).powi(2))
        .sum::<f64>();
    let observed: Vec<_> = (-8..=8)
        .map(|shift| {
            let (dot, recorded_energy) = diagnostic.iter().enumerate().fold(
                (0.0, 0.0),
                |(dot, recorded_energy), (i, &v)| {
                    let at = i as isize + shift;
                    let sample = if at >= 0 {
                        f64::from(aligned.get(at as usize).copied().unwrap_or(0.0))
                    } else {
                        0.0
                    };
                    (
                        dot + f64::from(v) * sample,
                        recorded_energy + sample * sample,
                    )
                },
            );
            (
                shift,
                dot.abs() / (energy * recorded_energy).max(1e-24).sqrt(),
            )
        })
        .collect();
    let best = observed
        .iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .expect("diagnostic search");
    if best.0.abs() > 2 || best.1 < MINIMUM_MARKER_CORRELATION {
        return Err(timing(format!(
            "Independent timing marker could not confirm the clock fit: position residual {} frames, normalized correlation {:.2}",
            best.0, best.1,
        )));
    }
    Ok(ClockMap {
        intercept,
        ratio,
        ppm,
        marker_correlation: scores,
        marker_projection_db,
        timing_residual_frames: best.0 as f64,
    })
}

/// The exact reference defines a calibrated, regularized discrete ESS inverse.
/// Zero padding retains linear tails. Each sweep, including LF, builds its own inverse.
fn deconvolve(reference: &[f32], recording: &[f32]) -> Vec<f32> {
    let n = (reference.len() + recording.len()).next_power_of_two();
    let x = fft(reference, n);
    let mut y = fft(recording, n);
    let floor = x.iter().map(|v| v.norm_sqr()).fold(0.0, f64::max) * 1e-10;
    for (y, x) in y.iter_mut().zip(x) {
        *y = *y * x.conj() / (x.norm_sqr() + floor);
    }
    inverse(y)
}
fn window_ir(full: &[f32], rate: Rate, tail: usize, acoustic_tail: usize) -> Vec<f32> {
    let pre = rate.hz() as usize / 50;
    let fade = (rate.hz() as usize / 5).min(acoustic_tail / 4);
    (0..pre + tail)
        .map(|i| {
            let index = if i < pre {
                full.len() - pre + i
            } else {
                i - pre
            };
            let w = if i < pre / 4 {
                (std::f64::consts::FRAC_PI_2 * i as f64 / (pre / 4) as f64)
                    .sin()
                    .powi(2)
            } else if i > pre + tail - fade {
                (std::f64::consts::FRAC_PI_2 * (pre + tail - i) as f64 / fade as f64)
                    .sin()
                    .powi(2)
            } else {
                1.0
            };
            (f64::from(full[index]) * w) as f32
        })
        .collect()
}

/// Propagate stationary measured PSD through the same inverse and IR window.
/// These derived noise realizations estimate uncertainty; they are never room evidence.
fn propagated_noise(
    noise: &Noise,
    probe: &Probe,
    frames: usize,
    tail: usize,
    frequencies: &[f32],
) -> Vec<f32> {
    let n = frames.next_power_of_two();
    let fs = f64::from(probe.recipe.rate.hz());
    let mut power = vec![0.0f64; frequencies.len()];
    let mut seed = 0x5eed_u64;
    for _ in 0..3 {
        let mut bins = vec![Complex64::default(); n];
        for k in 1..n / 2 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let phase = seed as f64 / u64::MAX as f64 * std::f64::consts::TAU;
            let psd = 10f64.powf(
                f64::from(noise.psd_dbfs_per_hz.at((k as f64 * fs / n as f64) as f32)) / 10.0,
            );
            bins[k] = Complex64::from_polar((psd * fs * n as f64 / 2.0).sqrt(), phase);
            bins[n - k] = bins[k].conj();
        }
        let noise_samples = inverse(bins);
        let full = deconvolve(&probe.reference, &noise_samples[..frames]);
        let ir = window_ir(
            &full,
            probe.recipe.rate,
            tail,
            tail - probe.recipe.filter_tail_frames,
        );
        let db = magnitude(&ir, probe.recipe.rate, frequencies);
        for (p, db) in power.iter_mut().zip(db) {
            *p += 10f64.powf(f64::from(db) / 10.0) / 3.0;
        }
    }
    power
        .into_iter()
        .map(|p| (10.0 * p.max(1e-24).log10()) as f32)
        .collect()
}

pub fn identify(
    samples: &[f32],
    input_rate: Rate,
    probe: &Probe,
    noise: &Noise,
    calibration: &Calibration,
    store: &Store,
) -> Result<(ClockMap, Response)> {
    if samples.iter().any(|s| !s.is_finite()) {
        return Err(quality(super::io::CallbackFault::NonFinite.to_string()));
    }
    if samples
        .iter()
        .any(|s| s.abs() >= super::io::INPUT_CLIPPING_PEAK)
    {
        return Err(quality(super::io::CallbackFault::Clipping.to_string()));
    }
    let mapping = clock(samples, input_rate, probe)?;
    let tail = (probe.recipe.tail_seconds * f64::from(probe.recipe.rate.hz())).round() as usize;
    let acoustic_tail_seconds = probe.recipe.acoustic_tail_seconds();
    let frames = probe.reference.len() + tail;
    let start = mapping.intercept + mapping.ratio * probe.recipe.sweep_start as f64;
    if start < 0.0 || start + mapping.ratio * frames as f64 >= samples.len() as f64 {
        return Err(quality(
            "Capture does not include the complete sweep and decay",
        ));
    }
    let mut aligned = resample(samples, start, mapping.ratio, frames);
    let mean = aligned.iter().map(|&v| f64::from(v)).sum::<f64>() / frames as f64;
    aligned.iter_mut().for_each(|v| *v -= mean as f32);
    let full = deconvolve(&probe.reference, &aligned);
    let causal_energy = full[..tail]
        .iter()
        .map(|&s| f64::from(s).powi(2))
        .sum::<f64>()
        .max(1e-24);
    let late = probe.recipe.rate.hz() as usize / 10;
    let late_energy_ratio = full[tail - late..tail]
        .iter()
        .map(|&s| f64::from(s).powi(2))
        .sum::<f64>()
        / causal_energy;
    let pre = probe.recipe.rate.hz() as usize / 50;
    let pre_arrival_energy_ratio = full[full.len() - tail..full.len() - pre]
        .iter()
        .map(|&s| f64::from(s).powi(2))
        .sum::<f64>()
        / causal_energy;
    let ir = window_ir(
        &full,
        probe.recipe.rate,
        tail,
        tail - probe.recipe.filter_tail_frames,
    );
    let frequencies = grid();
    let raw_db = magnitude(&ir, probe.recipe.rate, &frequencies);
    let noise_db = propagated_noise(noise, probe, frames, tail, &frequencies);
    let (cal_low, cal_high) = match calibration {
        Calibration::Response { response, .. } => (
            f64::from(response.frequency[0]),
            f64::from(*response.frequency.last().expect("validated calibration")),
        ),
        Calibration::Uncalibrated { .. } => (0.0, f64::INFINITY),
    };
    let trusted_band = Band::new(
        (probe.recipe.band.low * 1.05)
            .max(cal_low)
            .max(2.0 / acoustic_tail_seconds),
        (probe.recipe.band.high / 1.05).min(cal_high),
    )
    .map_err(|_| quality("Calibration and excitation have no supported common band"))?;
    let mut uncertainty_db = Vec::new();
    let mut db = Vec::new();
    for ((&hz, &raw), &floor) in frequencies.iter().zip(&raw_db).zip(&noise_db) {
        let snr = f64::from(raw - floor);
        let cal = calibration.correction_db(f64::from(hz));
        uncertainty_db.push((8.686 * 10f64.powf(-snr / 20.0)).min(60.0) as f32);
        db.push(raw - cal.unwrap_or(0.0) as f32);
    }
    let peak = ir
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.abs().total_cmp(&b.abs()))
        .expect("IR window")
        .0;
    let arrival_frames = peak as f64 - f64::from(probe.recipe.rate.hz()) / 50.0;
    Ok((
        mapping,
        Response {
            diagnostics: Diagnostics {
                pre_arrival_energy_ratio,
                late_energy_ratio,
                polarity: if ir[peak] >= 0.0 {
                    Polarity::Positive
                } else {
                    Polarity::Negative
                },
            },
            magnitude: Curve::new(frequencies, db)?,
            uncertainty_db,
            impulse: Waveform::save(&ir, probe.recipe.rate, store)?,
            arrival_frames,
            resolution_hz: 1.0 / acoustic_tail_seconds,
            trusted_band,
        },
    ))
}

pub fn combine(full: &Response, lf: &Response) -> Response {
    let mut result = full.clone();
    for i in 0..result.magnitude.db.len() {
        if lf
            .trusted_band
            .contains(f64::from(result.magnitude.frequency[i]))
            && (!full
                .trusted_band
                .contains(f64::from(result.magnitude.frequency[i]))
                || lf.uncertainty_db[i] < full.uncertainty_db[i])
        {
            result.magnitude.db[i] = lf.magnitude.db[i];
            result.uncertainty_db[i] = lf.uncertainty_db[i];
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement::{
        self,
        io::Provider,
        probe::Probe,
        simulation::{Plant, Scenario, Simulated},
    };
    #[test]
    fn long_stereo_fir_keeps_its_ringout_and_the_full_acoustic_tail() {
        let rate = Rate::Hz48000;
        let directory = tempfile::tempdir().unwrap();
        let store = Store::at(directory.path()).unwrap();
        let settings = measurement::Acquisition {
            sweep_seconds: 1.0,
            tail_seconds: 0.5,
            ..Default::default()
        };
        let gain = measurement::GainContract::default();
        let mut candidate = crate::correction::Correction::identity(rate);
        candidate.channels = [vec![0.0; 65537], vec![0.0; 80001]];
        for channel in &mut candidate.channels {
            channel[0] = 0.6;
            *channel.last_mut().unwrap() = 0.2;
        }
        let mut room = vec![0.0; 12001];
        room[0] = 0.8;
        room[12000] = 0.15;
        let mut provider = Simulated::new(Scenario {
            plant: Plant::Responses {
                rate,
                positions: vec![[room.clone(), room]],
            },
            input_gain: 1.0,
            noise_peak: 0.0000001,
            ..Default::default()
        })
        .unwrap();
        let cancel = crate::audio::Cancellation::new();
        let ambient = provider
            .record(&vec![[0.0; 2]; rate.hz() as usize], 0, &cancel)
            .unwrap();
        let ambient = noise(&ambient.samples, rate, settings.noise_limits, &store).unwrap();
        for speaker in [measurement::Speaker::Left, measurement::Speaker::Right] {
            let probe = Probe::new(rate, &settings, &gain, speaker, Some(&candidate)).unwrap();
            assert_eq!(probe.recipe.filter_tail_frames, 80000);
            assert!((probe.recipe.acoustic_tail_seconds() - settings.tail_seconds).abs() < 1e-12);
            let recording = provider.record(&probe.output, 0, &cancel).unwrap();
            recording.validate(probe.output.len()).unwrap();
            let (_, response) = identify(
                &recording.samples,
                rate,
                &probe,
                &ambient,
                &Calibration::Uncalibrated { acknowledged: true },
                &store,
            )
            .unwrap();
            assert!((response.resolution_hz - 2.0).abs() < 1e-12);
            let pre = rate.hz() as usize / 50;
            let tail = (probe.recipe.tail_seconds * f64::from(rate.hz())).round() as usize;
            let mut expected = vec![0.0; pre + tail];
            for (i, &coefficient) in candidate.channels[speaker.index()].iter().enumerate() {
                expected[pre + i] += coefficient * 0.8;
                expected[pre + i + 12000] += coefficient * 0.15;
            }
            let expected_db = magnitude(&expected, rate, &response.magnitude.frequency);
            let rms_error = (response
                .magnitude
                .db
                .iter()
                .zip(&expected_db)
                .zip(&response.magnitude.frequency)
                .filter(|(_, hz)| response.trusted_band.contains(f64::from(**hz)))
                .map(|((&actual, &expected), _)| f64::from(actual - expected).powi(2))
                .sum::<f64>()
                / response
                    .magnitude
                    .frequency
                    .iter()
                    .filter(|&&hz| response.trusted_band.contains(f64::from(hz)))
                    .count() as f64)
                .sqrt();
            assert!(rms_error < 0.1, "{speaker:?}: {rms_error} dB");
        }
    }
    #[test]
    fn acoustic_filtering_does_not_shift_the_independent_clock_check() {
        let probe = Probe::new(
            Rate::Hz48000,
            &measurement::Acquisition {
                sweep_seconds: 1.0,
                tail_seconds: 1.0,
                ..Default::default()
            },
            &measurement::GainContract::default(),
            measurement::Speaker::Left,
            None,
        )
        .unwrap();
        for delay in 1..=32 {
            let samples: Vec<_> = probe
                .output
                .iter()
                .enumerate()
                .map(|(i, frame)| {
                    0.5 * (frame[0] - i.checked_sub(delay).map_or(0.0, |j| probe.output[j][0]))
                })
                .collect();
            let mapping = clock(&samples, Rate::Hz48000, &probe).unwrap();
            assert!(mapping.ppm.abs() < 0.01, "delay {delay}: {}", mapping.ppm);
        }
    }
    #[test]
    fn reflected_reverberant_response_retains_capture_and_supported_bands() {
        let rate = Rate::Hz48000;
        let directory = tempfile::tempdir().unwrap();
        let store = Store::at(directory.path()).unwrap();
        let settings = measurement::Acquisition {
            sweep_seconds: 1.0,
            tail_seconds: 1.0,
            ..Default::default()
        };
        let probe = Probe::new(
            rate,
            &settings,
            &measurement::GainContract::default(),
            measurement::Speaker::Left,
            None,
        )
        .unwrap();
        let mut room = vec![0.0; rate.hz() as usize];
        for (i, sample) in room.iter_mut().enumerate() {
            let seconds = i as f64 / f64::from(rate.hz());
            *sample = (0.005
                * (-seconds / 0.8).exp()
                * (std::f64::consts::TAU * 80.0 * seconds).sin()) as f32;
        }
        room[0] += 1.0;
        room[288] += 0.92;
        let mut provider = Simulated::new(Scenario {
            plant: Plant::Responses {
                rate,
                positions: vec![[room.clone(), room]],
            },
            noise_peak: 0.0000001,
            ..Default::default()
        })
        .unwrap();
        let cancel = crate::audio::Cancellation::new();
        let background = provider
            .record(&vec![[0.0; 2]; rate.hz() as usize], 0, &cancel)
            .unwrap();
        let background = noise(&background.samples, rate, settings.noise_limits, &store).unwrap();
        let mut recording = provider.record(&probe.output, 0, &cancel).unwrap();
        for sample in &mut recording.samples[..rate.hz() as usize / 5] {
            *sample += 0.001;
        }
        let original = recording.samples.clone();
        let (mapping, response) = identify(
            &recording.samples,
            rate,
            &probe,
            &background,
            &Calibration::Uncalibrated { acknowledged: true },
            &store,
        )
        .unwrap();
        assert_eq!(recording.samples, original);
        assert!((mapping.ppm - 100.0).abs() < 2.0);
        assert!(
            response
                .magnitude
                .frequency
                .iter()
                .filter(|&&hz| response.trusted_band.contains(f64::from(hz)))
                .count()
                > 100
        );
        assert!(response.diagnostics.late_energy_ratio > 0.002);
    }
    #[test]
    fn marker_amplitude_is_diagnostic_and_does_not_decide_clock_validity() {
        let acquisition = measurement::Acquisition {
            sweep_seconds: 1.0,
            tail_seconds: 1.0,
            ..Default::default()
        };
        let probe = Probe::new(
            Rate::Hz48000,
            &acquisition,
            &measurement::GainContract::default(),
            measurement::Speaker::Left,
            None,
        )
        .unwrap();
        let mut samples: Vec<_> = probe.output.iter().map(|frame| frame[0]).collect();
        let end = probe.recipe.marker_frames[1];
        for sample in &mut samples[end..end + probe.marker.len()] {
            *sample *= 10.0_f32.powf(1.24 / 20.0);
        }
        let diagnostic = probe.recipe.diagnostic_marker_frame;
        for sample in &mut samples[diagnostic..diagnostic + probe.marker.len()] {
            *sample *= 0.1;
        }
        let mapping = clock(&samples, Rate::Hz48000, &probe).unwrap();
        assert!(mapping.ppm.abs() < 0.01);
        assert!(mapping.timing_residual_frames.abs() <= 2.0);
        assert!(
            (mapping.marker_projection_db[1] - mapping.marker_projection_db[0] - 1.24).abs() < 0.01
        );
        let mut legacy = serde_json::to_value(&mapping).unwrap();
        let document = legacy.as_object_mut().unwrap();
        let projections = document.remove("marker_projection_db").unwrap();
        document.insert("marker_gain_db".into(), projections);
        let reopened: ClockMap = serde_json::from_value(legacy).unwrap();
        assert!((reopened.marker_projection_db[1] - 1.24).abs() < 0.01);
    }
    #[test]
    fn one_hundred_ppm_smears_an_uncompensated_ess_but_affine_mapping_recovers_it() {
        let scenario = Scenario {
            plant: Plant::Identity,
            input_gain: 1.0,
            noise_peak: 0.0000001,
            ..Default::default()
        };
        let mut provider = Simulated::new(scenario).unwrap();
        let cancel = crate::audio::Cancellation::new();
        let settings = measurement::Acquisition::default();
        let probe = Probe::new(
            Rate::Hz48000,
            &settings,
            &measurement::GainContract::default(),
            measurement::Speaker::Left,
            None,
        )
        .unwrap();
        let raw = provider.record(&probe.output, 0, &cancel).unwrap();
        let mapping = clock(&raw.samples, raw.rate, &probe).unwrap();
        assert!((mapping.ppm - 100.0).abs() < 2.0);
        let frames = probe.reference.len() + 96000;
        let recovered = resample(
            &raw.samples,
            mapping.intercept + mapping.ratio * probe.recipe.sweep_start as f64,
            mapping.ratio,
            frames,
        );
        let uncompensated = resample(
            &raw.samples,
            mapping.intercept + probe.recipe.sweep_start as f64,
            1.0,
            frames,
        );
        let recovered = deconvolve(&probe.reference, &recovered);
        let uncompensated = deconvolve(&probe.reference, &uncompensated);
        let good = recovered.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        let bad = uncompensated.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        assert!(good > 0.8, "compensated impulse peak {good}");
        assert!(
            bad < good * 0.6,
            "uncompensated impulse should smear: {bad}/{good}"
        );
    }
}
