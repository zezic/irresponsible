//! Native room workspace. Drawing and interaction share logical-point geometry.
use super::chrome::ICON;
use super::*;
use irresponsible::{
    correction::room as correction,
    measurement::{
        self as measurement,
        workflow::{self, Control, Decision, Pass, PassId, Progress, RunKind, Scope, Session},
    },
};

pub(super) fn render_preview(
    path: &std::path::Path,
    dimensions: [u32; 2],
    scale: f64,
    state: PreviewState,
) -> Result<()> {
    if dimensions[0] < 880 || f64::from(dimensions[1]) < layout::MIN_HEIGHT {
        return Err(invalid("Room workspace requires at least 880 x 760 points"));
    }
    if !scale.is_finite() || !(0.5..=3.0).contains(&scale) {
        return Err(invalid("Render scale must be 0.5-3"));
    }
    let directory = tempfile::tempdir()?;
    let store = Store::at(directory.path())?;
    let scenario = measurement::simulation::Scenario::default();
    let id = measurement::validation::run(scenario.clone(), &store)?;
    let mut app = App::with_backend(store, audio::Backend::Simulated(scenario))?;
    app.window_chrome = Some(WindowChrome {
        height: 28.0,
        controls_end: 81.0,
    });
    app.open_room_session(id)?;
    app.room.opened = true;
    app.message = "Drag effort knots on the graph; drag the smoothing band at its top.".into();
    app.room.stage = match state {
        PreviewState::RoomMeasure => Stage::Measure,
        PreviewState::RoomVerification => Stage::Correction,
        _ => Stage::Correction,
    };
    if matches!(state, PreviewState::RoomVerification) {
        app.room.dialog = Some(Dialog::Verification);
    }
    if matches!(state, PreviewState::RoomFilter) {
        app.room.plot_kind = PlotKind::CorrectionFir;
    }
    app.room.name = "Studio / desk area".into();
    app.settings
        .measurements
        .insert(id, "Studio / desk area".into());
    let size = Size::new(f64::from(dimensions[0]), f64::from(dimensions[1]));
    crate::render::png(
        &app.build_scene(size, scale),
        path,
        (size.width * scale).round() as u32,
        (size.height * scale).round() as u32,
        app.theme.current.bg,
    )
}

