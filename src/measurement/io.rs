//! Exclusive experiment I/O. The owner closes listening streams before creating it.
use super::{CaptureTiming, SourcePlan};
use crate::{
    domain::{DeviceUid, Rate},
    dsp::Frame,
    error::{Result, invalid},
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// Protective digital full-scale margin; it does not diagnose analog clipping.
pub const INPUT_CLIPPING_PEAK: f32 = 0.999;

pub struct Recording {
    pub samples: Vec<f32>,
    pub rate: Rate,
    pub output_rate: Rate,
    pub input: CaptureTiming,
    pub output: CaptureTiming,
}
impl Recording {
    pub fn validate(&self, output_frames: usize) -> Result<()> {
        if self.input.discontinuities != 0 || self.output.discontinuities != 0 {
            return Err(invalid(
                "Audio samples were lost during recording. Check the devices and try again",
            ));
        }
        if self.output.frames != output_frames as u64 {
            return Err(invalid(
                "Speaker playback ended before the probe finished. Try again",
            ));
        }
        if self.input.frames != self.samples.len() as u64 {
            return Err(invalid(
                "Microphone recording has inconsistent sample counts",
            ));
        }
        if (self.samples.len() as f64)
            < output_frames as f64 * f64::from(self.rate.hz()) / f64::from(self.output_rate.hz())
                * 0.998
        {
            return Err(invalid(
                "Microphone recording ended before the probe finished. Try again",
            ));
        }
        if self.samples.iter().any(|s| !s.is_finite()) {
            return Err(invalid(CallbackFault::NonFinite.to_string()));
        }
        if self.samples.iter().any(|s| s.abs() >= INPUT_CLIPPING_PEAK) {
            return Err(invalid(CallbackFault::Clipping.to_string()));
        }
        Ok(())
    }
}

struct CaptureBuffer {
    samples: Vec<f32>,
    capacity: usize,
    first: Option<u64>,
    last: Option<u64>,
    ordinal: u64,
}
impl CaptureBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            samples: Vec::with_capacity(capacity),
            capacity,
            first: None,
            last: None,
            ordinal: 0,
        }
    }
    fn drain(&mut self, ring: &mut rtrb::Consumer<InputFrame>) -> Result<()> {
        while let Ok(frame) = ring.pop() {
            if frame.ordinal != self.ordinal || self.samples.len() == self.capacity {
                return Err(invalid("Measurement capture continuity or capacity failed"));
            }
            self.first.get_or_insert(frame.timestamp_ns);
            self.last = Some(frame.timestamp_ns);
            self.ordinal += 1;
            self.samples.push(frame.sample);
        }
        Ok(())
    }
}

pub trait Provider: Send {
    fn record(
        &mut self,
        output: &[Frame],
        position: usize,
        cancel: &crate::audio::Cancellation,
    ) -> Result<Recording>;
    fn elapsed(&self) -> Duration;
    fn set_deadline(&mut self, deadline: Duration);
    fn set_meters(&mut self, meters: Arc<crate::audio::Meters>);
}

