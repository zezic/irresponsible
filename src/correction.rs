use crate::{
    domain::{Rate, Target, gain_db},
    error::{Result, invalid},
    storage::{ContentHash, Store},
};
use rustfft::{FftPlanner, num_complex::Complex32};
use serde::{Deserialize, Serialize};
use std::{io::Cursor, path::Path};

mod autoeq;
mod generation;
pub use autoeq::{correction_curve, minimum_phase};
pub use generation::{
    Alignment, BassExtension, ConchaInterference, FirLength, GenerationParameter,
    GenerationSettings, Normalization,
};

pub const MAX_IR_FRAMES: usize = 262_144;
pub const MAX_WAV_BYTES: u64 = 16 * 1024 * 1024;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrebleExtension {
    Limited,
    #[default]
    AutoEq,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Curve {
    pub frequency: Vec<f32>,
    pub db: Vec<f32>,
}
impl Curve {
    pub fn new(frequency: Vec<f32>, db: Vec<f32>) -> Result<Self> {
        if frequency.len() != db.len()
            || frequency.len() < 2
            || frequency.len() > 100_000
            || frequency.iter().any(|f| !f.is_finite() || *f <= 0.0)
            || frequency.windows(2).any(|w| w[0] >= w[1])
            || db.iter().any(|d| !d.is_finite())
        {
            return Err(invalid(
                "Frequency curve requires finite, strictly increasing positive frequencies and finite dB values",
            ));
        }
        Ok(Self { frequency, db })
    }
    pub fn at(&self, hz: f32) -> f32 {
        let upper = self.frequency.partition_point(|&f| f < hz);
        if upper == 0 {
            return self.db[0];
        }
        if upper == self.frequency.len() {
            return self.db[upper - 1];
        }
        let lower = upper - 1;
        let t = (hz / self.frequency[lower]).ln()
            / (self.frequency[upper] / self.frequency[lower]).ln();
        self.db[lower] + t * (self.db[upper] - self.db[lower])
    }
    pub fn center(&mut self) -> f32 {
        self.align(Alignment::MidrangeMean)
    }
    pub fn align(&mut self, alignment: Alignment) -> f32 {
        let (low, high) = alignment.bounds();
        let offset = (0..64)
            .map(|i| self.at(low * (high / low).powf(i as f32 / 63.0)))
            .sum::<f32>()
            / 64.0;
        self.db.iter_mut().for_each(|d| *d -= offset);
        offset
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum SampleConversion {
    Native,
    Sinc { delay_frames: usize },
}
impl SampleConversion {
    pub fn delay(self) -> usize {
        match self {
            Self::Native => 0,
            Self::Sinc { delay_frames } => delay_frames,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WavSource {
    pub filename: String,
    pub original_rate: u32,
    pub source: ContentHash,
    pub leading_delay: [usize; 2],
    pub conversion: SampleConversion,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Origin {
    Imported {
        #[serde(flatten)]
        impulse: WavSource,
    },
    PublishedAutoEq {
        revision: String,
        path: String,
        provider: String,
        #[serde(flatten)]
        impulse: WavSource,
    },
    AutoEq {
        revision: String,
        path: String,
        reference: String,
        target: String,
        measurement_hash: String,
        target_hash: String,
        policy: String,
        #[serde(flatten)]
        generation: GenerationSettings,
    },
    Identity,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Correction {
    pub name: String,
    pub rate: Rate,
    pub channels: [Vec<f32>; 2],
    pub target: Target,
    pub origin: Origin,
    pub measured: Option<Curve>,
    pub target_curve: Option<Curve>,
}

impl Correction {
    pub fn identity(rate: Rate) -> Self {
        Self {
            name: "No correction".into(),
            rate,
            channels: [vec![1.0], vec![1.0]],
            target: Target::Custom,
            origin: Origin::Identity,
            measured: None,
            target_curve: None,
        }
    }
    pub fn validate(&self) -> Result<()> {
        if let Some(impulse) = self.wav_source()
            && (!(8000..=384000).contains(&impulse.original_rate)
                || impulse.leading_delay.iter().any(|&n| n >= MAX_IR_FRAMES)
                || impulse.conversion.delay() > MAX_IR_FRAMES)
        {
            return Err(invalid(
                "WAV source has invalid sample-rate or delay metadata",
            ));
        }
        for channel in &self.channels {
            if channel.is_empty()
                || channel.len() > MAX_IR_FRAMES
                || channel.iter().any(|v| !v.is_finite())
            {
                return Err(invalid(
                    "IR must contain 1-262144 finite coefficients per channel",
                ));
            }
            if channel.iter().map(|&v| f64::from(v).abs()).sum::<f64>() > 1_000_000.0 {
                return Err(invalid(
                    "IR absolute coefficient sum exceeds the +120 dB numerical gain limit",
                ));
            }
        }
        for curve in [&self.measured, &self.target_curve].into_iter().flatten() {
            Curve::new(curve.frequency.clone(), curve.db.clone())?;
        }
        Ok(())
    }
    pub fn read_wav(path: &Path, rate: Rate, store: &Store) -> Result<Self> {
        if std::fs::metadata(path)?.len() > MAX_WAV_BYTES {
            return Err(invalid("WAV limit is 16 MiB"));
        }
        let bytes = std::fs::read(path)?;
        let ir = Self::decode_wav(
            &bytes,
            &path.file_name().unwrap_or_default().to_string_lossy(),
            rate,
        )?;
        store.put(&bytes)?;
        Ok(ir)
    }
    pub fn decode_wav(bytes: &[u8], filename: &str, rate: Rate) -> Result<Self> {
        if bytes.len() as u64 > MAX_WAV_BYTES {
            return Err(invalid("WAV limit is 16 MiB"));
        }
        let mut reader = hound::WavReader::new(Cursor::new(bytes))?;
        let spec = reader.spec();
        if !matches!(spec.channels, 1 | 2)
            || !(8000..=384000).contains(&spec.sample_rate)
            || reader.duration() as usize > MAX_IR_FRAMES
        {
            return Err(invalid(
                "IR must be mono/stereo, 8-384 kHz, at most 262144 frames",
            ));
        }
        let samples: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Float => reader
                .samples::<f32>()
                .collect::<std::result::Result<_, _>>()?,
            hound::SampleFormat::Int => {
                let scale = 2.0_f32.powi(i32::from(spec.bits_per_sample) - 1);
                reader
                    .samples::<i32>()
                    .map(|s| s.map(|s| s as f32 / scale))
                    .collect::<std::result::Result<_, _>>()?
            }
        };
        if samples.is_empty()
            || !samples.len().is_multiple_of(spec.channels as usize)
            || samples.iter().any(|v| !v.is_finite())
        {
            return Err(invalid(
                "IR contains empty, incomplete, or non-finite sample data",
            ));
        }
        let channels = if spec.channels == 1 {
            [samples.clone(), samples]
        } else {
            [
                samples.iter().step_by(2).copied().collect(),
                samples.iter().skip(1).step_by(2).copied().collect(),
            ]
        };
        let leading_delay = channels
            .each_ref()
            .map(|ch| ch.iter().position(|&v| v != 0.0).unwrap_or(0));
        let (channels, conversion) = if spec.sample_rate != rate.hz() {
            let prepared = resample_ir(&channels, spec.sample_rate, rate.hz())?;
            (
                prepared.coefficients,
                SampleConversion::Sinc {
                    delay_frames: prepared.delay,
                },
            )
        } else {
            (channels, SampleConversion::Native)
        };
        let ir = Self {
            name: filename.into(),
            rate,
            channels,
            target: Target::Custom,
            origin: Origin::Imported {
                impulse: WavSource {
                    filename: filename.into(),
                    original_rate: spec.sample_rate,
                    source: ContentHash::of(bytes),
                    leading_delay,
                    conversion,
                },
            },
            measured: None,
            target_curve: None,
        };
        ir.validate()?;
        Ok(ir)
    }
    pub fn at_rate(&self, rate: Rate, store: &Store) -> Result<Self> {
        if rate == self.rate {
            return Ok(self.clone());
        }
        match &self.origin {
            Origin::Imported { impulse } | Origin::PublishedAutoEq { impulse, .. } => {
                let mut prepared =
                    Self::decode_wav(&store.get(&impulse.source)?, &impulse.filename, rate)?;
                if let Origin::PublishedAutoEq {
                    revision,
                    path,
                    provider,
                    ..
                } = &self.origin
                {
                    prepared = prepared.into_published(
                        self.name.clone(),
                        revision.clone(),
                        path.clone(),
                        provider.clone(),
                    );
                }
                Ok(prepared)
            }
            Origin::Identity => Ok(Self::identity(rate)),
            Origin::AutoEq { generation, .. } => {
                if self.requires_regeneration() {
                    return Err(invalid(
                        "Regenerate this correction with the current generation policy before changing rate",
                    ));
                }
                let measured = self
                    .measured
                    .as_ref()
                    .ok_or_else(|| invalid("Generated correction is missing its measurement"))?;
                let target = self
                    .target_curve
                    .as_ref()
                    .ok_or_else(|| invalid("Generated correction is missing its target"))?;
                let coefficients = generated_impulse(measured, target, rate, *generation);
                let mut ir = self.clone();
                ir.channels = [coefficients.clone(), coefficients];
                ir.rate = rate;
                ir.validate()?;
                Ok(ir)
            }
        }
    }
    pub fn requires_regeneration(&self) -> bool {
        matches!(&self.origin, Origin::AutoEq { policy, generation, .. } if *policy != generation.policy())
    }
    pub fn regenerate(&self, rate: Rate, generation: GenerationSettings) -> Result<Self> {
        let Origin::AutoEq { .. } = &self.origin else {
            return Err(invalid(
                "Published AutoEq and imported WAVs retain their coefficients. Choose an in-app target to edit its IR settings.",
            ));
        };
        let mut ir = self.clone();
        let measured = ir
            .measured
            .as_mut()
            .ok_or_else(|| invalid("Generated correction is missing its measurement"))?;
        let target = ir
            .target_curve
            .as_mut()
            .ok_or_else(|| invalid("Generated correction is missing its target"))?;
        measured.align(generation.alignment());
        target.align(generation.alignment());
        let coefficients = generated_impulse(measured, target, rate, generation);
        ir.channels = [coefficients.clone(), coefficients];
        ir.rate = rate;
        let Origin::AutoEq {
            path,
            reference,
            policy,
            generation: recipe,
            ..
        } = &mut ir.origin
        else {
            unreachable!("generated source was checked")
        };
        *reference = measurement_reference(path, generation.alignment())?;
        *policy = generation.policy();
        *recipe = generation;
        ir.validate()?;
        Ok(ir)
    }
    fn wav_source(&self) -> Option<&WavSource> {
        match &self.origin {
            Origin::Imported { impulse } | Origin::PublishedAutoEq { impulse, .. } => Some(impulse),
            Origin::Identity | Origin::AutoEq { .. } => None,
        }
    }
    pub fn into_published(
        mut self,
        name: String,
        revision: String,
        path: String,
        provider: String,
    ) -> Self {
        let Origin::Imported { impulse } = self.origin else {
            panic!("published WAV metadata is attached immediately after decoding")
        };
        self.origin = Origin::PublishedAutoEq {
            revision,
            path,
            provider,
            impulse,
        };
        self.name = name;
        self.target = Target::AutoEq;
        self
    }
    pub fn leading_delays(&self) -> [usize; 2] {
        self.wav_source().map_or([0, 0], |impulse| {
            impulse.leading_delay.map(|n| {
                (n as f64 * self.rate.hz() as f64 / f64::from(impulse.original_rate)).round()
                    as usize
                    + impulse.conversion.delay()
            })
        })
    }
    pub fn preparation_delay(&self) -> usize {
        self.wav_source()
            .map_or(0, |impulse| impulse.conversion.delay())
    }
    pub fn response(&self, points: usize) -> [Curve; 2] {
        let max_hz = 20000.0_f32.min(self.rate.hz() as f32 * 0.49);
        let frequencies: Vec<_> = (0..points)
            .map(|i| 20.0 * (max_hz / 20.0).powf(i as f32 / (points - 1) as f32))
            .collect();
        self.channels.each_ref().map(|ch| {
            let n = (ch.len() * 4).max(32768).next_power_of_two();
            let spectrum = spectrum(ch, n);
            let db = frequencies
                .iter()
                .map(|&f| {
                    let bin = f * n as f32 / self.rate.hz() as f32;
                    let k = bin.floor() as usize;
                    let magnitude = spectrum[k].norm() * (1.0 - bin.fract())
                        + spectrum[k + 1].norm() * bin.fract();
                    gain_db(magnitude)
                })
                .collect();
            Curve {
                frequency: frequencies.clone(),
                db,
            }
        })
    }
}

pub(crate) fn measurement_reference(path: &str, alignment: Alignment) -> Result<String> {
    let (source, _) = path
        .rsplit_once('/')
        .filter(|(source, name)| !source.is_empty() && !name.is_empty())
        .ok_or_else(|| invalid("Generated correction has an invalid measurement path"))?;
    let source = percent_encoding::percent_decode_str(source)
        .decode_utf8()
        .map_err(|error| invalid(error.to_string()))?;
    Ok(format!(
        "{} / raw reference; {} aligned",
        source.replace('/', " / "),
        alignment.label()
    ))
}

pub fn spectrum(coefficients: &[f32], n: usize) -> Vec<Complex32> {
    assert!(n >= coefficients.len());
    let mut result = vec![Complex32::default(); n];
    for (out, &value) in result.iter_mut().zip(coefficients) {
        out.re = value;
    }
    FftPlanner::new().plan_fft_forward(n).process(&mut result);
    result
}

struct ResampledImpulse {
    coefficients: [Vec<f32>; 2],
    delay: usize,
}

fn resample_ir(
    channels: &[Vec<f32>; 2],
    input_rate: u32,
    output_rate: u32,
) -> Result<ResampledImpulse> {
    use rubato::{
        Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
    };
    let ratio = output_rate as f64 / input_rate as f64;
    let mut sampler = SincFixedIn::<f32>::new(
        ratio,
        1.0,
        SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.94,
            interpolation: SincInterpolationType::Cubic,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        },
        512,
        2,
    )
    .map_err(|e| invalid(e.to_string()))?;
    const PADDING: usize = 256;
    // Rubato's output sequence starts at the first input sample. Pad both
    // ends explicitly so the sinc kernel can retain the first/last tap's
    // energy; report the resulting delay instead of truncating that energy.
    let delay = (PADDING as f64 * ratio - 1.0).round().max(0.0) as usize;
    let lengths = channels
        .each_ref()
        .map(|ch| ((ch.len() + PADDING * 2) as f64 * ratio).ceil() as usize);
    if lengths.iter().any(|&n| n > MAX_IR_FRAMES) {
        return Err(invalid(
            "Resampled IR including its kernel tail exceeds 262144 frames",
        ));
    }
    let mut output: [Vec<f32>; 2] = std::array::from_fn(|_| Vec::new());
    let total = channels.iter().map(Vec::len).max().expect("stereo");
    let mut offset = 0_usize;
    while output[0].len() < lengths[0].max(lengths[1]) {
        let input: [Vec<f32>; 2] = std::array::from_fn(|c| {
            (offset..offset + 512)
                .map(|i| {
                    i.checked_sub(PADDING)
                        .and_then(|i| channels[c].get(i))
                        .copied()
                        .unwrap_or(0.0)
                })
                .collect()
        });
        let block = sampler
            .process(&input, None)
            .map_err(|e| invalid(e.to_string()))?;
        for c in 0..2 {
            output[c].extend(block[c].iter().map(|v| *v / ratio as f32));
        }
        offset += 512;
        assert!(offset < total + 32768, "resampler must make progress");
    }
    Ok(ResampledImpulse {
        coefficients: std::array::from_fn(|c| output[c][..lengths[c]].to_vec()),
        delay,
    })
}

pub fn generated_impulse(
    measured: &Curve,
    target: &Curve,
    rate: Rate,
    settings: GenerationSettings,
) -> Vec<f32> {
    minimum_phase(
        &correction_curve(measured, target, settings),
        rate,
        settings.length(),
        settings.normalization(),
    )
}
