use crate::{
    correction::Correction,
    domain::*,
    dsp::{Engine, Frame, Processor},
    error::{Result, invalid},
    platform::{DefaultOutputLease, DeviceMonitor},
    storage::Store,
};
use cpal::{
    Device, SampleFormat, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const CAPTURE_CAPACITY: usize = 8192;
const PLAYBACK_CAPACITY: usize = 4096;
const BLOCK: usize = 128;
const QUEUE_TARGET: usize = 512;

#[derive(Default)]
struct ClockServo {
    correction: f64,
    integral: f64,
}
impl ClockServo {
    fn observe(&mut self, capture_frames: usize) -> f64 {
        let error = capture_frames as f64 - QUEUE_TARGET as f64;
        self.integral = (self.integral - error * 2e-9).clamp(-0.0015, 0.0015);
        let desired = (-error * 4e-6 + self.integral).clamp(-0.002, 0.002);
        self.correction += (desired - self.correction) * 0.002;
        1.0 + self.correction
    }
}

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub uid: DeviceUid,
    pub name: String,
    pub input_channels: u16,
    pub output_channels: u16,
    pub virtual_device: bool,
    pub rate: Option<Rate>,
    pub input_rate: Option<Rate>,
    pub output_rate: Option<Rate>,
}
pub fn devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    host.devices()?
        .map(|device| {
            let description = device.description()?;
            let input = device.default_input_config().ok();
            let output = device.default_output_config().ok();
            let uid = DeviceUid::new(device.id()?);
            #[cfg(target_os = "macos")]
            let virtual_device = crate::platform::is_virtual_device(&uid)?;
            #[cfg(not(target_os = "macos"))]
            let virtual_device = matches!(description.device_type(), cpal::DeviceType::Virtual);
            Ok(DeviceInfo {
                uid,
                name: description.name().to_owned(),
                input_channels: input.as_ref().map_or(0, |c| c.channels()),
                output_channels: output.as_ref().map_or(0, |c| c.channels()),
                virtual_device,
                input_rate: input
                    .as_ref()
                    .and_then(|c| Rate::try_from(c.sample_rate()).ok()),
                output_rate: output
                    .as_ref()
                    .and_then(|c| Rate::try_from(c.sample_rate()).ok()),
                rate: output
                    .or(input)
                    .and_then(|c| Rate::try_from(c.sample_rate()).ok()),
            })
        })
        .collect()
}

pub fn default_route(devices: &[DeviceInfo]) -> Option<Route> {
    let capture = devices
        .iter()
        .find(|d| d.name.contains("BlackHole 2ch") && d.input_channels >= 2)?;
    let playback = devices
        .iter()
        .find(|d| d.name.contains("US-4x4HR") && d.output_channels >= 2)?;
    let endpoint = |device: &DeviceInfo| Endpoint {
        uid: device.uid.clone(),
        label: device.name.clone(),
        channels: StereoPair::default(),
    };
    Some(Route {
        capture: endpoint(capture),
        playback: endpoint(playback),
    })
}