#[derive(Clone, Copy, Debug)]
pub struct InputFrame {
    pub sample: f32,
    pub ordinal: u64,
    pub timestamp_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Direction {
    #[error("Microphone input")]
    Input,
    #[error("Speaker output")]
    Output,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClockFault {
    #[error("delivered an empty audio buffer")]
    Empty,
    #[error("timestamps moved backwards")]
    Backwards,
    #[error("timestamps contain a large gap")]
    Gap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CallbackFault {
    #[error(
        "Microphone input reached the full-scale margin and may be overloaded. Reduce microphone gain or speaker level and try again"
    )]
    Clipping,
    #[error("Microphone delivered invalid samples. Check its driver and reconnect it")]
    NonFinite,
    #[error("Microphone recording buffer overflowed. Close busy applications and try again")]
    CaptureOverflow,
    #[error("Speaker playback buffer ran empty. Close busy applications and try again")]
    PlaybackUnderrun,
    #[error("Measurement time budget exhausted")]
    Deadline,
    #[error("{direction} {reason}. Recording stopped; check the audio device and try again")]
    Clock {
        direction: Direction,
        reason: ClockFault,
    },
    #[error("{direction}: {kind} Recording stopped; check the device and try again")]
    Backend {
        direction: Direction,
        kind: cpal::ErrorKind,
    },
    #[error("Microphone recording lost samples. Check the audio device and try again")]
    CaptureGap,
}

/// First failure wins. Only this real-time storage boundary encodes typed reasons.
#[derive(Default)]
pub struct FaultLatch(AtomicU64);
impl FaultLatch {
    pub fn set(&self, fault: CallbackFault) {
        let code = match fault {
            CallbackFault::Clipping => 1,
            CallbackFault::NonFinite => 2,
            CallbackFault::CaptureOverflow => 3,
            CallbackFault::PlaybackUnderrun => 4,
            CallbackFault::Deadline => 5,
            CallbackFault::CaptureGap => 6,
            CallbackFault::Clock { direction, reason } => {
                let base = match direction {
                    Direction::Input => 8,
                    Direction::Output => 12,
                };
                base + match reason {
                    ClockFault::Empty => 0,
                    ClockFault::Backwards => 1,
                    ClockFault::Gap => 2,
                }
            }
            CallbackFault::Backend { direction, kind } => {
                let base = match direction {
                    Direction::Input => 32,
                    Direction::Output => 64,
                };
                base + backend_code(kind)
            }
        };
        let _ = self
            .0
            .compare_exchange(0, code, Ordering::Relaxed, Ordering::Relaxed);
    }
    pub fn get(&self) -> Option<CallbackFault> {
        Some(match self.0.load(Ordering::Relaxed) {
            0 => return None,
            1 => CallbackFault::Clipping,
            2 => CallbackFault::NonFinite,
            3 => CallbackFault::CaptureOverflow,
            4 => CallbackFault::PlaybackUnderrun,
            5 => CallbackFault::Deadline,
            6 => CallbackFault::CaptureGap,
            code @ (8..=10 | 12..=14) => CallbackFault::Clock {
                direction: if code < 12 {
                    Direction::Input
                } else {
                    Direction::Output
                },
                reason: match code % 4 {
                    0 => ClockFault::Empty,
                    1 => ClockFault::Backwards,
                    2 => ClockFault::Gap,
                    _ => unreachable!(),
                },
            },
            code @ (32..=45 | 64..=77) => CallbackFault::Backend {
                direction: if code < 64 {
                    Direction::Input
                } else {
                    Direction::Output
                },
                kind: backend_kind(code % 32),
            },
            _ => unreachable!("fault latch contains only encoded callback failures"),
        })
    }
    pub fn failed(&self) -> bool {
        self.0.load(Ordering::Relaxed) != 0
    }
    pub fn check(&self) -> Result<()> {
        match self.get() {
            Some(CallbackFault::Deadline) => Err(super::workflow::Fault::TimeBudget.into()),
            Some(fault) => Err(invalid(fault.to_string())),
            None => Ok(()),
        }
    }
    pub fn backend(
        &self,
        direction: Direction,
        kind: cpal::ErrorKind,
        meters: &crate::audio::Meters,
    ) {
        #[cfg(feature = "alloc-audit")]
        let _audit = crate::audit::enter();
        if kind == cpal::ErrorKind::Xrun {
            meters.backend_xruns.fetch_add(1, Ordering::Relaxed);
        }
        self.set(CallbackFault::Backend { direction, kind });
    }
}

fn backend_code(kind: cpal::ErrorKind) -> u64 {
    use cpal::ErrorKind::*;
    match kind {
        DeviceBusy => 0,
        DeviceChanged => 1,
        DeviceNotAvailable => 2,
        HostUnavailable => 3,
        InvalidInput => 4,
        PermissionDenied => 5,
        RealtimeDenied => 6,
        ResourceExhausted => 7,
        StreamInvalidated => 8,
        UnsupportedConfig => 9,
        UnsupportedOperation => 10,
        Xrun => 11,
        BackendError => 12,
        Other => 13,
        _ => 13,
    }
}
fn backend_kind(code: u64) -> cpal::ErrorKind {
    use cpal::ErrorKind::*;
    match code {
        0 => DeviceBusy,
        1 => DeviceChanged,
        2 => DeviceNotAvailable,
        3 => HostUnavailable,
        4 => InvalidInput,
        5 => PermissionDenied,
        6 => RealtimeDenied,
        7 => ResourceExhausted,
        8 => StreamInvalidated,
        9 => UnsupportedConfig,
        10 => UnsupportedOperation,
        11 => Xrun,
        12 => BackendError,
        13 => Other,
        _ => unreachable!("backend fault code was encoded by backend_code"),
    }
}

/// A gross host timestamp sanity check, independent of acoustic clock fitting.
pub struct CallbackClock {
    rate: Rate,
    previous: Option<(u64, usize)>,
}
impl CallbackClock {
    pub fn new(rate: Rate) -> Self {
        Self {
            rate,
            previous: None,
        }
    }
    fn observe(&mut self, timestamp_ns: u64, frames: usize) -> std::result::Result<(), ClockFault> {
        if frames == 0 {
            return Err(ClockFault::Empty);
        }
        if let Some((time, previous_frames)) = self.previous {
            if timestamp_ns < time {
                return Err(ClockFault::Backwards);
            }
            let expected = previous_frames as f64 * 1e9 / f64::from(self.rate.hz());
            // CPAL exposes host-time estimates, not hardware sample positions. Allow
            // two 20 ms timestamp offsets plus quantization margin and clock drift.
            // Real sample loss is detected by transport errors and acoustic evidence.
            let tolerance = 50_000_000.0 + expected * 0.0021;
            if ((timestamp_ns - time) as f64 - expected).abs() > tolerance {
                return Err(ClockFault::Gap);
            }
        }
        self.previous = Some((timestamp_ns, frames));
        Ok(())
    }
}

/// The same bounded capture callback is used by CPAL and simulated devices.
pub struct CaptureCallback {
    pub ring: rtrb::Producer<InputFrame>,
    pub fault: Arc<FaultLatch>,
    pub ordinal: u64,
    pub channel: usize,
    pub channels: usize,
    pub meters: Arc<crate::audio::Meters>,
    clock: CallbackClock,
}
impl CaptureCallback {
    pub fn new(
        ring: rtrb::Producer<InputFrame>,
        fault: Arc<FaultLatch>,
        rate: Rate,
        channel: usize,
        channels: usize,
    ) -> Self {
        Self {
            ring,
            fault,
            ordinal: 0,
            channel,
            channels,
            meters: Arc::new(crate::audio::Meters::default()),
            clock: CallbackClock::new(rate),
        }
    }
    pub fn process(&mut self, data: &[f32], timestamp_ns: u64) {
        #[cfg(feature = "alloc-audit")]
        let _audit = crate::audit::enter();
        if let Err(reason) = self.clock.observe(timestamp_ns, data.len() / self.channels) {
            self.fault.set(CallbackFault::Clock {
                direction: Direction::Input,
                reason,
            });
        }
        for (offset, frame) in data.chunks_exact(self.channels).enumerate() {
            let sample = frame[self.channel];
            self.meters.record_input(sample.abs());
            if !sample.is_finite() {
                self.fault.set(CallbackFault::NonFinite);
            } else if sample.abs() >= INPUT_CLIPPING_PEAK {
                self.fault.set(CallbackFault::Clipping);
            }
            if self
                .ring
                .push(InputFrame {
                    sample,
                    ordinal: self.ordinal,
                    timestamp_ns: timestamp_ns
                        + offset as u64 * 1_000_000_000 / u64::from(self.clock.rate.hz()),
                })
                .is_err()
            {
                self.meters.overruns.fetch_add(1, Ordering::Relaxed);
                self.fault.set(CallbackFault::CaptureOverflow);
            }
            self.ordinal += 1;
        }
    }
}

pub struct PlaybackCallback {
    pub clock: CallbackClock,
    pub ring: rtrb::Consumer<Frame>,
    pub fault: Arc<FaultLatch>,
    pub played: Arc<AtomicU64>,
    pub total: usize,
    pub pair: crate::domain::StereoPair,
    pub channels: usize,
    pub cancel: crate::audio::Cancellation,
    pub first_timestamp: Arc<AtomicU64>,
    pub last_timestamp: Arc<AtomicU64>,
    pub deadline: Option<Instant>,
    pub meters: Arc<crate::audio::Meters>,
}
impl PlaybackCallback {
    pub fn process(&mut self, data: &mut [f32], timestamp_ns: u64) {
        #[cfg(feature = "alloc-audit")]
        let _audit = crate::audit::enter();
        if let Err(reason) = self.clock.observe(timestamp_ns, data.len() / self.channels) {
            self.fault.set(CallbackFault::Clock {
                direction: Direction::Output,
                reason,
            });
        }
        if self.deadline.is_some_and(|end| Instant::now() >= end) {
            self.fault.set(CallbackFault::Deadline);
        }
        let started = self.played.load(Ordering::Relaxed);
        let [left, right] = self.pair.indices();
        for frame in data.chunks_exact_mut(self.channels) {
            frame.fill(0.0);
            if self.cancel.is_cancelled() || self.fault.failed() {
                continue;
            }
            if self.played.load(Ordering::Relaxed) < self.total as u64 {
                match self.ring.pop() {
                    Ok(s) => {
                        frame[left] = s[0];
                        frame[right] = s[1];
                        self.meters.record_output(s[0].abs().max(s[1].abs()));
                        self.played.fetch_add(1, Ordering::Relaxed);
                        self.meters.output_frames.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(_) => {
                        self.meters.underruns.fetch_add(1, Ordering::Relaxed);
                        self.fault.set(CallbackFault::PlaybackUnderrun);
                    }
                }
            }
        }
        let emitted = self.played.load(Ordering::Relaxed) - started;
        if emitted > 0 {
            if started == 0 {
                self.first_timestamp.store(timestamp_ns, Ordering::Relaxed);
            }
            self.last_timestamp.store(
                timestamp_ns + (emitted - 1) * 1_000_000_000 / u64::from(self.clock.rate.hz()),
                Ordering::Relaxed,
            );
        }
    }
}

pub struct Hardware {
    plan: SourcePlan,
    started: Instant,
    deadline: Option<Instant>,
    meters: Arc<crate::audio::Meters>,
}
impl Hardware {
    pub fn new(plan: SourcePlan) -> Result<Self> {
        plan.validate()?;
        if !matches!(plan.microphone.uid, DeviceUid::Hardware(_))
            || !matches!(plan.route.playback.uid, DeviceUid::Hardware(_))
        {
            return Err(invalid("Hardware I/O requires hardware endpoints"));
        }
        let device = crate::audio::devices()?
            .into_iter()
            .find(|d| d.uid == plan.microphone.uid)
            .ok_or_else(|| invalid("Measurement microphone is unavailable"))?;
        if device.virtual_device || device.input_channels == 0 {
            return Err(invalid(
                "Select a physical microphone input; virtual and hardware-loopback monitoring must be disabled",
            ));
        }
        Ok(Self {
            plan,
            started: Instant::now(),
            deadline: None,
            meters: Arc::new(crate::audio::Meters::default()),
        })
    }
}
impl Provider for Hardware {
    fn set_meters(&mut self, meters: Arc<crate::audio::Meters>) {
        self.meters = meters;
    }
    fn set_deadline(&mut self, deadline: Duration) {
        self.deadline = Some(self.started + deadline);
    }
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
    fn record(
        &mut self,
        output: &[Frame],
        _: usize,
        cancel: &crate::audio::Cancellation,
    ) -> Result<Recording> {
        if self.deadline.is_some_and(|end| {
            Instant::now()
                + Duration::from_secs_f64(
                    output.len() as f64 / f64::from(self.plan.rate.hz()) + 0.5,
                )
                >= end
        }) {
            return Err(super::workflow::Fault::TimeBudget.into());
        }
        let _device_io = cancel.device_io();
        if cancel.is_cancelled() {
            return Err(super::workflow::Fault::Stopped.into());
        }
        let host = cpal::default_host();
        let input = host
            .device_by_id(self.plan.microphone.uid.id())
            .ok_or_else(|| invalid("Measurement microphone is unavailable"))?;
        let device = host
            .device_by_id(self.plan.route.playback.uid.id())
            .ok_or_else(|| invalid("Measurement playback is unavailable"))?;
        let input_config = crate::audio::config(&input, true)?;
        let output_config = crate::audio::config(&device, false)?;
        let input_rate = Rate::try_from(input_config.sample_rate)?;
        if output_config.sample_rate != self.plan.rate.hz() {
            return Err(invalid("Playback rate changed; restart measurement setup"));
        }
        self.plan
            .microphone
            .channels
            .validate(input_config.channels)?;
        self.plan
            .route
            .playback
            .channels
            .validate(output_config.channels)?;
        let fault = Arc::new(FaultLatch::default());
        let (capture_tx, mut capture_rx) = rtrb::RingBuffer::new(32768);
        let (mut playback_tx, playback_rx) = rtrb::RingBuffer::new(32768);
        let mut capture = CaptureCallback::new(
            capture_tx,
            fault.clone(),
            input_rate,
            usize::from(self.plan.microphone.channels.0),
            usize::from(input_config.channels),
        );
        capture.meters = self.meters.clone();
        let mut origin = None;
        let fail = fault.clone();
        let error_meters = self.meters.clone();
        let input_stream = input.build_input_stream(
            input_config,
            move |data: &[f32], info: &cpal::InputCallbackInfo| {
                let current = info.timestamp().capture;
                let origin = *origin.get_or_insert(current);
                let time = current
                    .duration_since(origin)
                    .as_nanos()
                    .min(u128::from(u64::MAX)) as u64;
                capture.process(data, time);
            },
            move |error| {
                fail.backend(Direction::Input, error.kind(), &error_meters);
            },
            None,
        )?;
        let played = Arc::new(AtomicU64::new(0));
        let first = Arc::new(AtomicU64::new(0));
        let last = Arc::new(AtomicU64::new(0));
        let mut playback = PlaybackCallback {
            clock: CallbackClock::new(self.plan.rate),
            ring: playback_rx,
            fault: fault.clone(),
            played: played.clone(),
            total: output.len(),
            pair: self.plan.route.playback.channels,
            channels: usize::from(output_config.channels),
            cancel: cancel.clone(),
            first_timestamp: first.clone(),
            last_timestamp: last.clone(),
            deadline: self.deadline,
            meters: self.meters.clone(),
        };
        let fail = fault.clone();
        let mut origin = None;
        let error_meters = self.meters.clone();
        let output_stream = device.build_output_stream(
            output_config,
            move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
                let current = info.timestamp().playback;
                let origin = *origin.get_or_insert(current);
                playback.process(
                    data,
                    current
                        .duration_since(origin)
                        .as_nanos()
                        .min(u128::from(u64::MAX)) as u64,
                );
            },
            move |error| {
                fail.backend(Direction::Output, error.kind(), &error_meters);
            },
            None,
        )?;
        let expected = ((output.len() as f64 / f64::from(self.plan.rate.hz()) + 2.0)
            * f64::from(input_rate.hz()))
        .ceil() as usize;
        let mut captured = CaptureBuffer::new(expected);
        let mut written = 0;
        while playback_tx.slots() > 0 && written < output.len() {
            playback_tx
                .push(output[written])
                .expect("available playback slot");
            written += 1;
        }
        input_stream.play()?;
        let priming = Instant::now() + Duration::from_secs(2);
        while captured.samples.len() < input_rate.hz() as usize / 50 {
            if cancel.is_cancelled() {
                return Err(super::workflow::Fault::Stopped.into());
            }
            fault.check()?;
            if Instant::now() > priming {
                return Err(invalid("Microphone did not begin continuous capture"));
            }
            captured.drain(&mut capture_rx)?;
            std::thread::sleep(Duration::from_millis(1));
        }
        // Confirm ADC delivery before starting the independent DAC timeline.
        output_stream.play()?;
        let deadline = Instant::now()
            + Duration::from_secs_f64(output.len() as f64 / f64::from(self.plan.rate.hz()) + 5.0);
        let mut complete = None;
        let mut capture_progress = Instant::now();
        let mut playback_progress = Instant::now();
        let mut previous_played = 0;
        loop {
            if self.deadline.is_some_and(|end| Instant::now() >= end) {
                return Err(super::workflow::Fault::TimeBudget.into());
            }
            if cancel.is_cancelled() {
                return Err(super::workflow::Fault::Stopped.into());
            }
            fault.check()?;
            let previous = captured.samples.len();
            captured.drain(&mut capture_rx)?;
            if captured.samples.len() != previous {
                capture_progress = Instant::now();
            }
            let current_played = played.load(Ordering::Relaxed);
            if current_played != previous_played {
                playback_progress = Instant::now();
                previous_played = current_played;
            }
            if capture_progress.elapsed() > Duration::from_secs(1)
                || playback_progress.elapsed() > Duration::from_secs(1)
            {
                return Err(invalid(
                    if capture_progress.elapsed() > Duration::from_secs(1) {
                        "Microphone stopped delivering audio. Check the device and try again"
                    } else {
                        "Speaker output stopped requesting audio. Check the device and try again"
                    },
                ));
            }
            while playback_tx.slots() > 0 && written < output.len() {
                playback_tx
                    .push(output[written])
                    .expect("available playback slot");
                written += 1;
            }
            if played.load(Ordering::Relaxed) == output.len() as u64 {
                let end = complete.get_or_insert_with(Instant::now);
                if end.elapsed() >= Duration::from_millis(250) {
                    break;
                }
            }
            if Instant::now() > deadline {
                return Err(invalid("Measurement stream stalled or device suspended"));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(output_stream);
        drop(input_stream);
        captured.drain(&mut capture_rx)?;
        fault.check()?;
        let recording = Recording {
            input: CaptureTiming {
                first_frame: 0,
                frames: captured.samples.len() as u64,
                first_timestamp_ns: captured.first,
                last_timestamp_ns: captured.last,
                discontinuities: 0,
            },
            output: CaptureTiming {
                first_frame: 0,
                frames: output.len() as u64,
                first_timestamp_ns: Some(first.load(Ordering::Relaxed)),
                last_timestamp_ns: Some(last.load(Ordering::Relaxed)),
                discontinuities: 0,
            },
            samples: captured.samples,
            rate: input_rate,
            output_rate: self.plan.rate,
        };
        recording.validate(output.len())?;
        Ok(recording)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timestamp_jitter_does_not_cut_short_level_check_output() {
        let fault = Arc::new(FaultLatch::default());
        let (capture_tx, mut capture_rx) = rtrb::RingBuffer::new(256);
        let mut capture = CaptureCallback::new(capture_tx, fault.clone(), Rate::Hz48000, 0, 1);
        let (mut output_tx, output_rx) = rtrb::RingBuffer::new(256);
        let played = Arc::new(AtomicU64::new(0));
        let mut playback = PlaybackCallback {
            clock: CallbackClock::new(Rate::Hz48000),
            ring: output_rx,
            fault: fault.clone(),
            played: played.clone(),
            total: 128 * 64,
            pair: crate::domain::StereoPair::default(),
            channels: 2,
            cancel: crate::audio::Cancellation::new(),
            first_timestamp: Arc::new(AtomicU64::new(0)),
            last_timestamp: Arc::new(AtomicU64::new(0)),
            deadline: None,
            meters: Arc::new(crate::audio::Meters::default()),
        };
        for packet in 0..64 {
            let timestamp = (packet as f64 * 128.0 / 48000.0 * 1e9).round() as u64
                + [0, 80_000, 20_000, 100_000][packet % 4];
            capture.process(&[0.01; 128], timestamp);
            for _ in 0..128 {
                capture_rx.pop().unwrap();
                output_tx.push([0.02, 0.0]).unwrap();
            }
            let mut output = [0.0; 256];
            playback.process(&mut output, timestamp);
            assert!(
                output.as_chunks::<2>().0.iter().all(|f| *f == [0.02, 0.0]),
                "level-check output cut short at packet {packet}; no samples were dropped"
            );
        }
        assert_eq!(played.load(Ordering::Relaxed), 128 * 64);
        assert_eq!(fault.get(), None);
    }
    #[test]
    fn host_clock_accepts_drift_jitter_quantization_and_variable_blocks() {
        for drift in [-2000.0, 100.0, 2000.0] {
            let mut clock = CallbackClock::new(Rate::Hz48000);
            let mut frames = 0;
            for packet in 0..1000 {
                let block = [32, 128, 197, 512][packet % 4];
                let ideal = frames as f64 * 1e9 / (48000.0 * (1.0 + drift * 1e-6));
                let timestamp = (ideal as u64 + [0, 80_000, 20_000, 100_000][packet % 4])
                    / 1_000_000
                    * 1_000_000;
                assert_eq!(clock.observe(timestamp, block), Ok(()));
                frames += block;
            }
        }
    }

    #[test]
    fn host_clock_rejects_regression_empty_buffers_and_large_gaps() {
        let mut clock = CallbackClock::new(Rate::Hz48000);
        assert_eq!(clock.observe(1_000_000, 128), Ok(()));
        assert_eq!(clock.observe(0, 128), Err(ClockFault::Backwards));
        let mut clock = CallbackClock::new(Rate::Hz48000);
        assert_eq!(clock.observe(0, 128), Ok(()));
        assert_eq!(clock.observe(1_000_000_000, 128), Err(ClockFault::Gap));
        assert_eq!(clock.observe(1_000_000_000, 0), Err(ClockFault::Empty));
    }

    #[test]
    fn capture_faults_keep_the_specific_first_cause() {
        for (input, expected) in [
            ([1.0, 0.0], CallbackFault::Clipping),
            ([f32::NAN, 0.0], CallbackFault::NonFinite),
            ([0.1, 0.1], CallbackFault::CaptureOverflow),
        ] {
            let fault = Arc::new(FaultLatch::default());
            let (tx, _rx) = rtrb::RingBuffer::new(1);
            let mut capture = CaptureCallback::new(tx, fault.clone(), Rate::Hz48000, 0, 1);
            capture.process(&input, 0);
            assert_eq!(fault.get(), Some(expected));
            assert_eq!(capture.meters.overruns.load(Ordering::Relaxed), 1);
            fault.set(CallbackFault::PlaybackUnderrun);
            assert_eq!(fault.get(), Some(expected));
        }
    }

    fn playback(total: usize) -> (PlaybackCallback, rtrb::Producer<Frame>) {
        let (tx, rx) = rtrb::RingBuffer::new(256);
        (
            PlaybackCallback {
                clock: CallbackClock::new(Rate::Hz48000),
                ring: rx,
                fault: Arc::new(FaultLatch::default()),
                played: Arc::new(AtomicU64::new(0)),
                total,
                pair: crate::domain::StereoPair::default(),
                channels: 2,
                cancel: crate::audio::Cancellation::new(),
                first_timestamp: Arc::new(AtomicU64::new(0)),
                last_timestamp: Arc::new(AtomicU64::new(0)),
                deadline: None,
                meters: Arc::new(crate::audio::Meters::default()),
            },
            tx,
        )
    }

    #[test]
    fn playback_underrun_silences_output_and_counts_the_actual_fault() {
        let (mut playback, _tx) = playback(128);
        let mut output = [1.0; 256];
        playback.process(&mut output, 0);
        assert!(output.iter().all(|s| *s == 0.0));
        assert_eq!(playback.fault.get(), Some(CallbackFault::PlaybackUnderrun));
        assert_eq!(playback.meters.underruns.load(Ordering::Relaxed), 1);
        assert_eq!(playback.played.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn timing_covers_emitted_samples_without_counting_post_probe_silence() {
        let (mut playback, mut tx) = playback(3);
        for _ in 0..3 {
            tx.push([0.1, 0.0]).unwrap();
        }
        let mut output = [1.0; 8];
        playback.process(&mut output, 1_000);
        assert_eq!(&output[6..], &[0.0, 0.0]);
        let last = 1_000 + 2 * 1_000_000_000 / 48_000;
        assert_eq!(playback.first_timestamp.load(Ordering::Relaxed), 1_000);
        assert_eq!(playback.last_timestamp.load(Ordering::Relaxed), last);
        playback.process(&mut output, 2_000_000);
        assert!(output.iter().all(|s| *s == 0.0));
        assert_eq!(playback.last_timestamp.load(Ordering::Relaxed), last);
        assert_eq!(playback.meters.output_frames.load(Ordering::Relaxed), 3);
        assert_eq!(playback.fault.get(), None);

        let (tx, mut rx) = rtrb::RingBuffer::new(3);
        let mut capture =
            CaptureCallback::new(tx, Arc::new(FaultLatch::default()), Rate::Hz48000, 0, 1);
        capture.process(&[0.1; 3], 1_000);
        let mut recorded = CaptureBuffer::new(3);
        recorded.drain(&mut rx).unwrap();
        assert_eq!(recorded.first, Some(1_000));
        assert_eq!(recorded.last, Some(last));
    }

    #[test]
    fn a_capture_fault_immediately_silences_pending_output() {
        let (mut playback, mut tx) = playback(128);
        for _ in 0..128 {
            tx.push([0.1, 0.0]).unwrap();
        }
        playback.fault.set(CallbackFault::Clipping);
        let mut output = [1.0; 256];
        playback.process(&mut output, 0);
        assert!(output.iter().all(|s| *s == 0.0));
        assert_eq!(playback.played.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn cancelled_and_expired_probes_emit_no_signal() {
        for expired in [false, true] {
            let (mut playback, mut tx) = playback(128);
            for _ in 0..128 {
                tx.push([0.1, 0.0]).unwrap();
            }
            if expired {
                playback.deadline = Some(Instant::now());
            } else {
                playback.cancel.cancel();
            }
            let mut output = [1.0; 256];
            playback.process(&mut output, 0);
            assert!(output.iter().all(|s| *s == 0.0));
            assert_eq!(playback.played.load(Ordering::Relaxed), 0);
            assert_eq!(
                playback.fault.get(),
                expired.then_some(CallbackFault::Deadline)
            );
        }
    }

    #[test]
    fn backend_failures_keep_direction_kind_and_xrun_counts() {
        use cpal::ErrorKind::*;
        for direction in [Direction::Input, Direction::Output] {
            for kind in [
                DeviceBusy,
                DeviceChanged,
                DeviceNotAvailable,
                HostUnavailable,
                InvalidInput,
                PermissionDenied,
                RealtimeDenied,
                ResourceExhausted,
                StreamInvalidated,
                UnsupportedConfig,
                UnsupportedOperation,
                Xrun,
                BackendError,
                Other,
            ] {
                let fault = FaultLatch::default();
                let meters = crate::audio::Meters::default();
                fault.backend(direction, kind, &meters);
                assert_eq!(
                    fault.get(),
                    Some(CallbackFault::Backend { direction, kind })
                );
                assert_eq!(
                    meters.backend_xruns.load(Ordering::Relaxed),
                    u64::from(kind == Xrun)
                );
                let message = fault.check().unwrap_err().to_string();
                assert!(message.contains(&direction.to_string()));
                assert!(message.contains(&kind.to_string()));
            }
        }
    }

    #[test]
    fn capture_buffer_rejects_an_actual_ordinal_gap() {
        let (mut tx, mut rx) = rtrb::RingBuffer::new(2);
        tx.push(InputFrame {
            sample: 0.1,
            ordinal: 0,
            timestamp_ns: 0,
        })
        .unwrap();
        tx.push(InputFrame {
            sample: 0.1,
            ordinal: 2,
            timestamp_ns: 1,
        })
        .unwrap();
        assert!(CaptureBuffer::new(8).drain(&mut rx).is_err());
    }
}
