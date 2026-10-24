//! Speaker-free endpoints feed emitted frames through a plant and the real callbacks.
use super::{
    CaptureTiming, SourcePlan,
    analysis::{fft, inverse, resample},
    io::{
        CallbackClock, CallbackFault, CaptureCallback, Direction, FaultLatch, PlaybackCallback,
        Provider, Recording,
    },
};
use crate::{
    audio::DeviceInfo,
    domain::*,
    dsp::Frame,
    error::{Result, invalid},
};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pace {
    Deterministic,
    RealTime,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Fault {
    None,
    DeviceLostOnTake(usize),
    DropOnTake(usize),
    ClipOnTake(usize),
    TransientOnTake(usize),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Scenario {
    pub rate: Rate,
    pub input_rate: Rate,
    pub drift_ppm: f64,
    pub latency_seconds: f64,
    pub input_start_seconds: f64,
    pub noise_peak: f32,
    pub input_gain: f32,
    pub nonlinearity: f32,
    pub scheduling_jitter_seconds: f64,
    pub timestamp_jitter_seconds: f64,
    pub timestamp_quantum_seconds: f64,
    pub seed: u64,
    pub pace: Pace,
    pub fault: Fault,
    pub plant: Plant,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Plant {
    Identity,
    Room,
    Responses {
        rate: Rate,
        positions: Vec<[Vec<f32>; 2]>,
    },
}

/// Host timestamp estimates vary independently of the ADC/DAC sample clocks.
/// Quantized callbacks may share a timestamp; neither changes the mic samples.
struct CallbackTimestamps {
    random: u64,
    jitter_ns: u64,
    quantum_ns: u64,
    previous: u64,
}
impl CallbackTimestamps {
    fn new(scenario: &Scenario, direction: Direction) -> Self {
        Self {
            random: scenario.seed
                ^ match direction {
                    Direction::Input => 0xa0761d6478bd642f,
                    Direction::Output => 0xe7037ed1a0b428db,
                },
            jitter_ns: (scenario.timestamp_jitter_seconds * 1e9).round() as u64,
            quantum_ns: ((scenario.timestamp_quantum_seconds * 1e9).round() as u64).max(1),
            previous: 0,
        }
    }
    fn stamp(&mut self, seconds: f64) -> u64 {
        self.random ^= self.random << 13;
        self.random ^= self.random >> 7;
        self.random ^= self.random << 17;
        let jitter = self.random % (self.jitter_ns + 1);
        let estimated = (seconds * 1e9).round() as u64 + jitter;
        let quantized = estimated / self.quantum_ns * self.quantum_ns;
        self.previous = self.previous.max(quantized);
        self.previous
    }
}
impl Default for Scenario {
    fn default() -> Self {
        Self {
            rate: Rate::Hz48000,
            input_rate: Rate::Hz48000,
            drift_ppm: 100.0,
            latency_seconds: 0.0173,
            input_start_seconds: 0.0,
            noise_peak: 0.000001,
            input_gain: 0.1,
            nonlinearity: 0.0,
            scheduling_jitter_seconds: 0.0005,
            timestamp_jitter_seconds: 0.0001,
            timestamp_quantum_seconds: 0.000001,
            seed: 42,
            pace: Pace::Deterministic,
            fault: Fault::None,
            plant: Plant::Room,
        }
    }
}
impl Scenario {
    pub fn validate(&self) -> Result<()> {
        if !self.drift_ppm.is_finite()
            || self.drift_ppm.abs() > 2000.0
            || !(0.0..=0.5).contains(&self.latency_seconds)
            || !(0.0..=0.1).contains(&self.noise_peak)
            || !(0.001..=1.0).contains(&self.input_gain)
            || !(0.0..=10.0).contains(&self.nonlinearity)
            || !(0.0..=0.02).contains(&self.scheduling_jitter_seconds)
            || !(0.0..=0.02).contains(&self.timestamp_jitter_seconds)
            || !(0.0..=0.001).contains(&self.timestamp_quantum_seconds)
            || !self.input_start_seconds.is_finite()
            || self.input_start_seconds.abs() > 0.1
            || self.seed == 0
        {
            return Err(invalid("Invalid simulated audio scenario"));
        }
        if matches!(
            self.fault,
            Fault::DeviceLostOnTake(0)
                | Fault::DropOnTake(0)
                | Fault::ClipOnTake(0)
                | Fault::TransientOnTake(0)
        ) {
            return Err(invalid("Scenario fault take numbers start at one"));
        }
        if let Plant::Responses { rate, positions } = &self.plant {
            if *rate != self.rate || positions.is_empty() || positions.len() > 64 {
                return Err(invalid(
                    "Custom room needs 1-64 stereo impulse pairs at the playback rate",
                ));
            }
            let mut frames = 0;
            for h in positions.iter().flatten() {
                if h.is_empty()
                    || h.len() > self.rate.hz() as usize * 3
                    || h.iter().any(|v| !v.is_finite())
                    || h.iter().map(|v| f64::from(v.abs())).sum::<f64>() > 1000.0
                {
                    return Err(invalid("Invalid or excessive simulated room impulse"));
                }
                frames += h.len();
            }
            if frames > 2_000_000 {
                return Err(invalid(
                    "Simulated room fixture exceeds two million coefficients",
                ));
            }
        }
        Ok(())
    }
    pub fn load(path: &std::path::Path) -> Result<Self> {
        if std::fs::metadata(path)?.len() > 16 * 1024 * 1024 {
            return Err(invalid("Scenario configuration exceeds 16 MiB"));
        }
        let scenario: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        scenario.validate()?;
        Ok(scenario)
    }
}

pub fn devices(scenario: &Scenario) -> Vec<DeviceInfo> {
    let rate = scenario.rate;
    [
        (EmulatedDevice::Capture, "Emulated music capture", 2, 0),
        (EmulatedDevice::Speakers, "Emulated stereo room", 0, 2),
        (EmulatedDevice::Microphone, "Emulated mono microphone", 1, 0),
    ]
    .into_iter()
    .map(|(id, name, input, output)| DeviceInfo {
        uid: DeviceUid::Emulated { emulated: id },
        name: name.into(),
        input_channels: input,
        output_channels: output,
        virtual_device: true,
        rate: Some(if id == EmulatedDevice::Microphone {
            scenario.input_rate
        } else {
            rate
        }),
        input_rate: (input != 0).then_some(if id == EmulatedDevice::Microphone {
            scenario.input_rate
        } else {
            rate
        }),
        output_rate: (output != 0).then_some(rate),
    })
    .collect()
}
pub fn route() -> Route {
    Route {
        capture: Endpoint {
            uid: DeviceUid::Emulated {
                emulated: EmulatedDevice::Capture,
            },
            label: "Emulated music capture".into(),
            channels: StereoPair::default(),
        },
        playback: Endpoint {
            uid: DeviceUid::Emulated {
                emulated: EmulatedDevice::Speakers,
            },
            label: "Emulated stereo room".into(),
            channels: StereoPair::default(),
        },
    }
}
pub fn microphone() -> Microphone {
    Endpoint {
        uid: DeviceUid::Emulated {
            emulated: EmulatedDevice::Microphone,
        },
        label: "Emulated mono microphone".into(),
        channels: MonoChannel(0),
    }
}
pub fn plan(rate: Rate) -> SourcePlan {
    SourcePlan {
        route: route(),
        microphone: microphone(),
        rate,
        layout: super::Layout::default(),
        calibration: super::Calibration::Uncalibrated { acknowledged: true },
        gain: super::GainContract::default(),
        acquisition: super::Acquisition::default(),
    }
}

/// Ground truth is private to the endpoint. The detector receives only ADC samples.
pub fn room_impulse(
    rate: Rate,
    speaker: usize,
    position: usize,
    plant: &Plant,
) -> Result<Vec<f32>> {
    if let Plant::Responses {
        rate: expected,
        positions,
    } = plant
    {
        if *expected != rate {
            return Err(invalid("Scenario impulse rate differs from playback"));
        }
        return positions
            .get(position)
            .map(|p| p[speaker].clone())
            .ok_or_else(|| invalid("Scenario has no room impulse for this microphone position"));
    }
    if matches!(plant, Plant::Identity) {
        return Ok(vec![1.0]);
    }
    let fs = f64::from(rate.hz());
    let n = (fs * 0.4) as usize;
    let mut h = vec![0.0; n];
    let delay = if speaker == 0 {
        0
    } else {
        (0.0007 * fs) as usize
    };
    h[delay] = if speaker == 0 { 0.9 } else { 0.8 };
    let reflection = (fs * (0.004 + position as f64 * 0.0003)) as usize;
    h[delay + reflection] -= 0.5;
    for (frequency, strength, decay) in [
        (45.0, 0.012, 0.12),
        (70.0 + speaker as f64 * 3.0, 0.018, 0.08),
        (120.0, 0.009, 0.06),
    ] {
        for (i, sample) in h.iter_mut().enumerate().skip(delay) {
            let t = (i - delay) as f64 / fs;
            *sample += (strength
                * (1.0 + position as f64 * 0.025)
                * (-t / decay).exp()
                * (std::f64::consts::TAU * frequency * t).sin()) as f32;
        }
    }
    for k in 1..20 {
        let at = (fs * (0.011 * k as f64 + 0.00013 * (k * k) as f64)) as usize + delay;
        if at < n {
            h[at] += (0.045 * (-k as f64 / 5.0).exp()) as f32;
        }
    }
    Ok(h)
}

pub struct Simulated {
    scenario: Scenario,
    started: Instant,
    virtual_seconds: f64,
    take: usize,
    random: u64,
    deadline: Option<Duration>,
    meters: Arc<crate::audio::Meters>,
}
impl Simulated {
    pub fn new(scenario: Scenario) -> Result<Self> {
        scenario.validate()?;
        Ok(Self {
            random: scenario.seed,
            scenario,
            started: Instant::now(),
            virtual_seconds: 0.0,
            take: 0,
            deadline: None,
            meters: Arc::new(crate::audio::Meters::default()),
        })
    }
    fn random(&mut self) -> f32 {
        self.random ^= self.random << 13;
        self.random ^= self.random >> 7;
        self.random ^= self.random << 17;
        (self.random as f64 / u64::MAX as f64 * 2.0 - 1.0) as f32
    }
}
impl Simulated {
    fn replay(
        &mut self,
        output: &[Frame],
        mic: &[f32],
        cancel: &crate::audio::Cancellation,
    ) -> Result<Recording> {
        let fault = Arc::new(FaultLatch::default());
        let played = Arc::new(AtomicU64::new(0));
        let first = Arc::new(AtomicU64::new(0));
        let last = Arc::new(AtomicU64::new(0));
        let (mut tx, rx) = rtrb::RingBuffer::new(8192);
        let mut playback = PlaybackCallback {
            clock: CallbackClock::new(self.scenario.rate),
            ring: rx,
            fault: fault.clone(),
            played: played.clone(),
            total: output.len(),
            pair: StereoPair::default(),
            channels: 2,
            cancel: cancel.clone(),
            first_timestamp: first.clone(),
            last_timestamp: last.clone(),
            deadline: self.deadline.map(|d| self.started + d),
            meters: self.meters.clone(),
        };
        let (capture_tx, mut capture_rx) = rtrb::RingBuffer::new(8192);
        let mut capture =
            CaptureCallback::new(capture_tx, fault.clone(), self.scenario.input_rate, 0, 1);
        capture.meters = self.meters.clone();
        let mut input_times = CallbackTimestamps::new(&self.scenario, Direction::Input);
        let mut output_times = CallbackTimestamps::new(&self.scenario, Direction::Output);
        let mut input_first = None;
        let mut input_last = None;
        let mut samples = Vec::with_capacity(mic.len());
        let mut out = [0.0; 256];
        let mut written = 0;
        let mut output_cursor = 0;
        let mut input_cursor = 0;
        let output_fs = f64::from(self.scenario.rate.hz());
        let input_fs =
            f64::from(self.scenario.input_rate.hz()) * (1.0 + self.scenario.drift_ppm * 1e-6);
        let output_start = (-self.scenario.input_start_seconds).max(0.0);
        let input_start = self.scenario.input_start_seconds.max(0.0);
        let started = Instant::now();
        while input_cursor < mic.len() || played.load(Ordering::Relaxed) < output.len() as u64 {
            if cancel.is_cancelled() {
                return Err(super::workflow::Fault::Stopped.into());
            }
            if self.deadline.is_some_and(|end| self.elapsed() >= end) {
                return Err(super::workflow::Fault::TimeBudget.into());
            }
            fault.check()?;
            while tx.slots() > 0 && written < output.len() {
                tx.push(output[written])
                    .expect("available paced playback slot");
                written += 1;
            }
            let elapsed = started.elapsed().as_secs_f64();
            let next_output = output_start + output_cursor as f64 / output_fs;
            let next_input = input_start + (input_cursor + 197).min(mic.len()) as f64 / input_fs;
            if elapsed - next_output.min(next_input) > 8192.0 / output_fs.max(input_fs) {
                return Err(super::workflow::Fault::Transport(
                    "Emulated callback scheduling exceeded transport capacity".into(),
                )
                .into());
            }
            if next_output <= elapsed {
                playback.process(&mut out, output_times.stamp(next_output - output_start));
                let remaining = output.len().saturating_sub(output_cursor).min(128);
                assert_eq!(
                    &out.as_chunks::<2>().0[..remaining],
                    &output[output_cursor.min(output.len())
                        ..output_cursor.min(output.len()) + remaining],
                    "paced output must match the plant's emitted reference"
                );
                output_cursor += 128;
            }
            if input_cursor < mic.len() && next_input <= elapsed {
                if matches!(self.scenario.fault,Fault::DropOnTake(t)if t==self.take)
                    && input_cursor / 197 == 10
                {
                    capture.ordinal += 197;
                    fault.set(CallbackFault::CaptureGap);
                }
                let end = (input_cursor + 197).min(mic.len());
                capture.process(
                    &mic[input_cursor..end],
                    input_times.stamp(input_cursor as f64 / input_fs),
                );
                input_cursor = end;
                while let Ok(frame) = capture_rx.pop() {
                    if frame.ordinal != samples.len() as u64 {
                        return Err(super::workflow::Fault::Transport(
                            "Emulated capture ordinal gap".into(),
                        )
                        .into());
                    }
                    input_first.get_or_insert(frame.timestamp_ns);
                    input_last = Some(frame.timestamp_ns);
                    samples.push(frame.sample);
                }
            }
            if next_output > elapsed && next_input > elapsed {
                let jitter = self.scenario.scheduling_jitter_seconds
                    * (0.5 + f64::from(self.random()) * 0.5);
                let wait = (next_output.min(next_input) - elapsed + jitter).min(0.002);
                std::thread::sleep(Duration::from_secs_f64(wait));
            }
        }
        fault.check()?;
        let recording = Recording {
            input: CaptureTiming {
                first_frame: 0,
                frames: samples.len() as u64,
                first_timestamp_ns: input_first,
                last_timestamp_ns: input_last,
                discontinuities: u64::from(fault.failed()),
            },
            output: CaptureTiming {
                first_frame: 0,
                frames: played.load(Ordering::Relaxed),
                first_timestamp_ns: Some(first.load(Ordering::Relaxed)),
                last_timestamp_ns: Some(last.load(Ordering::Relaxed)),
                discontinuities: u64::from(fault.failed()),
            },
            samples,
            rate: self.scenario.input_rate,
            output_rate: self.scenario.rate,
        };
        recording.validate(output.len())?;
        Ok(recording)
    }
}

impl Provider for Simulated {
    fn set_meters(&mut self, meters: Arc<crate::audio::Meters>) {
        self.meters = meters;
    }
    fn set_deadline(&mut self, deadline: Duration) {
        self.deadline = Some(deadline);
    }
    fn elapsed(&self) -> Duration {
        match self.scenario.pace {
            Pace::Deterministic => Duration::from_secs_f64(self.virtual_seconds),
            Pace::RealTime => self.started.elapsed(),
        }
    }
    fn record(
        &mut self,
        output: &[Frame],
        position: usize,
        cancel: &crate::audio::Cancellation,
    ) -> Result<Recording> {
        if self.deadline.is_some_and(|end| {
            self.elapsed()
                + Duration::from_secs_f64(output.len() as f64 / f64::from(self.scenario.rate.hz()))
                > end
        }) {
            return Err(super::workflow::Fault::TimeBudget.into());
        }
        self.take += 1;
        if matches!(self.scenario.fault, Fault::DeviceLostOnTake(t) if t == self.take) {
            return Err(invalid("Emulated device removed"));
        }
        let fault = Arc::new(FaultLatch::default());
        let played = Arc::new(AtomicU64::new(0));
        let first = Arc::new(AtomicU64::new(0));
        let last = Arc::new(AtomicU64::new(0));
        let (mut tx, rx) = rtrb::RingBuffer::new(8192);
        let mut callback = PlaybackCallback {
            clock: CallbackClock::new(self.scenario.rate),
            ring: rx,
            fault: fault.clone(),
            played: played.clone(),
            total: output.len(),
            pair: StereoPair::default(),
            channels: 2,
            cancel: cancel.clone(),
            first_timestamp: first.clone(),
            last_timestamp: last.clone(),
            meters: if self.scenario.pace == Pace::RealTime {
                Arc::new(crate::audio::Meters::default())
            } else {
                self.meters.clone()
            },
            deadline: if self.scenario.pace == Pace::RealTime {
                self.deadline.map(|d| self.started + d)
            } else {
                None
            },
        };
        let mut output_times = CallbackTimestamps::new(&self.scenario, Direction::Output);
        let mut emitted = Vec::with_capacity(output.len());
        let mut block = [0.0; 256];
        let mut written = 0;
        while emitted.len() < output.len() {
            if cancel.is_cancelled() {
                return Err(super::workflow::Fault::Stopped.into());
            }
            while tx.slots() > 0 && written < output.len() {
                tx.push(output[written]).expect("available simulated slot");
                written += 1;
            }
            callback.process(
                &mut block,
                output_times.stamp(emitted.len() as f64 / f64::from(self.scenario.rate.hz())),
            );
            let remaining = output.len() - emitted.len();
            emitted.extend(block.as_chunks::<2>().0.iter().take(remaining).copied());
        }
        let room = [
            room_impulse(self.scenario.rate, 0, position, &self.scenario.plant)?,
            room_impulse(self.scenario.rate, 1, position, &self.scenario.plant)?,
        ];
        let max_ir = room.iter().map(Vec::len).max().expect("stereo room");
        let n = (emitted.len() + max_ir).next_power_of_two();
        let mut sum = vec![rustfft::num_complex::Complex64::default(); n];
        for (c, h) in room.iter().enumerate() {
            let input: Vec<_> = emitted.iter().map(|s| s[c]).collect();
            let x = fft(&input, n);
            let h = fft(h, n);
            for ((sum, x), h) in sum.iter_mut().zip(x).zip(h) {
                *sum += x * h;
            }
        }
        let acoustic = inverse(sum);
        let ratio = f64::from(self.scenario.input_rate.hz()) / f64::from(self.scenario.rate.hz())
            * (1.0 + self.scenario.drift_ppm * 1e-6);
        let delay = (self.scenario.latency_seconds - self.scenario.input_start_seconds)
            * f64::from(self.scenario.input_rate.hz());
        let frames = ((output.len() as f64 / f64::from(self.scenario.rate.hz()) + 0.5)
            * f64::from(self.scenario.input_rate.hz()))
        .ceil() as usize;
        let mut mic = resample(&acoustic, -delay / ratio, 1.0 / ratio, frames);
        let noise = self.scenario.noise_peak;
        for s in &mut mic {
            *s *= self.scenario.input_gain;
            if self.scenario.nonlinearity > 0.0 {
                *s = (*s * self.scenario.nonlinearity).tanh() / self.scenario.nonlinearity;
            }
            *s += self.random() * noise;
        }
        if matches!(self.scenario.fault, Fault::ClipOnTake(t) if t == self.take) {
            mic[frames / 2] = 1.0;
        }
        if matches!(self.scenario.fault, Fault::TransientOnTake(t) if t == self.take) {
            for s in &mut mic[frames / 2..frames / 2 + 512] {
                *s += 0.2;
            }
        }
        if self.scenario.pace == Pace::RealTime {
            return self.replay(output, &mic, cancel);
        }
        let (tx, mut rx) = rtrb::RingBuffer::new(8192);
        let mut capture = CaptureCallback::new(tx, fault.clone(), self.scenario.input_rate, 0, 1);
        capture.meters = self.meters.clone();
        let mut input_times = CallbackTimestamps::new(&self.scenario, Direction::Input);
        let mut input_first = None;
        let mut input_last = None;
        let mut samples = Vec::with_capacity(frames);
        for (i, block) in mic.chunks(197).enumerate() {
            if self.deadline.is_some_and(|end| self.elapsed() >= end) {
                return Err(super::workflow::Fault::TimeBudget.into());
            }
            if cancel.is_cancelled() {
                return Err(super::workflow::Fault::Stopped.into());
            }
            if matches!(self.scenario.fault, Fault::DropOnTake(t) if t == self.take) && i == 10 {
                capture.ordinal += 197;
                fault.set(CallbackFault::CaptureGap);
            }
            let time = input_times.stamp(
                i as f64 * 197.0
                    / (f64::from(self.scenario.input_rate.hz())
                        * (1.0 + self.scenario.drift_ppm * 1e-6)),
            );
            capture.process(block, time);
            while let Ok(frame) = rx.pop() {
                input_first.get_or_insert(frame.timestamp_ns);
                input_last = Some(frame.timestamp_ns);
                samples.push(frame.sample);
            }
        }
        self.virtual_seconds += output.len() as f64 / f64::from(self.scenario.rate.hz());
        fault.check()?;
        let recording = Recording {
            input: CaptureTiming {
                first_frame: 0,
                frames: samples.len() as u64,
                first_timestamp_ns: input_first,
                last_timestamp_ns: input_last,
                discontinuities: u64::from(fault.failed()),
            },
            output: CaptureTiming {
                first_frame: 0,
                frames: played.load(Ordering::Relaxed),
                first_timestamp_ns: Some(first.load(Ordering::Relaxed)),
                last_timestamp_ns: Some(last.load(Ordering::Relaxed)),
                discontinuities: u64::from(fault.failed()),
            },
            samples,
            rate: self.scenario.input_rate,
            output_rate: self.scenario.rate,
        };
        recording.validate(output.len())?;
        Ok(recording)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microphone_discovery_uses_its_independent_nominal_rate() {
        let scenario = Scenario {
            input_rate: Rate::Hz44100,
            ..Scenario::default()
        };
        let devices = devices(&scenario);
        let mic = devices
            .iter()
            .find(|device| device.uid == microphone().uid)
            .unwrap();
        assert_eq!(mic.input_rate, Some(Rate::Hz44100));
        assert_eq!(mic.rate, Some(Rate::Hz44100));
        let speakers = devices
            .iter()
            .find(|device| device.uid == route().playback.uid)
            .unwrap();
        assert_eq!(speakers.output_rate, Some(Rate::Hz48000));
    }

    #[test]
    fn timestamp_jitter_is_seeded_independent_and_quantized() {
        let scenario = Scenario {
            timestamp_jitter_seconds: 0.0005,
            timestamp_quantum_seconds: 0.00001,
            ..Scenario::default()
        };
        let mut input = CallbackTimestamps::new(&scenario, Direction::Input);
        let mut repeated = CallbackTimestamps::new(&scenario, Direction::Input);
        let mut output = CallbackTimestamps::new(&scenario, Direction::Output);
        let mut different = false;
        let mut previous = 0;
        for packet in 0..100 {
            let time = packet as f64 * 128.0 / 48000.0;
            let timestamp = input.stamp(time);
            assert_eq!(timestamp, repeated.stamp(time));
            assert_eq!(timestamp % 10_000, 0);
            assert!(timestamp >= previous);
            different |= timestamp != output.stamp(time);
            previous = timestamp;
        }
        assert!(different);
    }

    #[test]
    fn perturbed_host_timestamps_preserve_original_mic_samples() {
        let exact = Scenario {
            plant: Plant::Identity,
            timestamp_jitter_seconds: 0.0,
            timestamp_quantum_seconds: 0.0,
            ..Scenario::default()
        };
        let perturbed = Scenario {
            timestamp_jitter_seconds: 0.0005,
            timestamp_quantum_seconds: 0.001,
            ..exact.clone()
        };
        let probe: Vec<_> = (0..4096)
            .map(|n| [0.01 * (n as f32 * 0.1).sin(), 0.0])
            .collect();
        let cancel = crate::audio::Cancellation::new();
        let exact = Simulated::new(exact)
            .unwrap()
            .record(&probe, 0, &cancel)
            .unwrap();
        let perturbed = Simulated::new(perturbed)
            .unwrap()
            .record(&probe, 0, &cancel)
            .unwrap();
        assert_eq!(perturbed.samples, exact.samples);
        assert_eq!(perturbed.output.frames, probe.len() as u64);
        assert_eq!(perturbed.input.frames, exact.input.frames);
        assert_ne!(
            perturbed.input.last_timestamp_ns,
            exact.input.last_timestamp_ns
        );
    }
}