#[derive(Default)]
pub struct Meters {
    pub spectrum: crate::spectrum::Capture,
    input: AtomicU32,
    output: AtomicU32,
    pub overloads: AtomicU64,
    pub underruns: AtomicU64,
    pub startup_silence: AtomicU64,
    pub overruns: AtomicU64,
    pub capture_queue: AtomicU32,
    pub playback_queue: AtomicU32,
    pub drift_ppm: AtomicU32,
    pub output_frames: AtomicU64,
    pub backend_xruns: AtomicU64,
}
impl Meters {
    pub(crate) fn record_input(&self, value: f32) {
        peak(&self.input, value);
    }
    pub(crate) fn record_output(&self, value: f32) {
        peak(&self.output, value);
        if value >= 1.0 {
            self.overloads.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn peaks(&self) -> (f32, f32) {
        (
            f32::from_bits(self.input.swap(0, Ordering::Relaxed)),
            f32::from_bits(self.output.swap(0, Ordering::Relaxed)),
        )
    }
    pub fn drift(&self) -> f32 {
        f32::from_bits(self.drift_ppm.load(Ordering::Relaxed))
    }
}
fn stream_error_callback(
    fault: Arc<AtomicBool>,
    meters: Arc<Meters>,
) -> impl FnMut(cpal::Error) + Send + 'static {
    move |error| {
        if error.kind() == cpal::ErrorKind::Xrun {
            meters.backend_xruns.fetch_add(1, Ordering::Relaxed);
        } else {
            fault.store(true, Ordering::Relaxed);
        }
    }
}
fn peak(meter: &AtomicU32, value: f32) {
    meter.fetch_max(value.abs().to_bits(), Ordering::Relaxed);
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SystemOutput {
    RouteThroughCapture,
    Unchanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeasurementId(u64);
pub enum Receipt {
    Submitted,
    Measurement(MeasurementId),
}

/// A cancellation lease. An old task can cancel only its own generation.
#[derive(Clone)]
pub struct Cancellation {
    generation: Arc<AtomicU64>,
    admitted: u64,
    device_io: Arc<AtomicU64>,
}
impl Default for Cancellation {
    fn default() -> Self {
        Self::new()
    }
}
impl Cancellation {
    pub fn new() -> Self {
        Self {
            generation: Arc::new(AtomicU64::new(0)),
            admitted: 0,
            device_io: Arc::new(AtomicU64::new(0)),
        }
    }
    fn issue(&self) -> Result<Self> {
        let mut observed = self.generation.load(Ordering::SeqCst);
        loop {
            if observed & 1 != 0 {
                return Err(invalid(
                    "Measurement cannot start while the system is sleeping",
                ));
            }
            let admitted = observed + 2;
            match self.generation.compare_exchange(
                observed,
                admitted,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    return Ok(Self {
                        generation: self.generation.clone(),
                        admitted,
                        device_io: self.device_io.clone(),
                    });
                }
                Err(current) => observed = current,
            }
        }
    }
    pub(crate) fn prepare_for_sleep(&self) {
        self.generation.fetch_or(1, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_millis(500);
        while self.device_io.load(Ordering::SeqCst) != 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
    }
    pub(crate) fn wake(&self) {
        let mut observed = self.generation.load(Ordering::SeqCst);
        while observed & 1 != 0 {
            match self.generation.compare_exchange(
                observed,
                observed + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return,
                Err(current) => observed = current,
            }
        }
    }
    pub(crate) fn device_io(&self) -> DeviceIoLease {
        self.device_io.fetch_add(1, Ordering::SeqCst);
        DeviceIoLease(self.device_io.clone())
    }
    pub fn id(&self) -> MeasurementId {
        MeasurementId(self.admitted)
    }
    pub fn is_cancelled(&self) -> bool {
        self.generation.load(Ordering::SeqCst) != self.admitted
    }
    pub fn cancel(&self) {
        let _ = self.generation.compare_exchange(
            self.admitted,
            self.admitted + 2,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }
    pub(crate) fn cancel_all(&self) {
        self.generation.fetch_add(2, Ordering::SeqCst);
    }
}
pub(crate) struct DeviceIoLease(Arc<AtomicU64>);
impl Drop for DeviceIoLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
struct Envelope {
    command: Command,
    measurement: Option<Cancellation>,
}

pub enum Command {
    Start(Route, SystemOutput),
    Stop,
    Retry,
    Controls(Controls),
    Activate(Box<Engine>),
    Measure(Box<crate::measurement::workflow::Plan>),
    CheckLevels(
        Box<crate::measurement::SourcePlan>,
        crate::measurement::Speaker,
    ),
    MeasurementControl {
        run: MeasurementId,
        control: crate::measurement::workflow::Control,
    },
    CancelMeasurement,
    Shutdown,
}
#[derive(Debug)]
pub enum Event {
    Measurement(MeasurementId, crate::measurement::workflow::Progress),
    MeasurementMuted(MeasurementId),
    Levels(MeasurementId, crate::measurement::levels::Report),
    Spectrum(crate::spectrum::Stream),
    State(Lifecycle),
    Activated(Arc<Correction>),
    Message(String),
    ActivationRejected(Arc<Correction>, String),
}
enum WorkerCommand {
    Controls(Controls),
    Activate(Box<Engine>),
}

pub struct Runtime {
    commands: SyncSender<Envelope>,
    cancellation: Cancellation,
    _power: Option<crate::platform::SleepMonitor>,
    pub events: Receiver<Event>,
    pub meters: Arc<Meters>,
    owner: Option<JoinHandle<()>>,
}
impl Runtime {
    pub fn new(correction: Arc<Correction>, controls: Controls, store: Store) -> Result<Self> {
        Self::with_backend(correction, controls, store, Backend::Hardware)
    }
    pub fn with_backend(
        correction: Arc<Correction>,
        controls: Controls,
        store: Store,
        backend: Backend,
    ) -> Result<Self> {
        if let Backend::Simulated(scenario) = &backend {
            scenario.validate()?;
            store.write("simulation.json", &serde_json::to_vec_pretty(scenario)?)?;
        }
        let cancellation = Cancellation::new();
        let power = if matches!(backend, Backend::Hardware) {
            Some(crate::platform::SleepMonitor::new(cancellation.clone())?)
        } else {
            None
        };
        let (commands, rx) = mpsc::sync_channel(16);
        let (tx, events) = mpsc::channel();
        let meters = Arc::new(Meters::default());
        let meter_copy = meters.clone();
        let owner = thread::Builder::new()
            .name("audio-owner".into())
            .spawn(move || owner(rx, tx, meter_copy, correction, controls, store, backend))?;
        Ok(Self {
            commands,
            cancellation,
            _power: power,
            events,
            meters,
            owner: Some(owner),
        })
    }
    /// Retain a prepared engine if the owner cannot accept it, so the UI
    /// can keep the candidate available for another explicit replacement.
    pub fn activate(&self, engine: Box<Engine>) -> std::result::Result<(), Box<Engine>> {
        match self.commands.try_send(Envelope {
            command: Command::Activate(engine),
            measurement: None,
        }) {
            Ok(()) => Ok(()),
            Err(
                mpsc::TrySendError::Full(Envelope {
                    command: Command::Activate(engine),
                    ..
                })
                | mpsc::TrySendError::Disconnected(Envelope {
                    command: Command::Activate(engine),
                    ..
                }),
            ) => Err(engine),
            Err(_) => unreachable!("only an activation command was submitted"),
        }
    }
    pub fn send(&self, command: Command) -> Result<Receipt> {
        let measurement = if matches!(command, Command::Measure(_) | Command::CheckLevels(..)) {
            Some(self.cancellation.issue()?)
        } else {
            None
        };
        if matches!(
            command,
            Command::Stop | Command::CancelMeasurement | Command::Shutdown
        ) {
            self.cancellation.cancel_all();
        }
        let receipt = measurement
            .as_ref()
            .map_or(Receipt::Submitted, |token| Receipt::Measurement(token.id()));
        self.commands
            .try_send(Envelope {
                command,
                measurement,
            })
            .map_err(|e| invalid(format!("Audio owner is busy or unavailable: {e}")))
            .map(|()| receipt)
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.cancellation.cancel_all();
        let _ = self.commands.send(Envelope {
            command: Command::Shutdown,
            measurement: None,
        });
        if let Some(owner) = self.owner.take() {
            let _ = owner.join();
        }
    }
}

struct Running {
    output: Option<cpal::Stream>,
    input: Option<cpal::Stream>,
    worker: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    fault: Arc<AtomicBool>,
    commands: SyncSender<WorkerCommand>,
    events: Receiver<Event>,
    started: Instant,
}
impl Running {
    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        drop(self.output.take());
        drop(self.input.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Clone)]
pub enum Backend {
    Hardware,
    Simulated(crate::measurement::simulation::Scenario),
}
impl Backend {
    pub fn devices(&self) -> Result<Vec<DeviceInfo>> {
        match self {
            Self::Hardware => devices(),
            Self::Simulated(scenario) => Ok(crate::measurement::simulation::devices(scenario)),
        }
    }
}

enum ExperimentRequest {
    Measure(Box<crate::measurement::workflow::Plan>),
    Levels(
        Box<crate::measurement::SourcePlan>,
        crate::measurement::Speaker,
    ),
}
struct Experiment {
    cancel: Cancellation,
    controls: SyncSender<crate::measurement::workflow::Control>,
    worker: JoinHandle<()>,
}
enum Mode {
    Listening,
    Measurement(Experiment),
    Muted(MeasurementId),
}

struct Owner {
    events: mpsc::Sender<Event>,
    meters: Arc<Meters>,
    correction: Arc<Correction>,
    controls: Controls,
    running: Option<Running>,
    lease: Option<DefaultOutputLease>,
    route: Option<(Route, SystemOutput)>,
    retry: Option<(Instant, RecoveryAttempt)>,
    recovery_attempt: Option<RecoveryAttempt>,
    device_monitor: Option<DeviceMonitor>,
    backend: Backend,
    mode: Mode,
    store: Store,
}

fn owner(
    rx: Receiver<Envelope>,
    events: mpsc::Sender<Event>,
    meters: Arc<Meters>,
    correction: Arc<Correction>,
    controls: Controls,
    store: Store,
    backend: Backend,
) {
    let mut owner = match Owner::new(events.clone(), meters, correction, controls, store, backend) {
        Ok(owner) => owner,
        Err(error) => {
            let _ = events.send(Event::State(Lifecycle::Failed(error.to_string())));
            return;
        }
    };
    owner.run(rx);
}

impl Owner {
    fn new(
        events: mpsc::Sender<Event>,
        meters: Arc<Meters>,
        correction: Arc<Correction>,
        controls: Controls,
        store: Store,
        backend: Backend,
    ) -> Result<Self> {
        Ok(Self {
            events,
            meters,
            correction,
            controls,
            running: None,
            lease: None,
            route: None,
            retry: None,
            recovery_attempt: None,
            device_monitor: if matches!(backend, Backend::Hardware) {
                Some(DeviceMonitor::new()?)
            } else {
                None
            },
            backend,
            mode: Mode::Listening,
            store,
        })
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }
    fn state(&self, state: Lifecycle) {
        self.emit(Event::State(state));
    }
    fn receipt(&mut self, event: Event) {
        if let Event::Activated(active) = &event {
            self.correction = active.clone();
        }
        self.emit(event);
    }
    fn stop_stream(&mut self) {
        if let Some(mut stream) = self.running.take() {
            stream.shutdown();
            for event in stream.events.try_iter() {
                self.receipt(event);
            }
        }
    }
    fn release(&mut self) {
        self.stop_stream();
        if let Some(mut lease) = self.lease.take()
            && let Err(error) = lease.restore()
        {
            self.emit(Event::Message(format!(
                "Could not restore system output: {error}. Choose an available hardware output in macOS Sound settings."
            )));
        }
    }
    fn start_selected(&mut self, attempt: Option<RecoveryAttempt>) {
        self.state(Lifecycle::Starting);
        let result = self
            .route
            .as_ref()
            .ok_or_else(|| invalid("Choose capture and playback devices"))
            .and_then(|(route, output)| {
                if let Some(monitor)=&mut self.device_monitor{monitor.watch(&[&route.capture.uid,&route.playback.uid])?;}
                let started = match &self.backend {
                    Backend::Simulated(scenario) => start_emulated(route, self.correction.clone(), self.controls, self.meters.clone(), &self.store, scenario.rate),
                    Backend::Hardware => start(
                    route,
                    self.correction.clone(),
                    self.controls,
                    self.meters.clone(),
                    &self.store,
                ),
                }?;
                if matches!(self.backend, Backend::Hardware) && *output == SystemOutput::RouteThroughCapture && self.lease.is_none() {
                    let info = devices()?
                        .into_iter()
                        .find(|device| device.uid == route.capture.uid)
                        .ok_or_else(|| invalid("Capture device is unavailable"))?;
                    if !info.virtual_device {
                        return Err(invalid("Choose a virtual capture device such as BlackHole to route Mac audio through the app"));
                    }
                    self.lease = DefaultOutputLease::acquire(&route.capture.uid)?;
                }
                Ok(started)
            });
        match result {
            Ok((stream, prepared, latency)) => {
                self.correction = prepared;
                self.running = Some(stream);
                self.emit(Event::Activated(self.correction.clone()));
                self.state(Lifecycle::Running {
                    rate: self.correction.rate,
                    estimated_latency_ms: latency,
                });
            }
            Err(error) if attempt.and_then(RecoveryAttempt::next).is_some() => {
                let next = attempt
                    .and_then(RecoveryAttempt::next)
                    .expect("remaining recovery attempt");
                self.recovery_attempt = Some(next);
                self.retry = Some((Instant::now() + Duration::from_secs(next.number()), next));
                self.state(Lifecycle::Recovering {
                    attempt: next,
                    reason: error.to_string(),
                });
            }
            Err(error) => {
                self.release();
                self.state(Lifecycle::Failed(error.to_string()));
            }
        }
    }
    fn cancel_experiment(&mut self) {
        let mode = std::mem::replace(&mut self.mode, Mode::Listening);
        let id = match mode {
            Mode::Measurement(run) => {
                let id = run.cancel.id();
                run.cancel.cancel();
                let _ = run.worker.join();
                Some(id)
            }
            Mode::Muted(id) => Some(id),
            Mode::Listening => None,
        };
        self.retry = None;
        if let Some(id) = id {
            self.mode = Mode::Muted(id);
            self.state(Lifecycle::Stopped);
            self.emit(Event::MeasurementMuted(id));
        }
    }
    fn submit_experiment(&mut self, request: ExperimentRequest, cancel: Cancellation) {
        let id = cancel.id();
        if let Err(error) = self.measure(request, cancel) {
            if matches!(self.mode, Mode::Muted(_)) {
                self.state(Lifecycle::Stopped);
            }
            self.emit(Event::Measurement(
                id,
                crate::measurement::workflow::Progress::Failed(error.to_string()),
            ));
        }
    }
    fn mute_for_measurement(&mut self, id: MeasurementId) {
        if matches!(self.mode, Mode::Measurement(_)) {
            self.cancel_experiment();
        }
        self.retry = None;
        self.recovery_attempt = None;
        if let Some(stream) = &self.running {
            let mut fading = self.controls;
            fading.volume = VolumeDb::try_from(-80.0).expect("fixed mute fade level");
            let _ = stream.commands.send(WorkerCommand::Controls(fading));
            thread::sleep(Duration::from_millis(40));
        }
        self.stop_stream();
        self.mode = Mode::Muted(id);
        self.emit(Event::MeasurementMuted(id));
    }
    fn measure(&mut self, request: ExperimentRequest, cancel: Cancellation) -> Result<()> {
        if cancel.is_cancelled() {
            self.mute_for_measurement(cancel.id());
            return Err(crate::measurement::workflow::Fault::Stopped.into());
        }
        let source = match &request {
            ExperimentRequest::Measure(plan) => {
                plan.validate()?;
                &plan.source
            }
            ExperimentRequest::Levels(source, _) => {
                source.validate()?;
                source.as_ref()
            }
        };
        if let Mode::Measurement(run) = &self.mode {
            if !run.cancel.is_cancelled() {
                return Err(invalid("A measurement already owns the output"));
            }
            self.cancel_experiment();
        }
        if self.running.is_none() {
            self.route = Some((source.route.clone(), SystemOutput::RouteThroughCapture));
        }
        let (route, output) = self.route.clone().expect("selected measurement route");
        if route.playback.uid != source.route.playback.uid
            || route.playback.channels != source.route.playback.channels
        {
            return Err(invalid(
                "Measurement must use the owned listening playback route",
            ));
        }
        if matches!(self.backend, Backend::Hardware) && output != SystemOutput::RouteThroughCapture
        {
            return Err(invalid(
                "Measurement requires the virtual-capture system-output lease; pause other music sources",
            ));
        }
        if matches!(self.backend, Backend::Hardware)
            && !crate::platform::is_virtual_device(&route.capture.uid)?
        {
            return Err(invalid(
                "Select a virtual listening capture device so system audio can stay silent during measurement",
            ));
        }
        self.mute_for_measurement(cancel.id());
        if matches!(self.backend, Backend::Hardware) {
            if self.lease.as_ref().is_some_and(|lease| !lease.owns()) {
                self.lease.take();
            }
            if !crate::platform::output_is(&route.capture.uid)? {
                if let Some(mut lease) = self.lease.take() {
                    lease.restore()?;
                }
                self.lease = DefaultOutputLease::acquire(&route.capture.uid)?;
            }
        }
        let mut provider: Box<dyn crate::measurement::io::Provider> = match &self.backend {
            Backend::Hardware => Box::new(crate::measurement::io::Hardware::new(source.clone())?),
            Backend::Simulated(scenario) => {
                let simulated = crate::measurement::simulation::route();
                let microphone = crate::measurement::simulation::microphone();
                if source.route.capture.uid != simulated.capture.uid
                    || source.route.playback.uid != simulated.playback.uid
                    || source.route.playback.channels != simulated.playback.channels
                    || source.microphone.uid != microphone.uid
                    || source.microphone.channels != microphone.channels
                {
                    return Err(invalid("Simulated runs require emulated endpoints"));
                }
                Box::new(crate::measurement::simulation::Simulated::new(
                    scenario.clone(),
                )?)
            }
        };
        if let Some(monitor) = &mut self.device_monitor {
            monitor.watch(&[&source.route.playback.uid, &source.microphone.uid])?;
            let _ = monitor.changed();
        }
        provider.set_meters(self.meters.clone());
        let rate = source.rate;
        let id = cancel.id();
        let (controls, receiver) = mpsc::sync_channel(8);
        let events = self.events.clone();
        let store = self.store.clone();
        let cancellation = cancel.clone();
        let worker = thread::Builder::new()
            .name("room-measurement".into())
            .spawn(move || {
                let result = match request {
                    ExperimentRequest::Measure(plan) => {
                        let progress_events = events.clone();
                        let run = crate::measurement::workflow::Run {
                            plan: *plan,
                            store: &store,
                            cancel: cancellation,
                            controls: receiver,
                            provider,
                            progress: Box::new(move |progress| {
                                let _ = progress_events.send(Event::Measurement(id, progress));
                            }),
                        };
                        run.execute().map(|_| ())
                    }
                    ExperimentRequest::Levels(source, speaker) => {
                        let _ = events.send(Event::Measurement(
                            id,
                            crate::measurement::workflow::Progress::Levels { speaker },
                        ));
                        let mut provider = provider;
                        crate::measurement::levels::check(
                            &source,
                            speaker,
                            provider.as_mut(),
                            &cancellation,
                            &store,
                        )
                        .map(|report| {
                            let _ = events.send(Event::Levels(id, report));
                        })
                    }
                };
                if let Err(error) = result {
                    let _ = events.send(Event::Measurement(
                        id,
                        crate::measurement::workflow::Progress::Failed(error.to_string()),
                    ));
                }
            })?;
        self.mode = Mode::Measurement(Experiment {
            cancel,
            controls,
            worker,
        });
        self.state(Lifecycle::Measuring { rate });
        Ok(())
    }
    fn poll_stream(&mut self) {
        while let Some(event) = self
            .running
            .as_ref()
            .and_then(|stream| stream.events.try_recv().ok())
        {
            self.receipt(event);
        }
        let Some(stream) = &self.running else {
            return;
        };
        if stream.fault.swap(false, Ordering::Relaxed)
            || stream.worker.as_ref().is_some_and(JoinHandle::is_finished)
        {
            self.stop_stream();
            let next = self
                .recovery_attempt
                .map_or(Some(RecoveryAttempt::First), RecoveryAttempt::next);
            if let Some(attempt) = next {
                self.recovery_attempt = Some(attempt);
                self.state(Lifecycle::Recovering {
                    attempt,
                    reason:
                        "Core Audio invalidated the stream. Waiting for the selected endpoints."
                            .into(),
                });
                self.retry = Some((
                    Instant::now() + Duration::from_secs(attempt.number()),
                    attempt,
                ));
            } else {
                self.release();
                self.state(Lifecycle::Failed(
                    "Repeated stream failure. Check devices and Audio MIDI Setup, then Retry."
                        .into(),
                ));
            }
        } else if stream.started.elapsed() > Duration::from_secs(10) {
            self.recovery_attempt = None;
        }
    }
    fn run(&mut self, commands: Receiver<Envelope>) {
        loop {
            if matches!(&self.mode, Mode::Measurement(run) if run.worker.is_finished()) {
                self.cancel_experiment();
            }
            self.poll_stream();
            let measuring = matches!(self.mode, Mode::Measurement(_));
            let device_change = self.device_monitor.as_mut().map_or(Ok(false), |monitor| {
                if measuring {
                    monitor.watched_changed()
                } else {
                    Ok(monitor.changed())
                }
            });
            let route_lost = self.lease.as_ref().is_some_and(|lease| !lease.owns());
            if (device_change.as_ref().map_or(true, |changed| *changed) || route_lost) && measuring
            {
                let reason = match &device_change {
                    Err(error) => format!("Cannot confirm measurement endpoints: {error}. Output is muted."),
                    Ok(_) if route_lost => "System output changed during measurement. Output is muted; resume listening explicitly.".into(),
                    Ok(_) => "The measurement microphone, speakers or their settings changed. Output is muted.".into(),
                };
                self.cancel_experiment();
                let Mode::Muted(id) = self.mode else {
                    unreachable!("measurement cancellation retains its identity")
                };
                self.emit(Event::Measurement(
                    id,
                    crate::measurement::workflow::Progress::Failed(reason),
                ));
            }
            if device_change.is_ok_and(|changed| changed)
                && self.running.is_none()
                && self.retry.is_none()
                && self.route.is_some()
                && matches!(self.mode, Mode::Listening)
            {
                self.recovery_attempt = Some(RecoveryAttempt::First);
                self.retry = Some((
                    Instant::now() + Duration::from_millis(500),
                    RecoveryAttempt::First,
                ));
            }
            let automatic_retry = self
                .retry
                .filter(|(deadline, _)| Instant::now() >= *deadline)
                .map(|(_, attempt)| attempt);
            // User commands, especially Stop, take priority over a retry deadline.
            let command =
                match commands.recv_timeout(Duration::from_millis(if automatic_retry.is_some() {
                    0
                } else {
                    20
                })) {
                    Ok(command) => command,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if let Some(attempt) = automatic_retry {
                            self.retry = None;
                            self.start_selected(Some(attempt));
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
            let Envelope {
                command,
                measurement,
            } = command;
            match command {
                Command::Shutdown => {
                    self.cancel_experiment();
                    break;
                }
                Command::Measure(plan) => self.submit_experiment(
                    ExperimentRequest::Measure(plan),
                    measurement.expect("measurement command lease"),
                ),
                Command::CheckLevels(source, speaker) => self.submit_experiment(
                    ExperimentRequest::Levels(source, speaker),
                    measurement.expect("level command lease"),
                ),
                Command::MeasurementControl { run: id, control } => {
                    if let Mode::Measurement(run) = &self.mode
                        && run.cancel.id() == id
                        && let Err(error) = run.controls.try_send(control)
                    {
                        self.emit(Event::Message(format!(
                            "Measurement command unavailable: {error}"
                        )));
                    }
                }
                Command::CancelMeasurement => self.cancel_experiment(),
                Command::Stop if !matches!(self.mode, Mode::Listening) => self.cancel_experiment(),
                Command::Stop => {
                    self.retry = None;
                    self.recovery_attempt = None;
                    self.release();
                    self.route = None;
                    self.state(Lifecycle::Stopped);
                }
                Command::Start(_, _) | Command::Retry
                    if matches!(self.mode, Mode::Measurement(_)) =>
                {
                    self.emit(Event::Message(
                        "Stop the active measurement before starting listening".into(),
                    ))
                }
                Command::Start(route, output) => {
                    self.mode = Mode::Listening;
                    self.retry = None;
                    self.recovery_attempt = None;
                    self.release();
                    self.route = Some((route, output));
                    self.start_selected(None);
                }
                Command::Retry => {
                    self.mode = Mode::Listening;
                    if self.lease.as_ref().is_some_and(|lease| !lease.owns()) {
                        self.lease.take();
                    }
                    self.retry = None;
                    self.recovery_attempt = None;
                    self.stop_stream();
                    self.start_selected(None);
                }
                Command::Controls(controls) => {
                    self.controls = controls;
                    if let Some(stream) = &self.running {
                        let _ = stream.commands.send(WorkerCommand::Controls(controls));
                    }
                }
                Command::Activate(engine) if !matches!(self.mode, Mode::Listening) => {
                    self.emit(Event::ActivationRejected(
                        engine.correction.clone(),
                        "Resume listening before auditioning a correction".into(),
                    ))
                }
                Command::Activate(engine) => {
                    if let Some(stream) = &self.running {
                        let correction = engine.correction.clone();
                        if stream
                            .commands
                            .send(WorkerCommand::Activate(engine))
                            .is_err()
                        {
                            self.emit(Event::ActivationRejected(
                                correction,
                                "Audio worker stopped before accepting the correction".into(),
                            ));
                        }
                    } else {
                        self.correction = engine.correction.clone();
                        self.emit(Event::Activated(self.correction.clone()));
                    }
                }
            }
        }
        self.cancel_experiment();
        self.release();
    }
}

pub(crate) fn config(device: &Device, input: bool) -> Result<StreamConfig> {
    let config = if input {
        device.default_input_config()?
    } else {
        device.default_output_config()?
    };
    Rate::try_from(config.sample_rate())?;
    if config.sample_format() != SampleFormat::F32 {
        return Err(invalid(
            "This Core Audio route must expose floating-point client samples",
        ));
    }
    Ok(config.config())
}

fn clock_resampler(ratio: f64) -> Result<SincFixedIn<f32>> {
    SincFixedIn::<f32>::new(
        ratio,
        1.005,
        SincInterpolationParameters {
            sinc_len: 128,
            f_cutoff: 0.94,
            interpolation: SincInterpolationType::Cubic,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        },
        BLOCK,
        2,
    )
    .map_err(|e| invalid(e.to_string()))
}

fn start(
    route: &Route,
    correction: Arc<Correction>,
    controls: Controls,
    meters: Arc<Meters>,
    store: &Store,
) -> Result<(Running, Arc<Correction>, f64)> {
    route.validate()?;
    if !matches!(route.capture.uid, DeviceUid::Hardware(_))
        || !matches!(route.playback.uid, DeviceUid::Hardware(_))
    {
        return Err(invalid(
            "Emulated routes require the isolated --simulate launch",
        ));
    }
    let host = cpal::default_host();
    let input = host.device_by_id(route.capture.uid.id()).ok_or_else(|| {
        invalid(format!(
            "Capture device '{}' is missing",
            route.capture.label
        ))
    })?;
    let output = host.device_by_id(route.playback.uid.id()).ok_or_else(|| {
        invalid(format!(
            "Playback device '{}' is missing",
            route.playback.label
        ))
    })?;
    let input_config = config(&input, true)?;
    let output_config = config(&output, false)?;
    route.capture.channels.validate(input_config.channels)?;
    route.playback.channels.validate(output_config.channels)?;
    let rate = Rate::try_from(output_config.sample_rate)?;
    let correction = Arc::new(correction.at_rate(rate, store)?);
    let engine = Box::new(Engine::new(correction.clone(), controls));
    let ratio = f64::from(output_config.sample_rate) / f64::from(input_config.sample_rate);
    let sampler = clock_resampler(ratio)?;
    let latency = 1000.0
        * ((QUEUE_TARGET + BLOCK) as f64 / input_config.sample_rate as f64
            + (QUEUE_TARGET
                + crate::dsp::PARTITION
                + sampler.output_delay()
                + correction.preparation_delay()) as f64
                / output_config.sample_rate as f64);
    let (mut capture_tx, capture_rx) = rtrb::RingBuffer::<Frame>::new(CAPTURE_CAPACITY);
    let (playback_tx, mut playback_rx) = rtrb::RingBuffer::<Frame>::new(PLAYBACK_CAPACITY);
    let stop = Arc::new(AtomicBool::new(false));
    let fault = Arc::new(AtomicBool::new(false));
    let input_fault = fault.clone();
    let capture_fault = fault.clone();
    let input_meters = meters.clone();
    let input_channels = usize::from(input_config.channels);
    let [left, right] = route.capture.channels.indices();
    let input_stream = input.build_input_stream(
        input_config,
        move |data: &[f32], _| {
            #[cfg(feature = "alloc-audit")]
            let _audit = crate::audit::enter();
            for frame in data.chunks_exact(input_channels) {
                let samples = [frame[left], frame[right]];
                if samples.iter().any(|value| !value.is_finite()) {
                    capture_fault.store(true, Ordering::Relaxed);
                    continue;
                }
                peak(&input_meters.input, samples[0].abs().max(samples[1].abs()));
                if capture_tx.push(samples).is_err() {
                    input_meters.overruns.fetch_add(1, Ordering::Relaxed);
                    capture_fault.store(true, Ordering::Relaxed);
                }
            }
        },
        stream_error_callback(input_fault, meters.clone()),
        None,
    )?;
    let output_meters = meters.clone();
    let output_fault = fault.clone();
    let output_channels = usize::from(output_config.channels);
    let [left, right] = route.playback.channels.indices();
    let mut ramp = 0.0_f32;
    enum PlaybackPhase {
        Priming,
        Streaming,
        Recovering,
    }
    let mut phase = PlaybackPhase::Priming;
    let output_stream = output.build_output_stream(
        output_config,
        move |data: &mut [f32], _| {
            #[cfg(feature = "alloc-audit")]
            let _audit = crate::audit::enter();
            for frame in data.chunks_exact_mut(output_channels) {
                frame.fill(0.0);
                match playback_rx.pop() {
                    Ok(mut samples) => {
                        phase = PlaybackPhase::Streaming;
                        ramp = (ramp + 1.0 / 512.0).min(1.0);
                        samples.iter_mut().for_each(|v| *v *= ramp);
                        frame[left] = samples[0];
                        frame[right] = samples[1];
                        let value = samples[0].abs().max(samples[1].abs());
                        peak(&output_meters.output, value);
                        if value > 1.0 {
                            output_meters.overloads.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(_) => {
                        ramp = 0.0;
                        match phase {
                            PlaybackPhase::Priming => {
                                output_meters
                                    .startup_silence
                                    .fetch_add(1, Ordering::Relaxed);
                            }
                            PlaybackPhase::Streaming | PlaybackPhase::Recovering => {
                                output_meters.underruns.fetch_add(1, Ordering::Relaxed);
                                phase = PlaybackPhase::Recovering;
                            }
                        }
                    }
                }
            }
            output_meters
                .output_frames
                .fetch_add((data.len() / output_channels) as u64, Ordering::Relaxed);
        },
        stream_error_callback(output_fault, meters.clone()),
        None,
    )?;
    let (commands, worker_rx) = mpsc::sync_channel(8);
    let (worker_events, events) = mpsc::channel();
    let worker_stop = stop.clone();
    let worker = thread::Builder::new()
        .name("stereo-dsp".into())
        .spawn(move || {
            process(
                capture_rx,
                playback_tx,
                sampler,
                engine,
                controls,
                worker_rx,
                worker_stop,
                meters,
                worker_events,
            )
        })?;
    let running = Running {
        output: Some(output_stream),
        input: Some(input_stream),
        worker: Some(worker),
        stop,
        fault,
        commands,
        events,
        started: Instant::now(),
    };
    running.input.as_ref().expect("built input").play()?;
    running.output.as_ref().expect("built output").play()?;
    Ok((running, correction, latency))
}

fn start_emulated(
    route: &Route,
    correction: Arc<Correction>,
    controls: Controls,
    meters: Arc<Meters>,
    store: &Store,
    rate: Rate,
) -> Result<(Running, Arc<Correction>, f64)> {
    let simulated = crate::measurement::simulation::route();
    if route.capture.uid != simulated.capture.uid
        || route.capture.channels != simulated.capture.channels
        || route.playback.uid != simulated.playback.uid
        || route.playback.channels != simulated.playback.channels
    {
        return Err(invalid("Emulated listening requires emulated endpoints"));
    }
    let correction = Arc::new(correction.at_rate(rate, store)?);
    let engine = Box::new(Engine::new(correction.clone(), controls));
    let sampler = clock_resampler(1.0)?;
    let (mut capture_tx, capture_rx) = rtrb::RingBuffer::new(CAPTURE_CAPACITY);
    let (playback_tx, mut playback_rx) = rtrb::RingBuffer::new(PLAYBACK_CAPACITY);
    let stop = Arc::new(AtomicBool::new(false));
    let fault = Arc::new(AtomicBool::new(false));
    let (commands, rx) = mpsc::sync_channel(8);
    let (tx, events) = mpsc::channel();
    let worker_stop = stop.clone();
    let worker = thread::Builder::new()
        .name("emulated-audio".into())
        .spawn(move || {
            let driver_stop = worker_stop.clone();
            let driver = thread::spawn(move || {
                let mut ordinal = 0u64;
                let mut deadline = Instant::now();
                while !driver_stop.load(Ordering::Relaxed) {
                    for _ in 0..128 {
                        let value = (std::f64::consts::TAU * 440.0 * ordinal as f64
                            / f64::from(rate.hz()))
                        .sin() as f32
                            * 0.05;
                        let _ = capture_tx.push([value, value]);
                        let _ = playback_rx.pop();
                        ordinal += 1;
                    }
                    // A delayed synthetic callback has no historical device buffers
                    // to recover. Keep subsequent callbacks paced instead of
                    // flooding the real listening queues with a catch-up burst.
                    deadline = (deadline + Duration::from_secs_f64(128.0 / f64::from(rate.hz())))
                        .max(Instant::now());
                    if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
                        thread::sleep(wait);
                    }
                }
            });
            process(
                capture_rx,
                playback_tx,
                sampler,
                engine,
                controls,
                rx,
                worker_stop.clone(),
                meters,
                tx,
            );
            worker_stop.store(true, Ordering::Relaxed);
            let _ = driver.join();
        })?;
    Ok((
        Running {
            output: None,
            input: None,
            worker: Some(worker),
            stop,
            fault,
            commands,
            events,
            started: Instant::now(),
        },
        correction,
        1000.0 * (QUEUE_TARGET + BLOCK + crate::dsp::PARTITION) as f64 / f64::from(rate.hz()),
    ))
}

#[allow(clippy::too_many_arguments)]
fn process(
    mut capture: rtrb::Consumer<Frame>,
    mut playback: rtrb::Producer<Frame>,
    mut sampler: SincFixedIn<f32>,
    engine: Box<Engine>,
    controls: Controls,
    commands: Receiver<WorkerCommand>,
    stop: Arc<AtomicBool>,
    meters: Arc<Meters>,
    events: mpsc::Sender<Event>,
) {
    let (mut spectrum, stream) = crate::spectrum::channel(engine.correction.rate);
    let _ = events.send(Event::Spectrum(stream));
    let mut processor = Processor::new(engine, controls);
    let mut input = sampler.input_buffer_allocate(true);
    let mut output = sampler.output_buffer_allocate(true);
    let mut started = false;
    let mut clock = ClockServo::default();
    let mut pending_engine = None;
    while !stop.load(Ordering::Relaxed) {
        while let Ok(command) = commands.try_recv() {
            match command {
                WorkerCommand::Controls(controls) => processor.controls(controls),
                WorkerCommand::Activate(engine) => {
                    if let Some(previous) = pending_engine.replace(engine) {
                        let _ = events.send(Event::ActivationRejected(
                            previous.correction.clone(),
                            "A newer playback selection superseded this replacement".into(),
                        ));
                    }
                }
            }
        }
        if !processor.transitioning()
            && let Some(engine) = pending_engine.take()
        {
            if engine.correction.rate != processor.active.correction.rate {
                let _ = events.send(Event::ActivationRejected(
                    engine.correction.clone(),
                    "Route rate changed while preparing. Prepare the correction again.".into(),
                ));
            } else {
                processor.replace(engine);
            }
        }
        let capture_frames = capture.slots();
        if capture_frames > CAPTURE_CAPACITY / 2 {
            // Restart through the existing owner after an outage, releasing
            // every stale queue, resampler history, and convolution tail.
            meters.overruns.fetch_add(1, Ordering::Relaxed);
            break;
        }
        let playback_frames = PLAYBACK_CAPACITY - playback.slots();
        meters
            .capture_queue
            .store(capture_frames as u32, Ordering::Relaxed);
        meters
            .playback_queue
            .store(playback_frames as u32, Ordering::Relaxed);
        if !started {
            started = capture_frames >= QUEUE_TARGET + BLOCK;
        }
        if !started || capture_frames < BLOCK || playback_frames >= QUEUE_TARGET {
            thread::sleep(Duration::from_micros(300));
            continue;
        }
        let ratio = clock.observe(capture_frames);
        meters
            .drift_ppm
            .store(((ratio - 1.0) as f32 * 1e6).to_bits(), Ordering::Relaxed);
        sampler
            .set_resample_ratio_relative(ratio, true)
            .expect("bounded clock correction is inside prepared ratio range");
        let (left, right) = input.split_at_mut(1);
        for (left, right) in left[0].iter_mut().zip(&mut right[0]).take(BLOCK) {
            let frame = capture
                .pop()
                .expect("capture occupancy checked before block");
            *left = frame[0];
            *right = frame[1];
        }
        let (_, count) = sampler
            .process_into_buffer(&input, &mut output, None)
            .expect("resampler buffers match its prepared format");
        let capture_spectrum = meters.spectrum.enabled();
        for (&left, &right) in output[0].iter().zip(&output[1]).take(count) {
            let (frame, adopted) = processor.tick([left, right]);
            if capture_spectrum {
                spectrum.push([left, right], frame);
            }
            playback
                .push(frame)
                .expect("bounded worker output fits the playback ring");
            if adopted {
                let _ = events.send(Event::Activated(processor.active.correction.clone()));
            }
        }
        if !capture_spectrum {
            spectrum.skip(count);
        }
    }
    for correction in processor
        .pending_correction()
        .cloned()
        .into_iter()
        .chain(pending_engine.map(|engine| engine.correction.clone()))
        .chain(commands.try_iter().filter_map(|command| match command {
            WorkerCommand::Activate(engine) => Some(engine.correction.clone()),
            WorkerCommand::Controls(_) => None,
        }))
    {
        let _ = events.send(Event::ActivationRejected(correction,
            "Listening stopped before this replacement completed; the confirmed correction is retained".into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurement_ownership_uses_physical_output_identity_instead_of_display_labels() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::at(directory.path()).unwrap();
        let (events, receiver) = mpsc::channel();
        let mut owner = Owner::new(
            events,
            Arc::new(Meters::default()),
            Arc::new(Correction::identity(Rate::Hz48000)),
            Controls::default(),
            store,
            Backend::Simulated(Default::default()),
        )
        .unwrap();
        let mut source = crate::measurement::simulation::plan(Rate::Hz48000);
        source.route.capture.label = "Renamed listening capture".into();
        source.route.playback.label = "Renamed speaker output".into();
        source.microphone.label = "Renamed microphone".into();
        owner
            .measure(
                ExperimentRequest::Levels(
                    Box::new(source.clone()),
                    crate::measurement::Speaker::Left,
                ),
                Cancellation::new().issue().unwrap(),
            )
            .unwrap();
        assert!(receiver.try_iter().any(|event| matches!(
            event,
            Event::State(Lifecycle::Measuring {
                rate: Rate::Hz48000
            })
        )));
        assert!(
            owner.running.is_none(),
            "measurement does not need a listening capture stream"
        );
        owner.cancel_experiment();
        assert!(
            receiver
                .try_iter()
                .any(|event| matches!(event, Event::State(Lifecycle::Stopped)))
        );
        owner.mode = Mode::Listening;
        owner.start_selected(None);
        source.route.playback.channels = StereoPair::try_from([2, 3]).unwrap();
        let result = owner.measure(
            ExperimentRequest::Levels(Box::new(source), crate::measurement::Speaker::Left),
            Cancellation::new().issue().unwrap(),
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("owned listening playback route")
        );
    }

    #[test]
    fn stopped_worker_rejects_queued_replacement_with_its_correction_identity() {
        let (_capture, input) = rtrb::RingBuffer::new(CAPTURE_CAPACITY);
        let (output, _playback) = rtrb::RingBuffer::new(PLAYBACK_CAPACITY);
        let (commands, receiver) = mpsc::sync_channel(8);
        let (events, receipts) = mpsc::channel();
        let mut correction = Correction::identity(Rate::Hz48000);
        correction.name = "Queued selection".into();
        let requested = Arc::new(correction);
        commands
            .send(WorkerCommand::Activate(Box::new(Engine::new(
                requested.clone(),
                Controls::default(),
            ))))
            .unwrap();
        process(
            input,
            output,
            clock_resampler(1.0).unwrap(),
            Box::new(Engine::new(
                Arc::new(Correction::identity(Rate::Hz48000)),
                Controls::default(),
            )),
            Controls::default(),
            receiver,
            Arc::new(AtomicBool::new(true)),
            Arc::new(Meters::default()),
            events,
        );
        assert!(receipts.try_iter().any(|event| matches!(event,
            Event::ActivationRejected(correction, _) if Arc::ptr_eq(&correction, &requested))));
    }

    #[test]
    fn cancellation_leases_reject_stale_starts_and_preserve_new_authority() {
        let source = Cancellation::new();
        let old = source.issue().unwrap();
        let current = source.issue().unwrap();
        assert!(old.is_cancelled());
        old.cancel();
        assert!(!current.is_cancelled());
        source.prepare_for_sleep();
        assert!(current.is_cancelled());
        assert!(source.issue().is_err());
        source.wake();
        let resumed = source.issue().unwrap();
        current.cancel();
        assert!(!resumed.is_cancelled());
    }
    #[test]
    fn stop_invalidates_queued_probes_even_when_the_command_queue_is_full() {
        let (commands, receiver) = mpsc::sync_channel(1);
        let (_events, events) = mpsc::channel();
        let runtime = Runtime {
            commands,
            cancellation: Cancellation::new(),
            _power: None,
            events,
            meters: Arc::new(Meters::default()),
            owner: None,
        };
        runtime
            .send(Command::CheckLevels(
                Box::new(crate::measurement::simulation::plan(Rate::Hz48000)),
                crate::measurement::Speaker::Left,
            ))
            .unwrap();
        assert!(runtime.send(Command::CancelMeasurement).is_err());
        let queued = receiver.recv().unwrap();
        assert!(queued.measurement.unwrap().is_cancelled());
    }

    #[test]
    fn dsp_worker_taps_input_and_processed_output_without_audio_devices() {
        let (mut capture, input) = rtrb::RingBuffer::new(CAPTURE_CAPACITY);
        let (output, mut playback) = rtrb::RingBuffer::new(PLAYBACK_CAPACITY);
        let (_commands, receiver) = mpsc::sync_channel(8);
        let (events, telemetry) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let meters = Arc::new(Meters::default());
        meters.spectrum.set_enabled(true);
        let worker_stop = stop.clone();
        let worker_meters = meters.clone();
        let controls = Controls::default();
        let worker = thread::spawn(move || {
            process(
                input,
                output,
                clock_resampler(1.0).unwrap(),
                Box::new(Engine::new(
                    Arc::new(Correction::identity(Rate::Hz44100)),
                    controls,
                )),
                controls,
                receiver,
                worker_stop,
                worker_meters,
                events,
            )
        });
        let Event::Spectrum(stream) = telemetry.recv_timeout(Duration::from_secs(5)).unwrap()
        else {
            panic!("worker announces its telemetry stream before processing");
        };
        let mut analyzer = crate::spectrum::Analyzer::new(crate::spectrum::Settings::default());
        analyzer.attach(stream);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut produced = 0;
        let mut played = 0;
        let mut output_peak = 0.0_f32;
        let mut polarity_error = 0.0_f32;
        let mut levels = Vec::new();
        for source in [
            crate::spectrum::Source::Input,
            crate::spectrum::Source::Output,
        ] {
            analyzer.configure(crate::spectrum::Settings {
                source,
                ..crate::spectrum::Settings::default()
            });
            while !analyzer.ready() && Instant::now() < deadline {
                while capture.slots() > CAPTURE_CAPACITY - QUEUE_TARGET - BLOCK {
                    let value =
                        0.25 * (std::f32::consts::TAU * 64.0 * produced as f32 / 4096.0).sin();
                    capture.push([value, -value]).unwrap();
                    produced += 1;
                }
                while let Ok(frame) = playback.pop() {
                    played += 1;
                    output_peak = output_peak.max(frame[0].abs());
                    polarity_error = polarity_error.max((frame[0] + frame[1]).abs());
                }
                analyzer.update();
                thread::sleep(Duration::from_millis(1));
            }
            levels.push(analyzer.bins().max_by(|a, b| a.1.total_cmp(&b.1)));
        }
        stop.store(true, Ordering::Relaxed);
        worker.join().unwrap();
        while playback.pop().is_ok() {
            played += 1;
        }
        assert!(played >= 8192 && output_peak > 0.0 && output_peak < 0.25);
        assert!(polarity_error < 1e-6);
        let (input_hz, input_db) =
            levels[0].expect("the actual worker must deliver a complete input window");
        let (output_hz, output_db) =
            levels[1].expect("the actual worker must deliver a complete output window");
        assert!((input_hz - 44100.0 / 64.0).abs() < 0.01);
        assert_eq!(output_hz, input_hz);
        assert!(
            (input_db + 12.0412).abs() < 0.1,
            "the input tap precedes listening volume: {input_db}"
        );
        assert!(
            (output_db - input_db - controls.volume.db() + 1.0).abs() < 0.1,
            "the output tap follows listening volume and identity headroom: {output_db}"
        );
        assert_eq!(meters.overruns.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn coreaudio_overload_does_not_request_a_route_restart() {
        let fault = Arc::new(AtomicBool::new(false));
        let meters = Arc::new(Meters::default());
        let mut callback = stream_error_callback(fault.clone(), meters.clone());
        for _ in 0..4 {
            callback(cpal::Error::new(cpal::ErrorKind::Xrun));
            assert!(
                !fault.load(Ordering::Relaxed),
                "a transient Core Audio overload must not invalidate live streams and trigger route recovery"
            );
        }
        assert_eq!(meters.backend_xruns.load(Ordering::Relaxed), 4);
        for error in [
            cpal::ErrorKind::DeviceNotAvailable,
            cpal::ErrorKind::StreamInvalidated,
        ] {
            callback(cpal::Error::new(error));
            assert!(fault.swap(false, Ordering::Relaxed));
        }
    }
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires BlackHole and the Tascam interface"]
    fn native_streams_continue_after_coreaudio_overload_notifications() {
        let directory = tempfile::tempdir().unwrap();
        let (events, receiver) = mpsc::channel();
        let meters = Arc::new(Meters::default());
        let mut owner = Owner::new(
            events,
            meters.clone(),
            Arc::new(Correction::identity(Rate::Hz44100)),
            Controls::default(),
            Store::at(directory.path()).unwrap(),
            Backend::Hardware,
        )
        .unwrap();
        owner.route = Some((
            default_route(&devices().unwrap()).expect("installed BlackHole/Tascam route"),
            SystemOutput::Unchanged,
        ));
        owner.start_selected(None);
        let initial: Vec<_> = receiver.try_iter().collect();
        assert!(
            initial
                .iter()
                .any(|event| matches!(event, Event::State(Lifecycle::Running { .. }))),
            "{initial:?}"
        );
        let fault = owner.running.as_ref().unwrap().fault.clone();
        let mut callback = stream_error_callback(fault, meters.clone());
        let frames = meters.output_frames.load(Ordering::Relaxed);
        for _ in 0..4 {
            callback(cpal::Error::new(cpal::ErrorKind::Xrun));
            thread::sleep(Duration::from_millis(100));
            owner.poll_stream();
            assert!(
                owner.running.is_some(),
                "overload notifications must not stop native streams"
            );
        }
        assert!(receiver.try_iter().all(|event| !matches!(
            event,
            Event::State(Lifecycle::Recovering { .. } | Lifecycle::Failed(_))
        )));
        assert!(meters.output_frames.load(Ordering::Relaxed) > frames);
        owner.release();
    }
    #[test]
    fn an_hour_of_clock_mismatch_keeps_occupancy_bounded() {
        for rate in [44100, 48000] {
            for ppm in [-1000.0, -100.0, 100.0, 1000.0] {
                let mut servo = ClockServo::default();
                let mut queue = QUEUE_TARGET as f64;
                for _ in 0..rate * 3600 / BLOCK {
                    let ratio = servo.observe(queue.round() as usize);
                    queue += BLOCK as f64 * (1.0 + ppm * 1e-6 - 1.0 / ratio);
                    assert!(
                        (64.0..2048.0).contains(&queue),
                        "{rate} Hz, {ppm} ppm: {queue} frames"
                    );
                }
                assert!((servo.correction * 1e6 + ppm).abs() < 3.0);
            }
        }
    }
}