/// Real-time native smoke automation owns only emulated endpoints and a temporary store.
pub(super) fn smoke(destination: &std::path::Path, layout: measurement::Layout) -> Result<()> {
    std::fs::create_dir_all(destination)?;
    let temporary = tempfile::tempdir()?;
    let store = Store::at(temporary.path())?;
    let scenario = measurement::simulation::Scenario {
        pace: measurement::simulation::Pace::RealTime,
        ..Default::default()
    };
    let mut app = App::with_backend(store, audio::Backend::Simulated(scenario))?;
    app.settings.window = WindowSize::try_from([1440, 960])?;
    app.settings.room.layout = layout;
    app.settings.room.acquisition.sweep_seconds = 1.0;
    app.settings.room.acquisition.tail_seconds = 1.0;
    app.settings.room.acquisition.noise_seconds = 1.0;
    app.settings
        .room
        .acquisition
        .noise_limits
        .maximum_variation_db = 0.0;
    app.room_action(Action::Open)?;
    let mut smoke = NativeSmoke {
        app,
        phase: SmokePhase::Route,
        started: Instant::now(),
        destination: destination.to_owned(),
        failure: None,
        ready: None,
        live_fit: None,
        sweep_captured: false,
    };
    let event_loop = EventLoop::new().map_err(|e| invalid(e.to_string()))?;
    event_loop
        .run_app(&mut smoke)
        .map_err(|e| invalid(e.to_string()))?;
    if let Some(error) = smoke.failure {
        return Err(invalid(error));
    }
    if !matches!(smoke.phase, SmokePhase::Complete) {
        return Err(invalid(
            "Native smoke window exited before validation finished",
        ));
    }
    Ok(())
}
#[derive(Debug)]
enum SmokePhase {
    Complete,
    Route,
    Right,
    Measurement,
    Review,
    Audition {
        filter: ContentHash,
        original: ContentHash,
    },
    Restore {
        original: ContentHash,
    },
    Edit,
    Move,
    Quiescent {
        until: Instant,
        passes: usize,
    },
}
struct NativeSmoke {
    app: App,
    phase: SmokePhase,
    started: Instant,
    destination: std::path::PathBuf,
    failure: Option<String>,
    ready: Option<workflow::Movement>,
    live_fit: Option<measurement::Band>,
    sweep_captured: bool,
}
impl NativeSmoke {
    fn capture(&mut self, name: &str) -> Result<()> {
        let size = self.app.viewport();
        let scale = self.app.scale();
        crate::render::png(
            &self.app.build_scene(size, scale),
            &self.destination.join(name),
            (size.width * scale).round() as u32,
            (size.height * scale).round() as u32,
            self.app.theme.current.bg,
        )
    }
    fn capture_at(&mut self, name: &str, dimensions: [u32; 2]) -> Result<()> {
        crate::render::png(
            &self.app.build_scene(
                Size::new(f64::from(dimensions[0]), f64::from(dimensions[1])),
                1.0,
            ),
            &self.destination.join(name),
            dimensions[0],
            dimensions[1],
            self.app.theme.current.bg,
        )
    }
    fn click(&mut self, event_loop: &ActiveEventLoop, action: super::Action) -> Result<()> {
        self.app.build_scene(self.app.viewport(), self.app.scale());
        let bounds = self
            .app
            .hits
            .iter()
            .rev()
            .find(|(r, a)| *a == action && r.width() > 0.0 && r.height() > 0.0)
            .map(|(r, _)| *r)
            .ok_or_else(|| invalid("Native smoke action has no visible control"))?;
        self.pointer(event_loop, bounds.center());
        self.mouse(event_loop, ElementState::Pressed);
        self.mouse(event_loop, ElementState::Released);
        Ok(())
    }
    fn click_scrolled(
        &mut self,
        event_loop: &ActiveEventLoop,
        action: super::Action,
    ) -> Result<()> {
        for _ in 0..16 {
            self.app.build_scene(self.app.viewport(), self.app.scale());
            if self
                .app
                .hits
                .iter()
                .any(|(r, a)| *a == action && r.width() > 0.0 && r.height() > 0.0)
            {
                return self.click(event_loop, action);
            }
            self.pointer(event_loop, self.app.room.settings_bounds.center());
            let id = self.app.window.as_ref().expect("native smoke window").id();
            self.app.window_event(
                event_loop,
                id,
                WindowEvent::MouseWheel {
                    device_id: winit::event::DeviceId::dummy(),
                    delta: winit::event::MouseScrollDelta::LineDelta(0.0, -8.0),
                    phase: winit::event::TouchPhase::Moved,
                },
            );
        }
        Err(invalid(
            "Native smoke action was not reachable by scrolling",
        ))
    }
    fn pointer(&mut self, event_loop: &ActiveEventLoop, at: Point) {
        let id = self.app.window.as_ref().expect("native smoke window").id();
        self.app.window_event(
            event_loop,
            id,
            WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: PhysicalPosition::new(at.x * self.app.scale(), at.y * self.app.scale()),
            },
        );
    }
    fn mouse(&mut self, event_loop: &ActiveEventLoop, state: ElementState) {
        let id = self.app.window.as_ref().expect("native smoke window").id();
        self.app.window_event(
            event_loop,
            id,
            WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state,
                button: MouseButton::Left,
            },
        );
    }
    fn inspect_controls(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        self.click(
            event_loop,
            super::Action::Room(Action::Stage(Stage::Measure)),
        )?;
        self.click(event_loop, super::Action::Room(Action::MicrophoneChannel))?;
        self.capture("microphone-channel.png")?;
        self.click(
            event_loop,
            super::Action::Room(Action::ChooseMicrophoneChannel(MonoChannel(0))),
        )?;
        self.capture("measure.png")?;
        self.capture_at("measure-minimum.png", [880, 760])?;
        self.click_scrolled(
            event_loop,
            super::Action::Room(Action::Fold(Section::Acquisition)),
        )?;
        self.capture_at("measure-expanded-minimum.png", [880, 760])?;
        self.click(
            event_loop,
            super::Action::Room(Action::Stage(Stage::Correction)),
        )?;
        self.capture("correction.png")?;
        self.capture_at("correction-minimum.png", [880, 760])?;
        self.click(
            event_loop,
            super::Action::Room(Action::Dialog(Some(Dialog::Measurements))),
        )?;
        self.capture("measurements.png")?;
        self.click(event_loop, super::Action::Room(Action::Dialog(None)))?;
        Ok(())
    }
    fn tick(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        if matches!(self.phase, SmokePhase::Complete) {
            event_loop.exit();
            return Ok(());
        }
        if self.started.elapsed() > Duration::from_secs(150) {
            return Err(invalid(format!(
                "Native room smoke timed out in {:?}: lifecycle {:?}, awaiting {}, active {:?}, status {}",
                self.phase,
                self.app.lifecycle,
                self.app.awaiting.is_some(),
                self.app.active.hash,
                self.app.message,
            )));
        }
        if let Lifecycle::Failed(reason) = &self.app.lifecycle {
            return Err(invalid(format!(
                "Native room smoke failed in {:?}: {reason}",
                self.phase
            )));
        }
        if let Some(Progress::Failed(error)) = &self.app.room.progress
            && !matches!(self.phase, SmokePhase::Quiescent { .. })
        {
            return Err(invalid(error.clone()));
        }
        if matches!(self.phase, SmokePhase::Measurement)
            && let Some(Progress::Move { movement, .. }) = self.app.room.progress
            && self.ready != Some(movement)
        {
            let name = match movement {
                workflow::Movement::Reference(_) => "return-reference.png",
                _ => "move-position.png",
            };
            self.capture(name)?;
            self.click(
                event_loop,
                super::Action::Room(Action::Movement {
                    run: self.app.room.expected.expect("smoke measurement"),
                    movement,
                    action: workflow::MoveAction::Ready,
                }),
            )?;
            self.ready = Some(movement);
        }
        if matches!(self.phase, SmokePhase::Measurement)
            && !self.sweep_captured
            && matches!(self.app.room.progress, Some(Progress::Sweep { .. }))
        {
            self.capture("measuring.png")?;
            self.sweep_captured = true;
        }
        if matches!(self.phase, SmokePhase::Measurement) {
            assert!(
                self.app.room.dialog.is_none(),
                "measurement proceeds after acknowledgement"
            );
        }
        if matches!(self.phase, SmokePhase::Measurement)
            && self.app.room.stage == Stage::Correction
            && self.live_fit.is_none()
            && let Some(band) = self.app.room.active_band
        {
            self.capture("live-fit.png")?;
            self.live_fit = Some(band);
        }
        match self.phase {
            SmokePhase::Route if matches!(self.app.lifecycle, Lifecycle::Running { .. }) => {
                assert!(self.app.room.levels.iter().all(Option::is_none));
                assert!(self.app.room.session.is_none());
                self.click(
                    event_loop,
                    super::Action::Room(Action::Stage(Stage::Measure)),
                )?;
                self.click(event_loop, super::Action::Room(Action::Measure))?;
                assert_eq!(
                    self.app.room.dialog,
                    Some(Dialog::Uncalibrated(RunKind::Measure))
                );
                assert!(!self.app.room.running);
                assert!(self.app.room.expected.is_none());
                self.capture("uncalibrated-confirmation.png")?;
                self.click(event_loop, super::Action::Room(Action::Dialog(None)))?;
                assert!(!self.app.room.running);
                assert!(self.app.room.expected.is_none());
                self.click(event_loop, super::Action::Room(Action::Measure))?;
                self.click(
                    event_loop,
                    super::Action::Room(Action::ContinueUncalibrated(RunKind::Measure)),
                )?;
                assert!(self.app.room.running);
                assert!(self.app.room.dialog.is_none());
                assert!(self.app.room.levels.iter().all(Option::is_none));
                self.phase = SmokePhase::Measurement;
                println!(
                    "Native smoke: fresh Measure click -> acknowledgement -> correction, without level checks"
                );
            }
            SmokePhase::Measurement
                if matches!(self.app.room.progress, Some(Progress::Finished { .. })) =>
            {
                assert!(
                    self.app
                        .room
                        .source
                        .as_ref()
                        .expect("completed source")
                        .levels
                        .is_none()
                );
                self.capture("measurement-complete.png")?;
                assert!(
                    self.app
                        .room
                        .source
                        .as_ref()
                        .expect("measured source")
                        .points[0]
                        .noise
                        .quality_warning()
                        .is_some()
                );
                self.click(
                    event_loop,
                    super::Action::Room(Action::Stage(Stage::Measure)),
                )?;
                self.capture("noise-advisory.png")?;
                self.click(
                    event_loop,
                    super::Action::Room(Action::CheckLevel(measurement::Speaker::Right)),
                )?;
                self.phase = SmokePhase::Right;
            }
            SmokePhase::Right if self.app.room.levels[1].is_some() && !self.app.room.running => {
                self.click(
                    event_loop,
                    super::Action::Room(Action::Stage(Stage::Correction)),
                )?;
                self.phase = SmokePhase::Review;
            }
            SmokePhase::Review => {
                let best = self
                    .app
                    .room
                    .manifest
                    .as_ref()
                    .and_then(|s| s.default_candidate(&self.app.room.history))
                    .ok_or_else(|| invalid("No prepared correction"))?;
                let filter = self.app.room.history[best.0]
                    .correction()
                    .ok_or_else(|| invalid("Simulated correction did not improve baseline"))?;
                assert_eq!(self.app.room.history[best.0].decision, Decision::Prepared);
                self.inspect_controls(event_loop)?;
                if self
                    .app
                    .room
                    .source
                    .as_ref()
                    .is_some_and(|s| s.points.len() > 1)
                {
                    self.app.room_action(Action::PointView)?;
                    self.capture("reference-view.png")?;
                    self.app.room_action(Action::PointView)?;
                    self.capture("side-view.png")?;
                    self.app.room_action(Action::PointView)?;
                }
                self.app.room_action(Action::SelectPass(best))?;
                self.capture("history-preview.png")?;
                self.app.room_action(Action::Prefer)?;
                self.app.room_action(Action::SaveMeasurement)?;
                self.capture("save-measurement.png")?;
                if let Some(TextFocus::Room { field, .. }) = &mut self.app.text_focus {
                    field.insert("Emulated room");
                }
                self.app.commit_room_text()?;
                self.app.room_action(Action::SavePass)?;
                self.capture("save-pass.png")?;
                if let Some(TextFocus::PresetName { field, .. }) = &mut self.app.text_focus {
                    field.insert("Emulated correction");
                }
                assert!(self.app.commit_preset_name());
                assert!(self.app.settings.presets.contains_key(&filter));
                self.app.room.opened = true;
                let original = self.app.active.hash;
                self.app.room_action(Action::Audition)?;
                self.phase = SmokePhase::Audition { filter, original };
                println!("Native smoke: exact-pass audition and persistence");
            }
            SmokePhase::Audition { filter, original }
                if self.app.awaiting.is_none() && self.app.active.hash == filter =>
            {
                assert_eq!(
                    self.app.store.load()?.active,
                    Some(original),
                    "temporary audition must not replace persisted playback"
                );
                self.app.room_action(Action::Audition)?;
                self.phase = SmokePhase::Restore { original };
            }
            SmokePhase::Restore { original }
                if self.app.awaiting.is_none() && self.app.active.hash == original =>
            {
                self.app.room_action(Action::BackToCandidate)?;
                self.click_scrolled(
                    event_loop,
                    super::Action::Room(Action::SetEffortMode(EffortMode::Advanced)),
                )?;
                let overlay = Overlay::new(self.app.room.graph);
                let level = self
                    .app
                    .settings
                    .room
                    .recipe
                    .effort
                    .at(1000.0, self.app.settings.room.recipe.range);
                self.app.cursor = overlay.point(1000.0, level);
                self.app.begin_room_drag();
                self.app.cursor.y = overlay.y(0.5);
                let Some(super::Drag::Room(drag)) = self.app.dragging else {
                    return Err(invalid("Native graph did not admit an effort knot"));
                };
                self.app.room_drag(drag, true);
                self.app.dragging = None;
                self.phase = SmokePhase::Edit;
                println!("Native smoke: effort line edit from immutable source");
            }
            SmokePhase::Edit if self.app.job.is_none() => {
                let pass = self.app.room.history.last().expect("edited pass");
                assert_eq!(pass.decision, Decision::Prepared);
                assert!(!pass.fully_verified());
                assert!(
                    self.app
                        .room
                        .manifest
                        .as_ref()
                        .expect("session")
                        .preferred
                        .is_some()
                );
                self.capture("effort-edit.png")?;
                self.app.room_action(Action::Fold(Section::Diagnostics))?;
                self.app.build_scene(Size::new(880.0, 760.0), 1.0);
                self.app.room.settings_scroll = (self.app.room.settings_height
                    - self.app.room.settings_bounds.height())
                .max(0.0);
                self.capture_at("details-minimum.png", [880, 760])?;
                self.app.room_action(Action::Fold(Section::Diagnostics))?;
                self.app.room.settings_scroll = 0.0;
                self.click(
                    event_loop,
                    super::Action::Room(Action::Dialog(Some(Dialog::Verification))),
                )?;
                self.click(event_loop, super::Action::Room(Action::Verify))?;
                let Some(Dialog::Uncalibrated(kind)) = self.app.room.dialog else {
                    return Err(invalid(
                        "Explicit Verify did not request microphone acknowledgement",
                    ));
                };
                self.click(
                    event_loop,
                    super::Action::Room(Action::ContinueUncalibrated(kind)),
                )?;
                self.phase = SmokePhase::Move;
            }
            SmokePhase::Move if matches!(self.app.room.progress, Some(Progress::Move { .. })) => {
                let window = self.app.window.as_ref().expect("native window").id();
                let passes = self.app.room.history.len();
                self.app
                    .window_event(event_loop, window, WindowEvent::CloseRequested);
                assert!(!self.app.room.running);
                assert!(self.app.room.muted);
                self.phase = SmokePhase::Quiescent {
                    until: Instant::now() + Duration::from_millis(300),
                    passes,
                };
                println!("Native smoke: close cancels pending automatic starts");
            }
            SmokePhase::Quiescent { until, passes } if Instant::now() >= until => {
                assert_eq!(self.app.room.history.len(), passes);
                self.app.save();
                let settings = self.app.store.load()?;
                assert_eq!(settings.measurements.len(), 1);
                assert_eq!(settings.presets.len(), 1);
                workflow::export_session(
                    self.app.room.session.expect("saved session"),
                    &self.destination.join("session"),
                    &self.app.store,
                )?;
                #[cfg(feature = "alloc-audit")]
                {
                    assert_eq!(
                        irresponsible::audit::ALLOCATIONS.load(Ordering::Relaxed),
                        0,
                        "audio callback allocation"
                    );
                    assert_eq!(
                        irresponsible::audit::DEALLOCATIONS.load(Ordering::Relaxed),
                        0,
                        "audio callback deallocation"
                    );
                    println!("Native callback audit: 0 allocations / 0 deallocations");
                }
                self.phase = SmokePhase::Complete;
                println!("Native smoke passed");
                event_loop.exit();
            }
            _ => {}
        }
        self.app.dirty = true;
        Ok(())
    }
}
impl ApplicationHandler for NativeSmoke {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.app.resumed(event_loop);
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        self.app.window_event(event_loop, id, event);
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.app.about_to_wait(event_loop);
        if let Err(error) = self.tick(event_loop) {
            self.failure = Some(error.to_string());
            event_loop.exit();
        }
    }
    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.app.exiting(event_loop);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Route, calibration, levels, positions and acquisition.
    Measure,
    Correction,
}
/// Whether acquisition covers one position or a guided area around it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    Single,
    Listening,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dialog {
    Verification,
    Measurements,
    Uncalibrated(RunKind),
    MeasurementName,
    PresetName,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    Acquisition,
    Advanced,
    Diagnostics,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetPreset {
    Gentle,
    Natural,
    Warm,
    Flat,
    /// Editable tilt, bass shelf and mid bell, seeded from the current target.
    Custom,
}
impl TargetPreset {
    const ALL: [Self; 5] = [
        Self::Gentle,
        Self::Natural,
        Self::Warm,
        Self::Flat,
        Self::Custom,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::Gentle => "Gentle",
            Self::Natural => "Natural",
            Self::Warm => "Warm",
            Self::Flat => "Flat",
            Self::Custom => "Custom",
        }
    }
    /// The target this chip selects when the current target is `current`.
    fn target(self, current: &correction::Target) -> correction::Target {
        match self {
            Self::Gentle => correction::Target::Gentle,
            Self::Natural => correction::Target::Natural,
            Self::Warm => correction::Target::Warm,
            Self::Flat => correction::Target::Flat,
            Self::Custom => {
                correction::Target::Parametric(current.parameters().unwrap_or_default())
            }
        }
    }
    fn selects(self, target: &correction::Target) -> bool {
        matches!(
            (self, target),
            (Self::Gentle, correction::Target::Gentle)
                | (Self::Natural, correction::Target::Natural)
                | (Self::Warm, correction::Target::Warm)
                | (Self::Flat, correction::Target::Flat)
                | (Self::Custom, correction::Target::Parametric(_))
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffortMode {
    Easy,
    Advanced,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChoiceField {
    Alignment,
    Orientation,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Choice {
    Alignment(correction::Alignment),
    Orientation(measurement::Orientation),
}
impl ChoiceField {
    pub(super) fn action(self) -> Action {
        match self {
            Self::Alignment => Action::Alignment,
            Self::Orientation => Action::Orientation,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Choose(Choice),
    CommitName,
    Dialog(Option<Dialog>),
    Fold(Section),
    SetTarget(TargetPreset),
    SetPlot(PlotKind),
    Trace(Trace),
    SetChannels(Channels),
    SetEffortMode(EffortMode),
    ChooseMicrophone(usize),
    MicrophoneChannel,
    ChooseMicrophoneChannel(MonoChannel),
    ParentComparison,
    PointView,
    Unsmoothed,
    Orientation,
    Listen,
    RestorePrevious,
    ResumeAcquisition,
    CheckLevel(measurement::Speaker),
    Open,
    Close,
    Stage(Stage),
    Measure,
    Verify,
    VerifySpatial,
    Stop,
    ResumeListening,
    Movement {
        run: audio::MeasurementId,
        movement: workflow::Movement,
        action: workflow::MoveAction,
    },
    Microphone,
    Calibration,
    RemoveCalibration,
    ContinueUncalibrated(RunKind),
    ImportTarget,
    AutoLevel,
    BoostLimit,
    CutLimit,
    LfDetail,
    HandsFree,
    Alignment,
    SetArea(Area),
    SelectPass(PassId),
    BackToCandidate,
    Audition,
    Prefer,
    SavePass,
    SaveCandidate,
    SaveMeasurement,
    OpenMeasurement(ContentHash),
    Graph,
    Export,
    ExportFilter,
    EditText(Text),
}
impl Action {
    pub(super) fn help(self) -> Option<&'static str> {
        Some(match self {
            Self::Close => "Return to the headphone workspace and end a temporary pass audition.",
            Self::Microphone => {
                "A separate physical mono microphone input. Normal stereo capture stays configured independently."
            }
            Self::MicrophoneChannel => {
                "Choose one of the selected microphone device's exposed input channels."
            }
            Self::Dialog(Some(Dialog::Verification)) => {
                "Measure the applied correction without changing it."
            }
            Self::Graph => {
                "Drag the effort line knots to shape correction strength; drag the smoothing band at the top to move the crossover or its edges. In advanced effort, click the line to add a knot and Alt-click a knot to remove it."
            }
            Self::SetEffortMode(EffortMode::Advanced) => {
                "Shape effort freely along frequency. Starts from the current easy line."
            }
            Self::SetEffortMode(EffortMode::Easy) => {
                "One effort level with fades at both ends of the correction range."
            }
            Self::BoostLimit => "Enable the maximum boost in dB. Disabled by default.",
            Self::CutLimit => "Enable cut limiting. Disabled by default.",
            Self::AutoLevel => {
                "Fit one common L/R target offset while preserving target shape and stereo balance."
            }
            Self::Audition => {
                "Listen to the exact selected pass at a matched level. Stop audition restores the previous listening filter."
            }
            Self::Prefer => {
                "Record a listening preference independently of the lowest measured-error pass."
            }
            Self::SavePass => {
                "Save the exact selected history filter and recipe as a correction preset."
            }
            Self::SaveMeasurement => {
                "Name the reusable original measurement source and its history separately from correction presets."
            }
            Self::VerifySpatial => {
                "Measure this exact filter at every included position. A reference-only measurement does not verify the listening area."
            }

            _ => return None,
        })
    }
}
impl Parameter {
    pub(super) fn help(self) -> &'static str {
        match self {
            Self::ExcitationLow | Self::ExcitationHigh => {
                "Frequency range emitted by the measurement sweep, separate from the correction range. Keep it within the speaker capability."
            }

            Self::ProbeLevel => {
                "Fixed measurement probe peak in dBFS, independent of listening volume. Begin quietly and keep hardware gain fixed."
            }
            Self::Sweep | Self::LfSeconds => {
                "Sweep duration. Longer sweeps collect more energy; LF detail uses its own exact probe inverse."
            }
            Self::Tail => {
                "Recorded decay after the sweep. Extend this if the room response has not settled above the measured noise floor."
            }
            Self::Noise => {
                "Dedicated background recording duration at the same microphone gain as the sweeps."
            }
            Self::Low | Self::High | Self::LfHigh => "Requested frequency boundary.",
            Self::Effort => {
                "Correction strength controls how far the response moves toward the target."
            }
            Self::LowFade | Self::HighFade => {
                "Octaves over which effort rises from zero at the correction range edge."
            }
            Self::BassSmoothing | Self::TrebleSmoothing => {
                "Gaussian smoothing of the measured response before target matching, in octave FWHM. Effort is applied afterward; enabled gain limits take precedence. Zero disables smoothing. Alt-click a bar to reset it."
            }
            Self::Crossover | Self::Transition => {
                "Bass smoothing applies below the transition band and treble smoothing above it, blended linearly across the band."
            }

            Self::Boost => {
                "Optional cumulative boost limit from 0 to +24 dB. The realized stereo FIR is checked against this bound."
            }
            Self::Cut => {
                "Optional cumulative attenuation limit from 0 to 36 dB. The realized stereo FIR is checked against this bound."
            }
            Self::Offset => {
                "One shared target offset in dB. It changes target level while preserving target shape and stereo balance."
            }

            Self::MoveSeconds => {
                "Time allowed to move the microphone before hands-free acquisition. Pause countdown, Ready now and Stop remain available."
            }
            Self::MidGain => {
                "Target bell gain at its center frequency, from -12 to +12 dB. Zero is neutral."
            }
            Self::MidCenter => {
                "Target bell center frequency from 20 Hz to 20 kHz. Defaults to 1 kHz."
            }
            Self::MidWidth => "Target bell full width at half gain, from 0.1 to 4 octaves.",
            Self::Tilt | Self::Bass => {
                "Acoustic target shape. Changing it prepares a new candidate from the immutable measurement source."
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Text {
    MeasurementName,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parameter {
    Low,
    High,
    Effort,
    LowFade,
    HighFade,
    BassSmoothing,
    TrebleSmoothing,
    Crossover,
    Transition,
    Boost,
    Cut,
    Offset,
    ProbeLevel,
    Sweep,
    Tail,
    Noise,
    LfSeconds,
    LfHigh,
    MoveSeconds,
    ExcitationLow,
    ExcitationHigh,
    Tilt,
    Bass,
    MidGain,
    MidCenter,
    MidWidth,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bar {
    Effort,
    BassSmoothing,
    TrebleSmoothing,
}
impl Bar {
    pub(super) fn parameter(self) -> Parameter {
        match self {
            Self::Effort => Parameter::Effort,
            Self::BassSmoothing => Parameter::BassSmoothing,
            Self::TrebleSmoothing => Parameter::TrebleSmoothing,
        }
    }
    fn maximum(self) -> f64 {
        match self {
            Self::Effort => 100.0,
            Self::BassSmoothing | Self::TrebleSmoothing => 2.0,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Effort => "Effort",
            Self::BassSmoothing => "Bass",
            Self::TrebleSmoothing => "Treble",
        }
    }
}
impl Parameter {
    fn format(self, value: f64) -> String {
        match self {
            Self::Low
            | Self::High
            | Self::MidCenter
            | Self::Effort
            | Self::Crossover
            | Self::ExcitationLow
            | Self::ExcitationHigh => format!("{value:.0}"),
            Self::BassSmoothing | Self::TrebleSmoothing | Self::LowFade | Self::HighFade => {
                format!("{value:.2}")
            }
            _ => format!("{value:.1}"),
        }
    }

    pub fn step(self) -> f32 {
        match self {
            Self::Effort => 5.0,
            Self::BassSmoothing | Self::TrebleSmoothing | Self::LowFade | Self::HighFade => {
                1.0 / 24.0
            }
            Self::Transition => 0.5,
            Self::Crossover => 50.0,
            Self::Tilt => 0.1,
            Self::Bass | Self::MidGain => 0.5,
            Self::MidWidth => 0.1,
            Self::Low
            | Self::High
            | Self::MidCenter
            | Self::LfHigh
            | Self::ExcitationLow
            | Self::ExcitationHigh => 10.0,
            _ => 1.0,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Low => "Correction from / Hz",
            Self::High => "Correction to / Hz",
            Self::Effort => "Effort / %",
            Self::LowFade => "Low fade / oct",
            Self::HighFade => "High fade / oct",
            Self::BassSmoothing => "Bass smoothing / oct",
            Self::TrebleSmoothing => "Treble smoothing / oct",
            Self::Crossover => "Crossover / Hz",
            Self::Transition => "Transition / oct",
            Self::Boost => "Maximum boost / dB",
            Self::Cut => "Maximum cut / dB",
            Self::Offset => "Manual target offset / dB",
            Self::ProbeLevel => "Probe peak / dBFS",
            Self::Sweep => "Sweep duration / s",
            Self::Tail => "Decay capture / s",
            Self::Noise => "Background duration / s",
            Self::LfSeconds => "LF detail duration / s",
            Self::LfHigh => "LF detail upper / Hz",
            Self::MoveSeconds => "Movement allowance / s",

            Self::ExcitationLow => "Sweep excitation from / Hz",
            Self::ExcitationHigh => "Sweep excitation to / Hz",

            Self::Tilt => "Downward tilt / dB per octave",
            Self::Bass => "Target bass shelf / dB",
            Self::MidGain => "Target mid bell / dB",
            Self::MidCenter => "Mid bell center / Hz",
            Self::MidWidth => "Mid bell width / oct",
        }
    }
}
#[derive(Clone, Copy, PartialEq)]
pub enum Drag {
    /// An easy-effort knee: horizontal sets that side's fade, vertical sets the level.
    Knee {
        lower: bool,
    },
    /// An advanced effort knot by index.
    Knot(usize),
    Crossover,
    /// A transition band edge; its distance from the crossover sets the width.
    TransitionEdge,
}
pub(super) struct ListeningRestore {
    pub correction: ContentHash,
    pub controls: Controls,
    pass: PassId,
    reference_db: f32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Trace {
    /// Baseline after the recipe's smoothing, as the fit sees it.
    Measured,
    /// Baseline before smoothing.
    Raw,
    Target,
    Predicted,
    Verified,
    Spread,
    Effort,
}
impl Trace {
    const ALL: [Self; 7] = [
        Self::Measured,
        Self::Raw,
        Self::Target,
        Self::Predicted,
        Self::Verified,
        Self::Spread,
        Self::Effort,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::Measured => "Measured (smoothed)",
            Self::Raw => "Raw",
            Self::Target => "Target",
            Self::Predicted => "Predicted",
            Self::Verified => "Measured result",
            Self::Spread => "Area spread",
            Self::Effort => "Effort",
        }
    }
    fn color(self, colors: appearance::Colors) -> vello::peniko::Color {
        match self {
            Self::Measured | Self::Raw | Self::Spread => colors.secondary,
            Self::Target => colors.basic[5],
            Self::Predicted => colors.basic[2],
            Self::Verified => colors.basic[3],
            Self::Effort => colors.basic[1],
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlotKind {
    RoomResponse,
    CorrectionFir,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Channels {
    Both,
    Left,
    Right,
}
pub struct Workspace {
    pub opened: bool,
    pub running: bool,
    pub muted: bool,
    pub expected: Option<audio::MeasurementId>,
    active_plan: Option<Box<workflow::Plan>>,
    recovery: Option<workflow::Recovery>,
    stage: Stage,
    pub(super) dialog: Option<Dialog>,
    expanded: std::collections::BTreeSet<Section>,
    pub session: Option<ContentHash>,
    source: Option<measurement::Source>,
    history: Vec<Pass>,
    manifest: Option<Session>,
    selected: Option<PassId>,
    candidate: Option<ContentHash>,
    pub active_band: Option<measurement::Band>,
    progress: Option<Progress>,
    pub(super) settings_scroll: f64,
    settings_height: f64,
    graph: Rect,
    settings_bounds: Rect,
    name: String,
    pub(super) return_to: Option<ListeningRestore>,
    plot_kind: PlotKind,
    traces: std::collections::BTreeSet<Trace>,
    channels: Channels,
    compare_parent: bool,
    show_unsmoothed: bool,
    point_view: Option<usize>,
    plot_data: Option<PlotData>,
    levels: [Option<measurement::levels::Report>; 2],
    previous: Option<ContentHash>,
    latest_noise: Option<measurement::Noise>,
}
impl Workspace {
    /// Whether playback holds this workspace's candidate, so a prepared edit should replace it.
    pub(super) fn listening_to_candidate(&self, active: ContentHash) -> bool {
        self.opened && self.return_to.is_none() && self.candidate == Some(active)
    }
}
impl Default for Workspace {
    fn default() -> Self {
        Self {
            opened: false,
            running: false,
            muted: false,
            expected: None,
            active_plan: None,
            recovery: None,
            stage: Stage::Measure,
            dialog: None,
            expanded: Default::default(),
            session: None,
            source: None,
            history: Vec::new(),
            manifest: None,
            selected: None,
            candidate: None,
            active_band: None,
            progress: None,
            settings_scroll: 0.0,
            settings_height: 0.0,
            graph: Rect::ZERO,
            settings_bounds: Rect::ZERO,
            name: "Listening room".into(),
            return_to: None,
            plot_kind: PlotKind::RoomResponse,
            traces: Trace::ALL
                .into_iter()
                .filter(|&t| t != Trace::Raw)
                .collect(),
            channels: Channels::Both,
            compare_parent: true,
            show_unsmoothed: false,
            point_view: None,
            plot_data: None,
            levels: [None, None],
            previous: None,
            latest_noise: None,
        }
    }
}

/// Pink-weighted output level including the engine's safety attenuation.
fn audition_level_db(filter: &Correction) -> f32 {
    let analysis = Analysis::new(
        filter,
        Controls {
            comparison: Comparison::PinkNoise,
            ..Controls::default()
        },
    );
    20.0 * analysis.pink_rms_gain.log10() + analysis.wet_db + analysis.headroom_db
}

fn audition_controls(base: Controls, filter: &Correction, reference_db: f32) -> Result<Controls> {
    let attenuation = reference_db - audition_level_db(filter);
    if attenuation > 0.001 {
        return Err(invalid(
            "This pass needs a new audition level reference. Stop audition and start it again.",
        ));
    }
    Ok(Controls {
        volume: VolumeDb::try_from(f32::from(base.volume) + attenuation.min(0.0))?,
        correction: CorrectionMode::On,
        comparison: Comparison::PinkNoise,
        trim: ComparisonTrim::default(),
    })
}

fn comparable_parent(pass: &Pass, parent: &Pass) -> bool {
    pass.objective == parent.objective
        && match (&pass.scope, &parent.scope) {
            (Scope::Prediction { band: a }, Scope::Prediction { band: b }) => a == b,
            (
                Scope::ReferenceFull {
                    position: a,
                    trusted: x,
                },
                Scope::ReferenceFull {
                    position: b,
                    trusted: y,
                },
            ) => a == b && x == y,
            (
                Scope::SpatialFull {
                    positions: a,
                    trusted: x,
                },
                Scope::SpatialFull {
                    positions: b,
                    trusted: y,
                },
            ) => a == b && x == y,
            (
                Scope::SpatialPartial {
                    positions: a,
                    required: x,
                    trusted: u,
                },
                Scope::SpatialPartial {
                    positions: b,
                    required: y,
                    trusted: v,
                },
            ) => a == b && x == y && u == v,
            _ => false,
        }
        && pass.measured.is_empty() == parent.measured.is_empty()
}

impl App {
    fn resume_room_listening(&mut self) -> Result<()> {
        let route = self
            .settings
            .route
            .clone()
            .ok_or_else(|| invalid("Choose the listening audio route before resuming"))?;
        self.submit_command(Command::Start(route, SystemOutput::RouteThroughCapture))
    }

    pub(super) fn save_room_preset(&mut self) -> Result<()> {
        if self.room.selected.is_some() {
            return self.room_action(Action::SavePass);
        }
        if self.job.is_some() {
            return Err(invalid(
                "Wait for correction preparation before saving the candidate",
            ));
        }
        if self.room.candidate.is_none() {
            return Err(invalid(
                "Prepare or select a correction before saving a preset",
            ));
        }
        self.room_action(Action::SaveCandidate)
    }

    pub(super) fn room_action(&mut self, action: Action) -> Result<()> {
        use self::Action::*;
        if self.room.running
            && !matches!(
                action,
                Stop | Movement { .. }
                    | Dialog(_)
                    | Fold(_)
                    | SetPlot(_)
                    | Trace(_)
                    | SetChannels(_)
                    | Stage(_)
                    | SelectPass(_)
                    | BackToCandidate
                    | ParentComparison
                    | Unsmoothed
                    | PointView
            )
        {
            return Err(invalid(
                "Stop measurement before editing its frozen plan or auditioning",
            ));
        }
        if matches!(
            action,
            SetTarget(_)
                | SetEffortMode(_)
                | ImportTarget
                | AutoLevel
                | BoostLimit
                | CutLimit
                | Alignment
        ) {
            self.branch_room_recipe();
        }
        match action {
            Choose(choice) => {
                match choice {
                    Choice::Alignment(value) => {
                        self.branch_room_recipe();
                        self.settings.room.recipe.alignment = value;
                        self.room_changed();
                    }
                    Choice::Orientation(value) => {
                        let measurement::Calibration::Response { orientation, .. } =
                            &mut self.settings.room.calibration
                        else {
                            unreachable!("calibrated orientation picker")
                        };
                        *orientation = value;
                    }
                }
                self.save();
                self.dismiss_picker();
            }
            CommitName => self.commit_room_text()?,
            Dialog(dialog) => {
                self.room.dialog = dialog;
                self.room.settings_scroll = 0.0;
                self.picker = None;
                if dialog.is_some()
                    && let Some(Progress::Move { movement, .. }) = self.room.progress
                {
                    self.command(Command::MeasurementControl {
                        run: self.room.expected.expect("measurement progress lease"),
                        control: Control {
                            movement,
                            action: workflow::MoveAction::Pause,
                        },
                    });
                }
            }
            Fold(section) => {
                if !self.room.expanded.remove(&section) {
                    self.room.expanded.insert(section);
                }
            }
            SetTarget(preset) => {
                let recipe = &mut self.settings.room.recipe;
                recipe.target = preset.target(&recipe.target);
                self.room_changed();
            }
            SetEffortMode(mode) => {
                let recipe = &mut self.settings.room.recipe;
                recipe.effort = match (mode, &recipe.effort) {
                    (EffortMode::Easy, correction::Effort::Advanced(points)) => {
                        correction::Effort::easy(
                            points.iter().map(|p| p.effort).fold(0.0, f64::max),
                        )
                    }
                    (EffortMode::Advanced, easy @ correction::Effort::Easy { .. }) => {
                        correction::Effort::Advanced(easy.points(recipe.range))
                    }
                    _ => return Ok(()),
                };
                self.room_changed();
            }
            Trace(trace) => {
                if !self.room.traces.remove(&trace) {
                    self.room.traces.insert(trace);
                }
            }
            SetPlot(kind) => self.room.plot_kind = kind,
            SetChannels(channels) => self.room.channels = channels,
            ParentComparison => self.room.compare_parent = !self.room.compare_parent,
            PointView => {
                let count = self.room.source.as_ref().map_or(0, |s| s.points.len());
                self.room.point_view = match self.room.point_view {
                    None if count > 0 => Some(0),
                    Some(i) if i + 1 < count => Some(i + 1),
                    _ => None,
                };
            }
            Unsmoothed => self.room.show_unsmoothed = !self.room.show_unsmoothed,
            Orientation => self.open_picker(PickerKind::RoomChoice(ChoiceField::Orientation)),

            Open => {
                self.room.recovery = workflow::Recovery::load(&self.store)?;
                self.cancel_preparation();
                if self.settings.window_mode == WindowMode::Compact {
                    self.toggle_compact();
                }
                self.room.opened = true;
                self.message =
                    "Choose the measurement microphone and speaker route, then measure.".into();
                if self.room.session.is_none()
                    && let Some(
                        workflow::Recovery::Session(id)
                        | workflow::Recovery::Spatial { session: id, .. },
                    ) = workflow::Recovery::load(&self.store)?
                {
                    self.open_room_session(id)?;
                    self.restore_room_source_settings()?;
                    self.message="Recovered saved measurement evidence. No probe starts until an explicit request.".into();
                }
                self.settings.window_mode = WindowMode::Full;
                self.room.stage = if self.room.session.is_some() {
                    self::Stage::Correction
                } else {
                    self::Stage::Measure
                };
            }
            Close => {
                self.stop_room_audition()?;
                self.cancel_preparation();
                self.preview = None;
                self.room.opened = false;
            }
            Stage(stage) => {
                if !self.room.running && stage == self::Stage::Measure {
                    self.room.recovery = workflow::Recovery::load(&self.store)?;
                }
                self.room.stage = stage;
                self.room.dialog = None;
                self.room.settings_scroll = 0.0;
            }
            CheckLevel(speaker) => {
                self.stop_room_audition()?;
                if !self.room.muted {
                    self.room.previous = Some(self.active.hash);
                }
                let source = self.room_source()?;
                self.submit_command(Command::CheckLevels(Box::new(source), speaker))?;
                self.room.levels[speaker.index()] = None;
                self.room.progress = Some(Progress::Levels { speaker });
                self.room.running = true;
            }
            Measure | ResumeAcquisition | Verify | VerifySpatial => {
                let kind = match action {
                    Measure => RunKind::Measure,
                    ResumeAcquisition => {
                        let noise_limits = self.settings.room.acquisition.noise_limits;
                        let recovery = workflow::Recovery::load(&self.store)?
                            .ok_or_else(|| invalid("No interrupted acquisition is available"))?;
                        if !matches!(recovery, workflow::Recovery::Acquisition(_)) {
                            return Err(invalid("There is no incomplete acquisition to resume"));
                        }
                        let source = recovery.source_plan(&self.store)?;
                        self.settings.route = Some(source.route.clone());
                        self.settings.room.microphone = Some(source.microphone.clone());
                        self.settings.room.layout = source.layout;
                        self.settings.room.gain = source.gain;
                        self.settings.room.calibration = source.calibration;
                        self.settings.room.acquisition = source.acquisition;
                        self.settings.room.acquisition.noise_limits = noise_limits;
                        RunKind::ResumeMeasurement
                    }
                    Verify => RunKind::Verify {
                        pass: self
                            .room
                            .selected
                            .or_else(|| {
                                self.room
                                    .history
                                    .iter()
                                    .rev()
                                    .find(|p| {
                                        p.correction().is_some()
                                            && p.correction() == self.room.candidate
                                    })
                                    .map(|p| p.id)
                            })
                            .ok_or_else(|| invalid("Create a correction before verification"))?,
                    },
                    VerifySpatial => {
                        let pass = self
                            .room
                            .selected
                            .or_else(|| {
                                self.room
                                    .history
                                    .iter()
                                    .rev()
                                    .find(|p| {
                                        p.correction() == self.room.candidate
                                            && p.correction().is_some()
                                    })
                                    .map(|p| p.id)
                            })
                            .ok_or_else(|| {
                                invalid("Select a correction pass for spatial verification")
                            })?;
                        if self.room.history[pass.0].correction().is_none() {
                            return Err(invalid(
                                "Select a correction filter; baseline positions are retained in the source",
                            ));
                        }
                        RunKind::VerifySpatial { pass }
                    }
                    _ => unreachable!(),
                };
                self.request_room_measurement(kind)?;
            }
            ContinueUncalibrated(kind) => {
                assert_eq!(
                    self.room.dialog,
                    Some(self::Dialog::Uncalibrated(kind)),
                    "confirmation belongs to the displayed measurement request"
                );
                let plan = self.room_measurement_plan(kind)?;
                self.start_room_measurement(plan)?;
            }
            Stop => {
                self.command(Command::CancelMeasurement);
                self.room.running = false;
                self.room.active_plan = None;
                self.room.active_band = None;
                self.room.progress = None;
                self.room.recovery = workflow::Recovery::load(&self.store)?;
            }
            ResumeListening => {
                self.resume_room_listening()?;
            }
            Movement {
                run,
                movement,
                action,
            } => self.command(Command::MeasurementControl {
                run,
                control: Control { movement, action },
            }),
            Microphone => self.open_picker(PickerKind::RoomMicrophone),
            MicrophoneChannel => self.open_picker(PickerKind::RoomMicrophoneChannel),
            ChooseMicrophoneChannel(channel) => {
                let mic = self
                    .settings
                    .room
                    .microphone
                    .as_mut()
                    .expect("channel selection has a microphone");
                let device = self
                    .devices
                    .iter()
                    .find(|d| d.uid == mic.uid)
                    .ok_or_else(|| invalid("Microphone is unavailable"))?;
                channel.validate(device.input_channels)?;
                mic.channels = channel;
                self.save();
                self.dismiss_picker();
            }
            ChooseMicrophone(index) => {
                let d = &self.devices[index];
                self.settings.room.microphone = Some(Endpoint {
                    uid: d.uid.clone(),
                    label: d.name.clone(),
                    channels: MonoChannel(0),
                });
                self.save();
                self.dismiss_picker();
            }
            Calibration => {
                self.room.dialog = None;
                self.room.stage = self::Stage::Measure;
                self.room.settings_scroll = 0.0;
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Measured microphone response: Hz, dB, optional degrees")
                    .pick_file()
                {
                    self.settings.room.calibration = measurement::Calibration::import(
                        &std::fs::read(path)?,
                        measurement::Orientation::TowardSpeakers,
                        &self.store,
                    )?;
                    self.save();
                }
            }
            RemoveCalibration => {
                self.settings.room.calibration = measurement::Calibration::default();
                self.save();
            }
            ImportTarget => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Acoustic room target: frequency Hz, response dB")
                    .pick_file()
                {
                    let mut recipe = self.settings.room.recipe.clone();
                    recipe.target = correction::Target::import(&std::fs::read(path)?)?;
                    recipe.validate()?;
                    self.settings.room.recipe = recipe;
                    self.room_changed();
                }
            }

            BoostLimit | CutLimit => {
                let limit = match action {
                    BoostLimit => &mut self.settings.room.recipe.maximum_boost_db,
                    CutLimit => &mut self.settings.room.recipe.maximum_cut_db,
                    _ => unreachable!(),
                };
                *limit = if limit.is_some() {
                    None
                } else {
                    Some(if action == BoostLimit { 3.0 } else { 12.0 })
                };
                self.room_changed();
            }
            AutoLevel => {
                self.settings.room.recipe.target_level =
                    match self.settings.room.recipe.target_level {
                        correction::TargetLevel::Automatic => correction::TargetLevel::Fixed(0.0),
                        correction::TargetLevel::Fixed(_) => correction::TargetLevel::Automatic,
                    };
                self.room_changed();
            }
            LfDetail => {
                self.settings.room.acquisition.lf_detail =
                    if self.settings.room.acquisition.lf_detail.is_some() {
                        None
                    } else {
                        Some(measurement::LfDetail {
                            band: measurement::Band::new(
                                self.settings.room.acquisition.band.low,
                                self.settings.room.acquisition.band.high.min(300.0),
                            )?,
                            seconds: 16.0,
                        })
                    };
                self.save();
            }
            HandsFree => {
                self.settings.room.progression = match self.settings.room.progression {
                    workflow::Progression::Manual => workflow::Progression::HandsFree {
                        movement_seconds: 15.0,
                    },
                    _ => workflow::Progression::Manual,
                };
                self.save();
            }
            Alignment => self.open_picker(PickerKind::RoomChoice(ChoiceField::Alignment)),

            SetArea(area) => {
                self.settings.room.layout = match area {
                    self::Area::Single => measurement::Layout::default(),
                    self::Area::Listening => measurement::Layout::around_listener(),
                };
                self.save();
            }

            Listen | RestorePrevious => {
                let hash = if action == Listen {
                    self.room.candidate
                } else {
                    self.room.previous
                }
                .ok_or_else(|| invalid("No correction selected"))?;
                if self.room.return_to.is_some() {
                    self.stop_room_audition()?;
                }
                let (view, engine) = View::recall(
                    hash,
                    self.current_rate(),
                    self.settings.controls,
                    &self.store,
                )?;
                if self.room.muted {
                    self.resume_room_listening()?;
                }
                self.preview = Some((view, engine));
                self.replace_loaded();
            }

            SelectPass(id) => {
                self.room.selected = Some(id);
                if !self.room.running {
                    self.room.active_band = None;
                }
                self.room.plot_data = Some(self.room_plot_for(&self.room.history[id.0])?);
            }
            BackToCandidate => {
                self.stop_room_audition()?;
                self.room.selected = None;
                let candidate = self
                    .room
                    .history
                    .iter()
                    .rev()
                    .find(|p| p.correction() == self.room.candidate)
                    .or_else(|| self.room.history.last());
                if let Some(p) = candidate {
                    self.room.plot_data = Some(self.room_plot_for(p)?);
                }
            }
            Audition => {
                let id = self
                    .room
                    .selected
                    .ok_or_else(|| invalid("Select a pass to audition"))?;
                if self.room.return_to.as_ref().is_some_and(|r| r.pass == id) {
                    return self.stop_room_audition();
                }
                let filter = self.room_audition_filter(id)?;
                if filter.rate != self.current_rate() {
                    return Err(invalid(
                        "Exact history audition requires the recorded playback rate.",
                    ));
                }
                let (base, reference_db) = if let Some(restore) = &self.room.return_to {
                    (restore.controls, restore.reference_db)
                } else {
                    let mut reference_db = audition_level_db(&filter);
                    for pass in &self.room.history {
                        let candidate = self.room_audition_filter(pass.id)?;
                        if candidate.rate == self.current_rate() {
                            reference_db = reference_db.min(audition_level_db(&candidate));
                        }
                    }
                    let active: Correction =
                        serde_json::from_slice(&self.store.get(&self.active.hash)?)?;
                    reference_db = reference_db.min(audition_level_db(&active));
                    (self.settings.controls, reference_db)
                };
                let controls = audition_controls(base, &filter, reference_db)?;
                let (view, engine) = match self.room.history[id.0].correction() {
                    Some(hash) => View::from_managed(filter, hash, controls)?,
                    None => View::prepare(filter, controls, &self.store)?,
                };
                if self.room.muted {
                    self.resume_room_listening()?;
                }
                if let Some(restore) = &mut self.room.return_to {
                    restore.pass = id;
                } else {
                    self.room.return_to = Some(ListeningRestore {
                        correction: self.active.hash,
                        controls: base,
                        pass: id,
                        reference_db,
                    });
                }
                self.settings.controls = controls;
                self.controls_changed();
                self.preview = Some((view, engine));
                self.replace_loaded();
            }
            Prefer => {
                let id = self.room.selected.ok_or_else(|| invalid("Select a pass"))?;
                let mut session = self.room.manifest.clone().expect("history has manifest");
                let hash = session.prefer(id, &self.store)?;
                self.update_room_manifest(hash)?;
            }
            SavePass => {
                let id = self.room.selected.ok_or_else(|| invalid("Select a pass"))?;
                let hash = if let Some(id) = self.room.history[id.0].correction() {
                    id
                } else {
                    self.store.put(&serde_json::to_vec(&Correction::identity(
                        self.current_rate(),
                    ))?)?
                };
                self.room_preset_name(hash, format!("{} / pass {}", self.room.name, id.0));
            }
            SaveCandidate => {
                let hash = self
                    .room
                    .candidate
                    .expect("save candidate has a prepared filter");
                self.room_preset_name(
                    hash,
                    format!("{} / {}", self.room.name, self.room_recipe().target.label()),
                );
            }
            SaveMeasurement => {
                if self.room.session.is_none() {
                    return Err(invalid("Complete a measurement first"));
                }
                self.room_text(Text::MeasurementName);
                self.room.dialog = Some(self::Dialog::MeasurementName);
            }
            OpenMeasurement(id) => {
                self.stop_room_audition()?;
                self.cancel_preparation();
                self.open_room_session(id)?;
                self.restore_room_source_settings()?;
                self.room.stage = self::Stage::Correction;
                self.room.dialog = None;
                self.message="Reopened saved measurement evidence. Edit correction settings without measuring again.".into();
            }

            Graph => {}
            Export => {
                let id = self
                    .room
                    .session
                    .ok_or_else(|| invalid("Save a measurement first"))?;
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Export complete room session")
                    .pick_folder()
                {
                    workflow::export_session(
                        id,
                        &path.join(format!("room-session-{}", &id.as_str()[..8])),
                        &self.store,
                    )?;
                }
            }
            ExportFilter => {
                let id=match self.room.selected {
                    Some(pass)=>self.room.history[pass.0].correction(),
                    None=>self.room.candidate,
                }.ok_or_else(||invalid("The baseline has no correction filter to export. Select a correction pass."))?;
                if let Some(path) = rfd::FileDialog::new()
                    .set_file_name("room-correction.wav")
                    .save_file()
                {
                    workflow::export_filter(id, &path, &self.store)?;
                }
            }
            EditText(target) => self.room_text(target),
        }
        Ok(())
    }
    fn restore_room_source_settings(&mut self) -> Result<()> {
        let source = self.room.source.as_ref().expect("opened source");
        self.settings.room.layout = source.plan.layout.clone();
        self.settings.room.calibration = source.plan.calibration.clone();
        self.settings.room.gain = source.plan.gain.clone();
        self.settings.room.acquisition = source.plan.acquisition.clone();
        self.settings.room.microphone = Some(source.plan.microphone.clone());
        if let Some(p) = self.room.history.last() {
            let frozen: correction::Frozen =
                serde_json::from_slice(&self.store.get(&p.objective)?)?;
            self.settings.room.recipe = frozen.recipe;
        }

        Ok(())
    }

    fn stop_room_audition(&mut self) -> Result<()> {
        if let Some(restore) = self.room.return_to.take() {
            self.settings.controls = restore.controls;
            self.settings.active = Some(restore.correction);
            self.controls_changed();
            let (view, engine) = View::recall(
                restore.correction,
                self.current_rate(),
                restore.controls,
                &self.store,
            )?;
            self.preview = Some((view, engine));
            self.replace_loaded();
            if let Some(candidate) = self.room.candidate {
                let (view, engine) = View::recall(
                    candidate,
                    self.current_rate(),
                    self.settings.controls,
                    &self.store,
                )?;
                self.preview = Some((view, engine));
            }
        }
        Ok(())
    }

    fn room_measurement_plan(&self, kind: RunKind) -> Result<workflow::Plan> {
        let mut source = self.room_source()?;
        if !kind.acquires() {
            let original = &self
                .room
                .source
                .as_ref()
                .ok_or_else(|| invalid("Open a saved measurement before checking its correction"))?
                .plan;
            if !source.same_acquisition_path(original)? {
                return Err(invalid(
                    "Microphone, playback, gain, calibration or sweep settings changed. Restore the saved acquisition settings or measure a new baseline.",
                ));
            }
            let noise_limits = source.acquisition.noise_limits;
            source = original.clone();
            source.acquisition.noise_limits = noise_limits;
        }
        let levels = measurement::levels::Checks::from_reports(self.room.levels.clone(), &source)?;
        let prefs = &self.settings.room;
        let mut recipe = self.room_recipe().clone();
        if kind.acquires() {
            recipe.spatial = correction::SpatialPolicy::SourceLayout;
        }
        let plan = workflow::Plan {
            levels,
            source,
            recipe,
            progression: prefs.progression.clone(),
            kind,
            session: if kind.acquires() {
                None
            } else {
                self.room.session
            },
        };
        plan.validate()?;
        Ok(plan)
    }
    fn request_room_measurement(&mut self, kind: RunKind) -> Result<()> {
        let plan = self.room_measurement_plan(kind)?;
        if matches!(
            self.settings.room.calibration,
            measurement::Calibration::Uncalibrated { .. }
        ) {
            self.room.dialog = Some(Dialog::Uncalibrated(kind));
            self.room.settings_scroll = 0.0;
            return Ok(());
        }
        self.start_room_measurement(plan)
    }
    fn start_room_measurement(&mut self, plan: workflow::Plan) -> Result<()> {
        self.stop_room_audition()?;
        plan.validate()?;
        let stage = if plan.kind.acquires() {
            Stage::Measure
        } else {
            Stage::Correction
        };
        self.cancel_preparation();
        let snapshot = Box::new(plan.clone());
        let recipe = plan.recipe.clone();
        if plan.kind.acquires() {
            self.room.recovery = None;
        }
        self.submit_command(Command::Measure(Box::new(plan)))?;
        self.settings.room.recipe = recipe;
        self.room.active_plan = Some(snapshot);
        self.room.running = true;
        self.room.progress = None;
        self.room.active_band = None;
        self.room.latest_noise = None;
        self.room.dialog = None;
        self.room.settings_scroll = 0.0;
        self.room.stage = stage;
        self.message =
            "Measurement in progress. Keep the microphone still during each sweep.".into();
        Ok(())
    }
    fn room_audition_filter(&self, id: PassId) -> Result<Correction> {
        let filter = if let Some(hash) = self.room.history[id.0].correction() {
            serde_json::from_slice::<Correction>(&self.store.get(&hash)?)?
        } else {
            Correction::identity(self.current_rate())
        };
        filter.validate()?;
        Ok(filter)
    }

    fn room_source(&self) -> Result<measurement::SourcePlan> {
        let prefs = &self.settings.room;
        Ok(measurement::SourcePlan {
            route: self
                .settings
                .route
                .clone()
                .ok_or_else(|| invalid("Choose the normal audio route first"))?,
            microphone: prefs
                .microphone
                .clone()
                .ok_or_else(|| invalid("Choose a physical mono microphone"))?,
            rate: self.current_rate(),
            layout: prefs.layout.clone(),
            calibration: match &prefs.calibration {
                measurement::Calibration::Uncalibrated { .. } => {
                    measurement::Calibration::Uncalibrated { acknowledged: true }
                }
                calibrated => calibrated.clone(),
            },
            gain: prefs.gain.clone(),
            acquisition: prefs.acquisition.clone(),
        })
    }
    pub(super) fn room_levels(&mut self, report: measurement::levels::Report) {
        let speaker = report.speaker;
        self.room.progress = Some(Progress::Levels { speaker });
        self.message = format!(
            "{speaker:?}: input peak {:.1} dBFS, RMS {:.1} dBFS. Keep mic gain and speaker controls fixed.",
            20.0 * report.input_peak.log10(),
            report.rms_dbfs
        );
        self.room.levels[speaker.index()] = Some(report);
        self.room.running = false;
    }
    fn room_text(&mut self, target: Text) {
        let value = match target {
            Text::MeasurementName => self.room.name.clone(),
        };
        let mut field = TextField::new(value);
        field.select_all();
        self.text_focus = Some(TextFocus::Room {
            target,
            field,
            layout: None,
        });
    }
    pub(super) fn commit_room_text(&mut self) -> Result<()> {
        let Some(TextFocus::Room { target, field, .. }) = &self.text_focus else {
            unreachable!("focused room entry")
        };
        let target = *target;
        let value = field.text().trim().to_owned();
        if value.is_empty() || value.chars().count() > 100 {
            return Err(invalid("Use 1-100 characters"));
        }
        match target {
            Text::MeasurementName => {
                let id = self.room.session.expect("measurement to name");
                self.room.name = value.clone();
                self.settings.measurements.insert(id, value);
            }
        };
        self.text_focus = None;
        if target == Text::MeasurementName {
            self.room.dialog = None;
        }
        self.save();
        self.dirty = true;
        Ok(())
    }
    fn room_recipe(&self) -> &correction::Recipe {
        if matches!(
            self.dragging,
            Some(
                super::Drag::Room(_)
                    | super::Drag::ValueBar {
                        target: ValueBar::Room(_),
                        ..
                    }
            )
        ) {
            return &self.settings.room.recipe;
        }
        if self.room.selected.is_some()
            && let Some(plot) = &self.room.plot_data
        {
            &plot.recipe
        } else {
            &self.settings.room.recipe
        }
    }
    fn branch_room_recipe(&mut self) {
        if self.room.selected.is_some() {
            self.settings.room.recipe = self.room_recipe().clone();
        }
    }
    pub(super) fn room_value(&self, p: Parameter) -> f64 {
        use Parameter::*;
        let pref = &self.settings.room;
        let r = self.room_recipe();
        let a = &pref.acquisition;
        let (level, low_fade, high_fade) = match r.effort {
            correction::Effort::Easy {
                level,
                low_fade_octaves,
                high_fade_octaves,
            } => (level, low_fade_octaves, high_fade_octaves),
            correction::Effort::Advanced(ref points) => (
                points.iter().map(|p| p.effort).fold(0.0, f64::max),
                0.0,
                0.0,
            ),
        };
        match p {
            Low => r.range.low,
            High => r.range.high,
            Effort => level * 100.0,
            LowFade => low_fade,
            HighFade => high_fade,
            BassSmoothing => r.smoothing.bass_octaves,
            TrebleSmoothing => r.smoothing.treble_octaves,
            Crossover => r.smoothing.crossover_hz,
            Transition => r.smoothing.transition_octaves,
            Boost => r
                .maximum_boost_db
                .expect("boost control requires an enabled limiter"),
            Cut => r
                .maximum_cut_db
                .expect("cut control requires an enabled limiter"),
            Offset => match r.target_level {
                correction::TargetLevel::Fixed(db) => db,
                _ => 0.0,
            },
            ProbeLevel => pref.gain.probe.db(),
            Sweep => a.sweep_seconds,
            Tail => a.tail_seconds,
            Noise => a.noise_seconds,
            LfSeconds => a.lf_detail.as_ref().map_or(16.0, |lf| lf.seconds),
            LfHigh => a.lf_detail.as_ref().map_or(300.0, |lf| lf.band.high),
            MoveSeconds => match pref.progression {
                workflow::Progression::HandsFree { movement_seconds } => movement_seconds,
                _ => 15.0,
            },

            ExcitationLow => a.band.low,
            ExcitationHigh => a.band.high,

            Tilt => r.target.parameters().map_or(0.0, |p| p.tilt_db_octave),
            Bass => r.target.parameters().map_or(0.0, |p| p.bass_db),
            MidGain => r.target.parameters().map_or(0.0, |p| p.mid_gain_db),
            MidCenter => r.target.parameters().map_or(1000.0, |p| p.mid_center_hz),
            MidWidth => r.target.parameters().map_or(1.0, |p| p.mid_width_octaves),
        }
    }
    pub(super) fn room_number(&mut self, p: Parameter, value: f64) -> Result<()> {
        if self.room.running {
            return Err(invalid("The run owns frozen settings"));
        }
        use Parameter::*;

        let mut next = self.settings.room.clone();
        next.recipe = self.room_recipe().clone();
        let a = &mut next.acquisition;
        let r = &mut next.recipe;
        match p {
            Low => r.range = measurement::Band::new(value, r.range.high)?,
            High => r.range = measurement::Band::new(r.range.low, value)?,
            Effort | LowFade | HighFade => {
                let correction::Effort::Easy {
                    level,
                    low_fade_octaves,
                    high_fade_octaves,
                } = &mut r.effort
                else {
                    unreachable!("easy effort controls are shown only for easy effort")
                };
                match p {
                    Effort => *level = value / 100.0,
                    LowFade => *low_fade_octaves = value,
                    _ => *high_fade_octaves = value,
                }
            }
            BassSmoothing => r.smoothing.bass_octaves = value,
            TrebleSmoothing => r.smoothing.treble_octaves = value,
            Crossover => r.smoothing.crossover_hz = value,
            Transition => r.smoothing.transition_octaves = value,
            Boost => r.maximum_boost_db = Some(value),
            Cut => r.maximum_cut_db = Some(value),
            Offset => r.target_level = correction::TargetLevel::Fixed(value),
            ProbeLevel => next.gain.probe = measurement::ProbeLevel::try_from(value)?,
            Sweep => a.sweep_seconds = value,
            Tail => a.tail_seconds = value,
            Noise => a.noise_seconds = value,
            LfSeconds => {
                a.lf_detail
                    .as_mut()
                    .ok_or_else(|| invalid("Enable LF detail"))?
                    .seconds = value
            }
            LfHigh => {
                a.lf_detail
                    .as_mut()
                    .ok_or_else(|| invalid("Enable LF detail"))?
                    .band
                    .high = value
            }
            MoveSeconds => {
                if !(5.0..=120.0).contains(&value) {
                    return Err(invalid("Movement interval must be 5-120 seconds"));
                }
                next.progression = workflow::Progression::HandsFree {
                    movement_seconds: value,
                };
            }

            ExcitationLow | ExcitationHigh => {
                a.band = match p {
                    ExcitationLow => measurement::Band::new(value, a.band.high)?,
                    _ => measurement::Band::new(a.band.low, value)?,
                };
                if let Some(lf) = &mut a.lf_detail {
                    let low = lf.band.low.max(a.band.low);
                    let high = lf.band.high.min(a.band.high);
                    lf.band = measurement::Band::new(low, high).map_err(|_| {
                        invalid("Disable LF detail or keep an overlapping LF excitation range")
                    })?;
                }
            }

            Tilt | Bass | MidGain | MidCenter | MidWidth => {
                let mut parameters = r.target.parameters().ok_or_else(|| invalid(
                    "Imported targets use their curve points; choose a parametric room target to edit its shape",
                ))?;
                match p {
                    Tilt => parameters.tilt_db_octave = value,
                    Bass => parameters.bass_db = value,
                    MidGain => parameters.mid_gain_db = value,
                    MidCenter => parameters.mid_center_hz = value,
                    MidWidth => parameters.mid_width_octaves = value,
                    _ => unreachable!(),
                }
                r.target = correction::Target::Parametric(parameters);
            }
        }
        r.validate()?;
        a.validate(self.current_rate())?;
        next.layout.validate()?;
        self.settings.room = next;
        if matches!(
            self.dragging,
            Some(super::Drag::ValueBar {
                target: ValueBar::Room(_),
                ..
            })
        ) {
            self.dirty = true;
        } else if matches!(
            p,
            Low | High
                | Effort
                | LowFade
                | HighFade
                | BassSmoothing
                | TrebleSmoothing
                | Crossover
                | Transition
                | Boost
                | Cut
                | Offset
                | Tilt
                | Bass
                | MidGain
                | MidCenter
                | MidWidth
        ) {
            self.room_changed();
        } else {
            self.save();
        }
        Ok(())
    }
    fn room_changed(&mut self) {
        let parent = self
            .room
            .selected
            .or_else(|| self.room.history.last().map(|p| p.id));
        self.save();
        self.room.selected = None;
        self.message = "Preparing a new room candidate from saved measurement evidence.".into();
        if let Some(session) = self.room.session {
            let store = self.store.clone();
            let recipe = self.settings.room.recipe.clone();
            let controls = self.settings.controls;
            let rate = self.current_rate();
            self.job(move |cancel| {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(JobResult::Nothing);
                }
                let (session, _, correction) =
                    workflow::prepare_at_rate(session, recipe, parent, rate, &store)?;
                let (view, engine) = View::prepare(correction, controls, &store)?;
                Ok(JobResult::RoomPrepared(session, view, engine))
            });
        }
    }
    pub(super) fn open_room_session(&mut self, id: ContentHash) -> Result<()> {
        let session = Session::load(id, &self.store)?;
        let source = measurement::Source::load(session.source, &self.store)?;
        let history = session.history(&self.store)?;
        self.room.name = self
            .settings
            .measurements
            .get(&id)
            .cloned()
            .unwrap_or_else(|| self.room.name.clone());
        if self
            .room
            .source
            .as_ref()
            .is_none_or(|old| old.points.len() != source.points.len())
        {
            self.room.point_view = None;
        }
        let candidate = session.default_candidate(&history);
        let displayed = candidate
            .or(session.best)
            .and_then(|id| history.get(id.0))
            .or_else(|| history.last())
            .map(|p| p.id);
        self.room.candidate = candidate.and_then(|id| history[id.0].correction());
        self.room.source = Some(source);
        self.room.session = Some(id);
        self.room.history = history;
        self.room.manifest = Some(session);
        self.room.selected = None;
        self.room.plot_data = displayed
            .map(|id| self.room_plot_for(&self.room.history[id.0]))
            .transpose()?;
        Ok(())
    }
    pub(super) fn accept_room_preparation(&mut self, id: ContentHash) -> Result<()> {
        let mut prepared = Session::load(id, &self.store)?;
        if let Some(current) = &self.room.manifest {
            prepared.rebase_preference(current)?;
        }
        let id = prepared.save(&self.store)?;
        self.update_room_manifest(id)
    }
    pub(super) fn update_room_manifest(&mut self, id: ContentHash) -> Result<()> {
        let selected = self.room.selected;
        let source = self.room.manifest.as_ref().map(|s| s.source);
        let old = self.room.session;
        let next_source = Session::load(id, &self.store)?.source;
        let name = if source == Some(next_source) {
            old.and_then(|old| self.settings.measurements.remove(&old))
        } else {
            None
        };
        self.open_room_session(id)?;
        if source == self.room.manifest.as_ref().map(|s| s.source)
            && let Some(selected) = selected
        {
            self.room.selected = Some(selected);
            self.room.plot_data = Some(self.room_plot_for(&self.room.history[selected.0])?);
        }
        if let Some(name) = name {
            self.settings.measurements.insert(id, name);
            self.save();
        }
        Ok(())
    }
    pub(super) fn room_progress(&mut self, progress: Progress) {
        match &progress {
            Progress::Levels { .. } => self.room.active_band = None,
            Progress::Source { session } => {
                if let Err(error) = self.update_room_manifest(*session) {
                    self.message = error.to_string();
                }
            }
            Progress::Noise { noise, .. } => {
                if let Some(warning) = noise.quality_warning() {
                    self.message = warning;
                }
                self.room.latest_noise = Some(*noise.clone());
                self.room.active_band = None;
            }
            Progress::Pass { session, .. } => {
                if let Err(e) = self.update_room_manifest(*session) {
                    self.message = e.to_string();
                }
            }
            Progress::Fitting { band, .. } => {
                self.room.active_band = Some(*band);
                self.room.stage = Stage::Correction;
                if self.room.dialog.is_none() {
                    self.room.settings_scroll = 0.0;
                }
            }
            Progress::Sweep { band, .. } => {
                self.room.active_band = Some(*band);
                self.reload_room_recovery();
            }
            Progress::Finished { session, reason } => {
                self.room.running = false;
                self.room.active_plan = None;
                self.room.active_band = None;
                self.room.stage = Stage::Correction;
                if let Err(e) = self.update_room_manifest(*session) {
                    self.message = e.to_string();
                } else {
                    if *reason != workflow::StopReason::Prepared
                        && let Some(best) = self.room.manifest.as_ref().and_then(|s| s.best)
                    {
                        self.room.candidate = self.room.history[best.0].correction();
                        if self.room.selected.is_none() {
                            self.room.plot_data =
                                self.room_plot_for(&self.room.history[best.0]).ok();
                        }
                    }
                    self.message = if *reason == workflow::StopReason::Prepared {
                        "Correction prepared. Verification is needed before treating it as verified.".into()
                    } else {
                        format!(
                            "{}. The previous correction remains available.",
                            stop_label(*reason)
                        )
                    };
                }
            }
            Progress::Failed(error) => {
                self.room.running = false;
                self.room.active_plan = None;
                self.room.active_band = None;
                self.message = error.clone();
                self.reload_room_recovery();
            }
            Progress::Background { .. } => {
                self.room.active_band = None;
                self.room.latest_noise = None;
                self.reload_room_recovery();
            }
            Progress::Move { .. } => {
                self.room.active_band = None;
                self.reload_room_recovery();
            }
        }
        self.room.progress = Some(progress);
    }
    /// The saved acquisition checkpoint shows which captures are already kept.
    fn reload_room_recovery(&mut self) {
        match workflow::Recovery::load(&self.store) {
            Ok(recovery) => self.room.recovery = recovery,
            Err(error) => {
                self.message = format!("Could not read saved acquisition progress: {error}")
            }
        }
    }
    fn room_frequency(&self, x: f64) -> f64 {
        20.0 * 1000f64.powf(((x - self.room.graph.x0) / self.room.graph.width()).clamp(0.0, 1.0))
    }
    /// The graph handle under the cursor, if any.
    fn room_handle(&self) -> Option<Drag> {
        let overlay = Overlay::new(self.room.graph);
        let recipe = self.room_recipe();
        if overlay.strip.contains(self.cursor) {
            let (low, high) = recipe.smoothing.transition();
            let near = |hz: f64| (overlay.x(hz) - self.cursor.x).abs() < 7.0;
            return if near(recipe.smoothing.crossover_hz) {
                Some(Drag::Crossover)
            } else if near(low) || near(high) {
                Some(Drag::TransitionEdge)
            } else {
                None
            };
        }
        if !self.room.traces.contains(&Trace::Effort) {
            return None;
        }
        let near =
            |p: &correction::EffortPoint| overlay.point(p.hz, p.effort).distance(self.cursor) < 8.0;
        match &recipe.effort {
            correction::Effort::Easy {
                low_fade_octaves,
                high_fade_octaves,
                level,
            } => [
                (true, recipe.range.low * low_fade_octaves.exp2()),
                (false, recipe.range.high / high_fade_octaves.exp2()),
            ]
            .into_iter()
            .find(|&(_, hz)| near(&correction::EffortPoint { hz, effort: *level }))
            .map(|(lower, _)| Drag::Knee { lower }),
            correction::Effort::Advanced(points) => points.iter().position(near).map(Drag::Knot),
        }
    }
    pub(super) fn begin_room_drag(&mut self) {
        if self.room.running || self.room.plot_kind != PlotKind::RoomResponse {
            return;
        }
        self.branch_room_recipe();
        let handle = self.room_handle();
        let overlay = Overlay::new(self.room.graph);
        let hz = self.room_frequency(self.cursor.x);
        let recipe = &mut self.settings.room.recipe;
        let correction::Effort::Advanced(points) = &mut recipe.effort else {
            self.dragging = handle.map(super::Drag::Room);
            return;
        };
        match handle {
            Some(Drag::Knot(i)) if self.modifiers.alt_key() => {
                if points.len() > 2 {
                    points.remove(i);
                    self.room_changed();
                }
            }
            Some(handle) => self.dragging = Some(super::Drag::Room(handle)),
            None if self.room.traces.contains(&Trace::Effort) => {
                let range = recipe.range;
                let on_line = (overlay.y(recipe.effort.at(hz, range)) - self.cursor.y).abs() < 8.0;
                let correction::Effort::Advanced(points) = &mut recipe.effort else {
                    unreachable!("checked above")
                };
                let i = points.partition_point(|p| p.hz < hz);
                let free = points.get(i).is_none_or(|p| p.hz > hz * 1.01)
                    && i.checked_sub(1).is_none_or(|j| points[j].hz < hz / 1.01);
                if on_line && free && points.len() < correction::Effort::MAX_POINTS {
                    points.insert(
                        i,
                        correction::EffortPoint {
                            hz,
                            effort: overlay.effort(self.cursor.y),
                        },
                    );
                    self.dragging = Some(super::Drag::Room(Drag::Knot(i)));
                }
            }
            None => {}
        }
    }

    pub(super) fn room_drag(&mut self, drag: Drag, finished: bool) {
        let overlay = Overlay::new(self.room.graph);
        let hz = self.room_frequency(self.cursor.x);
        let effort = overlay.effort(self.cursor.y);
        let mut recipe = self.settings.room.recipe.clone();
        let range = recipe.range;
        match (drag, &mut recipe.effort) {
            (
                Drag::Knee { lower },
                correction::Effort::Easy {
                    level,
                    low_fade_octaves,
                    high_fade_octaves,
                },
            ) => {
                *level = effort;
                let (fade, other) = if lower {
                    ((hz / range.low).log2(), *high_fade_octaves)
                } else {
                    ((range.high / hz).log2(), *low_fade_octaves)
                };
                let fade = fade.clamp(0.0, (range.octaves() - other).max(0.0));
                if lower {
                    *low_fade_octaves = fade;
                } else {
                    *high_fade_octaves = fade;
                }
            }
            (Drag::Knot(i), correction::Effort::Advanced(points)) => {
                let low = i.checked_sub(1).map_or(20.0, |j| points[j].hz * 1.01);
                let high = points.get(i + 1).map_or(20000.0, |p| p.hz / 1.01);
                points[i] = correction::EffortPoint {
                    hz: hz.clamp(low, high),
                    effort,
                };
            }
            (Drag::Crossover, _) => {
                recipe.smoothing.crossover_hz = hz.clamp(20.0, 20000.0);
            }
            (Drag::TransitionEdge, _) => {
                recipe.smoothing.transition_octaves =
                    (2.0 * (hz / recipe.smoothing.crossover_hz).log2().abs()).min(8.0);
            }
            _ => unreachable!("effort handles match the effort mode"),
        }
        match recipe.validate() {
            Ok(()) => self.settings.room.recipe = recipe,
            Err(error) => self.message = error.to_string(),
        }
        self.dirty = true;
        if finished {
            self.room_changed();
        }
    }
    pub(super) fn room_scroll(&mut self, amount: f64) {
        if self.room.settings_bounds.contains(self.cursor) {
            self.room.settings_scroll = (self.room.settings_scroll - amount).clamp(
                0.0,
                (self.room.settings_height - self.room.settings_bounds.height()).max(0.0),
            );
        }
    }
}

struct MeasuredCurves {
    curves: [Curve; 2],
    band: measurement::Band,
}
struct ResponseTraces {
    baseline: [Curve; 2],
    predicted: [Curve; 2],
    measured: Option<MeasuredCurves>,
    parent: Option<MeasuredCurves>,
}
struct PlotData {
    recipe: correction::Recipe,
    target: Curve,
    filter: [Curve; 2],
    parent_filter: Option<[Curve; 2]>,
    analysis: Analysis,
    rate: Rate,
    offset: f32,
    unsmoothed: [Curve; 2],
    spread_db: [Vec<f32>; 2],
    spatial: ResponseTraces,
    points: Vec<ResponseTraces>,
    smoothed: Option<Smoothed>,
}
impl App {
    fn room_plot_for(&self, pass: &Pass) -> Result<PlotData> {
        let frozen: correction::Frozen = serde_json::from_slice(&self.store.get(&pass.objective)?)?;
        frozen.validate()?;
        let source = self
            .room
            .source
            .as_ref()
            .expect("pass view owns its source");
        let zero = || {
            std::array::from_fn(|_| Curve {
                frequency: frozen.target.frequency.clone(),
                db: vec![0.0; frozen.target.db.len()],
            })
        };
        let response = |pass: &Pass| -> Result<[Curve; 2]> {
            if let Some(id) = pass.correction() {
                let c: Correction = serde_json::from_slice(&self.store.get(&id)?)?;
                c.validate()?;
                Ok(c.response(481))
            } else {
                Ok(zero())
            }
        };
        let filter = response(pass)?;
        let exact = if let Some(id) = pass.correction() {
            serde_json::from_slice::<Correction>(&self.store.get(&id)?)?
        } else {
            Correction::identity(frozen.rate)
        };
        exact.validate()?;
        let analysis = Analysis::new(
            &exact,
            Controls {
                comparison: Comparison::Uncompensated,
                ..Controls::default()
            },
        );
        let parent = pass.parent.map(|id| &self.room.history[id.0]);
        let parent_filter = parent.map(&response).transpose()?;
        let raw = |curves: &[Curve; 2]| {
            std::array::from_fn(|c| Curve {
                frequency: curves[c].frequency.clone(),
                db: curves[c]
                    .db
                    .iter()
                    .map(|v| v - frozen.alignment.trim_db[c] as f32)
                    .collect(),
            })
        };
        let add = |baseline: &[Curve; 2], correction: &[Curve; 2]| {
            std::array::from_fn(|c| Curve {
                frequency: frozen.target.frequency.clone(),
                db: frozen
                    .target
                    .frequency
                    .iter()
                    .enumerate()
                    .map(|(i, &f)| baseline[c].db[i] + correction[c].at(f))
                    .collect(),
            })
        };
        let measured_at = |pass: &Pass, position: usize| {
            pass.measured
                .iter()
                .find(|pair| pair[0].position == position)
                .map(|pair| MeasuredCurves {
                    curves: pair.each_ref().map(|t| t.response.magnitude.clone()),
                    band: pair[0].response.trusted_band,
                })
        };
        let spatial_measurement = |pass: &Pass| -> Result<Option<MeasuredCurves>> {
            if frozen.baseline.points.len() == 1 {
                return Ok(measured_at(pass, 0));
            }
            if !matches!(pass.scope, Scope::SpatialFull { .. }) {
                return Ok(None);
            }
            let mut points = Vec::new();
            let mut weights = Vec::new();
            let mut band = None;
            for (position, &weight) in frozen.baseline.weights.iter().enumerate() {
                if weight == 0.0 {
                    continue;
                }
                let measured = measured_at(pass, position)
                    .ok_or_else(|| invalid("Spatial pass omits a required point"))?;
                band = Some(measured.band);
                points.push(measured.curves);
                weights.push(weight);
            }
            Ok(Some(MeasuredCurves {
                curves: correction::power_average(&points, &weights)?,
                band: band.expect("nonempty spatial evidence"),
            }))
        };
        let baseline = raw(&frozen.baseline.magnitude);
        let parent_spatial = if let Some(parent) = parent {
            spatial_measurement(parent)?.or_else(|| {
                parent_filter.as_ref().map(|filter| MeasuredCurves {
                    curves: add(&baseline, filter),
                    band: source.points[source.plan.layout.reference].full[0]
                        .response
                        .trusted_band,
                })
            })
        } else {
            None
        };
        let spatial = ResponseTraces {
            predicted: add(&baseline, &filter),
            baseline,
            measured: spatial_measurement(pass)?,
            parent: parent_spatial,
        };
        let points = frozen
            .baseline
            .points
            .iter()
            .enumerate()
            .map(|(position, point)| {
                let baseline = raw(point);
                let parent = parent.and_then(|p| measured_at(p, position)).or_else(|| {
                    parent_filter.as_ref().map(|filter| MeasuredCurves {
                        curves: add(&baseline, filter),
                        band: source.points[position].full[0].response.trusted_band,
                    })
                });
                ResponseTraces {
                    predicted: add(&baseline, &filter),
                    baseline,
                    measured: measured_at(pass, position),
                    parent,
                }
            })
            .collect();
        let unsmoothed = std::array::from_fn(|c| Curve {
            frequency: frozen.target.frequency.clone(),
            db: frozen.requested_goal[c]
                .db
                .iter()
                .zip(&frozen.baseline.magnitude[c].db)
                .map(|(&g, &b)| g - b + frozen.alignment.trim_db[c] as f32)
                .collect(),
        });
        Ok(PlotData {
            recipe: frozen.recipe,
            target: frozen.target,
            filter,
            parent_filter,
            analysis,
            rate: frozen.rate,
            offset: frozen.target_offset_db as f32,
            unsmoothed,
            spread_db: frozen.baseline.spread_db,
            spatial,
            points,
            smoothed: None,
        })
    }
    fn room_preset_name(&mut self, source: ContentHash, name: String) {
        let mut field = TextField::new(name);
        field.select_all();
        self.text_focus = Some(TextFocus::PresetName {
            source,
            field,
            layout: None,
        });
        self.room.dialog = Some(Dialog::PresetName);
    }
    pub(super) fn room_graph_cursor(&self) -> winit::window::CursorIcon {
        use winit::window::CursorIcon;
        if self.room.running || self.room.plot_kind != PlotKind::RoomResponse {
            return CursorIcon::Default;
        }
        match self.dragging {
            Some(super::Drag::Room(drag)) => Some(drag),
            _ => self.room_handle(),
        }
        .map_or(CursorIcon::Default, |handle| match handle {
            Drag::Crossover | Drag::TransitionEdge => CursorIcon::EwResize,
            Drag::Knee { .. } | Drag::Knot(_) => CursorIcon::Move,
        })
    }
    pub(super) fn room_choices(&self, field: ChoiceField) -> Vec<(String, super::Action, bool)> {
        let prefs = &self.settings.room;
        let choices: Vec<(Choice, &str, bool)> = match field {
            ChoiceField::Alignment => [
                correction::Alignment::None,
                correction::Alignment::Level,
                correction::Alignment::Time,
                correction::Alignment::LevelAndTime,
            ]
            .into_iter()
            .map(|alignment| {
                (
                    Choice::Alignment(alignment),
                    alignment_label(alignment),
                    alignment == self.room_recipe().alignment,
                )
            })
            .collect(),
            ChoiceField::Orientation => [
                measurement::Orientation::TowardSpeakers,
                measurement::Orientation::Upward,
            ]
            .into_iter()
            .map(|value| {
                let selected = matches!(
                    &prefs.calibration,
                    measurement::Calibration::Response { orientation, .. } if *orientation == value
                );
                (
                    Choice::Orientation(value),
                    orientation_label(value),
                    selected,
                )
            })
            .collect(),
        };
        choices
            .into_iter()
            .map(|(choice, label, selected)| {
                (
                    label.into(),
                    super::Action::Room(Action::Choose(choice)),
                    selected,
                )
            })
            .collect()
    }
    pub(super) fn begin_room_bar(&mut self) {
        self.branch_room_recipe();
    }
    pub(super) fn room_bar_from_fraction(&mut self, bar: Bar, fraction: f64) {
        let parameter = bar.parameter();
        let maximum = bar.maximum();
        let step = match bar {
            Bar::Effort => 1.0,
            Bar::BassSmoothing | Bar::TrebleSmoothing => 1.0 / 24.0,
        };
        let value = ((fraction.clamp(0.0, 1.0) * maximum / step).round() * step).min(maximum);
        if let Err(error) = self.room_number(parameter, value) {
            self.message = error.to_string();
        }
    }
    pub(super) fn finish_room_bar(&mut self) {
        self.room_changed();
    }
    pub(super) fn reset_room_bar(&mut self, bar: Bar) {
        let defaults = correction::Recipe::default();
        let value = match bar {
            Bar::Effort => 100.0,
            Bar::BassSmoothing => defaults.smoothing.bass_octaves,
            Bar::TrebleSmoothing => defaults.smoothing.treble_octaves,
        };
        if let Err(error) = self.room_number(bar.parameter(), value) {
            self.message = error.to_string();
        }
    }
    pub(super) fn room_scene(&mut self, size: Size, scale: f64) -> Scene {
        let chrome = self.window_chrome.expect("room workspace chrome");
        let colors = self.theme.current;
        let help_enabled = self.help_enabled();
        let route_editable = self.route_editable() && !self.room.running;
        if matches!(self.room.dialog, Some(Dialog::PresetName))
            && !matches!(self.text_focus, Some(TextFocus::PresetName { .. }))
            || matches!(self.room.dialog, Some(Dialog::MeasurementName))
                && !matches!(
                    self.text_focus,
                    Some(TextFocus::Room {
                        target: Text::MeasurementName,
                        ..
                    })
                )
        {
            self.room.dialog = None;
        }
        let top = chrome.height;
        let geometry = layout::RoomLayout::new(size, chrome);
        let navigation_y = geometry.navigation.y0;
        let bottom = geometry.status.y0;
        let center = geometry.actions;
        let content = geometry.body;
        let smoothing = self.room_recipe().smoothing;
        if let Some(data) = &mut self.room.plot_data {
            data.refresh_smoothed(smoothing, self.room.point_view);
        }
        let mut preferences = self.settings.room.clone();
        preferences.recipe = self.room_recipe().clone();
        if self.room.running
            && let Some(plan) = &self.room.active_plan
        {
            preferences.layout = plan.source.layout.clone();
            preferences.acquisition = plan.source.acquisition.clone();
            preferences.gain = plan.source.gain.clone();
            preferences.calibration = plan.source.calibration.clone();
        }
        let parameters: Vec<_> = ALL_PARAMETERS
            .iter()
            .filter(|&&p| match p {
                Parameter::Boost => preferences.recipe.maximum_boost_db.is_some(),
                Parameter::Cut => preferences.recipe.maximum_cut_db.is_some(),
                _ => true,
            })
            .map(|&p| (p, self.room_value(p)))
            .collect();
        let options = self.picker.as_ref().map(|_| self.filtered_picker_options());
        let status = room_status(&self.room, &preferences);
        let estimate = workflow::Estimate::for_preferences(&preferences, self.current_rate())
            .expect("room preferences are validated on every edit");
        let mut p = Painter::new(
            &mut self.fonts,
            &mut self.layout,
            &self.icons,
            scale,
            self.cursor,
            self.pressed,
            colors,
        );
        p.rect(size.to_rect(), colors.body);
        p.rect(Rect::new(0.0, 0.0, size.width, top), colors.bg);
        let title_y = (top - CONTROL) * 0.5;
        let mut title_x = chrome.controls_end + GAP;
        p.icon(
            Icon::Waveform,
            Rect::from_center_size((title_x + 15.0, top * 0.5), (ICON, ICON)),
            colors.fg,
        );
        title_x += 38.0;
        let title_width = p.advance("IRresponsible", Face::Title);
        p.text(
            "IRresponsible",
            Rect::new(title_x, 0.0, title_x + title_width, top),
            Face::Title,
            Align::Start,
            colors.fg,
        );
        title_x += title_width + INSET;
        let selector_width = p.advance("Measurements", Face::Label).ceil() + 36.0;
        p.titlebar_selector(
            Rect::new(
                title_x,
                title_y,
                title_x + selector_width,
                title_y + CONTROL,
            ),
            "Measurements",
            super::Action::Room(Action::Dialog(Some(Dialog::Measurements))),
            self.room.dialog == Some(Dialog::Measurements),
        );
        let status_left = title_x + selector_width + INSET;
        let transport = if self.room.running {
            room_status(&self.room, &preferences)
        } else if self.room.return_to.is_some() {
            "Audition / level matched".into()
        } else if self.room.muted {
            "Output muted".into()
        } else {
            match self.lifecycle {
                Lifecycle::Running {
                    rate,
                    estimated_latency_ms,
                } => format!("{} Hz / app ~{estimated_latency_ms:.1} ms", rate.hz()),
                Lifecycle::Starting => "Starting audio".into(),
                Lifecycle::Measuring { .. } => "Finishing measurement".into(),
                _ => "Audio stopped".into(),
            }
        };
        let palette_x = size.width - INSET - CONTROL;
        let bypass_x = palette_x - GAP - p.advance("Bypass", Face::Label).ceil() - INSET;
        let transport_label = if self.room.running {
            "Stop"
        } else if self.room.muted {
            "Resume"
        } else if matches!(self.lifecycle, Lifecycle::Running { .. }) {
            "Pause"
        } else {
            "Start"
        };
        let transport_x = bypass_x - GAP - p.advance(transport_label, Face::Label).ceil() - INSET;
        p.text(
            &transport,
            Rect::new(status_left, 0.0, transport_x - INSET, top),
            Face::Stat,
            Align::End,
            if matches!(self.lifecycle, Lifecycle::Running { .. }) {
                colors.basic[2]
            } else {
                colors.secondary
            },
        );
        p.button(
            transport_x,
            title_y,
            transport_label,
            if self.room.running {
                super::Action::Room(Action::Stop)
            } else if self.room.muted {
                super::Action::Room(Action::ResumeListening)
            } else {
                super::Action::StartStop
            },
            true,
        );
        p.chip(
            bypass_x,
            title_y,
            "Bypass",
            super::Action::Bypass,
            self.settings.controls.correction == CorrectionMode::Bypass,
            !self.room.running,
        );
        p.ghost_icon(
            Rect::new(
                size.width - 40.0,
                (top - CONTROL) / 2.0,
                size.width - 16.0,
                (top + CONTROL) / 2.0,
            ),
            Icon::Palette,
            super::Action::ThemePicker(
                match (self.settings.appearance.mode, self.system_appearance) {
                    (ThemeMode::Fixed, _) => ThemeSlot::Fixed,
                    (_, Appearance::Light) => ThemeSlot::Light,
                    (_, Appearance::Dark) => ThemeSlot::Dark,
                },
            ),
            !self.room.running,
        );

        let mut x = geometry.navigation.x0;
        for (label, stage) in [
            ("1  Measure", Stage::Measure),
            ("2  Correction", Stage::Correction),
        ] {
            x =
                p.chip(
                    x,
                    navigation_y,
                    label,
                    super::Action::Room(Action::Stage(stage)),
                    self.room.stage == stage,
                    true,
                )
                .x1 + GAP;
        }
        let back_label = "Headphones";
        let back_width = p.advance(back_label, Face::Label).ceil() + INSET;
        p.button(
            geometry.navigation.x1 - back_width,
            navigation_y,
            back_label,
            super::Action::Room(Action::Close),
            !self.room.running,
        );
        let scroll = if self.room.dialog.is_none() {
            self.room.settings_scroll
        } else {
            0.0
        };
        let origin = content.y0 - scroll;
        let start_hits = p.hits.len();
        p.scene.push_clip_layer(
            vello::peniko::Fill::NonZero,
            vello::kurbo::Affine::scale(scale),
            &content,
        );
        let view = SettingsView {
            preferences: &preferences,
            workspace: &self.room,
            preparing: self.job.is_some(),
            parameters: &parameters,
            measurements: &self.settings.measurements,
            route: self.settings.route.as_ref(),
            route_editable,
        };
        let (height, graph, scrollport) = match self.room.stage {
            Stage::Measure => (
                draw_measure(
                    &mut p,
                    content,
                    origin,
                    &view,
                    &mut self.text_focus,
                    self.peaks.0,
                ),
                Rect::ZERO,
                content,
            ),
            Stage::Correction => {
                let [graph_column, sidebar] = correction_columns(content);
                let plot = draw_correction(&mut p, graph_column, &view, &status);
                let height = draw_correction_settings(
                    &mut p,
                    sidebar,
                    sidebar.y0 - scroll,
                    &view,
                    &mut self.text_focus,
                );
                (height, plot, sidebar)
            }
        };
        self.room.graph = graph;
        p.scene.pop_layer();
        for (bounds, _) in &mut p.hits[start_hits..] {
            *bounds = bounds.intersect(content);
        }
        if self.room.dialog.is_none() {
            self.room.settings_bounds = scrollport;
            self.room.settings_height = height;
            self.room.settings_scroll = scroll.clamp(0.0, (height - scrollport.height()).max(0.0));
            let scrollport = Rect::new(
                scrollport.x0,
                scrollport.y0,
                size.width - 4.0,
                scrollport.y1,
            );
            if let Some(bar) = Scrollbar::new(scrollport, height, self.room.settings_scroll) {
                p.scrollbar(bar, super::Action::Scroll(ScrollSurface::RoomSettings, bar));
            }
        }
        let listening = self.room.listening_to_candidate(self.active.hash);
        draw_workspace_actions(
            &mut p,
            center,
            &self.room,
            &preferences,
            self.job.is_some(),
            listening,
            &estimate,
        );
        p.rect(Rect::new(0.0, bottom, size.width, size.height), colors.bg);
        p.text(
            &self.message,
            Rect::new(INSET, bottom, size.width - INSET, size.height),
            Face::Small,
            Align::Start,
            colors.secondary,
        );

        if let Some(dialog) = self.room.dialog {
            p.rect(size.to_rect(), opacity(vello::peniko::Color::BLACK, 0.5));
            p.hits.clear();
            let width = if dialog == Dialog::Measurements {
                600.0
            } else {
                520.0
            };
            let card = Rect::from_center_size(
                (size.width / 2.0, (size.height + top) / 2.0),
                (
                    width,
                    (size.height - top - 48.0).min(match dialog {
                        Dialog::MeasurementName | Dialog::PresetName => 184.0,
                        Dialog::Uncalibrated(_) => 232.0,
                        Dialog::Verification => 260.0,
                        Dialog::Measurements => {
                            (224.0 + self.settings.measurements.len() as f64 * 48.0).min(620.0)
                        }
                    }),
                ),
            );
            p.rounded(card, 8.0, colors.body);
            p.outline(card, 8.0, RULE, colors.ghost);
            let title = match dialog {
                Dialog::Uncalibrated(_) => "Measure without microphone calibration?",
                Dialog::Verification => "Measure result",
                Dialog::Measurements => "Measurements",
                Dialog::MeasurementName => "Save measurement source",
                Dialog::PresetName => "Save correction preset",
            };
            p.text(
                title,
                Rect::new(
                    card.x0 + INSET,
                    card.y0 + 8.0,
                    card.x1 - 48.0,
                    card.y0 + 32.0,
                ),
                Face::Title,
                Align::Start,
                colors.fg,
            );
            p.ghost_icon(
                Rect::new(
                    card.x1 - 40.0,
                    card.y0 + 8.0,
                    card.x1 - 16.0,
                    card.y0 + 32.0,
                ),
                Icon::X,
                super::Action::Room(Action::Dialog(None)),
                true,
            );
            p.line(
                (card.x0, card.y0 + 40.0),
                (card.x1, card.y0 + 40.0),
                colors.pane(),
                RULE,
            );
            let bounds = Rect::new(
                card.x0 + INSET,
                card.y0 + 48.0,
                card.x1 - INSET,
                card.y1 - 56.0,
            );
            let scrollport = Rect::new(bounds.x0, bounds.y0, card.x1 - 4.0, bounds.y1);
            let view = SettingsView {
                preferences: &preferences,
                workspace: &self.room,
                preparing: self.job.is_some(),
                parameters: &parameters,
                measurements: &self.settings.measurements,
                route: self.settings.route.as_ref(),
                route_editable,
            };
            let start_hits = p.hits.len();
            p.scene.push_clip_layer(
                vello::peniko::Fill::NonZero,
                vello::kurbo::Affine::scale(scale),
                &bounds,
            );
            let height = draw_settings(
                &mut p,
                bounds,
                bounds.y0 - self.room.settings_scroll,
                &view,
                &mut self.text_focus,
            );
            p.scene.pop_layer();
            for (r, _) in &mut p.hits[start_hits..] {
                *r = r.intersect(bounds);
            }
            self.room.settings_bounds = scrollport;
            self.room.settings_height = height;
            self.room.settings_scroll = self
                .room
                .settings_scroll
                .clamp(0.0, (height - bounds.height()).max(0.0));
            if let Some(bar) = Scrollbar::new(scrollport, height, self.room.settings_scroll) {
                p.scrollbar(bar, super::Action::Scroll(ScrollSurface::RoomSettings, bar));
            }
            p.line(
                (card.x0, card.y1 - 48.0),
                (card.x1, card.y1 - 48.0),
                colors.pane(),
                RULE,
            );
            let y = card.y1 - 36.0;
            p.button(
                card.x0 + INSET,
                y,
                if self.room.running {
                    "Stop"
                } else if matches!(dialog, Dialog::Uncalibrated(_)) {
                    "Cancel"
                } else {
                    "Close"
                },
                super::Action::Room(if self.room.running {
                    Action::Stop
                } else {
                    Action::Dialog(None)
                }),
                true,
            );
            let mut right = card.x1 - INSET;
            let mut trailing =
                |label: &str, action: super::Action, primary: bool, enabled: bool| {
                    let width = p.advance(label, Face::Label).ceil() + INSET;
                    let bounds = Rect::new(right - width, y, right, y + CONTROL);
                    p.button_at(bounds, label, action, primary, enabled);
                    right = bounds.x0 - GAP;
                };
            match dialog {
                Dialog::Uncalibrated(kind) => {
                    trailing(
                        "Measure without calibration",
                        super::Action::Room(Action::ContinueUncalibrated(kind)),
                        true,
                        !self.room.running,
                    );
                    trailing(
                        "Load calibration...",
                        super::Action::Room(Action::Calibration),
                        false,
                        !self.room.running,
                    );
                }
                Dialog::Verification => {
                    let enabled = self.room.candidate.is_some() && !self.room.running;
                    trailing(
                        "Check reference",
                        super::Action::Room(Action::Verify),
                        true,
                        enabled,
                    );
                    trailing(
                        "Check all positions",
                        super::Action::Room(Action::VerifySpatial),
                        false,
                        enabled,
                    );
                }
                Dialog::Measurements => trailing(
                    "Save measurement...",
                    super::Action::Room(Action::SaveMeasurement),
                    false,
                    self.room.session.is_some(),
                ),
                Dialog::MeasurementName => {
                    trailing("Save", super::Action::Room(Action::CommitName), true, true)
                }
                Dialog::PresetName => trailing("Save", super::Action::CommitPresetName, true, true),
            }
        }
        if let (Some(picker), Some(options)) = (&mut self.picker, options) {
            Self::draw_picker(&mut p, picker, &options, size.to_rect(), chrome);
        }
        let hint = if help_enabled {
            help::target(&p.hits, self.cursor, false)
        } else {
            None
        };
        self.help.draw(&mut p, hint, self.cursor, size, top);
        self.hits = p.hits;
        self.dirty = false;
        p.scene
    }
}

const ALL_PARAMETERS: &[Parameter] = &[
    Parameter::Low,
    Parameter::High,
    Parameter::Effort,
    Parameter::LowFade,
    Parameter::HighFade,
    Parameter::BassSmoothing,
    Parameter::TrebleSmoothing,
    Parameter::Crossover,
    Parameter::Transition,
    Parameter::Boost,
    Parameter::Cut,
    Parameter::Offset,
    Parameter::ProbeLevel,
    Parameter::Sweep,
    Parameter::Tail,
    Parameter::Noise,
    Parameter::LfSeconds,
    Parameter::LfHigh,
    Parameter::MoveSeconds,
    Parameter::ExcitationLow,
    Parameter::ExcitationHigh,
    Parameter::Tilt,
    Parameter::Bass,
    Parameter::MidGain,
    Parameter::MidCenter,
    Parameter::MidWidth,
];
fn draw_parameter(
    p: &mut Painter<'_, super::Action>,
    bounds: Rect,
    parameter: Parameter,
    value: f64,
    focus: &mut Option<TextFocus>,
    enabled: bool,
) {
    let label = Rect::new(bounds.x0, bounds.y0, bounds.x1 - 116.0, bounds.y1);
    p.text(
        parameter.label(),
        label,
        Face::Label,
        Align::Start,
        p.colors.fg,
    );
    let stepper = Rect::new(bounds.x1 - 112.0, bounds.y0, bounds.x1, bounds.y1);
    if enabled {
        App::draw_number(
            p,
            stepper,
            NumberField::Room(parameter),
            &parameter.format(value),
            focus,
        );
    } else {
        p.text(
            &parameter.format(value),
            stepper,
            Face::Value,
            Align::End,
            p.colors.secondary,
        );
    }
}
struct SettingsView<'a> {
    preferences: &'a measurement::Preferences,
    workspace: &'a Workspace,
    preparing: bool,
    parameters: &'a [(Parameter, f64)],
    measurements: &'a std::collections::BTreeMap<ContentHash, String>,
    route: Option<&'a Route>,
    /// Playback can change only while listening streams are stopped.
    route_editable: bool,
}
fn alignment_label(alignment: correction::Alignment) -> &'static str {
    match alignment {
        correction::Alignment::None => "Off",
        correction::Alignment::Level => "Level",
        correction::Alignment::Time => "Time",
        correction::Alignment::LevelAndTime => "Level and time",
    }
}
fn orientation_label(orientation: measurement::Orientation) -> &'static str {
    match orientation {
        measurement::Orientation::TowardSpeakers => "Toward speakers / 0 degrees",
        measurement::Orientation::Upward => "Upward / 90 degrees",
    }
}
struct Form {
    bounds: Rect,
    y: f64,
    enabled: bool,
}
impl Form {
    fn new(bounds: Rect, y: f64, enabled: bool) -> Self {
        Self { bounds, y, enabled }
    }
    fn row(&self) -> Rect {
        Rect::new(self.bounds.x0, self.y, self.bounds.x1, self.y + CONTROL)
    }
    fn number(
        &mut self,
        p: &mut Painter<'_, super::Action>,
        view: &SettingsView<'_>,
        parameter: Parameter,
        focus: &mut Option<TextFocus>,
    ) {
        draw_parameter(
            p,
            self.row(),
            parameter,
            parameter_value(view, parameter),
            focus,
            self.enabled,
        );
        self.y += ROW;
    }
    fn toggle(
        &mut self,
        p: &mut Painter<'_, super::Action>,
        label: &str,
        action: Action,
        on: bool,
    ) {
        p.toggle(
            self.row(),
            label,
            super::Action::Room(action),
            on,
            self.enabled,
        );
        self.y += ROW;
    }
    fn dropdown(
        &mut self,
        p: &mut Painter<'_, super::Action>,
        label: &str,
        value: &str,
        field: ChoiceField,
    ) {
        let row = self.row();
        let width = (p.advance(value, Face::Label).ceil() + 36.0).min(row.width() * 0.6);
        p.text(
            label,
            Rect::new(row.x0, row.y0, row.x1 - width - GAP, row.y1),
            Face::Label,
            Align::Start,
            p.colors.fg,
        );
        p.dropdown(
            Rect::new(row.x1 - width, row.y0, row.x1, row.y1),
            value,
            super::Action::Room(field.action()),
            self.enabled,
        );
        self.y += ROW;
    }
    fn button(&mut self, p: &mut Painter<'_, super::Action>, label: &str, action: Action) {
        p.button(
            self.bounds.x0,
            self.y,
            label,
            super::Action::Room(action),
            self.enabled,
        );
        self.y += ROW;
    }
    fn chips<'a>(
        &mut self,
        p: &mut Painter<'_, super::Action>,
        chips: impl Iterator<Item = (&'a str, Action, bool)>,
    ) {
        let mut x = self.bounds.x0;
        for (label, action, selected) in chips {
            let width = p.advance(label, Face::Label).ceil() + INSET;
            if x > self.bounds.x0 && x + width > self.bounds.x1 {
                x = self.bounds.x0;
                self.y += ROW;
            }
            x =
                p.chip(
                    x,
                    self.y,
                    label,
                    super::Action::Room(action),
                    selected,
                    self.enabled,
                )
                .x1 + GAP;
        }
        self.y += ROW;
    }
    fn section_with_badge(
        &mut self,
        p: &mut Painter<'_, super::Action>,
        title: &str,
        icon: Icon,
        label: &str,
        tone: Tone,
    ) {
        let start = self.y;
        self.section(p, title, icon);
        let y = self.y - CONTROL - GAP;
        debug_assert!(y >= start);
        badge(
            p,
            Rect::new(self.bounds.x0, y, self.bounds.x1, y + CONTROL),
            label,
            tone,
        );
    }
    fn section(&mut self, p: &mut Painter<'_, super::Action>, title: &str, icon: Icon) {
        if self.y > self.bounds.y0 {
            p.line(
                (self.bounds.x0, self.y + 2.0),
                (self.bounds.x1, self.y + 2.0),
                p.colors.pane(),
                RULE,
            );
            self.y += 14.0;
        }
        let row = self.row();
        p.icon(
            icon,
            Rect::from_center_size((row.x0 + CONTROL * 0.5, row.center().y), (ICON, ICON)),
            p.colors.fg,
        );
        p.text(
            title,
            Rect::new(row.x0 + CONTROL + GAP, row.y0, row.x1, row.y1),
            Face::Label,
            Align::Start,
            p.colors.fg,
        );
        self.y += CONTROL + GAP;
    }
    fn note(&mut self, p: &mut Painter<'_, super::Action>, text: &str) {
        let mut line = String::new();
        for word in text.split_whitespace() {
            let next = if line.is_empty() {
                word.into()
            } else {
                format!("{line} {word}")
            };
            if !line.is_empty() && p.advance(&next, Face::Small) > self.bounds.width() {
                p.text(
                    &line,
                    self.row(),
                    Face::Small,
                    Align::Start,
                    p.colors.secondary,
                );
                self.y += 18.0;
                line = word.into();
            } else {
                line = next;
            }
        }
        if !line.is_empty() {
            p.text(
                &line,
                self.row(),
                Face::Small,
                Align::Start,
                p.colors.secondary,
            );
            self.y += 24.0;
        }
    }
    fn fold(
        &mut self,
        p: &mut Painter<'_, super::Action>,
        w: &Workspace,
        section: Section,
        label: &str,
    ) -> bool {
        let open = w.expanded.contains(&section);
        let row = self.row();
        p.ghost_icon(
            Rect::new(row.x0, row.y0, row.x0 + CONTROL, row.y1),
            if open {
                Icon::CaretDown
            } else {
                Icon::CaretRight
            },
            super::Action::Room(Action::Fold(section)),
            true,
        );
        p.text(
            label,
            Rect::new(row.x0 + CONTROL + GAP, row.y0, row.x1, row.y1),
            Face::Label,
            Align::Start,
            p.colors.fg,
        );
        p.hits
            .push((row, super::Action::Room(Action::Fold(section))));
        self.y += ROW;
        open
    }
}
fn draw_recovery(p: &mut Painter<'_, super::Action>, form: &mut Form, workspace: &Workspace) {
    if workspace.running {
        return;
    }
    if let Some(workflow::Recovery::Acquisition(checkpoint)) = &workspace.recovery {
        let complete = checkpoint
            .points
            .iter()
            .flat_map(|p| p.full.iter())
            .filter(|take| take.is_some())
            .count();
        form.section(p, "Interrupted measurement", Icon::Waveform);
        form.note(
            p,
            &format!(
                "{} / {} positions / {} of {} full-range speaker captures saved",
                checkpoint.plan.microphone.label,
                checkpoint.plan.layout.positions.len(),
                complete,
                checkpoint.points.len() * 2
            ),
        );
        form.button(p, "Resume saved acquisition", Action::ResumeAcquisition);
        form.note(p,"Resume restores its saved microphone, levels, sweep settings and locations. Starting a new measurement begins a fresh acquisition.");
    }
}
/// Input peak on a -60..0 dBFS scale with the hot zone and the clipping edge marked.
fn draw_level_meter(p: &mut Painter<'_, super::Action>, bounds: Rect, w: &Workspace, live: f32) {
    const FLOOR_DB: f32 = -60.0;
    let colors = p.colors;
    let live_capture = w.running
        && matches!(
            w.progress,
            Some(Progress::Levels { .. } | Progress::Background { .. } | Progress::Sweep { .. })
        );
    let bar = Rect::new(bounds.x0, bounds.y0 + 4.0, bounds.x1, bounds.y0 + 12.0);
    let x = |peak: f32| {
        let db = 20.0 * peak.max(1e-12).log10();
        bar.x0 + bar.width() * f64::from(((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0))
    };
    let tone = |peak: f32| {
        if peak >= measurement::io::INPUT_CLIPPING_PEAK {
            Tone::Bad
        } else if peak >= measurement::levels::RECOMMENDED_CHECK_PEAK {
            Tone::Warning
        } else {
            Tone::Good
        }
    };
    p.rounded(bar, 2.0, colors.ghost);
    let hot = x(measurement::levels::RECOMMENDED_CHECK_PEAK);
    p.rect(
        Rect::new(hot, bar.y0, bar.x1, bar.y1),
        opacity(Tone::Warning.color(colors), 0.25),
    );
    p.line(
        (bar.x1, bounds.y0),
        (bar.x1, bar.y1 + 4.0),
        Tone::Bad.color(colors),
        2.0,
    );
    let label = |p: &mut Painter<'_, super::Action>, text: String, at: Rect, align: Align, ink| {
        p.text(&text, at, Face::Small, align, ink)
    };
    let caption = Rect::new(bounds.x0, bar.y1 + 4.0, bounds.x1, bounds.y1);
    if live_capture {
        p.rounded(
            Rect::new(bar.x0, bar.y0, x(live), bar.y1),
            2.0,
            tone(live).color(colors),
        );
        let db = 20.0 * live.max(1e-12).log10();
        label(
            p,
            if db < FLOOR_DB {
                format!("Input peak below {FLOOR_DB:.0} dBFS")
            } else {
                format!("Input peak {db:.1} dBFS")
            },
            caption,
            Align::Start,
            colors.secondary,
        );
    } else if w.running {
        label(
            p,
            "Microphone idle / output silent".into(),
            caption,
            Align::Start,
            colors.secondary,
        );
    } else {
        let mut text = Vec::new();
        for speaker in measurement::Speaker::ALL {
            let Some(report) = &w.levels[speaker.index()] else {
                continue;
            };
            let at = x(report.input_peak);
            p.line(
                (at, bar.y0 - 3.0),
                (at, bar.y1 + 3.0),
                tone(report.input_peak).color(colors),
                2.0,
            );
            text.push(format!(
                "{} {:.1} dBFS",
                speaker_name(speaker),
                20.0 * report.input_peak.log10()
            ));
        }
        if text.is_empty() {
            text.push("No level check yet".into());
        }
        label(p, text.join(" / "), caption, Align::Start, colors.secondary);
    }
    label(
        p,
        "Hot above -6 / clip at 0 dBFS".into(),
        caption,
        Align::End,
        colors.secondary,
    );
}
/// How a state badge reads at a glance.
#[derive(Clone, Copy)]
enum Tone {
    Neutral,
    Good,
    Warning,
    Bad,
}
impl Tone {
    fn color(self, colors: appearance::Colors) -> vello::peniko::Color {
        match self {
            Self::Neutral => colors.secondary,
            Self::Good => colors.basic[2],
            Self::Warning => colors.basic[3],
            Self::Bad => colors.basic[1],
        }
    }
}
fn badge(p: &mut Painter<'_, super::Action>, row: Rect, label: &str, tone: Tone) {
    let ink = tone.color(p.colors);
    let width = p.advance(label, Face::Small).ceil() + 2.0 * GAP;
    let bounds = Rect::new(row.x1 - width, row.y0 + 2.0, row.x1, row.y1 - 2.0);
    p.rounded(bounds, RADIUS, opacity(ink, 0.16));
    p.text(label, bounds, Face::Small, Align::Center, ink);
}

/// One capture of the acquisition sequence at a position, in recording order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Capture {
    Noise,
    Full(measurement::Speaker),
    Bass(measurement::Speaker),
    Repeat(measurement::Speaker),
}
impl Capture {
    fn sequence(lf: bool, reference: bool) -> impl Iterator<Item = Self> {
        let speakers = |take: fn(measurement::Speaker) -> Self| measurement::Speaker::ALL.map(take);
        std::iter::once(Self::Noise)
            .chain(speakers(Self::Full))
            .chain(lf.then(|| speakers(Self::Bass)).into_iter().flatten())
            .chain(
                reference
                    .then(|| speakers(Self::Repeat))
                    .into_iter()
                    .flatten(),
            )
    }
    fn saved(
        self,
        point: &workflow::PointCheckpoint,
        repeat: &[Option<measurement::Take>; 2],
    ) -> bool {
        match self {
            Self::Noise => point.noise.is_some(),
            Self::Full(s) => point.full[s.index()].is_some(),
            Self::Bass(s) => point.lf[s.index()].is_some(),
            Self::Repeat(s) => repeat[s.index()].is_some(),
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum CaptureState {
    Pending,
    Recording,
    Saved,
}
/// Capture states per position of the acquisition shown on the Measure stage.
struct Captures {
    lf: bool,
    states: Vec<Vec<(Capture, CaptureState)>>,
}
impl Captures {
    fn new(w: &Workspace, layout: &measurement::Layout, lf: bool) -> Self {
        let checkpoint = match &w.recovery {
            Some(workflow::Recovery::Acquisition(checkpoint))
                if checkpoint.points.len() == layout.positions.len() =>
            {
                Some(checkpoint)
            }
            _ => None,
        };
        let completed = !w.running
            && checkpoint.is_none()
            && w.source
                .as_ref()
                .is_some_and(|s| s.points.len() == layout.positions.len());
        let recording = match w.progress {
            Some(Progress::Background { position } | Progress::Sweep { position, .. })
                if w.running && w.active_plan.as_ref().is_some_and(|p| p.kind.acquires()) =>
            {
                Some(position)
            }
            _ => None,
        };
        let states = (0..layout.positions.len())
            .map(|position| {
                let mut current = recording == Some(position);
                Capture::sequence(lf, position == layout.reference)
                    .map(|capture| {
                        let saved = completed
                            || checkpoint.is_some_and(|c| {
                                capture.saved(&c.points[position], &c.repeat_reference)
                            });
                        let state = if saved {
                            CaptureState::Saved
                        } else if current {
                            current = false;
                            CaptureState::Recording
                        } else {
                            CaptureState::Pending
                        };
                        (capture, state)
                    })
                    .collect()
            })
            .collect();
        Self { lf, states }
    }
    fn position_done(&self, position: usize) -> bool {
        self.states[position]
            .iter()
            .all(|(_, s)| *s == CaptureState::Saved)
    }
    fn state(&self, position: usize, capture: Capture) -> Option<CaptureState> {
        self.states[position]
            .iter()
            .find(|(c, _)| *c == capture)
            .map(|(_, s)| *s)
    }
}

/// The position the run is at or asks for next.
fn active_position(w: &Workspace) -> Option<usize> {
    match w.progress {
        Some(Progress::Move { movement, .. }) => Some(movement.position()),
        Some(Progress::Background { position } | Progress::Sweep { position, .. }) if w.running => {
            Some(position)
        }
        _ => None,
    }
}

/// Merged preparation and acquisition stage. Returns the content height.
fn draw_measure(
    p: &mut Painter<'_, super::Action>,
    bounds: Rect,
    top: f64,
    view: &SettingsView<'_>,
    focus: &mut Option<TextFocus>,
    peak: f32,
) -> f64 {
    let w = view.workspace;
    let prefs = view.preferences;
    let mut y = top;
    if w.running || matches!(w.progress, Some(Progress::Failed(_))) {
        draw_run_banner(
            p,
            Rect::new(bounds.x0, y, bounds.x1, y + 76.0),
            w,
            prefs,
            &room_status(w, prefs),
        );
        y += 88.0;
    }
    let [left, right] = layout::RoomLayout::columns(Rect::new(bounds.x0, y, bounds.x1, bounds.y1));
    let left = draw_preparation(p, left, view, focus, peak);
    let right = draw_positions(p, right, view, focus);
    left.max(right) - top + INSET
}

/// Route, calibration, speaker level and acquisition settings. Returns the bottom edge.
fn draw_preparation(
    p: &mut Painter<'_, super::Action>,
    bounds: Rect,
    view: &SettingsView<'_>,
    focus: &mut Option<TextFocus>,
    peak: f32,
) -> f64 {
    let w = view.workspace;
    let prefs = view.preferences;
    let mut f = Form::new(bounds, bounds.y0, !w.running);
    draw_recovery(p, &mut f, w);

    let microphone = prefs.microphone.as_ref();
    let playback = view.route.map(|r| &r.playback);
    let (label, tone) = match (microphone, playback) {
        (None, _) => ("Choose microphone", Tone::Warning),
        (_, None) => ("Choose speakers", Tone::Warning),
        _ => ("Ready", Tone::Good),
    };
    f.section_with_badge(p, "Route", Icon::PlugsConnected, label, tone);
    for (caption, device, channel, device_action, channel_action, device_enabled) in [
        (
            "Microphone",
            microphone.map_or("Choose microphone", |m| m.label.as_str()),
            microphone.map_or_else(|| "--".into(), |m| (m.channels.0 + 1).to_string()),
            super::Action::Room(Action::Microphone),
            super::Action::Room(Action::MicrophoneChannel),
            !w.running,
        ),
        (
            "Speakers",
            playback.map_or("Choose playback", |d| d.label.as_str()),
            playback.map_or_else(
                || "--".into(),
                |d| {
                    let [l, r] = d.channels.indices();
                    format!("{} / {}", l + 1, r + 1)
                },
            ),
            super::Action::Playback,
            super::Action::PlaybackPair,
            view.route_editable,
        ),
    ] {
        let row = f.row();
        let caption_width = 84.0;
        p.text(
            caption,
            Rect::new(row.x0, row.y0, row.x0 + caption_width, row.y1),
            Face::Label,
            Align::Start,
            p.colors.secondary,
        );
        let (device_bounds, channel_bounds) =
            layout::Layout::split_route(Rect::new(row.x0 + caption_width, row.y0, row.x1, row.y1));
        p.dropdown(device_bounds, device, device_action, device_enabled);
        p.dropdown(
            channel_bounds,
            &channel,
            channel_action,
            device_enabled && device != "Choose microphone" && device != "Choose playback",
        );
        f.y += ROW;
    }
    if !view.route_editable && !w.running {
        f.note(p, "Pause listening to change the speaker route.");
    }

    let (label, tone) = match &prefs.calibration {
        measurement::Calibration::Uncalibrated { .. } => ("Uncalibrated", Tone::Warning),
        measurement::Calibration::Response { .. } => ("Calibrated", Tone::Good),
    };
    f.section_with_badge(
        p,
        "Microphone calibration",
        Icon::PlugsConnected,
        label,
        tone,
    );
    match &prefs.calibration {
        measurement::Calibration::Uncalibrated { .. } => {
            f.note(
                p,
                "Without a response file the microphone's own coloration ends up in the correction.",
            );
            f.button(p, "Load calibration file...", Action::Calibration);
        }
        measurement::Calibration::Response {
            orientation,
            metadata,
            ..
        } => {
            if let Some(name) = metadata.first() {
                f.note(p, name);
            }
            f.dropdown(
                p,
                "Orientation",
                orientation_label(*orientation),
                ChoiceField::Orientation,
            );
            let end = p
                .button(
                    bounds.x0,
                    f.y,
                    "Replace...",
                    super::Action::Room(Action::Calibration),
                    f.enabled,
                )
                .x1;
            p.button(
                end + GAP,
                f.y,
                "Remove",
                super::Action::Room(Action::RemoveCalibration),
                f.enabled,
            );
            f.y += ROW;
        }
    }

    let (label, tone) = level_badge(&w.levels);
    f.section_with_badge(p, "Speaker level", Icon::Waveform, label, tone);
    f.number(p, view, Parameter::ProbeLevel, focus);
    draw_level_meter(p, Rect::new(bounds.x0, f.y, bounds.x1, f.y + 40.0), w, peak);
    f.y += 48.0;
    let end = p
        .button(
            bounds.x0,
            f.y,
            "Check left",
            super::Action::Room(Action::CheckLevel(measurement::Speaker::Left)),
            f.enabled,
        )
        .x1;
    p.button(
        end + GAP,
        f.y,
        "Check right",
        super::Action::Room(Action::CheckLevel(measurement::Speaker::Right)),
        f.enabled,
    );
    f.y += ROW + 4.0;
    f.note(
        p,
        "Optional. Begin quietly; disable direct monitoring and voice processing on the microphone input.",
    );

    f.y += GAP;
    if f.fold(p, w, Section::Acquisition, "Measurement settings") {
        for parameter in [Parameter::Sweep, Parameter::Tail, Parameter::Noise] {
            f.number(p, view, parameter, focus);
        }
        f.toggle(
            p,
            "Extra bass measurement",
            Action::LfDetail,
            prefs.acquisition.lf_detail.is_some(),
        );
        if prefs.acquisition.lf_detail.is_some() {
            for parameter in [Parameter::LfSeconds, Parameter::LfHigh] {
                f.number(p, view, parameter, focus);
            }
        }
        if let Some(noise) = latest_noise(w) {
            let graph = Rect::new(bounds.x0 + 40.0, f.y + 8.0, bounds.x1, f.y + 108.0);
            draw_noise_graph(p, graph, noise);
            f.y += 140.0;
            f.note(
                p,
                &format!(
                    "Noise-level variation {:.1} dB (warning above {:.1} dB). Peak-to-average {:.1} dB (warning above {:.1} dB). Background sets uncertainty; it is not subtracted from the room response.",
                    noise.window_variation_db,
                    noise.limits.maximum_variation_db,
                    noise.crest_db,
                    noise.limits.maximum_crest_db,
                ),
            );
        }
    }
    f.y
}

/// Badge for the recorded speaker level checks; the worst check decides the tone.
fn level_badge(levels: &[Option<measurement::levels::Report>; 2]) -> (&'static str, Tone) {
    let peaks = levels.each_ref().map(|r| r.as_ref().map(|r| r.input_peak));
    let worst = peaks.iter().flatten().copied().fold(0.0, f32::max);
    match peaks {
        [None, None] => ("Not checked", Tone::Neutral),
        _ if worst >= measurement::io::INPUT_CLIPPING_PEAK => ("Clipping", Tone::Bad),
        _ if worst >= measurement::levels::RECOMMENDED_CHECK_PEAK => ("Hot", Tone::Warning),
        [Some(_), Some(_)] => ("Both checked", Tone::Good),
        [Some(_), None] => ("Left checked", Tone::Good),
        [None, Some(_)] => ("Right checked", Tone::Good),
    }
}

fn latest_noise(w: &Workspace) -> Option<&measurement::Noise> {
    w.latest_noise.as_ref().or_else(|| {
        if w.running {
            return None;
        }
        w.source
            .as_ref()
            .map(|s| &s.points[s.plan.layout.reference].noise)
    })
}

/// Listening area, placement diagram, capture progress and background noise. Returns the bottom edge.
fn draw_positions(
    p: &mut Painter<'_, super::Action>,
    bounds: Rect,
    view: &SettingsView<'_>,
    focus: &mut Option<TextFocus>,
) -> f64 {
    let w = view.workspace;
    let prefs = view.preferences;
    let layout = &prefs.layout;
    let mut f = Form::new(bounds, bounds.y0, !w.running);
    let count = layout.positions.len();
    f.section_with_badge(
        p,
        "Microphone positions",
        Icon::Circle,
        &if count == 1 {
            "1 position".into()
        } else {
            format!("{count} positions")
        },
        Tone::Neutral,
    );
    let area = if count > 1 {
        Area::Listening
    } else {
        Area::Single
    };
    f.chips(
        p,
        [
            (
                "Single position",
                Action::SetArea(Area::Single),
                area == Area::Single,
            ),
            (
                "Listening area",
                Action::SetArea(Area::Listening),
                area == Area::Listening,
            ),
        ]
        .into_iter(),
    );
    let captures = Captures::new(w, layout, prefs.acquisition.lf_detail.is_some());
    let diagram = Rect::new(bounds.x0, f.y, bounds.x1, f.y + 168.0);
    draw_placement(p, diagram, layout, &captures, active_position(w));
    f.y = diagram.y1 + GAP;
    let prompt = match active_position(w) {
        Some(position) if matches!(w.progress, Some(Progress::Move { .. })) => {
            format!("Move the microphone: {}", layout.positions[position].name)
        }
        Some(position) => format!("Recording at {}. Keep the microphone still.", layout.positions[position].name),
        None if area == Area::Single => {
            "Microphone at ear height in your usual listening position.".into()
        }
        None => "Start at your usual head position, then follow the prompts to the nearby positions, keeping the microphone on a stand.".into(),
    };
    f.note(p, &prompt);
    if area == Area::Listening {
        f.toggle(
            p,
            "Countdown between positions",
            Action::HandsFree,
            matches!(prefs.progression, workflow::Progression::HandsFree { .. }),
        );
        if matches!(prefs.progression, workflow::Progression::HandsFree { .. }) {
            f.number(p, view, Parameter::MoveSeconds, focus);
        }
    }

    f.y += GAP;
    draw_capture_grid(p, &mut f, layout, &captures);

    if let Some(noise) = latest_noise(w) {
        let row = f.row();
        p.text(
            &format!("Background {:.1} dBFS RMS", noise.rms_dbfs),
            row,
            Face::Label,
            Align::Start,
            p.colors.fg,
        );
        let warning = noise.quality_warning();
        if warning.is_some() {
            badge(p, row, "Varied", Tone::Warning);
        } else {
            badge(p, row, "Steady", Tone::Good);
        }
        f.y += ROW;
        if let Some(warning) = warning {
            f.note(p, &warning);
        }
    }
    f.y
}

/// Top view of the speakers and microphone positions, laid out from their coordinates.
fn draw_placement(
    p: &mut Painter<'_, super::Action>,
    bounds: Rect,
    layout: &measurement::Layout,
    captures: &Captures,
    active: Option<usize>,
) {
    let colors = p.colors;
    p.rounded(bounds, RADIUS, colors.bg);
    let speaker = |x: f64| Rect::from_center_size((x, bounds.y0 + 20.0), (22.0, 14.0));
    for (label, x) in [
        ("L", bounds.x0 + bounds.width() * 0.3),
        ("R", bounds.x0 + bounds.width() * 0.7),
    ] {
        p.rounded(speaker(x), 2.0, colors.secondary);
        p.text(
            label,
            Rect::from_center_size((x, bounds.y0 + 38.0), (24.0, 16.0)),
            Face::Small,
            Align::Center,
            colors.secondary,
        );
    }
    let coordinates: Vec<[f64; 3]> = layout
        .positions
        .iter()
        .map(|p| p.coordinates_cm.unwrap_or([0.0; 3]))
        .collect();
    let extent = coordinates
        .iter()
        .flat_map(|c| [c[0].abs(), c[1].abs()])
        .fold(10.0, f64::max);
    let center = vello::kurbo::Point::new(bounds.center().x, bounds.y0 + 104.0);
    let scale = 50.0 / extent;
    for (position, c) in coordinates.iter().enumerate() {
        // Forward is toward the speakers, so it points up the view.
        let at = vello::kurbo::Point::new(center.x + c[0] * scale, center.y - c[1] * scale);
        let dot = Rect::from_center_size(at, (22.0, 22.0));
        let done = captures.position_done(position);
        if active == Some(position) {
            p.outline(dot.inflate(3.0, 3.0), 14.0, 2.0, colors.basic[4]);
        }
        if done {
            p.rounded(dot, 11.0, colors.basic[2]);
        } else {
            p.outline(dot, 11.0, 1.5, colors.secondary);
        }
        p.text(
            &(position + 1).to_string(),
            dot,
            Face::Small,
            Align::Center,
            if done { colors.bg } else { colors.fg },
        );
        if c[2] != 0.0 {
            p.text(
                &format!("{:+.0} cm", c[2]),
                Rect::new(dot.x1 + 4.0, dot.y0, dot.x1 + 60.0, dot.y1),
                Face::Small,
                Align::Start,
                colors.secondary,
            );
        }
    }
    p.text(
        "Top view / listener faces the speakers",
        Rect::new(
            bounds.x0 + GAP,
            bounds.y1 - 22.0,
            bounds.x1 - GAP,
            bounds.y1 - 4.0,
        ),
        Face::Small,
        Align::Start,
        colors.secondary,
    );
}

fn draw_capture_grid(
    p: &mut Painter<'_, super::Action>,
    f: &mut Form,
    layout: &measurement::Layout,
    captures: &Captures,
) {
    use measurement::Speaker::{Left, Right};
    let mut columns = vec![
        ("Noise", Capture::Noise),
        ("Left", Capture::Full(Left)),
        ("Right", Capture::Full(Right)),
    ];
    if captures.lf {
        columns.extend([
            ("Bass L", Capture::Bass(Left)),
            ("Bass R", Capture::Bass(Right)),
        ]);
    }
    let bounds = f.bounds;
    let cell = 64.0;
    let first = bounds.x1 - cell * columns.len() as f64;
    let cells = |p: &mut Painter<'_, super::Action>,
                 y: f64,
                 values: &mut dyn Iterator<Item = Option<CaptureState>>| {
        for (i, state) in values.enumerate() {
            let rect = Rect::new(
                first + i as f64 * cell + 2.0,
                y + 3.0,
                first + (i + 1) as f64 * cell - 2.0,
                y + CONTROL - 3.0,
            );
            let Some(state) = state else { continue };
            let (label, tone) = match state {
                CaptureState::Pending => ("--", Tone::Neutral),
                CaptureState::Recording => ("Now", Tone::Warning),
                CaptureState::Saved => ("Done", Tone::Good),
            };
            let ink = tone.color(p.colors);
            if state != CaptureState::Pending {
                p.rounded(rect, RADIUS, opacity(ink, 0.16));
            }
            p.text(label, rect, Face::Small, Align::Center, ink);
        }
    };
    for (i, (label, _)) in columns.iter().enumerate() {
        p.text(
            label,
            Rect::new(
                first + i as f64 * cell,
                f.y,
                first + (i + 1) as f64 * cell,
                f.y + CONTROL,
            ),
            Face::Small,
            Align::Center,
            p.colors.secondary,
        );
    }
    f.y += CONTROL;
    let row = |p: &mut Painter<'_, super::Action>,
               f: &mut Form,
               name: String,
               values: &mut dyn Iterator<Item = Option<CaptureState>>| {
        p.line((bounds.x0, f.y), (bounds.x1, f.y), p.colors.pane(), 1.0);
        p.text(
            &name,
            Rect::new(bounds.x0, f.y, first - GAP, f.y + CONTROL),
            Face::Label,
            Align::Start,
            p.colors.fg,
        );
        cells(p, f.y, values);
        f.y += CONTROL + 4.0;
    };
    for &position in &layout.order {
        row(
            p,
            f,
            format!("{}  {}", position + 1, layout.positions[position].name),
            &mut columns.iter().map(|&(_, c)| captures.state(position, c)),
        );
        if position == layout.reference {
            row(
                p,
                f,
                "    Repeat for consistency".into(),
                &mut columns.iter().map(|&(_, c)| match c {
                    Capture::Full(s) => captures.state(position, Capture::Repeat(s)),
                    _ => None,
                }),
            );
        }
    }
    f.y += GAP;
}

fn draw_noise_graph(p: &mut Painter<'_, super::Action>, plot: Rect, noise: &measurement::Noise) {
    p.rect(plot, p.colors.bg);
    let (low, high) = {
        let maximum = noise
            .psd_dbfs_per_hz
            .db
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        let minimum = noise
            .psd_dbfs_per_hz
            .db
            .iter()
            .copied()
            .fold(f32::INFINITY, f32::min);
        let high = (maximum / 20.0).ceil() * 20.0 + 20.0;
        ((high - 80.0).min((minimum / 20.0).floor() * 20.0), high)
    };
    for db in [low, (low + high) / 2.0, high] {
        let y = plot.y0 + f64::from((high - db) / (high - low)) * plot.height();
        p.line((plot.x0, y), (plot.x1, y), p.colors.ghost, 1.0);
        p.text(
            &format!("{db:.0}"),
            Rect::new(plot.x0 - 38.0, y - 10.0, plot.x0 - 5.0, y + 10.0),
            Face::SmallValue,
            Align::End,
            p.colors.secondary,
        );
    }
    let curve = Curve {
        frequency: noise.psd_dbfs_per_hz.frequency.clone(),
        db: noise
            .psd_dbfs_per_hz
            .db
            .iter()
            .map(|d| 48.0 * (*d - low) / (high - low) - 24.0)
            .collect(),
    };
    p.curve(&curve, plot, p.colors.secondary, 1.5);
    p.text(
        "20 Hz",
        Rect::new(plot.x0, plot.y1 + 4.0, plot.x0 + 80.0, plot.y1 + 24.0),
        Face::SmallValue,
        Align::Start,
        p.colors.secondary,
    );
    p.text(
        "20 kHz / dBFS per Hz",
        Rect::new(plot.x1 - 160.0, plot.y1 + 4.0, plot.x1, plot.y1 + 24.0),
        Face::SmallValue,
        Align::End,
        p.colors.secondary,
    );
}
fn draw_settings(
    p: &mut Painter<'_, super::Action>,
    bounds: Rect,
    top: f64,
    view: &SettingsView<'_>,
    focus: &mut Option<TextFocus>,
) -> f64 {
    let w = view.workspace;
    let mut f = Form::new(bounds, top, !w.running);
    match w.dialog.expect("dialog settings") {
        Dialog::Uncalibrated(_) => {
            f.note(p,"No microphone response file is loaded. The microphone's own coloration will be included in the measurement and can influence the correction.");
            f.note(p,"Continue for this measurement, or load a calibration file matching your microphone. This applies to the selected measurement.");
        }
        Dialog::Verification => {
            f.note(p, "Measure the selected correction once. Keep the microphone at the reference listening position and use the same speaker volume and microphone gain.");
            f.note(p, "Verification records the actual response without changing the filter. Checking all positions guides you through the saved listening area.");
        }
        Dialog::Measurements => {
            f.note(
    p,
    "Saved measurements retain microphone recordings and calibration. Reopen one to adjust the correction without measuring again.",
);
            f.button(p, "New measurement", Action::Stage(Stage::Measure));
            for (&id, name) in view.measurements {
                p.list_row(
                    Rect::new(bounds.x0, f.y, bounds.x1, f.y + 48.0),
                    name,
                    "Reusable measurement",
                    super::Action::Room(Action::OpenMeasurement(id)),
                    w.session == Some(id),
                );
                f.y += 48.0;
            }
            if view.measurements.is_empty() {
                f.note(p, "No named measurements saved yet.");
            }
            f.y += 12.0;
            f.button(p, "Export complete session...", Action::Export);
        }
        Dialog::MeasurementName => {
            if let Some(TextFocus::Room {
                target: Text::MeasurementName,
                field,
                layout,
            }) = focus
            {
                *layout = Some(Box::new(p.field(
                    f.row(),
                    field,
                    "",
                    [super::Action::Room(Action::EditText(Text::MeasurementName)); 2],
                    true,
                    "Measurement name",
                )));
                f.y += 32.0;
            }
            f.note(
                p,
                "Save the measurement source independently of correction presets.",
            );
        }
        Dialog::PresetName => {
            if let Some(TextFocus::PresetName { field, layout, .. }) = focus {
                *layout = Some(Box::new(p.field(
                    f.row(),
                    field,
                    "",
                    [super::Action::PresetName; 2],
                    true,
                    "Preset name",
                )));
                f.y += 32.0;
            }
            f.note(p, "Save the exact selected stereo filter and its recipe.");
        }
    }
    f.y - top + 8.0
}

fn estimate_label(estimate: &workflow::Estimate) -> String {
    let duration = |d: std::time::Duration| {
        let seconds = d.as_secs_f64().round() as u64;
        match (seconds / 60, seconds % 60) {
            (0, s) => format!("{s} s"),
            (m, 0) => format!("{m} min"),
            (m, s) => format!("{m} min {s} s"),
        }
    };
    let mut text = format!("About {} of recording", duration(estimate.acquisition));
    match (estimate.moves, estimate.movement) {
        (0, _) => {}
        (moves, Some(movement)) => {
            text += &format!(" and {moves} timed moves ({})", duration(movement))
        }
        (1, None) => text += " and 1 microphone move",
        (moves, None) => text += &format!(" and {moves} microphone moves"),
    }
    text + ". Place the microphone at the reference position."
}
fn stop_label(reason: workflow::StopReason) -> &'static str {
    match reason {
        workflow::StopReason::Prepared => "Candidate prepared / verification needed",
        workflow::StopReason::ProcessedCorrectionVerified => "Processed correction verified",
        workflow::StopReason::ImprovedLimitsReached => "Improved / correction limits reached",
        workflow::StopReason::NoMeasurableImprovement => "No measurable improvement",
        workflow::StopReason::TimeLimit => "Time budget reached",
        workflow::StopReason::Stopped => "Stopped",
    }
}
fn speaker_name(speaker: measurement::Speaker) -> &'static str {
    match speaker {
        measurement::Speaker::Left => "Left",
        measurement::Speaker::Right => "Right",
    }
}
fn room_status(workspace: &Workspace, prefs: &measurement::Preferences) -> String {
    match &workspace.progress {
        Some(Progress::Levels { speaker }) => {
            if workspace.running {
                format!("Checking {} speaker level", speaker_name(*speaker))
            } else {
                format!("{} speaker level check complete", speaker_name(*speaker))
            }
        }
        Some(Progress::Background { position }) => {
            format!("Point {} / recording background noise", position + 1)
        }
        Some(Progress::Sweep {
            position,
            speaker,
            purpose,
            band,
        }) => format!(
            "Point {} / {speaker:?} / {purpose} / {}",
            position + 1,
            band_label(*band)
        ),
        Some(Progress::Fitting { band }) => format!("Creating correction / {}", band_label(*band)),
        Some(Progress::Move { movement, .. }) => format!(
            "{} / {}",
            if matches!(movement, workflow::Movement::Reference(_)) {
                "Return microphone to reference"
            } else {
                "Move microphone"
            },
            prefs.layout.positions[movement.position()].name
        ),
        Some(Progress::Failed(error)) => error.clone(),
        Some(Progress::Finished { reason, .. }) => stop_label(*reason).into(),
        _ if workspace.running => "Preparing measurement".into(),
        _ if workspace.candidate.is_some() => "Candidate ready / load correction to listen".into(),
        _ => "Place the microphone, then start measurement".into(),
    }
}
fn frequency_label(hz: f64) -> String {
    if hz >= 1000.0 {
        format!("{:.1}k", hz / 1000.0)
    } else {
        format!("{hz:.0}")
    }
}
fn band_label(band: measurement::Band) -> String {
    format!(
        "{}-{} Hz",
        frequency_label(band.low),
        frequency_label(band.high)
    )
}
fn scope_label(scope: &Scope) -> String {
    match scope {
        Scope::Prediction { band } => format!("Unverified / {}", band_label(*band)),
        Scope::ReferenceFull { trusted, .. } => {
            format!("Reference measured / {}", band_label(*trusted))
        }
        Scope::SpatialFull { positions, trusted } => {
            format!(
                "{} positions measured / {}",
                positions.len(),
                band_label(*trusted)
            )
        }
        Scope::SpatialPartial {
            positions,
            required,
            ..
        } => format!(
            "Spatial / {} of {} positions",
            positions.len(),
            required.len()
        ),
    }
}
fn heading(
    p: &mut Painter<'_, super::Action>,
    bounds: Rect,
    y: f64,
    title: &str,
    subtitle: &str,
    trailing: Option<(&str, super::Action, bool)>,
) {
    let title_right = trailing.map_or(bounds.x1, |(label, _, _)| {
        bounds.x1 - p.advance(label, Face::Label).ceil() - INSET - GAP
    });
    p.text(
        title,
        Rect::new(bounds.x0, y + 8.0, title_right, y + 32.0),
        Face::Title,
        Align::Start,
        p.colors.fg,
    );
    p.text(
        subtitle,
        Rect::new(bounds.x0, y + 32.0, bounds.x1, y + 50.0),
        Face::Small,
        Align::Start,
        p.colors.secondary,
    );
    if let Some((label, action, enabled)) = trailing {
        let width = p.advance(label, Face::Label).ceil() + INSET;
        p.button(bounds.x1 - width, y + 8.0, label, action, enabled);
    }
}
fn parameter_value(view: &SettingsView<'_>, parameter: Parameter) -> f64 {
    view.parameters
        .iter()
        .find(|(p, _)| *p == parameter)
        .expect("listed room parameter")
        .1
}
fn draw_bar(p: &mut Painter<'_, super::Action>, bounds: Rect, bar: Bar, view: &SettingsView<'_>) {
    let value = parameter_value(view, bar.parameter());
    let text = match bar {
        Bar::Effort => format!("{value:.0} %"),
        Bar::BassSmoothing | Bar::TrebleSmoothing => octave_label(value),
    };
    let start = p.hits.len();
    p.value_bar(
        bounds,
        bar.label(),
        &text,
        value / bar.maximum(),
        super::Action::ValueBar(ValueBar::Room(bar)),
    );
    if view.workspace.running {
        p.hits.truncate(start);
        p.rect(bounds, opacity(p.colors.body, 0.4));
    }
}
/// Octave widths read as the familiar 1/N fractions where they are one.
fn octave_label(octaves: f64) -> String {
    if octaves == 0.0 {
        return "Off".into();
    }
    let denominator = (1.0 / octaves).round();
    if (1.0..=48.0).contains(&denominator) && (1.0 / denominator - octaves).abs() < 1e-6 {
        if denominator == 1.0 {
            "1 oct".into()
        } else {
            format!("1/{denominator:.0} oct")
        }
    } else {
        format!("{octaves:.2} oct")
    }
}
/// Width of the correction settings column beside the graph.
const SIDEBAR_WIDTH: f64 = 320.0;
/// Graph column and settings sidebar of the correction stage.
fn correction_columns(content: Rect) -> [Rect; 2] {
    let sidebar = Rect::new(
        content.x1 - SIDEBAR_WIDTH,
        content.y0,
        content.x1,
        content.y1,
    );
    [
        Rect::new(content.x0, content.y0, sidebar.x0 - INSET, content.y1),
        sidebar,
    ]
}
fn draw_correction(
    p: &mut Painter<'_, super::Action>,
    bounds: Rect,
    view: &SettingsView<'_>,
    status: &str,
) -> Rect {
    let w = view.workspace;
    let enabled = !w.running;
    let selected = w.selected.is_some();
    heading(
        p,
        bounds,
        bounds.y0 - 8.0,
        &format!("Correction / {}", w.name),
        "",
        Some((
            "Save preset...",
            super::Action::Room(if selected {
                Action::SavePass
            } else {
                Action::SaveCandidate
            }),
            enabled && (selected || w.candidate.is_some() && !view.preparing),
        )),
    );
    let mut y = bounds.y0 + CONTROL + INSET;
    if w.running {
        draw_run_banner(
            p,
            Rect::new(bounds.x0, y, bounds.x1, y + 76.0),
            w,
            view.preferences,
            status,
        );
        y += 88.0;
    }
    let spatial_view = w.source.as_ref().is_some_and(|s| s.points.len() > 1);
    let view_controls = spatial_view || w.plot_kind == PlotKind::CorrectionFir || selected;
    let below = layout::GraphLayout::ROOM_CHROME_HEIGHT + if view_controls { 36.0 } else { 0.0 };
    let graph_height = (bounds.y1 - y - below).max(200.0);
    let plot = draw_plot(p, bounds, y, graph_height, view);
    y += graph_height + layout::GraphLayout::ROOM_CHROME_HEIGHT;
    let mut x = bounds.x0;
    if spatial_view {
        x =
            p.button(
                x,
                y,
                &w.point_view
                    .map_or_else(|| "Spatial mean".into(), |i| format!("Point {}", i + 1)),
                super::Action::Room(Action::PointView),
                true,
            )
            .x1 + GAP;
    }
    if w.plot_kind == PlotKind::CorrectionFir {
        p.toggle(
            Rect::new(x, y, (x + 230.0).min(bounds.x1), y + 24.0),
            "Unsmoothed request",
            super::Action::Room(Action::Unsmoothed),
            w.show_unsmoothed,
            true,
        );
        x += 238.0;
    }
    if selected {
        p.toggle(
            Rect::new(x, y, (x + 210.0).min(bounds.x1), y + 24.0),
            "Compare parent",
            super::Action::Room(Action::ParentComparison),
            w.compare_parent,
            true,
        );
    }
    plot
}
/// Target, smoothing, limits, effort and range beside the graph. Returns the content height.
fn draw_correction_settings(
    p: &mut Painter<'_, super::Action>,
    bounds: Rect,
    top: f64,
    view: &SettingsView<'_>,
    focus: &mut Option<TextFocus>,
) -> f64 {
    let w = view.workspace;
    let recipe = &view.preferences.recipe;
    let mut f = Form::new(bounds, top, !w.running);
    f.section(p, "Target", Icon::SlidersHorizontal);
    f.chips(
        p,
        TargetPreset::ALL
            .into_iter()
            .map(|preset| {
                (
                    preset.label(),
                    Action::SetTarget(preset),
                    preset.selects(&recipe.target),
                )
            })
            .chain(std::iter::once((
                "Import...",
                Action::ImportTarget,
                matches!(recipe.target, correction::Target::Custom(_)),
            ))),
    );
    if let correction::Target::Parametric(_) = recipe.target {
        for parameter in [
            Parameter::Tilt,
            Parameter::Bass,
            Parameter::MidGain,
            Parameter::MidCenter,
            Parameter::MidWidth,
        ] {
            f.number(p, view, parameter, focus);
        }
    }
    let automatic = matches!(recipe.target_level, correction::TargetLevel::Automatic);
    f.toggle(p, "Fit common L/R level", Action::AutoLevel, automatic);
    if automatic {
        let offset = w.plot_data.as_ref().map_or(0.0, |d| d.offset);
        f.note(p, &format!("Fitted target offset {offset:+.1} dB"));
    } else {
        f.number(p, view, Parameter::Offset, focus);
    }

    f.section(p, "Smoothing", Icon::Waveform);
    for bar in [Bar::BassSmoothing, Bar::TrebleSmoothing] {
        draw_bar(p, f.row(), bar, view);
        f.y += ROW;
    }
    f.number(p, view, Parameter::Crossover, focus);
    f.number(p, view, Parameter::Transition, focus);

    f.section(p, "Limits", Icon::SlidersHorizontal);
    for (label, action, parameter, limit) in [
        (
            "Limit boost",
            Action::BoostLimit,
            Parameter::Boost,
            recipe.maximum_boost_db,
        ),
        (
            "Limit cut",
            Action::CutLimit,
            Parameter::Cut,
            recipe.maximum_cut_db,
        ),
    ] {
        f.toggle(p, label, action, limit.is_some());
        if limit.is_some() {
            f.number(p, view, parameter, focus);
        }
    }

    f.section(p, "Effort", Icon::SlidersHorizontal);
    let easy = matches!(recipe.effort, correction::Effort::Easy { .. });
    f.chips(
        p,
        [
            ("Easy", Action::SetEffortMode(EffortMode::Easy), easy),
            (
                "Advanced",
                Action::SetEffortMode(EffortMode::Advanced),
                !easy,
            ),
        ]
        .into_iter(),
    );
    if easy {
        draw_bar(p, f.row(), Bar::Effort, view);
        f.y += ROW;
        f.number(p, view, Parameter::LowFade, focus);
        f.number(p, view, Parameter::HighFade, focus);
    } else {
        f.note(
            p,
            "Drag knots on the graph. Click the line to add a knot; Alt-click a knot to remove it.",
        );
    }

    f.section(p, "Correction range", Icon::Waveform);
    f.number(p, view, Parameter::Low, focus);
    f.number(p, view, Parameter::High, focus);

    f.y += GAP;
    if f.fold(p, w, Section::Advanced, "Advanced filter settings") {
        f.dropdown(
            p,
            "Speaker alignment",
            alignment_label(recipe.alignment),
            ChoiceField::Alignment,
        );
        f.button(p, "Restore previous correction", Action::RestorePrevious);
        let exportable = w.selected.map_or(w.candidate.is_some(), |id| {
            w.history[id.0].correction().is_some()
        });
        if exportable {
            f.button(p, "Export filter...", Action::ExportFilter);
        }
    }
    let mut details = Form::new(bounds, f.y, true);
    if details.fold(p, w, Section::Diagnostics, "Measurement details") {
        draw_diagnostics(p, &mut details, w);
    }
    details.y - top + INSET
}
fn draw_diagnostics(p: &mut Painter<'_, super::Action>, f: &mut Form, w: &Workspace) {
    let Some(pass) = w
        .selected
        .and_then(|id| w.history.get(id.0))
        .or_else(|| {
            w.history
                .iter()
                .rev()
                .find(|p| p.correction() == w.candidate)
        })
        .or_else(|| w.history.last())
    else {
        return;
    };
    let metrics = &pass.metrics;
    let headroom = w.plot_data.as_ref().map_or_else(
        || "--".into(),
        |d| format!("{:.1} dB", -d.analysis.headroom_db),
    );
    let verification = if pass.fully_verified() {
        match pass.scope {
            Scope::ReferenceFull { .. } => "Reference verified",
            Scope::SpatialFull { .. } => "Positions verified",
            _ => unreachable!("verified pass has a full measurement scope"),
        }
    } else {
        "Unverified"
    };
    for (label, value) in [
        ("Processed goal RMS", format!("{:.2} dB", metrics.score())),
        ("Required headroom", headroom),
        ("Verification", verification.into()),
    ] {
        let row = f.row();
        p.text(label, row, Face::Label, Align::Start, p.colors.secondary);
        p.text(&value, row, Face::Value, Align::End, p.colors.fg);
        f.y += ROW;
    }
    f.note(
        p,
        &format!(
            "Processed goal L/R {:.2}/{:.2} dB RMS{}",
            metrics.processed_rms_db[0],
            metrics.processed_rms_db[1],
            pass.parent.map_or_else(String::new, |id| {
                let parent = &w.history[id.0];
                if comparable_parent(pass, parent) {
                    format!(
                        " / parent {}: {:.2} dB RMS / change {:+.2} dB",
                        id.0,
                        parent.metrics.score(),
                        metrics.score() - parent.metrics.score()
                    )
                } else {
                    format!(
                        " / parent {} uses a different objective or verification scope; scores are not comparable",
                        id.0
                    )
                }
            })
        ),
    );
    f.note(
        p,
        &format!(
            "{} / Original target L/R {:.2}/{:.2} dB / requested {:.2}/{:.2} dB / coverage {:.0}/{:.0}% / uncertainty {:.2} dB",
            scope_label(&pass.scope),
            metrics.target_rms_db[0],
            metrics.target_rms_db[1],
            metrics.requested_rms_db[0],
            metrics.requested_rms_db[1],
            metrics.coverage[0] * 100.0,
            metrics.coverage[1] * 100.0,
            metrics.decision_uncertainty()
        ),
    );
    f.note(
        p,
        &format!(
            "Observed repeat difference {:.2} dB / noise uncertainty {:.2} dB. Outside-fit difference {:.2} dB / arrival difference {:.2} ms.",
            metrics.repeatability_db,
            metrics.noise_uncertainty_db,
            metrics.unexpected_outside_fit_db,
            metrics.alignment_error_ms,
        ),
    );
    if pass.decision == Decision::Provisional {
        f.note(
            p,
            "Incomplete verification. This measurement is retained as evidence and does not certify the complete correction objective.",
        );
    }
    if let Some(source) = &w.source {
        let position = w.point_view.unwrap_or(source.plan.layout.reference);
        if let Some(warning) = source.points[position].noise.quality_warning() {
            f.note(
                p,
                &format!("{}: {warning}", source.plan.layout.positions[position].name),
            );
        }
    }
    if let Some(data) = &w.plot_data {
        f.note(
            p,
            &format!(
                "{} Hz / FIR support {:.1} ms / minimum phase / convolution buffering {:.2} ms. Shaded bands: outside the correction range.",
                data.rate.hz(),
                data.recipe.taps.frames() as f64 / f64::from(data.rate.hz()) * 1000.0,
                128000.0 / f64::from(data.rate.hz())
            ),
        );
    }
}
fn draw_plot(
    p: &mut Painter<'_, super::Action>,
    bounds: Rect,
    y: f64,
    height: f64,
    view: &SettingsView<'_>,
) -> Rect {
    let w = view.workspace;
    let geometry = layout::GraphLayout::room(bounds, y, height);
    let well = geometry.graph;

    let mut x = well.x0;
    for (name, kind) in [
        ("Room response", PlotKind::RoomResponse),
        ("Correction FIR", PlotKind::CorrectionFir),
    ] {
        x =
            p.chip(
                x,
                y,
                name,
                super::Action::Room(Action::SetPlot(kind)),
                w.plot_kind == kind,
                true,
            )
            .x1 + GAP;
    }
    let channel_width = ["L + R", "L", "R"]
        .into_iter()
        .map(|label| p.advance(label, Face::Label).ceil() + INSET)
        .sum::<f64>()
        + 2.0 * GAP;
    let mut x = well.x1 - channel_width;
    for (name, channels) in [
        ("L + R", Channels::Both),
        ("L", Channels::Left),
        ("R", Channels::Right),
    ] {
        x =
            p.chip(
                x,
                y,
                name,
                super::Action::Room(Action::SetChannels(channels)),
                w.channels == channels,
                true,
            )
            .x1 + GAP;
    }
    let plot = geometry.plot;
    draw_graph(p, plot, &view.preferences.recipe, w);
    let ly = geometry.legend.y0;
    let mut x = well.x0;
    if w.plot_kind == PlotKind::CorrectionFir {
        p.text(
            "Left: solid / Right: dashed",
            Rect::new(x, ly, well.x1, ly + 22.0),
            Face::Small,
            Align::Start,
            p.colors.secondary,
        );
    } else {
        let data = w.plot_data.as_ref();
        for trace in Trace::ALL.into_iter().filter(|trace| match trace {
            Trace::Spread => data.is_some_and(|d| d.points.len() > 1),
            Trace::Verified => data.is_some_and(|d| d.traces(w.point_view).measured.is_some()),
            _ => true,
        }) {
            let width = p.advance(trace.label(), Face::Small).ceil() + 21.0;
            let visible = w.traces.contains(&trace);
            let ink = opacity(trace.color(p.colors), if visible { 1.0 } else { 0.35 });
            p.rounded(Rect::new(x, ly + 7.0, x + 7.0, ly + 14.0), 2.0, ink);
            p.text(
                trace.label(),
                Rect::new(x + 13.0, ly, x + width, ly + 22.0),
                Face::Small,
                Align::Start,
                opacity(p.colors.fg, if visible { 0.8 } else { 0.35 }),
            );
            p.hits.push((
                Rect::new(x, ly, x + width, ly + 22.0),
                super::Action::Room(Action::Trace(trace)),
            ));
            x += width + 4.0;
        }
    }
    plot
}
fn draw_run_banner(
    p: &mut Painter<'_, super::Action>,
    bounds: Rect,
    w: &Workspace,
    prefs: &measurement::Preferences,
    status: &str,
) {
    p.rounded(bounds, RADIUS, p.colors.bg);
    if let Some(Progress::Move {
        movement,
        seconds,
        paused,
    }) = &w.progress
    {
        p.text(
            &seconds.map_or_else(|| "Ready?".into(), |s| format!("{:.0}s", s.ceil())),
            Rect::new(
                bounds.x0 + 12.0,
                bounds.y0 + 4.0,
                bounds.x0 + 100.0,
                bounds.y0 + 42.0,
            ),
            Face::Countdown,
            Align::Start,
            p.colors.fg,
        );
        p.text(
            status,
            Rect::new(
                bounds.x0 + 108.0,
                bounds.y0 + 4.0,
                bounds.x1 - 12.0,
                bounds.y0 + 28.0,
            ),
            Face::Label,
            Align::Start,
            p.colors.fg,
        );
        let point = &prefs.layout.positions[movement.position()];
        p.text(
            &point.coordinates_cm.map_or_else(
                || "Place the mic at the saved location".into(),
                |c| {
                    format!(
                        "{:.0}, {:.0}, {:.0} cm / {}",
                        c[0],
                        c[1],
                        c[2],
                        if *paused {
                            "countdown paused"
                        } else {
                            "keep microphone still during sweeps"
                        }
                    )
                },
            ),
            Rect::new(
                bounds.x0 + 108.0,
                bounds.y0 + 28.0,
                bounds.x1 - 12.0,
                bounds.y0 + 46.0,
            ),
            Face::Small,
            Align::Start,
            p.colors.secondary,
        );
        let mut x = bounds.x0 + 12.0;
        let run = w.expected.expect("movement owns measurement lease");
        if seconds.is_some() {
            x =
                p.button(
                    x,
                    bounds.y0 + 48.0,
                    if *paused {
                        "Resume countdown"
                    } else {
                        "Pause countdown"
                    },
                    super::Action::Room(Action::Movement {
                        run,
                        movement: *movement,
                        action: if *paused {
                            workflow::MoveAction::Resume
                        } else {
                            workflow::MoveAction::Pause
                        },
                    }),
                    true,
                )
                .x1 + GAP;
        }
        x =
            p.button(
                x,
                bounds.y0 + 48.0,
                "Ready now",
                super::Action::Room(Action::Movement {
                    run,
                    movement: *movement,
                    action: workflow::MoveAction::Ready,
                }),
                true,
            )
            .x1 + GAP;
        p.button(
            x,
            bounds.y0 + 48.0,
            "Stop",
            super::Action::Room(Action::Stop),
            true,
        );
    } else {
        p.text(
            status,
            Rect::new(
                bounds.x0 + 12.0,
                bounds.y0 + 8.0,
                bounds.x1 - 12.0,
                bounds.y0 + 32.0,
            ),
            Face::Label,
            Align::Start,
            p.colors.fg,
        );
        p.text(
            if matches!(w.progress, Some(Progress::Levels { .. })) {
                "A short reference signal plays from the selected speaker."
            } else if matches!(w.progress, Some(Progress::Failed(_))) {
                "Adjust setup and try again."
            } else if w.active_plan.as_ref().is_some_and(|plan| plan.kind.acquires()) {
                "Background noise, then left and right sweeps at each position. Keep the microphone still."
            } else if prefs.layout.positions.len() > 1 {
                "The reference check measures this listening position. Other positions remain predicted."
            } else {
                "Measuring the playing correction at the reference position."
            },
            Rect::new(
                bounds.x0 + 12.0,
                bounds.y0 + 34.0,
                bounds.x1 - 12.0,
                bounds.y0 + 56.0,
            ),
            Face::Small,
            Align::Start,
            p.colors.secondary,
        );
    }
}
fn draw_workspace_actions(
    p: &mut Painter<'_, super::Action>,
    bounds: Rect,
    w: &Workspace,
    prefs: &measurement::Preferences,
    preparing: bool,
    listening: bool,
    estimate: &workflow::Estimate,
) {
    let y = bounds.y1 - INSET - CONTROL;
    p.rect(
        Rect::new(bounds.x0, y - 12.0, bounds.x1, bounds.y1),
        p.colors.body,
    );
    p.line(
        (bounds.x0, y - 12.0),
        (bounds.x1, y - 12.0),
        p.colors.pane(),
        RULE,
    );
    let (label, action, enabled, status) = if w.running {
        ("Stop", Action::Stop, true, room_status(w, prefs))
    } else {
        match w.stage {
            Stage::Measure => (
                "Measure speakers",
                Action::Measure,
                true,
                estimate_label(estimate),
            ),
            Stage::Correction => (
                if listening { "Listening" } else { "Listen" },
                Action::Listen,
                !listening && w.candidate.is_some() && w.selected.is_none() && !preparing,
                match (preparing, listening) {
                    (true, true) => "Preparing correction / it plays as soon as it is ready",
                    (true, false) => "Preparing correction...",
                    (false, true) => {
                        "Edits play as soon as they are prepared / compare with Bypass"
                    }
                    (false, false) => "Listen to hear edits as you make them",
                }
                .into(),
            ),
        }
    };
    let width = p.advance(label, Face::Label).ceil() + INSET;
    let right = bounds.x1;
    let mut actions_left = right - width;
    p.button_at(
        Rect::new(actions_left, y, right, y + CONTROL),
        label,
        super::Action::Room(action),
        true,
        enabled,
    );
    if !w.running && w.stage == Stage::Correction {
        let label = "Measure result...";
        let width = p.advance(label, Face::Label).ceil() + INSET;
        let right = actions_left - GAP;
        actions_left = right - width;
        p.button_at(
            Rect::new(actions_left, y, right, y + CONTROL),
            label,
            super::Action::Room(Action::Dialog(Some(Dialog::Verification))),
            false,
            w.candidate.is_some() && !preparing,
        );
    }
    p.text(
        &status,
        Rect::new(bounds.x0, y, actions_left - INSET, y + CONTROL),
        Face::Small,
        Align::Start,
        p.colors.secondary,
    );
}

fn channel_curve(
    p: &mut Painter<'_, super::Action>,
    curve: &Curve,
    plot: Rect,
    ink: vello::peniko::Color,
    width: f64,
    channel: usize,
) {
    if channel == 0 {
        p.curve(curve, plot, ink, width);
    } else {
        p.curve_dashed(curve, plot, ink, width);
    }
}

/// Graph overlay geometry shared by drawing and dragging: a smoothing strip along the
/// top of the plot and effort mapped from 0 at the bottom to 1 under the strip.
struct Overlay {
    plot: Rect,
    strip: Rect,
}
impl Overlay {
    const STRIP: f64 = 22.0;
    const MARGIN: f64 = 8.0;
    fn new(plot: Rect) -> Self {
        Self {
            plot,
            strip: Rect::new(plot.x0, plot.y0, plot.x1, plot.y0 + Self::STRIP),
        }
    }
    fn x(&self, hz: f64) -> f64 {
        self.plot.x0 + (hz / 20.0).log10() / 3.0 * self.plot.width()
    }
    fn top(&self) -> f64 {
        self.strip.y1 + Self::MARGIN
    }
    fn bottom(&self) -> f64 {
        self.plot.y1 - Self::MARGIN
    }
    fn y(&self, effort: f64) -> f64 {
        self.bottom() - effort * (self.bottom() - self.top())
    }
    /// Effort under `y`, in whole percent.
    fn effort(&self, y: f64) -> f64 {
        (((self.bottom() - y) / (self.bottom() - self.top())).clamp(0.0, 1.0) * 100.0).round()
            / 100.0
    }
    fn point(&self, hz: f64, effort: f64) -> vello::kurbo::Point {
        vello::kurbo::Point::new(self.x(hz), self.y(effort))
    }
    /// A curve whose dB values the painter maps onto these screen heights.
    fn screen_curve(&self, points: impl Iterator<Item = (f64, f64)>) -> Curve {
        let (frequency, db) = points
            .map(|(hz, y)| {
                (
                    hz as f32,
                    (24.0 - (y - self.plot.y0) / self.plot.height() * 48.0) as f32,
                )
            })
            .unzip();
        Curve { frequency, db }
    }
}

/// Vertical dB window fitted to the displayed response, in the painter's +/-24 frame.
#[derive(Clone, Copy)]
struct DbAxis {
    center: f32,
    span: f32,
}
impl DbAxis {
    const FIXED: Self = Self {
        center: 0.0,
        span: 48.0,
    };
    fn fit<'a>(curves: impl Iterator<Item = &'a Curve>, range: measurement::Band) -> Self {
        let (low, high) = curves
            .flat_map(|c| c.frequency.iter().zip(&c.db))
            .filter(|(hz, _)| range.contains(f64::from(**hz)))
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), (_, &db)| {
                (lo.min(db), hi.max(db))
            });
        if !low.is_finite() {
            return Self::FIXED;
        }
        Self {
            center: ((low + high) / 12.0).round() * 6.0,
            span: ((high - low + 12.0) / 24.0).ceil().max(2.0) * 24.0,
        }
    }
    fn map(self, curve: &Curve) -> Curve {
        Curve {
            frequency: curve.frequency.clone(),
            db: curve
                .db
                .iter()
                .map(|db| (db - self.center) * 48.0 / self.span)
                .collect(),
        }
    }
    fn label(self, frame_db: f32) -> f32 {
        self.center + frame_db * self.span / 48.0
    }
}

/// Response traces smoothed with the recipe being edited, so smoothing edits show at once.
struct Smoothed {
    key: (correction::Smoothing, Option<usize>),
    baseline: [Curve; 2],
    predicted: [Curve; 2],
    measured: Option<[Curve; 2]>,
}
impl PlotData {
    fn traces(&self, point_view: Option<usize>) -> &ResponseTraces {
        point_view.map_or(&self.spatial, |point| &self.points[point])
    }
    fn refresh_smoothed(&mut self, smoothing: correction::Smoothing, point_view: Option<usize>) {
        let key = (smoothing, point_view);
        if self.smoothed.as_ref().is_some_and(|s| s.key == key) {
            return;
        }
        let traces = self.traces(point_view);
        let smooth = |curves: &[Curve; 2]| {
            curves
                .each_ref()
                .map(|c| correction::smoothed(c, smoothing))
        };
        self.smoothed = Some(Smoothed {
            key,
            baseline: smooth(&traces.baseline),
            predicted: smooth(&traces.predicted),
            measured: traces.measured.as_ref().map(|m| smooth(&m.curves)),
        });
    }
}

fn draw_graph(
    p: &mut Painter<'_, super::Action>,
    plot: Rect,
    recipe: &correction::Recipe,
    workspace: &Workspace,
) {
    let colors = p.colors;
    p.rect(plot, colors.bg);
    let overlay = Overlay::new(plot);
    let x = |f: f64| overlay.x(f);
    let channel_enabled = |c| {
        matches!(
            (workspace.channels, c),
            (Channels::Both, _) | (Channels::Left, 0) | (Channels::Right, 1)
        )
    };
    let response = workspace.plot_kind == PlotKind::RoomResponse;
    let acquired = if workspace.plot_data.is_none() && response {
        workspace.source.as_ref().map(correction::aggregate)
    } else {
        None
    };
    let relative = |curve: &Curve, offset: f32| Curve {
        frequency: curve.frequency.clone(),
        db: curve.db.iter().map(|v| v - offset).collect(),
    };
    let axis = match (&workspace.plot_data, &acquired) {
        (Some(data), _) if response => {
            let smoothed = data
                .smoothed
                .as_ref()
                .expect("smoothed traces refreshed before drawing");
            let shown: Vec<_> = (0..2)
                .filter(|&c| channel_enabled(c))
                .map(|c| relative(&smoothed.baseline[c], data.offset))
                .chain(std::iter::once(relative(&data.target, data.offset)))
                .collect();
            DbAxis::fit(shown.iter(), recipe.range)
        }
        (None, Some(data)) => DbAxis::fit(data.magnitude.iter(), measurement::Band::default()),
        _ => DbAxis::FIXED,
    };
    for hz in [
        20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0,
    ] {
        let at = x(hz);
        p.line((at, plot.y0), (at, plot.y1), colors.ghost, 1.0);
        p.text(
            &if hz >= 1000.0 {
                format!("{}k", hz / 1000.0)
            } else {
                format!("{hz}")
            },
            Rect::new(
                (at - 20.0).max(plot.x0),
                plot.y1 + 6.0,
                (at + 20.0).min(plot.x1),
                plot.y1 + 24.0,
            ),
            Face::Small,
            Align::Center,
            colors.secondary,
        );
    }
    for db in [-24.0f32, -12.0, 0.0, 12.0, 24.0] {
        let y = plot.y0 + f64::from(24.0 - db) / 48.0 * plot.height();
        p.line((plot.x0, y), (plot.x1, y), colors.ghost, 1.0);
        p.text(
            &format!("{:+.0}", axis.label(db)),
            Rect::new(plot.x0 - 34.0, y - 9.0, plot.x0 - 4.0, y + 9.0),
            Face::Value,
            Align::End,
            colors.secondary,
        );
    }
    p.hits.push((plot, super::Action::Room(Action::Graph)));
    if let Some(pass) = workspace
        .selected
        .and_then(|id| workspace.history.get(id.0))
    {
        for band in &pass.changed {
            p.rect(
                Rect::new(x(band.low), plot.y0, x(band.high), plot.y1),
                opacity(colors.basic[2], 0.05),
            );
        }
    }
    if let Some(band) = workspace.active_band {
        let rect = Rect::new(x(band.low), plot.y0, x(band.high), plot.y1);
        p.rect(rect, opacity(colors.basic[2], 0.13));
        p.outline(rect, 0.0, 1.5, colors.basic[2]);
    }
    if let Some(data) = &workspace.plot_data {
        let traces = data.traces(workspace.point_view);
        let shown = |curve: &Curve| axis.map(&relative(curve, data.offset));
        for (i, &hz) in data
            .target
            .frequency
            .iter()
            .enumerate()
            .take(data.target.frequency.len() - 1)
        {
            if !recipe.range.contains(f64::from(hz)) {
                p.rect(
                    Rect::new(
                        x(f64::from(hz)),
                        plot.y0,
                        x(f64::from(data.target.frequency[i + 1])),
                        plot.y1,
                    ),
                    opacity(colors.secondary, 0.12),
                );
            }
        }
        if response {
            draw_smoothing_band(p, &overlay, recipe.smoothing);
        }
        for c in (0..2).filter(|&c| channel_enabled(c)) {
            if !response {
                let ink = colors.basic[4 + c];
                channel_curve(p, &data.filter[c], plot, ink, 1.8, c);
                if workspace.selected.is_some()
                    && workspace.compare_parent
                    && let Some(parent) = &data.parent_filter
                {
                    channel_curve(p, &parent[c], plot, opacity(colors.secondary, 0.65), 1.0, c);
                }
                if workspace.show_unsmoothed {
                    channel_curve(
                        p,
                        &data.unsmoothed[c],
                        plot,
                        opacity(colors.secondary, 0.7),
                        1.0,
                        c,
                    );
                }
                continue;
            }
            let smoothed = data
                .smoothed
                .as_ref()
                .expect("smoothed traces refreshed before drawing");
            if workspace.traces.contains(&Trace::Spread)
                && workspace.point_view.is_none()
                && data.points.len() > 1
            {
                for sign in [-1.0, 1.0] {
                    let spread = Curve {
                        frequency: smoothed.baseline[c].frequency.clone(),
                        db: smoothed.baseline[c]
                            .db
                            .iter()
                            .zip(&data.spread_db[c])
                            .map(|(&b, &s)| b + sign * s)
                            .collect(),
                    };
                    channel_curve(
                        p,
                        &shown(&spread),
                        plot,
                        opacity(colors.secondary, 0.22),
                        1.0,
                        c,
                    );
                }
            }
            if workspace.traces.contains(&Trace::Raw) {
                channel_curve(
                    p,
                    &shown(&traces.baseline[c]),
                    plot,
                    opacity(colors.secondary, 0.3),
                    1.0,
                    c,
                );
            }
            if workspace.traces.contains(&Trace::Measured) {
                channel_curve(
                    p,
                    &shown(&smoothed.baseline[c]),
                    plot,
                    opacity(colors.secondary, 0.8),
                    1.2,
                    c,
                );
            }
            if workspace.traces.contains(&Trace::Predicted) {
                channel_curve(
                    p,
                    &shown(&smoothed.predicted[c]),
                    plot,
                    colors.basic[2],
                    1.5,
                    c,
                );
            }
            if workspace.traces.contains(&Trace::Verified)
                && let (Some(measured), Some(curves)) = (&traces.measured, &smoothed.measured)
            {
                p.with_clip(
                    Rect::new(
                        x(measured.band.low),
                        plot.y0,
                        x(measured.band.high),
                        plot.y1,
                    ),
                    |p| channel_curve(p, &shown(&curves[c]), plot, colors.basic[3], 2.0, c),
                );
            }
            if workspace.selected.is_some()
                && workspace.compare_parent
                && let Some(parent) = &traces.parent
            {
                p.with_clip(
                    Rect::new(x(parent.band.low), plot.y0, x(parent.band.high), plot.y1),
                    |p| {
                        channel_curve(
                            p,
                            &shown(&parent.curves[c]),
                            plot,
                            opacity(colors.secondary, 0.7),
                            1.0,
                            c,
                        )
                    },
                );
            }
        }
        if response && workspace.traces.contains(&Trace::Target) {
            p.with_clip(
                Rect::new(x(recipe.range.low), plot.y0, x(recipe.range.high), plot.y1),
                |p| p.curve(&shown(&data.target), plot, colors.basic[5], 1.5),
            );
        }
        if response && workspace.traces.contains(&Trace::Effort) {
            draw_effort(p, &overlay, recipe, !workspace.running);
        }
    } else if let Some(data) = &acquired {
        let curves = workspace
            .point_view
            .map_or(&data.magnitude, |point| &data.points[point]);
        if workspace.traces.contains(&Trace::Measured) {
            for (channel, curve) in curves.iter().enumerate() {
                if channel_enabled(channel) {
                    channel_curve(p, &axis.map(curve), plot, colors.secondary, 1.5, channel);
                }
            }
        }
        p.text(
            "Captured transfer / dB / preparing target fit",
            Rect::new(plot.x0 + GAP, plot.y0 + GAP, plot.x1 - GAP, plot.y0 + 32.0),
            Face::Small,
            Align::Start,
            colors.fg,
        );
    } else {
        p.text(
            if workspace.source.is_some() {
                "Preparing the first stereo FIR from the captured baseline"
            } else {
                "Complete a baseline measurement to see room response"
            },
            plot,
            Face::Label,
            Align::Center,
            colors.secondary,
        );
    }
}
/// Shaded bass-to-treble smoothing transition with the crossover marked, labeled in the top strip.
fn draw_smoothing_band(
    p: &mut Painter<'_, super::Action>,
    overlay: &Overlay,
    smoothing: correction::Smoothing,
) {
    let colors = p.colors;
    let plot = overlay.plot;
    let (low, high) = smoothing.transition();
    let (x0, x1) = (overlay.x(low).max(plot.x0), overlay.x(high).min(plot.x1));
    let crossover = overlay.x(smoothing.crossover_hz);
    p.rect(
        Rect::new(x0, plot.y0, x1, plot.y1),
        opacity(colors.basic[5], 0.04),
    );
    p.rect(
        Rect::new(x0, overlay.strip.y0, x1, overlay.strip.y1),
        opacity(colors.basic[5], 0.14),
    );
    p.line(
        (crossover, plot.y0),
        (crossover, plot.y1),
        opacity(colors.basic[5], 0.35),
        1.0,
    );
    for edge in [x0, x1] {
        p.line(
            (edge, overlay.strip.y0),
            (edge, overlay.strip.y1),
            opacity(colors.basic[5], 0.6),
            1.5,
        );
    }
    p.line(
        (crossover, overlay.strip.y0),
        (crossover, overlay.strip.y1),
        colors.basic[5],
        2.0,
    );
    let label = |p: &mut Painter<'_, super::Action>, text: &str, bounds: Rect, align: Align| {
        if bounds.width() > p.advance(text, Face::Small) + 8.0 {
            p.text(text, bounds, Face::Small, align, colors.secondary);
        }
    };
    let strip = overlay.strip;
    label(
        p,
        &format!("Bass {}", octave_label(smoothing.bass_octaves)),
        Rect::new(strip.x0 + GAP, strip.y0, x0 - GAP, strip.y1),
        Align::End,
    );
    label(
        p,
        &format!("Treble {}", octave_label(smoothing.treble_octaves)),
        Rect::new(x1 + GAP, strip.y0, strip.x1 - GAP, strip.y1),
        Align::Start,
    );
    label(
        p,
        &frequency_label(smoothing.crossover_hz),
        Rect::new(crossover + 4.0, strip.y0, x1 - 4.0, strip.y1),
        Align::Start,
    );
}
/// The effort line with its draggable knots on its own 0-100 % scale.
fn draw_effort(
    p: &mut Painter<'_, super::Action>,
    overlay: &Overlay,
    recipe: &correction::Recipe,
    editable: bool,
) {
    let ink = Trace::Effort.color(p.colors);
    let range = recipe.range;
    let knots = recipe.effort.points(range);
    let mut frequencies: Vec<f64> = knots
        .iter()
        .map(|k| k.hz)
        .chain([20.0, range.low, range.high, 20000.0])
        .collect();
    frequencies.sort_by(f64::total_cmp);
    frequencies.dedup();
    let line = frequencies.into_iter().flat_map(|hz| {
        let outside = !range.contains(hz);
        let at_edge = hz == range.low || hz == range.high;
        let value = recipe.effort.at(hz, range);
        // Effort steps to zero at a range edge the line does not fade into.
        let edge = (at_edge && value > 0.0).then_some((hz, overlay.y(0.0)));
        let (before, after) = match edge {
            Some(zero) if hz == range.low => (Some(zero), None),
            Some(zero) => (None, Some(zero)),
            None => (None, None),
        };
        before
            .into_iter()
            .chain(std::iter::once((
                hz,
                overlay.y(if outside { 0.0 } else { value }),
            )))
            .chain(after)
    });
    p.curve(&overlay.screen_curve(line), overlay.plot, ink, 2.0);
    let plot = overlay.plot;
    for (effort, label) in [(1.0, "100%"), (0.0, "0%")] {
        let y = overlay.y(effort);
        p.text(
            label,
            Rect::new(plot.x1 - 44.0, y - 16.0, plot.x1 - 4.0, y),
            Face::Small,
            Align::End,
            opacity(ink, 0.8),
        );
    }
    if !editable {
        return;
    }
    let handles: Vec<_> = match &recipe.effort {
        correction::Effort::Easy {
            level,
            low_fade_octaves,
            high_fade_octaves,
        } => vec![
            (range.low * low_fade_octaves.exp2(), *level),
            (range.high / high_fade_octaves.exp2(), *level),
        ],
        correction::Effort::Advanced(points) => points.iter().map(|k| (k.hz, k.effort)).collect(),
    };
    for (hz, effort) in handles {
        let center = overlay.point(hz, effort);
        p.rounded(Rect::from_center_size(center, (10.0, 10.0)), 5.0, ink);
        p.rounded(Rect::from_center_size(center, (4.0, 4.0)), 2.0, p.colors.bg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn workspace() -> (tempfile::TempDir, App) {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::at(directory.path()).unwrap();
        let scenario = measurement::simulation::Scenario::default();
        let session = measurement::validation::run(scenario.clone(), &store).unwrap();
        let mut app = App::with_backend(store, audio::Backend::Simulated(scenario)).unwrap();
        app.window_chrome = Some(WindowChrome {
            height: 32.0,
            controls_end: 81.0,
        });
        app.room_action(Action::OpenMeasurement(session)).unwrap();
        app.room.opened = true;
        (directory, app)
    }
    fn fresh_workspace() -> (tempfile::TempDir, App) {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::at(directory.path()).unwrap();
        let mut app =
            App::with_backend(store, audio::Backend::Simulated(Default::default())).unwrap();
        app.window_chrome = Some(WindowChrome {
            height: 32.0,
            controls_end: 81.0,
        });
        app.settings.room.calibration = measurement::Calibration::Uncalibrated {
            acknowledged: false,
        };
        app.room.opened = true;
        app.room.stage = Stage::Measure;
        (directory, app)
    }

    #[test]
    fn mid_target_controls_preserve_tilt_bass_and_validate_width_and_gain() {
        let (_directory, mut app) = fresh_workspace();
        assert_eq!(app.room_value(Parameter::MidGain), 0.0);
        app.room_number(Parameter::MidGain, -3.0).unwrap();
        app.room_number(Parameter::MidWidth, 2.0).unwrap();
        app.room_number(Parameter::MidCenter, 750.0).unwrap();
        app.room_number(Parameter::Tilt, 0.5).unwrap();
        app.room_number(Parameter::Bass, 2.0).unwrap();
        let parameters = app.settings.room.recipe.target.parameters().unwrap();
        assert_eq!(parameters.mid_gain_db, -3.0);
        assert_eq!(parameters.mid_center_hz, 750.0);
        assert_eq!(parameters.mid_width_octaves, 2.0);
        assert_eq!(parameters.tilt_db_octave, 0.5);
        assert_eq!(parameters.bass_db, 2.0);
        for (parameter, value) in [
            (Parameter::MidCenter, 0.0),
            (Parameter::MidCenter, 20001.0),
            (Parameter::MidCenter, f64::NAN),
            (Parameter::MidWidth, 0.0),
            (Parameter::MidWidth, 4.1),
            (Parameter::MidGain, 12.1),
            (Parameter::MidGain, f64::NAN),
        ] {
            assert!(app.room_number(parameter, value).is_err());
        }
        assert_eq!(app.room_value(Parameter::MidGain), -3.0);
        assert_eq!(app.room_value(Parameter::MidWidth), 2.0);
        let _ = app.room_scene(Size::new(1200.0, 800.0), 1.0);
    }

    #[test]
    fn custom_target_and_graph_handles_edit_the_recipe() {
        let (_directory, mut app) = workspace();
        app.room_action(Action::SetTarget(TargetPreset::Warm))
            .unwrap();
        app.room_action(Action::SetTarget(TargetPreset::Custom))
            .unwrap();
        let recipe = &app.settings.room.recipe;
        assert!(TargetPreset::Custom.selects(&recipe.target));
        assert_eq!(recipe.target.db(50.0), correction::Target::Warm.db(50.0));

        let _ = app.room_scene(Size::new(1200.0, 800.0), 1.0);
        let overlay = Overlay::new(app.room.graph);
        let easy = app.settings.room.recipe.effort.clone();
        app.room_action(Action::SetEffortMode(EffortMode::Advanced))
            .unwrap();
        let range = app.settings.room.recipe.range;
        for hz in [21.0, 500.0, 19000.0] {
            assert!(
                (app.settings.room.recipe.effort.at(hz, range) - easy.at(hz, range)).abs() < 1e-9
            );
        }
        let knots = app.settings.room.recipe.effort.points(range).len();
        app.cursor = overlay.point(1000.0, 1.0);
        app.begin_room_drag();
        app.cursor.y = overlay.y(0.25);
        let Some(super::super::Drag::Room(drag @ Drag::Knot(_))) = app.dragging else {
            panic!("clicking the effort line adds a knot");
        };
        app.room_drag(drag, true);
        app.dragging = None;
        let effort = &app.settings.room.recipe.effort;
        assert_eq!(effort.points(range).len(), knots + 1);
        assert!((effort.at(1000.0, range) - 0.25).abs() < 0.02);

        let crossover = app.settings.room.recipe.smoothing.crossover_hz;
        app.cursor = Point::new(overlay.x(crossover), overlay.strip.center().y);
        app.begin_room_drag();
        app.cursor.x = overlay.x(400.0);
        let Some(super::super::Drag::Room(drag @ Drag::Crossover)) = app.dragging else {
            panic!("the smoothing strip grabs the crossover");
        };
        app.room_drag(drag, true);
        app.dragging = None;
        assert!((app.settings.room.recipe.smoothing.crossover_hz - 400.0).abs() < 5.0);
    }

    #[test]
    fn gain_limit_toggles_and_numeric_ranges_are_independent() {
        let (_directory, mut app) = fresh_workspace();
        assert!(app.settings.room.recipe.maximum_boost_db.is_none());
        assert!(app.settings.room.recipe.maximum_cut_db.is_none());
        app.room_action(Action::BoostLimit).unwrap();
        app.room_number(Parameter::Boost, 24.0).unwrap();
        assert_eq!(app.settings.room.recipe.maximum_boost_db, Some(24.0));
        assert!(app.room_number(Parameter::Boost, 24.5).is_err());
        app.room_action(Action::CutLimit).unwrap();
        app.room_number(Parameter::Cut, 36.0).unwrap();
        assert_eq!(app.settings.room.recipe.maximum_cut_db, Some(36.0));
        assert!(app.room_number(Parameter::Cut, 36.5).is_err());
        app.room_action(Action::BoostLimit).unwrap();
        assert!(app.settings.room.recipe.maximum_boost_db.is_none());
        assert_eq!(app.settings.room.recipe.maximum_cut_db, Some(36.0));
        app.room_action(Action::CutLimit).unwrap();
        assert!(app.settings.room.recipe.maximum_cut_db.is_none());
        let _ = app.room_scene(Size::new(1200.0, 800.0), 1.0);
    }

    #[test]
    fn fresh_measurement_needs_no_level_reports_and_cancel_sends_no_probe() {
        let (_directory, mut app) = fresh_workspace();
        assert!(app.room.session.is_none());
        assert!(app.room.levels.iter().all(Option::is_none));
        let plan = app.room_measurement_plan(RunKind::Measure).unwrap();
        assert!(plan.levels.is_none());
        plan.validate().unwrap();
        app.action(super::super::Action::Room(Action::Measure));
        assert_eq!(
            app.room.dialog,
            Some(Dialog::Uncalibrated(RunKind::Measure))
        );
        assert!(!app.room.running);
        assert!(app.room.expected.is_none());
        app.action(super::super::Action::Room(Action::Dialog(None)));
        assert!(app.room.dialog.is_none());
        assert!(!app.room.running);
        assert!(app.room.expected.is_none());
        app.action(super::super::Action::Room(Action::Measure));
        assert_eq!(
            app.room.dialog,
            Some(Dialog::Uncalibrated(RunKind::Measure))
        );
        assert!(app.room.expected.is_none());
        assert!(matches!(
            app.settings.room.calibration,
            measurement::Calibration::Uncalibrated {
                acknowledged: false
            }
        ));
    }

    fn finish_job(app: &mut App) {
        let end = Instant::now() + Duration::from_secs(15);
        while app.job.is_some() {
            assert!(Instant::now() < end, "{}", app.message);
            app.poll();
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    #[test]
    fn audition_matches_actual_output_for_filters_with_different_peak_gains() {
        let first = Correction::identity(Rate::Hz48000);
        let mut second = first.clone();
        second.channels = [vec![1.0, -0.5], vec![1.0, -0.5]];
        let reference = audition_level_db(&first).min(audition_level_db(&second));
        let base = Controls {
            volume: VolumeDb::try_from(-18.0).unwrap(),
            ..Controls::default()
        };
        assert!((audition_level_db(&first) - audition_level_db(&second)).abs() > 0.1);
        let output_rms = |filter: &Correction| {
            let controls = audition_controls(base, filter, reference).unwrap();
            assert!(controls.volume.db() <= base.volume.db());
            let mut engine = Engine::new(Arc::new(filter.clone()), controls);
            let frequencies: Vec<_> = (0..64)
                .map(|i| 20.0_f64 * 1000.0_f64.powf(i as f64 / 63.0))
                .collect();
            let mut power = 0.0;
            for frame in 0..144_000 {
                let input = frequencies
                    .iter()
                    .enumerate()
                    .map(|(i, &f)| {
                        (std::f64::consts::TAU * f * frame as f64 / 48_000.0 + i as f64).sin()
                    })
                    .sum::<f64>() as f32
                    * 0.001;
                let output =
                    engine.tick([input; 2], CorrectionMode::On)[0] * controls.volume.gain();
                if frame >= 48_000 {
                    power += f64::from(output).powi(2);
                }
            }
            (power / 96_000.0).sqrt()
        };
        let error_db = 20.0 * (output_rms(&first) / output_rms(&second)).log10();
        assert!(error_db.abs() < 0.25, "Audition mismatch: {error_db} dB");
    }

    #[test]
    fn audition_switches_the_selected_exact_pass_and_preserves_the_candidate() {
        let (_directory, mut app) = workspace();
        let candidate = app.room.candidate;
        let selected = app.room.history.last().unwrap().id;
        let hash = app.room.history[selected.0].correction().unwrap();
        let document: serde_json::Value =
            serde_json::from_slice(&app.store.get(&hash).unwrap()).unwrap();
        let stored = app
            .store
            .put(&serde_json::to_vec_pretty(&document).unwrap())
            .unwrap();
        let workflow::Artifact::Candidate { correction, .. } =
            &mut app.room.history[selected.0].trial.artifact
        else {
            unreachable!("selected pass is a correction");
        };
        *correction = stored;
        app.room_action(Action::SelectPass(selected)).unwrap();
        app.room_action(Action::Audition).unwrap();
        assert_eq!(
            app.awaiting.as_ref().unwrap().hash,
            app.room.history[selected.0].correction().unwrap(),
            "audition must preserve the stored pass identity",
        );
        let reference = app.room.return_to.as_ref().unwrap().reference_db;
        let restore = app.room.return_to.as_ref().unwrap().correction;
        app.room_action(Action::SelectPass(PassId(0))).unwrap();
        app.room_action(Action::Audition).unwrap();
        assert_eq!(app.room.return_to.as_ref().unwrap().pass, PassId(0));
        assert_eq!(app.room.return_to.as_ref().unwrap().reference_db, reference);
        assert_eq!(app.room.return_to.as_ref().unwrap().correction, restore);
        assert_eq!(app.room.candidate, candidate);
        app.room_action(Action::Audition).unwrap();
        assert!(app.room.return_to.is_none());
        assert_eq!(app.room.candidate, candidate);
    }

    #[test]
    fn microphone_menu_selects_exposed_integer_channels_without_changing_normal_capture() {
        let (_directory, mut app) = workspace();
        let uid = app.settings.room.microphone.as_ref().unwrap().uid.clone();
        app.devices
            .iter_mut()
            .find(|d| d.uid == uid)
            .unwrap()
            .input_channels = 4;
        let capture = serde_json::to_vec(&app.settings.route.as_ref().unwrap().capture).unwrap();
        app.action(super::super::Action::Room(Action::MicrophoneChannel));
        let options = app.filtered_picker_options();
        assert_eq!(options.len(), 4);
        for (i, option) in options.iter().enumerate() {
            assert!(
                option.action
                    == super::super::Action::Room(Action::ChooseMicrophoneChannel(MonoChannel(
                        i as u16
                    )))
            );
        }
        app.action(options[3].action);
        assert_eq!(
            app.settings.room.microphone.as_ref().unwrap().channels,
            MonoChannel(3)
        );
        assert_eq!(
            serde_json::to_vec(&app.settings.route.as_ref().unwrap().capture).unwrap(),
            capture
        );
        assert!(app.picker.is_none());
        assert!(
            app.room_action(Action::ChooseMicrophoneChannel(MonoChannel(4)))
                .is_err()
        );
        assert_eq!(
            app.settings.room.microphone.as_ref().unwrap().channels,
            MonoChannel(3)
        );
    }

    #[test]
    fn value_bar_drag_prepares_one_unverified_descendant_of_the_previewed_pass() {
        let (_directory, mut app) = workspace();
        let selected = app.room.history.last().unwrap().id;
        app.room_action(Action::SelectPass(selected)).unwrap();
        let count = app.room.history.len();
        let active = app.active.hash;
        app.build_scene(Size::new(1440.0, 960.0), 1.0);
        let target = ValueBar::Room(Bar::Effort);
        let bounds = app
            .hits
            .iter()
            .find(|(_, a)| *a == super::super::Action::ValueBar(target))
            .unwrap()
            .0;
        app.cursor = Point::new(bounds.x0 + bounds.width() * 0.8, bounds.center().y);
        app.action(super::super::Action::ValueBar(target));
        for fraction in [0.6, 0.4, 0.25] {
            app.cursor.x = bounds.x0 + bounds.width() * fraction;
            app.value_bar_from_cursor(target, bounds);
            assert!(app.job.is_none());
        }
        assert_eq!(app.room.history.len(), count);
        assert_eq!(app.room.selected, Some(selected));
        app.finish_drag();
        app.dragging = None;
        finish_job(&mut app);
        assert_eq!(app.room.history.len(), count + 1);
        let pass = app.room.history.last().unwrap();
        assert_eq!(pass.parent, Some(selected));
        assert_eq!(pass.decision, Decision::Prepared);
        assert!(!pass.fully_verified());
        assert_eq!(app.room_value(Parameter::Effort), 25.0);
        assert_eq!(app.active.hash, active);
    }
    #[test]
    fn source_and_exact_pass_naming_use_native_fields_and_keep_the_room_workspace_open() {
        let (_directory, mut app) = workspace();
        let selected = app.room.history.last().unwrap().id;
        app.room_action(Action::SelectPass(selected)).unwrap();
        let exact = app.room.history[selected.0].correction().unwrap();
        let source = app.room.session.unwrap();
        app.action(super::super::Action::Room(Action::SaveMeasurement));
        app.build_scene(Size::new(880.0, 760.0), 1.0);
        if let Some(TextFocus::Room { field, .. }) = &mut app.text_focus {
            field.insert("Desk source");
        } else {
            panic!("source name editor");
        }
        app.action(super::super::Action::Room(Action::CommitName));
        assert_eq!(
            app.settings.measurements.get(&source).unwrap(),
            "Desk source"
        );
        assert!(app.room.dialog.is_none());
        app.action(super::super::Action::Room(Action::SavePass));
        app.build_scene(Size::new(880.0, 760.0), 1.0);
        if let Some(TextFocus::PresetName { source, field, .. }) = &mut app.text_focus {
            assert_eq!(*source, exact);
            field.insert("Exact earlier pass");
        } else {
            panic!("preset name editor");
        }
        app.action(super::super::Action::CommitPresetName);
        app.build_scene(Size::new(880.0, 760.0), 1.0);
        assert!(app.room.opened);
        assert!(app.room.dialog.is_none());
        assert_eq!(
            app.settings.presets.get(&exact).unwrap(),
            "Exact earlier pass"
        );
        let reopened = app.store.load().unwrap();
        assert_eq!(reopened.measurements.get(&source).unwrap(), "Desk source");
        assert_eq!(reopened.presets.get(&exact).unwrap(), "Exact earlier pass");
    }
    #[test]
    fn room_actions_and_scrollbars_leave_controls_unobstructed() {
        let (_directory, mut app) = workspace();
        app.room.expanded.insert(Section::Diagnostics);
        app.build_scene(Size::new(880.0, 760.0), 1.0);
        let rail = app
            .hits
            .iter()
            .find_map(|(_, action)| match action {
                super::super::Action::Scroll(ScrollSurface::RoomSettings, bar) => Some(bar.rail),
                _ => None,
            })
            .unwrap();
        assert!(rail.x0 >= app.room.settings_bounds.x1);
        app.room.dialog = Some(Dialog::Verification);
        app.build_scene(Size::new(880.0, 760.0), 1.0);
        let bounds = |action| {
            app.hits
                .iter()
                .rev()
                .find(|(_, a)| *a == super::super::Action::Room(action))
                .unwrap()
                .0
        };
        let close = bounds(Action::Dialog(None));
        let all = bounds(Action::VerifySpatial);
        let reference = bounds(Action::Verify);
        assert!(close.x1 + GAP <= all.x0);
        assert!(all.x1 + GAP <= reference.x0);
        assert_eq!(reference.x1, 880.0 - close.x0);
    }

    #[test]
    fn minimum_window_retains_route_and_scrollable_workspace() {
        let (_directory, mut app) = workspace();
        for stage in [Stage::Measure, Stage::Correction] {
            app.room_action(Action::Stage(stage)).unwrap();
            app.build_scene(Size::new(880.0, 760.0), 1.0);
            assert_eq!(
                app.room.settings_bounds.width(),
                if stage == Stage::Correction {
                    SIDEBAR_WIDTH
                } else {
                    880.0 - 2.0 * INSET
                }
            );
            let primary = match stage {
                Stage::Measure => Action::Measure,
                Stage::Correction => Action::Listen,
            };
            let button = app
                .hits
                .iter()
                .rev()
                .find(|(_, action)| *action == super::super::Action::Room(primary))
                .unwrap()
                .0;
            assert_eq!(button.x1, 880.0 - INSET);
            assert_eq!(button.height(), CONTROL);
            if stage == Stage::Correction {
                let channels = app
                    .hits
                    .iter()
                    .find(|(_, action)| {
                        *action == super::super::Action::Room(Action::SetChannels(Channels::Right))
                    })
                    .unwrap()
                    .0;
                assert_eq!(channels.x1, app.room.graph.x1);
                assert_eq!(app.room.settings_bounds.x1, button.x1);
                assert!(app.room.graph.x1 + INSET <= app.room.settings_bounds.x0);
            }

            if stage != Stage::Correction {
                for action in [
                    super::super::Action::Room(Action::Microphone),
                    super::super::Action::Room(Action::MicrophoneChannel),
                    super::super::Action::Playback,
                    super::super::Action::PlaybackPair,
                ] {
                    assert!(
                        app.hits
                            .iter()
                            .any(|(r, a)| *a == action && r.width() > 0.0 && r.height() == CONTROL)
                    );
                }
            }
            for (_, action) in &app.hits {
                if let super::super::Action::Room(Action::Stage(_)) = action {
                    let r = app.hits.iter().find(|(_, a)| a == action).unwrap().0;
                    assert!(r.y1 <= app.room.settings_bounds.y0);
                }
            }
            app.cursor = app.room.settings_bounds.center();
            app.room_scroll(-10000.0);
            app.build_scene(Size::new(880.0, 760.0), 1.0);
            assert!(
                (app.room.settings_scroll
                    - (app.room.settings_height - app.room.settings_bounds.height()).max(0.0))
                .abs()
                    < 1.0
            );
            assert!(
                app.hits
                    .iter()
                    .filter(|(r, _)| r.width() > 0.0 && r.height() > 0.0)
                    .all(|(r, _)| r.x0 >= 0.0 && r.y0 >= 0.0 && r.x1 <= 880.0 && r.y1 <= 760.0)
            );
        }
    }
    #[test]
    fn fitting_opens_the_live_graph_without_changing_the_preview() {
        let (_directory, mut app) = workspace();
        let selected = app.room.history.last().unwrap().id;
        app.room_action(Action::SelectPass(selected)).unwrap();

        app.room.stage = Stage::Measure;
        let band = measurement::Band::new(60.0, 300.0).unwrap();
        app.room_progress(Progress::Fitting { band });
        assert_eq!(app.room.stage, Stage::Correction);
        assert_eq!(app.room.active_band, Some(band));
        assert_eq!(app.room.selected, Some(selected));

        app.room.running = true;
        app.room_action(Action::Trace(Trace::Spread)).unwrap();
        app.room_action(Action::SetChannels(Channels::Right))
            .unwrap();
        assert!(!app.room.traces.contains(&Trace::Spread));
        assert_eq!(app.room.selected, Some(selected));
        assert_eq!(app.room.active_band, Some(band));
    }
    #[test]
    fn playback_selectors_follow_existing_stream_ownership_while_the_mic_stays_separate() {
        let (_directory, mut app) = workspace();
        app.room.stage = Stage::Measure;
        let size = Size::new(880.0, 760.0);
        app.lifecycle = Lifecycle::Running {
            rate: Rate::Hz48000,
            estimated_latency_ms: 16.0,
        };
        app.build_scene(size, 1.0);
        assert!(!app.hits.iter().any(|(_, a)| matches!(
            a,
            super::super::Action::Playback | super::super::Action::PlaybackPair
        )));
        assert!(
            app.hits
                .iter()
                .any(|(_, a)| *a == super::super::Action::Room(Action::MicrophoneChannel))
        );
        app.lifecycle = Lifecycle::Measuring {
            rate: Rate::Hz44100,
        };
        app.room.running = true;
        assert_eq!(app.current_rate(), Rate::Hz44100);
        app.build_scene(size, 1.0);
        assert!(!app.hits.iter().any(|(_, a)| matches!(
            a,
            super::super::Action::Playback
                | super::super::Action::PlaybackPair
                | super::super::Action::Room(Action::MicrophoneChannel)
        )));
        app.action(super::super::Action::StartStop);
        assert!(!app.room.running);
        assert!(app.room.muted);
        app.lifecycle = Lifecycle::Stopped;
        app.build_scene(size, 1.0);
        assert!(
            app.hits
                .iter()
                .any(|(_, a)| *a == super::super::Action::Playback)
        );
        assert!(
            app.hits
                .iter()
                .any(|(_, a)| *a == super::super::Action::PlaybackPair)
        );
    }
    #[test]
    fn explicit_listening_resume_uses_the_selected_route_and_keeps_failed_submission_muted() {
        let (_directory, mut app) = fresh_workspace();
        app.room.muted = true;
        let route = app.settings.route.take().unwrap();
        assert!(app.room_action(Action::ResumeListening).is_err());
        assert!(app.room.muted);
        app.settings.route = Some(route);
        let output = &mut app.settings.route.as_mut().unwrap().playback;
        output.uid = app
            .devices
            .iter()
            .find(|device| device.input_channels > 0 && device.output_channels == 0)
            .unwrap()
            .uid
            .clone();
        app.room_action(Action::ResumeListening).unwrap();
        assert!(matches!(app.lifecycle, Lifecycle::Starting));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(app.lifecycle, Lifecycle::Failed(_)) && Instant::now() < deadline {
            app.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            matches!(app.lifecycle, Lifecycle::Failed(_)),
            "resuming must submit the edited route, not a cached listening route"
        );
        app.runtime.send(Command::Shutdown).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(
            app.runtime.events.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ) {
            assert!(Instant::now() < deadline, "emulated owner did not stop");
            std::thread::sleep(Duration::from_millis(5));
        }
        app.settings.route = Some(measurement::simulation::route());
        app.room.muted = true;
        assert!(app.room_action(Action::ResumeListening).is_err());
        assert!(
            app.room.muted,
            "failed command submission must preserve the muted state"
        );
    }
    #[test]
    fn reopening_a_source_cancels_an_unfinished_candidate_instead_of_applying_it_later() {
        let (_directory, mut app) = workspace();
        let source = app.room.session.unwrap();
        let history = app.room.manifest.as_ref().unwrap().passes.clone();
        app.room_number(Parameter::Effort, 50.0).unwrap();
        assert!(app.job.is_some());
        app.room_action(Action::OpenMeasurement(source)).unwrap();
        finish_job(&mut app);
        assert_eq!(app.room.session, Some(source));
        assert_eq!(app.room.manifest.as_ref().unwrap().passes, history);
        assert_eq!(app.room_value(Parameter::Effort), 100.0);
    }
    #[test]
    fn interrupted_acquisition_is_offered_with_its_saved_setup() {
        let (_directory, mut app) = workspace();
        let plan = app.room.source.as_ref().unwrap().plan.clone();
        let original_name = plan.layout.positions[0].name.clone();
        let checkpoint =
            workflow::Recovery::Acquisition(Box::new(workflow::AcquisitionCheckpoint {
                points: vec![workflow::PointCheckpoint::default(); plan.layout.positions.len()],
                repeat_reference: [None, None],
                plan,
            }));
        app.store
            .write(
                "room-recovery.json",
                &serde_json::to_vec(&checkpoint).unwrap(),
            )
            .unwrap();
        app.settings.room.layout.positions[0].name = "Future placement".into();
        app.room_action(Action::Stage(Stage::Measure)).unwrap();
        app.build_scene(Size::new(1440.0, 960.0), 1.0);
        assert!(app.hits.iter().any(|(_, action)| *action == super::super::Action::Room(Action::ResumeAcquisition)));
        app.room_action(Action::ResumeAcquisition).unwrap();
        assert_eq!(app.settings.room.layout.positions[0].name, original_name);
        assert_eq!(
            app.room.dialog,
            Some(Dialog::Uncalibrated(RunKind::ResumeMeasurement))
        );
        assert!(!app.room.running);
        app.room_action(Action::Dialog(None)).unwrap();
        assert!(matches!(
            workflow::Recovery::load(&app.store).unwrap(),
            Some(workflow::Recovery::Acquisition(_))
        ));
    }
    #[test]
    fn historical_edits_are_descendants_and_do_not_mutate_evidence_or_activate_audio() {
        let (_directory, mut app) = workspace();
        let source = app.room.manifest.as_ref().unwrap().source;
        let active = app.active.hash;
        let original = app.room.manifest.as_ref().unwrap().passes.clone();
        app.room_number(Parameter::Effort, 25.0).unwrap();
        finish_job(&mut app);
        let snapshot = app.room.history[1].id;
        app.room_action(Action::SelectPass(snapshot)).unwrap();
        assert_eq!(app.room_value(Parameter::Effort), 100.0);
        app.room_number(Parameter::Effort, 50.0).unwrap();
        finish_job(&mut app);
        let pass = app.room.history.last().unwrap();
        assert_eq!(pass.parent, Some(snapshot));
        assert!(!pass.fully_verified());
        assert_eq!(pass.decision, Decision::Prepared);
        assert_eq!(app.room.manifest.as_ref().unwrap().source, source);
        assert_eq!(
            &app.room.manifest.as_ref().unwrap().passes[..original.len()],
            &original
        );
        assert_eq!(app.active.hash, active);
        let frozen: correction::Frozen =
            serde_json::from_slice(&app.store.get(&pass.objective).unwrap()).unwrap();
        assert_eq!(frozen.recipe.effort, correction::Effort::easy(0.5));
    }

    #[test]
    fn excitation_bounds_are_separate_from_correction_and_limit_lf_acquisition() {
        let (_directory, mut app) = workspace();
        let correction = app.settings.room.recipe.range;
        app.room_number(Parameter::ExcitationLow, 80.0).unwrap();
        app.room_number(Parameter::ExcitationHigh, 18000.0).unwrap();
        app.room_action(Action::LfDetail).unwrap();
        assert_eq!(
            app.settings.room.acquisition.band,
            measurement::Band::new(80.0, 18000.0).unwrap()
        );
        assert_eq!(
            app.settings
                .room
                .acquisition
                .lf_detail
                .as_ref()
                .unwrap()
                .band,
            measurement::Band::new(80.0, 300.0).unwrap()
        );
        assert_eq!(app.settings.room.recipe.range, correction);
        assert!(app.room_number(Parameter::ExcitationHigh, 40.0).is_err());
        assert_eq!(app.settings.room.acquisition.band.high, 18000.0);
        assert!(app.room_number(Parameter::ExcitationLow, 500.0).is_err());
        assert_eq!(app.settings.room.acquisition.band.low, 80.0);
    }
}
