use irresponsible::{
    appearance::{self, Appearance, Theme, ThemeMode, ThemeSlot, WindowMode},
    audio::{self, Command, Event, Runtime, SystemOutput},
    catalog::{Catalog, CatalogFilter, CatalogTarget, Source},
    correction::{
        Alignment, BassExtension, ConchaInterference, Correction, Curve, FirLength,
        GenerationParameter, GenerationSettings, Normalization, Origin, TrebleExtension,
    },
    domain::*,
    dsp::{Analysis, Engine},
    error::{Result, invalid},
    frame_schedule::{FrameKind, FrameSchedule, Occlusion, RedrawPermit, SurfaceArea},
    platform::login,
    spectrum::{self as analysis, Analyzer, Choice as SpectrumChoice, Setting as SpectrumSetting},
    storage::{ContentHash, Settings, Store},
};
mod chrome;
mod help;
mod layout;
mod scrollbar;
mod spectrum;
mod text_field;
mod window_chrome;

use chrome::{
    Align, CONTROL, Face, GAP, INSET, Icon, Icons, MenuChecks, Painter, RADIUS, ROW, RULE, SIDEBAR,
    opacity,
};
use layout::{GraphLayout, Layout};
use parley::{FontContext, LayoutContext};
use scrollbar::Scrollbar;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};
use text_field::TextField;
use tray_icon::{
    TrayIcon, TrayIconBuilder,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem},
};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};
use vello::{
    Scene,
    kurbo::{Point, Rect, Size, Vec2},
    util::{RenderContext, RenderSurface},
};
use window_chrome::WindowChrome;
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition},
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{Window, WindowId},
};

const WIDTH: f64 = 1100.0;
const HEIGHT: f64 = 860.0;
#[derive(Clone, Copy, PartialEq)]
enum Action {
    Preferences,
    Compact,
    ThemePicker(ThemeSlot),
    FollowSystem,
    LaunchAtLogin,
    LoginSettings,
    ChooseTheme(ThemeSlot, usize),
    PresetPicker,
    LoadPreset(ContentHash),
    SavePreset,
    PresetName,
    CommitPresetName,
    CancelPresetName,
    PickerSearch,
    ClearPickerSearch,
    Scroll(ScrollSurface, Scrollbar),
    StartStop,
    Retry,
    Capture,
    Playback,
    CapturePair,
    PlaybackPair,
    RefreshDevices,
    Search,
    ClearSearch,
    RefreshCatalog,
    CatalogFilter,
    ChooseCatalogFilter(CatalogFilter),
    Select(usize),
    AutoEq,
    Harman,
    Reference,
    TrebleExtension,
    Flat,
    Import,
    ReplaceLoaded,
    Cancel,
    Bypass,
    Compensation,
    NudgeNumber(NumberField, StepDirection),
    EditNumber(NumberField),
    GenerationLength,
    GenerationAlignment,
    GenerationNormalization,
    GenerationBass,
    GenerationConcha,
    GenerationCutLimit,
    ChooseLength(FirLength),
    ChooseAlignment(Alignment),
    ChooseNormalization(Normalization),
    GenerationDefaults,
    ValueBar(ValueBar),
    Details,
    ShowLoaded,
    ShowCandidate,
    ShowMeasured,
    ShowTarget,
    ShowPrediction,
    BrowsePresets,
    BrowseGeneration,
    BrowseGeneral,
    BrowseSpectrum,
    SpectrumEnabled,
    SpectrumPicker(SpectrumSetting),
    ChooseSpectrum(SpectrumChoice),
    Recall(ContentHash),
    DeletePreset(ContentHash),
    RenamePreset(ContentHash),
    RecoverSettings,
    ChooseDevice(RouteSide, usize),
    ChoosePair(RouteSide, StereoPair),
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum RouteSide {
    Capture,
    Playback,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ScrollSurface {
    Sidebar,
    Picker,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ValueBar {
    Volume,
    SpectrumIntensity,
}
#[derive(Clone, Copy, PartialEq)]
enum Drag {
    ValueBar {
        target: ValueBar,
        bounds: Rect,
    },
    TextSelection,
    Scroll {
        surface: ScrollSurface,
        bar: Scrollbar,
        grab: f64,
    },
}
#[derive(Clone, PartialEq, Eq)]
enum ClickTarget {
    Titlebar,
    Filter,
    Picker,
    PresetName,
    Number(NumberField),
    Profile(Selection),
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum NumberField {
    ComparisonTrim,
    SpectrumIntensity,
    Generation(GenerationParameter),
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum StepDirection {
    Down,
    Up,
}
impl StepDirection {
    fn sign(self) -> f32 {
        match self {
            Self::Down => -1.0,
            Self::Up => 1.0,
        }
    }
}
enum TextFocus {
    Search,
    Picker,
    PresetName {
        source: ContentHash,
        field: TextField,
        layout: Option<Box<chrome::FieldLayout>>,
    },
    Number {
        target: NumberField,
        field: TextField,
        layout: Option<Box<chrome::FieldLayout>>,
    },
}

#[derive(Clone, Copy)]
enum ClickCount {
    Single,
    Double,
    Triple,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Navigation {
    Previous,
    Next,
}

#[derive(Clone, Copy)]
enum PickerKind {
    CatalogFilter,
    GenerationLength,
    GenerationAlignment,
    GenerationNormalization,
    Spectrum(SpectrumSetting),
    Device(RouteSide),
    Pair(RouteSide),
    Graph(Point),
    Themes(ThemeSlot),
    Presets,
}
impl PickerKind {
    fn action(self) -> Option<Action> {
        Some(match self {
            Self::Themes(slot) => Action::ThemePicker(slot),
            Self::Presets => Action::PresetPicker,
            Self::Spectrum(setting) => Action::SpectrumPicker(setting),
            Self::CatalogFilter => Action::CatalogFilter,
            Self::GenerationLength => Action::GenerationLength,
            Self::GenerationAlignment => Action::GenerationAlignment,
            Self::GenerationNormalization => Action::GenerationNormalization,
            Self::Device(RouteSide::Capture) => Action::Capture,
            Self::Device(RouteSide::Playback) => Action::Playback,
            Self::Pair(RouteSide::Capture) => Action::CapturePair,
            Self::Pair(RouteSide::Playback) => Action::PlaybackPair,
            Self::Graph(_) => return None,
        })
    }
}

struct Picker {
    kind: PickerKind,
    filter: TextField,
    layout: Option<Box<chrome::FieldLayout>>,
    highlighted: Option<Action>,
    scroll: usize,
    bounds: Option<Rect>,
    visible_rows: usize,
}
impl Picker {
    fn new(kind: PickerKind, highlighted: Option<Action>) -> Self {
        Self {
            kind,
            filter: TextField::new(String::new()),
            layout: None,
            highlighted,
            scroll: 0,
            bounds: None,
            visible_rows: 12,
        }
    }
}
struct MenuOption {
    label: String,
    action: Action,
    checks: MenuChecks,
}
struct GraphToggle {
    label: &'static str,
    action: Action,
    selected: bool,
    color: vello::peniko::Color,
}

#[derive(Clone, PartialEq, Eq)]
enum Selection {
    Catalog(usize),
    Saved(ContentHash),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectionIntent {
    Compare,
    Load,
}

struct SelectedProfile {
    source: Selection,
    intent: SelectionIntent,
}
impl SelectedProfile {
    fn compare(source: Selection) -> Self {
        Self {
            source,
            intent: SelectionIntent::Compare,
        }
    }
}

#[derive(Default, PartialEq, Eq)]
enum Browser {
    #[default]
    Catalog,
    Presets,
    Generation,
    General,
    Spectrum,
}

impl Browser {
    fn is_preferences(&self) -> bool {
        matches!(
            self,
            Self::Generation | Self::General | Self::Presets | Self::Spectrum
        )
    }
}

#[derive(Clone)]
struct View {
    correction: Arc<Correction>,
    response: [Curve; 2],
    analysis: Analysis,
    /// Managed source identity, independent of the playback sample rate.
    hash: ContentHash,
}
impl View {
    fn prepare(
        correction: Correction,
        controls: Controls,
        store: &Store,
    ) -> Result<(Self, Box<Engine>)> {
        correction.validate()?;
        let hash = store.put(&serde_json::to_vec(&correction)?)?;
        Self::from_managed(correction, hash, controls)
    }
    fn recall(
        hash: ContentHash,
        rate: Rate,
        controls: Controls,
        store: &Store,
    ) -> Result<(Self, Box<Engine>)> {
        let correction: Correction = serde_json::from_slice(&store.get(&hash)?)?;
        correction.validate()?;
        Self::from_managed(correction.at_rate(rate, store)?, hash, controls)
    }
    fn from_managed(
        correction: Correction,
        hash: ContentHash,
        controls: Controls,
    ) -> Result<(Self, Box<Engine>)> {
        correction.validate()?;
        let correction = Arc::new(correction);
        let engine = Box::new(Engine::new(correction.clone(), controls));
        let view = Self {
            response: correction.response(400),
            analysis: engine.analysis,
            correction,
            hash,
        };
        Ok((view, engine))
    }
}
enum JobResult {
    Catalog(Catalog),
    Prepared(View, Box<Engine>),
    Nothing,
}
type Task = Box<dyn FnOnce(Arc<AtomicBool>) -> Result<JobResult> + Send>;
struct Job {
    receiver: Receiver<Result<JobResult>>,
    cancel: Arc<AtomicBool>,
    next: Option<Task>,
}

struct MenuBar {
    _tray: TrayIcon,
    show: MenuItem,
    bypass: CheckMenuItem,
    louder: MenuItem,
    quieter: MenuItem,
    stop: MenuItem,
    quit: MenuItem,
}
impl MenuBar {
    fn new() -> Result<Self> {
        let menu = Menu::new();
        let show = MenuItem::new("Show IRresponsible", true, None);
        let bypass = CheckMenuItem::new("Bypass correction", true, false, None);
        let louder = MenuItem::new("Volume +2 dB", true, None);
        let quieter = MenuItem::new("Volume -2 dB", true, None);
        let stop = MenuItem::new("Stop", true, None);
        let quit = MenuItem::new("Quit", true, None);
        menu.append_items(&[&show, &bypass, &louder, &quieter, &stop, &quit])
            .map_err(|e| invalid(e.to_string()))?;
        let tray = TrayIconBuilder::new()
            .with_title("IR")
            .with_tooltip("IRresponsible - stopped")
            .with_menu(Box::new(menu))
            .build()
            .map_err(|e| invalid(e.to_string()))?;
        Ok(Self {
            _tray: tray,
            show,
            bypass,
            louder,
            quieter,
            stop,
            quit,
        })
    }
    fn status(&self, lifecycle: &Lifecycle, controls: Controls) {
        let status = match lifecycle {
            Lifecycle::Running { .. } => {
                if controls.correction == CorrectionMode::On {
                    "corrected"
                } else {
                    "bypassed"
                }
            }
            Lifecycle::Starting => "starting",
            Lifecycle::Recovering { .. } => "recovering",
            Lifecycle::Failed(_) => "failed",
            Lifecycle::Stopped => "stopped",
        };
        self.bypass
            .set_checked(controls.correction == CorrectionMode::Bypass);
        let _ = self._tray.set_tooltip(Some(format!(
            "IRresponsible - {status}, {:.0} dB",
            controls.volume.db()
        )));
    }
}

#[derive(Clone, Copy)]
enum Presentation {
    Presented,
    Unavailable,
}
struct Graphics {
    context: RenderContext,
    surface: RenderSurface<'static>,
    renderer: vello::Renderer,
}
impl Graphics {
    fn new(window: Arc<Window>) -> Result<Self> {
        let mut context = RenderContext::new();
        let size = window.inner_size();
        let surface = pollster::block_on(context.create_surface(
            window,
            size.width,
            size.height,
            vello::wgpu::PresentMode::AutoVsync,
        ))
        .map_err(|e| invalid(e.to_string()))?;
        let renderer = vello::Renderer::new(
            &context.devices[surface.dev_id].device,
            vello::RendererOptions::default(),
        )
        .map_err(|e| invalid(e.to_string()))?;
        Ok(Self {
            context,
            surface,
            renderer,
        })
    }
    fn draw(
        &mut self,
        scene: &Scene,
        background: vello::peniko::Color,
        window: &Window,
    ) -> Result<Presentation> {
        let device = &self.context.devices[self.surface.dev_id];
        self.renderer
            .render_to_texture(
                &device.device,
                &device.queue,
                scene,
                &self.surface.target_view,
                &vello::RenderParams {
                    base_color: background,
                    width: self.surface.config.width,
                    height: self.surface.config.height,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .map_err(|e| invalid(e.to_string()))?;
        let frame = match self.surface.surface.get_current_texture() {
            vello::wgpu::CurrentSurfaceTexture::Success(frame)
            | vello::wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            _ => {
                self.context.configure_surface(&self.surface);
                return Ok(Presentation::Unavailable);
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = device.device.create_command_encoder(&Default::default());
        self.surface.blitter.copy(
            &device.device,
            &mut encoder,
            &self.surface.target_view,
            &view,
        );
        device.queue.submit([encoder.finish()]);
        window.pre_present_notify();
        frame.present();
        Ok(Presentation::Presented)
    }
}

pub fn print_chrome_metrics() -> Result<()> {
    println!("{:?}", window_chrome::for_preview()?);
    Ok(())
}

pub fn run() -> Result<()> {
    let event_loop = EventLoop::new().map_err(|e| invalid(e.to_string()))?;
    let mut app = App::new(Store::open()?)?;
    event_loop
        .run_app(&mut app)
        .map_err(|e| invalid(e.to_string()))
}

#[derive(Default)]
pub enum PreviewState {
    #[default]
    Comparison,
    GraphMenu,
    FilterMenu,
    TrimEdit,
    Generation,
    GenerationHelp,
    Preferences,
    LoginApproval,
    Themes,
    Light,
    Presets,
    PresetName,
    Compact,
    PresetList,
    Failed,
    Running,
    Spectrum,
    SpectrumSettings,
    SpectrumLight,
}
impl std::str::FromStr for PreviewState {
    type Err = irresponsible::error::Error;
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "comparison" => Ok(Self::Comparison),
            "graph-menu" => Ok(Self::GraphMenu),
            "filter-menu" => Ok(Self::FilterMenu),
            "trim-edit" => Ok(Self::TrimEdit),
            "generation" => Ok(Self::Generation),
            "generation-help" => Ok(Self::GenerationHelp),
            "preferences" => Ok(Self::Preferences),
            "login-approval" => Ok(Self::LoginApproval),
            "themes" => Ok(Self::Themes),
            "light" => Ok(Self::Light),
            "presets" => Ok(Self::Presets),
            "preset-name" => Ok(Self::PresetName),
            "compact" => Ok(Self::Compact),
            "presets-list" => Ok(Self::PresetList),
            "failed" => Ok(Self::Failed),
            "running" => Ok(Self::Running),
            "spectrum" => Ok(Self::Spectrum),
            "spectrum-settings" => Ok(Self::SpectrumSettings),
            "spectrum-light" => Ok(Self::SpectrumLight),
            _ => Err(invalid(
                "Render state must be comparison, graph-menu, filter-menu, trim-edit, generation, generation-help, preferences, login-approval, themes, light, presets, preset-name, compact, presets-list, failed, running, spectrum, spectrum-settings, or spectrum-light",
            )),
        }
    }
}

pub fn render_preview(
    path: &std::path::Path,
    dimensions: [u32; 2],
    scale: f64,
    state: PreviewState,
) -> Result<()> {
    WindowSize::try_from(dimensions)?;
    let compact_preview = matches!(state, PreviewState::Compact)
        || dimensions[0] < 880
        || f64::from(dimensions[1]) < layout::MIN_HEIGHT;
    if !scale.is_finite() || !(0.5..=3.0).contains(&scale) {
        return Err(invalid("Render scale must be between 0.5 and 3"));
    }
    let directory = tempfile::tempdir()?;
    let store = Store::at(directory.path())?;
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let catalog = if fixtures.exists() {
        Some(
            Catalog::parse(
                &std::fs::read_to_string(fixtures.join("autoeq-index.md"))?,
                &std::fs::read_to_string(fixtures.join("autoeq-recommended.md"))?,
                irresponsible::catalog::UPSTREAM_REVISION,
            )?
            .with_inventory(&std::fs::read(fixtures.join("autoeq-inventory.json"))?)?,
        )
    } else {
        None
    };
    if let Some(catalog) = &catalog {
        let entry = catalog
            .entries
            .iter()
            .find(|entry| entry.path == "oratory1990/over-ear/Beyerdynamic%20DT%20990%20Pro")
            .expect("fixture has the loaded profile");
        let correction = irresponsible::catalog::generate(
            entry,
            &catalog.revision,
            &std::fs::read(fixtures.join("dt990-pro.csv"))?,
            irresponsible::catalog::TargetData::Reference(catalog.target(entry, Target::Harman)?),
            Rate::Hz44100,
            GenerationSettings::default(),
        )?;
        let hash = store.put(&serde_json::to_vec(&correction)?)?;
        store.save(&Settings {
            active: Some(hash),
            ..Settings::default()
        })?;
    }
    let mut app = App::new(store)?;
    app.settings
        .presets
        .insert(app.active.hash, "DT 990 / everyday".into());
    app.catalog = catalog;
    if let Some(catalog) = &app.catalog {
        let index = catalog.entries.iter().position(|entry| entry.path == "Auriculares%20Argentina/over-ear/Beyerdynamic%20DT%20990%20Pro%20(250%20Ohm)").expect("fixture has the comparison profile");
        let entry = &catalog.entries[index];
        app.settings.generation = app.settings.generation.with_treble(TrebleExtension::AutoEq);
        app.settings.catalog_target = Some(CatalogTarget::Reference);
        let correction = irresponsible::catalog::generate(
            entry,
            &catalog.revision,
            &std::fs::read(fixtures.join("dt990-argentina.csv"))?,
            irresponsible::catalog::TargetData::Reference(catalog.target(entry, Target::Harman)?),
            Rate::Hz44100,
            app.settings.generation,
        )?;
        app.selected = Some(SelectedProfile::compare(Selection::Catalog(index)));
        app.scroll = catalog
            .filtered(app.filter.text(), app.settings.catalog_filter)
            .position(|(selected, _)| selected == index)
            .unwrap_or(0) as f64
            * Layout::RESULT_HEIGHT;
        app.preview = Some(View::prepare(
            correction,
            app.settings.controls,
            &app.store,
        )?);
        app.message = "Fixture preview / selected Argentina Harman profile with extended treble. Audio is stopped.".into();
    }
    app.window_chrome = Some(window_chrome::for_preview()?);
    let size = Size::new(f64::from(dimensions[0]), f64::from(dimensions[1]));
    if compact_preview {
        app.action(Action::Compact);
    }
    match state {
        PreviewState::Spectrum | PreviewState::SpectrumSettings | PreviewState::SpectrumLight => {
            if matches!(state, PreviewState::SpectrumLight) {
                app.system_appearance = Appearance::Light;
                app.theme = Theme::new(app.settings.appearance.selected(Appearance::Light));
            }
            let (mut tap, stream) = analysis::channel(Rate::Hz44100);
            app.spectrum.attach(stream);
            for i in 0..app.settings.spectrum.resolution.frames() {
                let t = i as f32 / 44100.0;
                let value: f32 = [
                    (86.0, 0.15),
                    (220.0, 0.07),
                    (660.0, 0.04),
                    (1800.0, 0.018),
                    (6200.0, 0.008),
                ]
                .iter()
                .map(|&(hz, level)| level * (std::f32::consts::TAU * hz * t).sin())
                .sum();
                tap.push([value, -value], [value, -value]);
            }
            app.spectrum.update();
            app.lifecycle = Lifecycle::Running {
                rate: Rate::Hz44100,
                estimated_latency_ms: 30.5,
            };
            app.message = "Spectrum fixture / no audio streams opened.".into();
            if matches!(state, PreviewState::SpectrumSettings) {
                app.action(Action::BrowseSpectrum);
            }
        }
        PreviewState::Comparison => {}
        PreviewState::Running => {
            app.lifecycle = Lifecycle::Running {
                rate: Rate::Hz44100,
                estimated_latency_ms: 30.5,
            };
            app.details = true;
            app.message =
                "Fixture preview / simulated running state; no audio streams opened.".into();
        }
        PreviewState::Failed => {
            app.lifecycle = Lifecycle::Failed("Fixture route failure".into());
        }
        PreviewState::Preferences => {
            app.action(Action::Preferences);
            app.login = Ok(login::Status::Disabled);
        }
        PreviewState::LoginApproval => {
            app.action(Action::Preferences);
            app.login = Ok(login::Status::ApprovalRequired);
        }
        PreviewState::Themes => {
            app.action(Action::Preferences);
            app.open_picker(PickerKind::Themes(ThemeSlot::Dark));
        }
        PreviewState::Light => {
            app.system_appearance = Appearance::Light;
            app.theme = Theme::new(app.settings.appearance.selected(Appearance::Light));
            app.action(Action::Preferences);
        }
        PreviewState::Presets => app.open_picker(PickerKind::Presets),
        PreviewState::PresetName => app.action(Action::SavePreset),
        PreviewState::Compact => {}
        PreviewState::PresetList => {
            if let Some((view, _)) = &app.preview {
                app.settings
                    .presets
                    .insert(view.hash, view.correction.name.clone());
            }
            app.action(Action::BrowsePresets);
        }
        PreviewState::Generation => app.action(Action::BrowseGeneration),
        PreviewState::GenerationHelp => {
            app.action(Action::BrowseGeneration);
            app.frames.resized(SurfaceArea::NonEmpty, Instant::now());
            app.build_scene(size, scale);
            let bounds = app
                .hits
                .iter()
                .find(|(_, action)| {
                    *action
                        == Action::EditNumber(NumberField::Generation(
                            GenerationParameter::Smoothing,
                        ))
                })
                .expect("generation preview has a smoothing control")
                .0;
            app.cursor = Point::new(INSET + 4.0, bounds.center().y);
            let target = app.help_target();
            let start = Instant::now();
            app.help.update(target, start);
            app.help.update(target, start + help::DELAY);
        }
        PreviewState::FilterMenu => app.open_picker(PickerKind::CatalogFilter),
        PreviewState::GraphMenu => {
            let geometry = if compact_preview {
                GraphLayout::compact(
                    size,
                    app.window_chrome.expect("preview chrome"),
                    app.has_candidate(),
                )
            } else {
                Layout::new(
                    size,
                    app.details,
                    app.has_candidate(),
                    app.window_chrome.expect("preview chrome"),
                )
                .graph_layout()
            };
            app.open_picker(PickerKind::Graph(Point::new(
                geometry.graph.x0 + 24.0,
                geometry.graph.y0 + 64.0,
            )));
        }
        PreviewState::TrimEdit => {
            app.action(Action::EditNumber(NumberField::ComparisonTrim));
            app.text_key(&Key::Character("-3.25".into()), Some("-3.25"));
            app.focused_field_mut()
                .expect("trim preview is focused")
                .select_all();
        }
    }
    crate::render::png(
        &app.build_scene(size, scale),
        path,
        (size.width * scale).round() as u32,
        (size.height * scale).round() as u32,
        app.theme.current.bg,
    )
}

struct App {
    store: Store,
    settings: Settings,
    theme: Theme,
    system_appearance: Appearance,
    runtime: Runtime,
    spectrum: Analyzer,
    lifecycle: Lifecycle,
    devices: Vec<audio::DeviceInfo>,
    catalog: Option<Catalog>,
    selected: Option<SelectedProfile>,
    filter: TextField,
    text_focus: Option<TextFocus>,
    scroll: f64,
    active: View,
    preview: Option<(View, Box<Engine>)>,
    awaiting: Option<View>,
    job: Option<Job>,
    message: String,
    window: Option<Arc<Window>>,
    graphics: Option<Graphics>,
    menu: Option<MenuBar>,
    fonts: FontContext,
    layout: LayoutContext,
    cursor: Point,
    modifiers: ModifiersState,
    hits: Vec<(Rect, Action)>,
    dragging: Option<Drag>,
    frames: FrameSchedule,
    details: bool,
    show_loaded: bool,
    show_candidate: bool,
    show_measured: bool,
    show_target: bool,
    show_prediction: bool,
    peaks: (f32, f32),
    dirty: bool,
    settings_healthy: bool,
    login: std::result::Result<login::Status, String>,
    browser: Browser,
    persist_after: Option<Instant>,
    icons: Icons,
    pressed: Option<Action>,
    help: help::Help,
    picker: Option<Picker>,
    last_click: Option<(Instant, Point, ClickTarget, ClickCount)>,
    filter_layout: Option<chrome::FieldLayout>,
    clipboard: Option<arboard::Clipboard>,
    window_chrome: Option<WindowChrome>,
}

impl App {
    fn new(store: Store) -> Result<Self> {
        let (mut settings, mut message, settings_healthy) = match store.load() {
            Ok(settings) => (
                settings,
                "Audio starts automatically. Select a profile to compare; double-click to load."
                    .into(),
                true,
            ),
            Err(error) => (
                Settings::default(),
                format!(
                    "Settings could not be read: {error}. Existing settings and cached IRs are preserved."
                ),
                false,
            ),
        };
        let devices = match audio::devices() {
            Ok(devices) => devices,
            Err(e) => {
                message = e.to_string();
                Vec::new()
            }
        };
        if settings.route.is_none() {
            settings.route = audio::default_route(&devices);
        }
        let rate = settings
            .route
            .as_ref()
            .and_then(|r| devices.iter().find(|d| d.uid == r.playback.uid))
            .and_then(|d| d.rate)
            .unwrap_or_default();
        let active = if let Some(hash) = &settings.active {
            match View::recall(*hash, rate, settings.controls, &store) {
                Ok((view, _)) => view,
                Err(error) => {
                    message = format!(
                        "Saved correction is unavailable: {error}. Start will use an identity filter."
                    );
                    View::prepare(Correction::identity(rate), settings.controls, &store)?.0
                }
            }
        } else {
            View::prepare(Correction::identity(rate), settings.controls, &store)?.0
        };
        if let Origin::AutoEq { generation, .. } = &active.correction.origin {
            settings.generation = *generation;
        }
        let runtime = Runtime::new(active.correction.clone(), settings.controls, store.clone())?;
        let catalog = Source::new(store.clone())?.cached_catalog().ok();
        let system_appearance = Appearance::Dark;
        let theme = Theme::new(settings.appearance.selected(system_appearance));
        Ok(Self {
            theme,
            system_appearance,
            store,
            spectrum: Analyzer::new(settings.spectrum),
            settings,
            runtime,
            lifecycle: Lifecycle::Stopped,
            devices,
            catalog,
            selected: None,
            filter: TextField::new("DT 990".into()),
            text_focus: None,
            scroll: 0.0,
            active,
            preview: None,
            awaiting: None,
            job: None,
            message,
            window: None,
            graphics: None,
            menu: None,
            fonts: chrome::fonts(),
            layout: LayoutContext::new(),
            cursor: Point::ORIGIN,
            modifiers: ModifiersState::empty(),
            hits: Vec::new(),
            dragging: None,
            frames: FrameSchedule::new(Instant::now()),
            details: false,
            show_loaded: true,
            show_candidate: true,
            show_measured: true,
            show_target: true,
            show_prediction: false,
            peaks: (0.0, 0.0),
            dirty: true,
            settings_healthy,
            login: login::status().map_err(|error| error.to_string()),
            browser: Browser::Catalog,
            persist_after: None,
            icons: Icons::new(),
            pressed: None,
            help: help::Help::default(),
            picker: None,
            last_click: None,
            filter_layout: None,
            clipboard: None,
            window_chrome: None,
        })
    }
    fn refresh_login(&mut self) {
        self.login = login::status().map_err(|error| error.to_string());
        self.dirty = true;
    }
    fn save(&mut self) {
        if self.settings_healthy
            && let Err(e) = self.store.save(&self.settings)
        {
            self.message = format!("Could not save settings: {e}");
        }
    }
    fn command(&mut self, command: Command) {
        let starting = matches!(command, Command::Start(_, _));
        match self.runtime.send(command) {
            Ok(()) if starting => self.lifecycle = Lifecycle::Starting,
            Ok(()) => {}
            Err(error) => self.message = error.to_string(),
        }
        self.dirty = true;
    }
    fn controls_changed(&mut self) {
        self.active.analysis.configure(self.settings.controls);
        if let Some((view, engine)) = &mut self.preview {
            view.analysis.configure(self.settings.controls);
            engine.configure(self.settings.controls);
        }
        self.command(Command::Controls(self.settings.controls));
        self.persist_after = Some(Instant::now() + Duration::from_millis(300));
        if let Some(menu) = &self.menu {
            menu.status(&self.lifecycle, self.settings.controls);
        }
    }
    fn volume(&mut self, db: f32) {
        self.settings.controls.volume =
            VolumeDb::try_from(db.clamp(-80.0, 0.0)).expect("slider clamps to valid volume");
        self.controls_changed();
    }
    fn current_rate(&self) -> Rate {
        if let Lifecycle::Running { rate, .. } = self.lifecycle {
            return rate;
        }
        self.settings
            .route
            .as_ref()
            .and_then(|r| self.devices.iter().find(|d| d.uid == r.playback.uid))
            .and_then(|d| d.rate)
            .unwrap_or_default()
    }
    fn job(&mut self, task: impl FnOnce(Arc<AtomicBool>) -> Result<JobResult> + Send + 'static) {
        self.submit(Box::new(task));
    }
    fn submit(&mut self, task: Task) {
        self.message =
            "Preparing the selected profile. The loaded correction keeps playing.".into();
        self.dirty = true;
        if let Some(job) = &mut self.job {
            job.cancel.store(true, Ordering::Relaxed);
            job.next = Some(task);
            return;
        }
        let (tx, receiver) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        match std::thread::Builder::new()
            .name("correction-prepare".into())
            .spawn(move || {
                let result = task(flag.clone());
                let _ = tx.send(if flag.load(Ordering::Relaxed) {
                    Ok(JobResult::Nothing)
                } else {
                    result
                });
            }) {
            Ok(_) => {
                self.job = Some(Job {
                    receiver,
                    cancel,
                    next: None,
                })
            }
            Err(error) => self.message = error.to_string(),
        }
    }
    fn initialize(&mut self) {
        if let Some(route) = self.settings.route.clone() {
            self.command(Command::Start(route, SystemOutput::RouteThroughCapture));
        }
        if self.catalog.as_ref().is_none_or(|catalog| {
            catalog.entries.is_empty() || !catalog.inventory_known() || !catalog.metadata_known()
        }) {
            self.action(Action::RefreshCatalog);
        }
    }
    fn target(&mut self, target: Target) {
        let Some(Selection::Catalog(index)) =
            self.selected.as_ref().map(|selected| &selected.source)
        else {
            self.message = "Select an exact headphone and measurement source first.".into();
            self.dirty = true;
            return;
        };
        let index = *index;
        self.selected
            .as_mut()
            .expect("selected catalog profile")
            .intent = SelectionIntent::Compare;
        self.show_candidate = true;
        let catalog = self
            .catalog
            .as_ref()
            .expect("selected entry belongs to catalog")
            .clone();
        let entry = catalog.entries[index].clone();
        let rate = self.current_rate();
        let controls = self.settings.controls;
        let store = self.store.clone();
        let generation = self.settings.generation;
        self.preview = None;
        self.job(move |cancel| {
            let ir = Source::new(store.clone())?
                .prepare(&catalog, &entry, target, rate, generation, &cancel)?;
            let (view, engine) = View::prepare(ir, controls, &store)?;
            Ok(JobResult::Prepared(view, engine))
        });
    }
    fn editable_catalog_index(&self) -> Option<usize> {
        let catalog = self.catalog.as_ref()?;
        if let Some(Selection::Catalog(index)) =
            self.selected.as_ref().map(|selected| &selected.source)
        {
            return Some(*index);
        }
        let mut view = self
            .preview
            .as_ref()
            .map(|(view, _)| view)
            .or(self.awaiting.as_ref())
            .unwrap_or(&self.active);
        if let Some(Selection::Saved(hash)) =
            self.selected.as_ref().map(|selected| &selected.source)
            && view.hash != *hash
        {
            if self.active.hash != *hash {
                return None;
            }
            view = &self.active;
        }
        let path = match &view.correction.origin {
            Origin::AutoEq { path, .. } | Origin::PublishedAutoEq { path, .. } => path,
            _ => return None,
        };
        catalog.entries.iter().position(|entry| entry.path == *path)
    }
    fn catalog_target(&self) -> CatalogTarget {
        self.settings
            .catalog_target
            .unwrap_or(match self.active.correction.target {
                Target::Harman | Target::Reference => CatalogTarget::Reference,
                Target::Flat => CatalogTarget::Flat,
                Target::AutoEq | Target::Custom => CatalogTarget::AutoEq,
            })
    }
    fn select_saved(&mut self, hash: ContentHash) {
        let source = Selection::Saved(hash);
        if self
            .selected
            .as_ref()
            .is_some_and(|selected| selected.source == source)
        {
            return;
        }
        self.selected = Some(SelectedProfile::compare(source));
        self.prepare_selected();
    }
    fn prepare_selected(&mut self) {
        let source = self
            .selected
            .as_ref()
            .expect("preparation has a selection")
            .source
            .clone();
        match source {
            Selection::Catalog(index) => {
                let entry = &self.catalog.as_ref().expect("selected catalog").entries[index];
                if let Some(target) = self.catalog_target().resolve(entry, self.current_rate()) {
                    self.target(target);
                } else {
                    self.preview = None;
                    if let Some(job) = &mut self.job {
                        job.cancel.store(true, Ordering::Relaxed);
                        job.next = None;
                    }
                    self.message = format!(
                        "This profile cannot use {}. Choose another mode or profile.",
                        self.catalog_target().label()
                    );
                }
            }
            Selection::Saved(hash) => {
                self.selected
                    .as_mut()
                    .expect("selected saved profile")
                    .intent = SelectionIntent::Compare;
                self.preview = None;
                self.show_candidate = true;
                let store = self.store.clone();
                let rate = self.current_rate();
                let controls = self.settings.controls;
                self.job(move |_| {
                    let (view, engine) = View::recall(hash, rate, controls, &store)?;
                    Ok(JobResult::Prepared(view, engine))
                });
            }
        }
    }
    fn selection_is_active(&self) -> bool {
        if let Some((view, _)) = &self.preview {
            return view.hash == self.active.hash;
        }
        match self.selected.as_ref().map(|selected| &selected.source) {
            Some(Selection::Saved(hash)) => *hash == self.active.hash,
            Some(Selection::Catalog(index)) => {
                let catalog = self.catalog.as_ref().expect("selected catalog");
                let entry = &catalog.entries[*index];
                let target = self.catalog_target().resolve(entry, self.current_rate());
                if target != Some(self.active.correction.target) {
                    return false;
                }
                match &self.active.correction.origin {
                    Origin::PublishedAutoEq { path, revision, .. } => {
                        *path == entry.path && *revision == catalog.revision
                    }
                    Origin::AutoEq {
                        path,
                        revision,
                        target,
                        generation,
                        ..
                    } => {
                        *path == entry.path
                            && *revision == catalog.revision
                            && *generation == self.settings.generation
                            && (self.active.correction.target == Target::Flat
                                || catalog
                                    .target(entry, self.active.correction.target)
                                    .is_ok_and(|definition| definition.description() == *target))
                    }
                    _ => false,
                }
            }
            None => false,
        }
    }
    fn replace_loaded(&mut self) {
        if let Some(selected) = &mut self.selected {
            selected.intent = SelectionIntent::Compare;
        }
        if let Some((view, mut engine)) = self.preview.take() {
            engine.configure(self.settings.controls);
            match self.runtime.activate(engine) {
                Ok(()) => {
                    self.awaiting = Some(view);
                    self.message = "Replacing the loaded profile; waiting for confirmation.".into();
                }
                Err(engine) => {
                    self.preview = Some((view, engine));
                    self.message = "The audio engine could not accept the replacement. The loaded profile is unchanged; retry Replace loaded profile.".into();
                }
            }
        }
        self.dirty = true;
    }
    fn activate_requested(&mut self) {
        if !self
            .selected
            .as_ref()
            .is_some_and(|selected| selected.intent == SelectionIntent::Load)
        {
            return;
        }
        if self.selection_is_active() {
            self.selected.as_mut().expect("requested selection").intent = SelectionIntent::Compare;
        } else if self.awaiting.is_none() && self.preview.is_some() {
            self.replace_loaded();
        }
    }
    fn request_load(&mut self) {
        if self.selection_is_active() {
            return;
        }
        if self.preview.is_none() && self.job.is_none() && self.awaiting.is_none() {
            self.prepare_selected();
        }
        if self.preview.is_some() || self.job.is_some() || self.awaiting.is_some() {
            self.selected
                .as_mut()
                .expect("load has a selected profile")
                .intent = SelectionIntent::Load;
            self.activate_requested();
        }
    }
    fn click(&mut self, action: Action) {
        let source = match action {
            Action::Select(index) => Some(Selection::Catalog(index)),
            Action::Recall(hash) => Some(Selection::Saved(hash)),
            _ => None,
        };
        if let Some(source) = source {
            let count = self.record_click(ClickTarget::Profile(source));
            self.action(action);
            if matches!(count, ClickCount::Double) {
                self.request_load();
            }
        } else {
            self.last_click = None;
            self.action(action);
        }
    }
    fn action(&mut self, action: Action) {
        self.dirty |= self.help.clear();
        if !matches!(action, Action::EditNumber(target) if matches!(self.text_focus, Some(TextFocus::Number { target: current, .. }) if current == target))
            && matches!(self.text_focus, Some(TextFocus::Number { .. }))
            && !self.commit_number()
        {
            return;
        }
        if matches!(action, Action::PresetName | Action::CommitPresetName) {
            if action == Action::CommitPresetName {
                self.commit_preset_name();
            }
            self.dirty = true;
            return;
        }
        if self.picker.is_some()
            && !matches!(action, Action::PickerSearch | Action::ClearPickerSearch)
        {
            self.dismiss_picker();
        }
        if action == Action::Search
            && self.settings.window_mode == WindowMode::Compact
            && self.browser != Browser::Presets
        {
            self.toggle_compact();
        }
        self.text_focus = if action == Action::Search {
            Some(TextFocus::Search)
        } else {
            None
        };
        if action == Action::Search
            && matches!(
                self.browser,
                Browser::Generation | Browser::General | Browser::Spectrum
            )
        {
            self.browser = Browser::Catalog;
            self.scroll = 0.0;
        }
        match action {
            Action::Scroll(..) => unreachable!("scrollbar dragging begins on mouse press"),
            Action::Preferences => {
                self.dismiss_picker();
                self.browser = if self.browser.is_preferences() {
                    Browser::Catalog
                } else {
                    Browser::General
                };
                self.scroll = 0.0;
                if self.browser == Browser::General {
                    self.refresh_login();
                }
            }
            Action::Compact => self.toggle_compact(),
            Action::ThemePicker(slot) => self.open_picker(PickerKind::Themes(slot)),
            Action::PresetPicker => self.open_picker(PickerKind::Presets),
            Action::LaunchAtLogin => {
                let change = self
                    .login
                    .as_ref()
                    .expect("login toggle has an available status")
                    .change()
                    .expect("login toggle is available");
                match login::change(change) {
                    Ok(status) => self.login = Ok(status),
                    Err(error) => {
                        self.refresh_login();
                        self.message = format!("Could not update login launch: {error}");
                    }
                }
            }
            Action::LoginSettings => {
                if let Err(error) = login::open_settings() {
                    self.message = error.to_string();
                }
            }
            Action::FollowSystem => {
                let preferences = &mut self.settings.appearance;
                preferences.mode = match preferences.mode {
                    ThemeMode::System => {
                        preferences.fixed = preferences.selected(self.system_appearance).clone();
                        ThemeMode::Fixed
                    }
                    ThemeMode::Fixed => ThemeMode::System,
                };
                self.theme
                    .show(preferences.selected(self.system_appearance));
                self.save();
            }
            Action::ChooseTheme(slot, index) => {
                self.settings
                    .appearance
                    .set_theme(slot, appearance::registry()[index].id.clone());
                self.theme
                    .show(self.settings.appearance.selected(self.system_appearance));
                self.dismiss_picker();
                self.save();
            }
            Action::LoadPreset(hash) => {
                self.dismiss_picker();
                self.select_saved(hash);
                self.request_load();
            }
            Action::SavePreset => {
                if matches!(self.active.correction.origin, Origin::Identity) {
                    self.message = "Load a correction before saving a preset.".into();
                    return;
                }
                self.edit_preset_name(self.active.hash);
            }
            Action::RenamePreset(hash) => self.edit_preset_name(hash),
            Action::PresetName => unreachable!("the preset name field retains its editor"),
            Action::CommitPresetName => {
                self.commit_preset_name();
            }
            Action::CancelPresetName => {
                self.text_focus = None;
            }
            Action::PickerSearch => self.text_focus = Some(TextFocus::Picker),
            Action::ClearPickerSearch => {
                self.picker
                    .as_mut()
                    .expect("picker clear has an open menu")
                    .filter
                    .clear();
                self.text_focus = Some(TextFocus::Picker);
                self.filter_picker();
            }

            Action::EditNumber(target) => {
                let value = self.number_value(target);
                let mut field = TextField::new(value.to_string());
                field.select_all();
                self.text_focus = Some(TextFocus::Number {
                    target,
                    field,
                    layout: None,
                });
            }
            Action::GenerationLength => self.open_picker(PickerKind::GenerationLength),
            Action::GenerationAlignment => self.open_picker(PickerKind::GenerationAlignment),
            Action::GenerationNormalization => {
                self.open_picker(PickerKind::GenerationNormalization)
            }
            Action::ChooseLength(value) => {
                self.generation_changed(self.settings.generation.with_length(value));
            }
            Action::ChooseAlignment(value) => {
                self.generation_changed(self.settings.generation.with_alignment(value));
            }
            Action::ChooseNormalization(value) => {
                self.generation_changed(self.settings.generation.with_normalization(value));
            }
            Action::GenerationBass => {
                let bass = match self.settings.generation.bass() {
                    BassExtension::Extended => BassExtension::Tapered,
                    BassExtension::Tapered => BassExtension::Extended,
                };
                self.generation_changed(self.settings.generation.with_bass(bass));
            }
            Action::GenerationConcha => {
                let concha = match self.settings.generation.concha() {
                    ConchaInterference::Absent => ConchaInterference::Present,
                    ConchaInterference::Present => ConchaInterference::Absent,
                };
                self.generation_changed(self.settings.generation.with_concha(concha));
            }
            Action::GenerationCutLimit => {
                let generation = self.settings.generation;
                let generation = if generation.cut_limit().is_some() {
                    generation.without_cut_limit()
                } else {
                    generation
                        .with(
                            GenerationParameter::CutLimit,
                            generation.value(GenerationParameter::CutLimit),
                        )
                        .expect("the default cut limit is valid")
                };
                self.generation_changed(generation);
            }
            Action::GenerationDefaults => {
                self.generation_changed(self.settings.generation.reset_parameters())
            }
            Action::SpectrumEnabled => {
                self.settings.spectrum.enabled = !self.settings.spectrum.enabled;
                self.spectrum.configure(self.settings.spectrum);
                self.save();
            }
            Action::SpectrumPicker(setting) => self.open_picker(PickerKind::Spectrum(setting)),
            Action::ChooseSpectrum(choice) => {
                self.settings.spectrum.choose(choice);
                if matches!(choice, SpectrumChoice::FrameRate(_)) {
                    self.frames.limit_changed(Instant::now());
                }
                self.spectrum.configure(self.settings.spectrum);
                self.save();
            }
            Action::BrowseSpectrum => {
                self.browser = Browser::Spectrum;
                self.scroll = 0.0;
            }
            Action::BrowseGeneral => {
                self.browser = Browser::General;
                self.refresh_login();
                self.dismiss_picker();
                self.scroll = 0.0;
            }
            Action::BrowseGeneration => {
                self.browser = Browser::Generation;
                self.scroll = 0.0;
            }
            Action::CatalogFilter => self.open_picker(PickerKind::CatalogFilter),
            Action::ChooseCatalogFilter(filter) => {
                self.settings.catalog_filter = filter;
                self.scroll = 0.0;
                self.save();
            }
            Action::BrowsePresets => {
                if self.browser != Browser::Presets {
                    self.filter.clear();
                }
                self.browser = Browser::Presets;
                self.selected = None;
                self.scroll = 0.0;
            }
            Action::Recall(hash) => self.select_saved(hash),
            Action::DeletePreset(hash) => {
                self.settings.presets.remove(&hash);
                self.save();
                self.message = "Preset removed. The loaded IR keeps playing.".into();
            }
            Action::RecoverSettings => {
                let outcome = (|| -> Result<()> {
                    let path = self.store.root().join("settings.json");
                    let bytes = std::fs::read(&path)?;
                    let hash = self.store.put(&bytes)?;
                    self.store
                        .write(&format!("settings-backup-{}.json", hash.as_str()), &bytes)?;
                    self.store.save(&self.settings)
                })();
                match outcome {
                    Ok(()) => {
                        self.settings_healthy = true;
                        self.message =
                            "Current settings saved. The unreadable document was backed up.".into();
                    }
                    Err(e) => self.message = e.to_string(),
                }
            }
            Action::StartStop => match self.lifecycle {
                Lifecycle::Running { .. } | Lifecycle::Starting | Lifecycle::Recovering { .. } => {
                    self.command(Command::Stop)
                }
                _ => {
                    if let Some(route) = self.settings.route.clone() {
                        self.command(Command::Start(route, SystemOutput::RouteThroughCapture));
                    } else {
                        self.message = "Select both audio devices first.".into();
                    }
                }
            },
            Action::Retry => self.command(Command::Retry),
            Action::Capture => self.open_picker(PickerKind::Device(RouteSide::Capture)),
            Action::Playback => self.open_picker(PickerKind::Device(RouteSide::Playback)),
            Action::CapturePair => self.open_picker(PickerKind::Pair(RouteSide::Capture)),
            Action::PlaybackPair => self.open_picker(PickerKind::Pair(RouteSide::Playback)),
            Action::ChooseDevice(side, index) => {
                self.choose_device(side, index);
            }
            Action::ChoosePair(side, pair) => {
                self.choose_pair(side, pair);
            }
            Action::RefreshDevices => match audio::devices() {
                Ok(devices) => {
                    self.devices = devices;
                    if self.settings.route.is_none() {
                        self.settings.route = audio::default_route(&self.devices);
                    }
                }
                Err(e) => self.message = e.to_string(),
            },
            Action::Search => {}
            Action::ClearSearch => {
                self.filter.clear();
                self.scroll = 0.0;
                self.text_focus = Some(TextFocus::Search);
            }
            Action::RefreshCatalog => {
                if self.job.is_none() {
                    let store = self.store.clone();
                    self.job(move |cancel| {
                        Ok(JobResult::Catalog(Source::new(store)?.refresh(&cancel)?))
                    });
                    self.message = "Loading the AutoEq library...".into();
                }
            }
            Action::Select(index) => {
                let source = Selection::Catalog(index);
                if self
                    .selected
                    .as_ref()
                    .is_some_and(|selected| selected.source == source)
                {
                    return;
                }
                self.selected = Some(SelectedProfile::compare(source));
                self.prepare_selected();
            }
            Action::AutoEq | Action::Harman | Action::Reference | Action::Flat => {
                self.settings.catalog_target = Some(match action {
                    Action::AutoEq => CatalogTarget::AutoEq,
                    Action::Harman | Action::Reference => CatalogTarget::Reference,
                    Action::Flat => CatalogTarget::Flat,
                    _ => unreachable!(),
                });
                self.save();
                if let Some(index) = self.editable_catalog_index() {
                    self.selected = Some(SelectedProfile::compare(Selection::Catalog(index)));
                    self.prepare_selected();
                } else {
                    self.message =
                        "Load the source library to change this profile's target.".into();
                }
            }
            Action::TrebleExtension => {
                let treble = match self.settings.generation.treble() {
                    TrebleExtension::Limited => TrebleExtension::AutoEq,
                    TrebleExtension::AutoEq => TrebleExtension::Limited,
                };
                self.generation_changed(self.settings.generation.with_treble(treble));
            }
            Action::Import => {
                if self.job.is_none() {
                    self.selected = None;
                    let rate = self.current_rate();
                    let controls = self.settings.controls;
                    let store = self.store.clone();
                    self.job(move |cancel| {
                        let Some(path) = pollster::block_on(
                            rfd::AsyncFileDialog::new()
                                .set_title("Import correction impulse response")
                                .add_filter("Impulse response", &["wav"])
                                .pick_file(),
                        ) else {
                            return Ok(JobResult::Nothing);
                        };
                        if cancel.load(Ordering::Relaxed) {
                            return Ok(JobResult::Nothing);
                        }
                        let (view, engine) = View::prepare(
                            Correction::read_wav(path.path(), rate, &store)?,
                            controls,
                            &store,
                        )?;
                        Ok(JobResult::Prepared(view, engine))
                    });
                }
            }
            Action::ReplaceLoaded => self.replace_loaded(),
            Action::Cancel => {
                if let Some(selected) = &mut self.selected {
                    selected.intent = SelectionIntent::Compare;
                }
                if let Some(job) = &mut self.job {
                    job.cancel.store(true, Ordering::Relaxed);
                    job.next = None;
                    self.message =
                        "Cancelling the preparation; the current filter remains active.".into();
                } else if self.preview.is_some() {
                    self.message = "Comparison dismissed. The loaded profile is unchanged.".into();
                }
                self.preview = None;
            }
            Action::Bypass => {
                self.settings.controls.correction = match self.settings.controls.correction {
                    CorrectionMode::On => CorrectionMode::Bypass,
                    CorrectionMode::Bypass => CorrectionMode::On,
                };
                self.controls_changed();
            }
            Action::Compensation => {
                self.settings.controls.comparison =
                    if self.settings.controls.comparison == Comparison::PinkNoise {
                        Comparison::Uncompensated
                    } else {
                        Comparison::PinkNoise
                    };
                self.controls_changed();
            }
            Action::NudgeNumber(target, direction) => {
                let step = match target {
                    NumberField::ComparisonTrim => 0.5,
                    NumberField::SpectrumIntensity => 1.0,
                    NumberField::Generation(parameter) => parameter.step(),
                };
                if let Err(error) =
                    self.set_number(target, self.number_value(target) + step * direction.sign())
                {
                    self.message = error.to_string();
                }
            }
            Action::ValueBar(target) => {
                let bounds = self
                    .hits
                    .iter()
                    .rev()
                    .find(|(_, hit)| *hit == action)
                    .expect("a value bar action comes from its visible control")
                    .0;
                self.dragging = Some(Drag::ValueBar { target, bounds });
                self.value_bar_from_cursor(target, bounds);
            }
            Action::Details => self.details = !self.details,
            Action::ShowLoaded => self.show_loaded = !self.show_loaded,
            Action::ShowCandidate => self.show_candidate = !self.show_candidate,
            Action::ShowMeasured => self.show_measured = !self.show_measured,
            Action::ShowTarget => self.show_target = !self.show_target,
            Action::ShowPrediction => self.show_prediction = !self.show_prediction,
        }
        self.dirty = true;
    }
    fn focused_field(&self) -> Option<&TextField> {
        match &self.text_focus {
            Some(TextFocus::Search) => Some(&self.filter),
            Some(TextFocus::Picker) => {
                Some(&self.picker.as_ref().expect("picker text focus").filter)
            }
            Some(TextFocus::PresetName { field, .. }) => Some(field),
            Some(TextFocus::Number { field, .. }) => Some(field),
            None => None,
        }
    }
    fn focused_field_mut(&mut self) -> Option<&mut TextField> {
        match &mut self.text_focus {
            Some(TextFocus::Search) => Some(&mut self.filter),
            Some(TextFocus::Picker) => {
                Some(&mut self.picker.as_mut().expect("picker text focus").filter)
            }
            Some(TextFocus::PresetName { field, .. }) => Some(field),
            Some(TextFocus::Number { field, .. }) => Some(field),
            None => None,
        }
    }
    fn focused_layout(&self) -> Option<&chrome::FieldLayout> {
        match &self.text_focus {
            Some(TextFocus::Search) => self.filter_layout.as_ref(),
            Some(TextFocus::Picker) => self
                .picker
                .as_ref()
                .expect("picker text focus")
                .layout
                .as_deref(),
            Some(TextFocus::PresetName { layout, .. }) => layout.as_deref(),
            Some(TextFocus::Number { layout, .. }) => layout.as_deref(),
            None => None,
        }
    }
    fn number_value(&self, target: NumberField) -> f32 {
        match target {
            NumberField::ComparisonTrim => self.settings.controls.trim.db(),
            NumberField::SpectrumIntensity => self.settings.spectrum.intensity.percent(),
            NumberField::Generation(parameter) => self.settings.generation.value(parameter),
        }
    }
    fn set_number(&mut self, target: NumberField, value: f32) -> Result<()> {
        if !value.is_finite() {
            return Err(invalid("Enter a finite number, or press Escape to cancel"));
        }
        match target {
            NumberField::ComparisonTrim => {
                self.settings.controls.trim = ComparisonTrim::try_from(value.clamp(-12.0, 12.0))?;
                self.controls_changed();
            }
            NumberField::SpectrumIntensity => {
                self.settings.spectrum.intensity =
                    analysis::Intensity::try_from(value.clamp(0.0, 100.0))?;
                self.persist_after = Some(Instant::now() + Duration::from_millis(300));
                self.dirty = true;
            }
            NumberField::Generation(parameter) => {
                self.generation_changed(self.settings.generation.with(parameter, value)?);
            }
        }
        Ok(())
    }
    fn draw_number(
        p: &mut Painter<'_, Action>,
        bounds: Rect,
        target: NumberField,
        formatted: &str,
        focus: &mut Option<TextFocus>,
    ) {
        let editing = match focus.as_ref() {
            Some(TextFocus::Number {
                target: current,
                field,
                ..
            }) if *current == target => Some(field),
            _ => None,
        };
        let field_layout = p.stepper(
            bounds,
            formatted,
            [
                Action::NudgeNumber(target, StepDirection::Down),
                Action::NudgeNumber(target, StepDirection::Up),
                Action::EditNumber(target),
            ],
            editing,
        );
        if let Some(TextFocus::Number {
            target: current,
            layout,
            ..
        }) = focus
            && *current == target
        {
            *layout = field_layout.map(Box::new);
        }
    }
    fn generation_changed(&mut self, generation: GenerationSettings) {
        let displayed = self.preview.as_ref().map_or(&self.active, |(view, _)| view);
        if self.settings.generation == generation && !displayed.correction.requires_regeneration() {
            return;
        }
        self.settings.generation = generation;
        self.save();
        if matches!(
            self.selected.as_ref().map(|selected| &selected.source),
            Some(Selection::Catalog(_))
        ) {
            if self.catalog_target() != CatalogTarget::AutoEq {
                self.prepare_selected();
            } else {
                self.message="Published AutoEq retains its coefficients; choose an in-app target to edit the IR.".into();
            }
        } else {
            let displayed = self.preview.as_ref().map_or(&self.active, |(view, _)| view);
            let hash = match self.selected.as_ref().map(|selected| &selected.source) {
                Some(Selection::Saved(hash)) => *hash,
                _ => displayed.hash,
            };
            if displayed.hash == hash
                && !matches!(displayed.correction.origin, Origin::AutoEq { .. })
            {
                self.message="Published AutoEq and imported WAVs retain their coefficients. Choose an in-app target to edit the IR.".into();
                self.dirty = true;
                return;
            }
            let store = self.store.clone();
            let rate = self.current_rate();
            let controls = self.settings.controls;
            self.selected = None;
            self.preview = None;
            self.show_candidate = true;
            self.job(move |_| {
                let correction: Correction = serde_json::from_slice(&store.get(&hash)?)?;
                correction.validate()?;
                let (view, engine) =
                    View::prepare(correction.regenerate(rate, generation)?, controls, &store)?;
                Ok(JobResult::Prepared(view, engine))
            });
        }
        self.dirty = true;
    }
    fn commit_number(&mut self) -> bool {
        let Some(TextFocus::Number { target, field, .. }) = &self.text_focus else {
            return true;
        };
        let target = *target;
        let text = field.text().trim();
        let parsed = if let Some((a, b)) = text.split_once('/') {
            a.trim()
                .parse::<f32>()
                .ok()
                .zip(b.trim().parse::<f32>().ok())
                .map(|(a, b)| a / b)
        } else {
            text.parse::<f32>().ok()
        };
        let result = parsed
            .ok_or_else(|| invalid("Enter a number, or press Escape to cancel"))
            .and_then(|value| self.set_number(target, value));
        match result {
            Ok(()) => {
                self.text_focus = None;
                true
            }
            Err(error) => {
                self.message = error.to_string();
                self.dirty = true;
                false
            }
        }
    }
    fn text_key(&mut self, key: &Key, text: Option<&str>) -> bool {
        if self.picker.is_some() {
            match key {
                Key::Named(NamedKey::Escape) => {
                    self.dismiss_picker();
                    return true;
                }
                Key::Named(NamedKey::Enter) => {
                    let action = self.picker.as_ref().expect("open menu").highlighted;
                    if let Some(action) = action {
                        self.action(action);
                    }
                    return true;
                }
                Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) => {
                    self.navigate_picker(if *key == Key::Named(NamedKey::ArrowUp) {
                        Navigation::Previous
                    } else {
                        Navigation::Next
                    });
                    return true;
                }
                _ => {}
            }
        }
        if matches!(self.text_focus, Some(TextFocus::PresetName { .. })) {
            match key {
                Key::Named(NamedKey::Enter | NamedKey::Tab) => {
                    self.commit_preset_name();
                    return true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.text_focus = None;
                    self.dirty = true;
                    return true;
                }
                _ => {}
            }
        }

        let number = match self.text_focus {
            Some(TextFocus::Number { target, .. }) => Some(target),
            _ => None,
        };
        if let Some(target) = number {
            match key {
                Key::Named(NamedKey::Enter | NamedKey::Tab) => {
                    self.commit_number();
                    return true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.text_focus = None;
                    self.dirty = true;
                    return true;
                }
                Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) => {
                    if self.commit_number() {
                        self.action(if *key == Key::Named(NamedKey::ArrowUp) {
                            Action::NudgeNumber(target, StepDirection::Up)
                        } else {
                            Action::NudgeNumber(target, StepDirection::Down)
                        });
                        self.action(Action::EditNumber(target));
                    }
                    return true;
                }
                _ => {}
            }
        }
        let before = self
            .focused_field()
            .expect("text key has focus")
            .text()
            .to_owned();
        let command = self.modifiers.super_key() || self.modifiers.control_key();
        let clipboard_key = if command {
            if let Key::Character(key) = key {
                Some(key.to_ascii_lowercase())
            } else {
                None
            }
        } else {
            None
        };
        let handled = match clipboard_key.as_deref() {
            Some("c" | "x" | "v") => {
                let result = (|| -> Result<()> {
                    if self.clipboard.is_none() {
                        self.clipboard =
                            Some(arboard::Clipboard::new().map_err(|e| invalid(e.to_string()))?);
                    }
                    if clipboard_key.as_deref() == Some("v") {
                        let text = self
                            .clipboard
                            .as_mut()
                            .expect("clipboard initialized")
                            .get_text()
                            .map_err(|e| invalid(e.to_string()))?;
                        self.focused_field_mut()
                            .expect("focused paste")
                            .insert(&text);
                    } else if let Some(text) = self
                        .focused_field()
                        .expect("focused copy")
                        .selected_text()
                        .map(str::to_owned)
                    {
                        self.clipboard
                            .as_mut()
                            .expect("clipboard initialized")
                            .set_text(text)
                            .map_err(|e| invalid(e.to_string()))?;
                        if clipboard_key.as_deref() == Some("x") {
                            self.focused_field_mut().expect("focused cut").cut();
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    self.message = format!("Clipboard: {error}");
                }
                true
            }
            _ => {
                let modifiers = self.modifiers;
                self.focused_field_mut()
                    .expect("focused key")
                    .key(key, text, modifiers)
            }
        };
        if matches!(self.text_focus, Some(TextFocus::Picker))
            && self.focused_field().expect("picker focus").text() != before
        {
            self.filter_picker();
        }
        if matches!(self.text_focus, Some(TextFocus::Search)) && self.filter.text() != before {
            self.scroll = 0.0;
        }
        self.dirty |= handled;
        handled || number.is_some()
    }
    fn record_click(&mut self, target: ClickTarget) -> ClickCount {
        let now = Instant::now();
        let count = match self.last_click.as_ref() {
            Some((time, point, previous, count))
                if target == *previous
                    && now.duration_since(*time) < Duration::from_millis(500)
                    && point.distance(self.cursor) < 4.0 =>
            {
                match count {
                    ClickCount::Single => ClickCount::Double,
                    ClickCount::Double => ClickCount::Triple,
                    ClickCount::Triple => ClickCount::Single,
                }
            }
            _ => ClickCount::Single,
        };
        self.last_click = Some((now, self.cursor, target, count));
        count
    }
    fn navigate_catalog(&mut self, direction: Navigation) {
        if self.browser != Browser::Catalog || self.settings.window_mode == WindowMode::Compact {
            return;
        }
        let Some(catalog) = &self.catalog else {
            return;
        };
        let entries: Vec<_> = catalog
            .filtered(self.filter.text(), self.settings.catalog_filter)
            .map(|(index, _)| index)
            .collect();
        if entries.is_empty() {
            return;
        }
        let current = match self.selected.as_ref().map(|selected| &selected.source) {
            Some(Selection::Catalog(index)) => entries.iter().position(|&entry| entry == *index),
            _ => None,
        };
        let next = match current {
            Some(index) if direction == Navigation::Next => (index + 1).min(entries.len() - 1),
            Some(index) => index.saturating_sub(1),
            None => 0,
        };
        let focused = matches!(self.text_focus, Some(TextFocus::Search));
        self.action(Action::Select(entries[next]));
        if focused {
            self.text_focus = Some(TextFocus::Search);
        }
        let top = next as f64 * Layout::RESULT_HEIGHT;
        let height = self.geometry().listing.height();
        if top < self.scroll {
            self.scroll = top;
        } else if top + Layout::RESULT_HEIGHT > self.scroll + height {
            self.scroll = top + Layout::RESULT_HEIGHT - height;
        }
        self.dirty = true;
    }
    fn route_editable(&self) -> bool {
        matches!(self.lifecycle, Lifecycle::Stopped | Lifecycle::Failed(_))
    }
    fn choose_device(&mut self, side: RouteSide, index: usize) {
        assert!(self.route_editable(), "route edits require stopped streams");
        let device = &self.devices[index];
        let endpoint = |device: &audio::DeviceInfo| Endpoint {
            uid: device.uid.clone(),
            label: device.name.clone(),
            pair: StereoPair::default(),
        };
        if let Some(route) = &mut self.settings.route {
            let selected = match side {
                RouteSide::Capture => &mut route.capture,
                RouteSide::Playback => &mut route.playback,
            };
            *selected = endpoint(device);
        } else {
            let capture = if side == RouteSide::Capture {
                Some(device)
            } else {
                self.devices.iter().find(|d| d.input_channels >= 2)
            };
            let playback = if side == RouteSide::Playback {
                Some(device)
            } else {
                self.devices.iter().find(|d| d.output_channels >= 2)
            };
            if let (Some(capture), Some(playback)) = (capture, playback) {
                self.settings.route = Some(Route {
                    capture: endpoint(capture),
                    playback: endpoint(playback),
                });
            } else {
                self.message = "Connect both a stereo capture and playback device before configuring the route.".into();
            }
        }
        self.save();
    }
    fn choose_pair(&mut self, side: RouteSide, pair: StereoPair) {
        assert!(
            self.route_editable(),
            "channel edits require stopped streams"
        );
        let route = self
            .settings
            .route
            .as_mut()
            .expect("channel picker has a route");
        match side {
            RouteSide::Capture => route.capture.pair = pair,
            RouteSide::Playback => route.playback.pair = pair,
        }
        self.save();
    }
    fn graph_toggles(&self) -> impl Iterator<Item = GraphToggle> {
        let colors = self.theme.current;
        let candidate = self
            .preview
            .as_ref()
            .map(|(view, _)| view)
            .or(self.awaiting.as_ref());
        let metadata = candidate
            .filter(|view| view.correction.measured.is_some())
            .unwrap_or(&self.active);
        [
            Some(GraphToggle {
                label: "Loaded IR",
                action: Action::ShowLoaded,
                selected: self.show_loaded,
                color: colors.basic[3],
            }),
            candidate.map(|_| GraphToggle {
                label: "Selected IR",
                action: Action::ShowCandidate,
                selected: self.show_candidate,
                color: colors.basic[2],
            }),
            metadata.correction.measured.as_ref().map(|_| GraphToggle {
                label: "Measured",
                action: Action::ShowMeasured,
                selected: self.show_measured,
                color: colors.secondary,
            }),
            metadata
                .correction
                .target_curve
                .as_ref()
                .map(|_| GraphToggle {
                    label: "Target",
                    action: Action::ShowTarget,
                    selected: self.show_target,
                    color: colors.basic[4],
                }),
            metadata.correction.measured.as_ref().map(|_| GraphToggle {
                label: "Predicted",
                action: Action::ShowPrediction,
                selected: self.show_prediction,
                color: colors.basic[1],
            }),
        ]
        .into_iter()
        .flatten()
    }
    fn open_picker(&mut self, kind: PickerKind) {
        let options = self.picker_options(kind);
        let highlighted = options
            .iter()
            .find(|option| {
                matches!(
                    option.checks,
                    MenuChecks::Radio(true) | MenuChecks::Checkbox(true)
                )
            })
            .or(options.first())
            .map(|option| option.action);
        let mut picker = Picker::new(kind, highlighted);
        picker.scroll = options
            .iter()
            .position(|option| Some(option.action) == highlighted)
            .unwrap_or(0)
            .saturating_sub(4);
        self.picker = Some(picker);
        self.text_focus = Some(TextFocus::Picker);
        self.dirty = true;
    }
    fn dismiss_picker(&mut self) {
        self.picker = None;
        self.theme
            .show(self.settings.appearance.selected(self.system_appearance));
        if matches!(self.text_focus, Some(TextFocus::Picker)) {
            self.text_focus = None;
        }
        self.dirty = true;
    }
    fn filtered_picker_options(&self) -> Vec<MenuOption> {
        let picker = self.picker.as_ref().expect("filtered open picker");
        let query = search_key(picker.filter.text());
        let terms: Vec<_> = query.split_whitespace().collect();
        self.picker_options(picker.kind)
            .into_iter()
            .filter(|option| {
                let label = search_key(&option.label);
                terms.iter().all(|term| label.contains(term))
            })
            .collect()
    }

    fn filter_picker(&mut self) {
        let options = self.filtered_picker_options();
        let picker = self.picker.as_mut().expect("filtered open picker");
        picker.highlighted = options.first().map(|option| option.action);
        if let Some(Action::ChooseTheme(_, index)) = picker.highlighted {
            self.theme.show(&appearance::registry()[index].id);
        }
        picker.scroll = 0;
        self.dirty = true;
    }
    fn navigate_picker(&mut self, direction: Navigation) {
        let options = self.filtered_picker_options();
        let picker = self.picker.as_mut().expect("navigate open picker");
        if options.is_empty() {
            picker.highlighted = None;
            return;
        }
        let index = options
            .iter()
            .position(|option| Some(option.action) == picker.highlighted)
            .unwrap_or(0);
        let index = match direction {
            Navigation::Previous => index.saturating_sub(1),
            Navigation::Next => (index + 1).min(options.len() - 1),
        };
        picker.highlighted = Some(options[index].action);
        if let Action::ChooseTheme(_, theme_index) = options[index].action {
            self.theme.show(&appearance::registry()[theme_index].id);
        }
        picker.scroll = picker.scroll.min(index);
        if index >= picker.scroll + picker.visible_rows {
            picker.scroll = index + 1 - picker.visible_rows;
        }
        self.dirty = true;
    }
    fn edit_preset_name(&mut self, source: ContentHash) {
        let name = match self.settings.presets.get(&source) {
            Some(name) => name.clone(),
            None if source == self.active.hash => self.active.correction.name.clone(),
            None => {
                self.message = "This preset was removed.".into();
                return;
            }
        };
        self.dismiss_picker();
        let mut field = TextField::new(name);
        field.select_all();
        self.text_focus = Some(TextFocus::PresetName {
            source,
            field,
            layout: None,
        });
    }
    fn commit_preset_name(&mut self) -> bool {
        let Some(TextFocus::PresetName { source, field, .. }) = &self.text_focus else {
            return true;
        };
        let name = field.text().trim();
        if name.is_empty() || name.chars().count() > 100 {
            self.message = "Use a preset name between 1 and 100 characters.".into();
            self.dirty = true;
            return false;
        }
        self.settings.presets.insert(*source, name.to_owned());
        self.text_focus = None;
        self.save();
        self.message = "Preset saved.".into();
        self.dirty = true;
        true
    }
    fn picker_options(&self, picker: PickerKind) -> Vec<MenuOption> {
        let endpoint = |side| {
            self.settings.route.as_ref().map(|route| match side {
                RouteSide::Capture => &route.capture,
                RouteSide::Playback => &route.playback,
            })
        };
        let options: Vec<(String, Action, bool)> = match picker {
            PickerKind::Themes(slot) => appearance::registry()
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    (
                        entry.id.name().to_owned(),
                        Action::ChooseTheme(slot, index),
                        self.settings.appearance.theme(slot) == &entry.id,
                    )
                })
                .collect(),
            PickerKind::Presets => self
                .settings
                .presets
                .iter()
                .map(|(hash, name)| {
                    (
                        name.clone(),
                        Action::LoadPreset(*hash),
                        *hash == self.active.hash,
                    )
                })
                .collect(),
            PickerKind::GenerationLength => FirLength::ALL
                .into_iter()
                .map(|value| {
                    (
                        format!("{} taps", value.frames()),
                        Action::ChooseLength(value),
                        self.settings.generation.length() == value,
                    )
                })
                .collect(),
            PickerKind::GenerationAlignment => Alignment::ALL
                .into_iter()
                .map(|value| {
                    (
                        value.label().into(),
                        Action::ChooseAlignment(value),
                        self.settings.generation.alignment() == value,
                    )
                })
                .collect(),
            PickerKind::GenerationNormalization => Normalization::ALL
                .into_iter()
                .map(|value| {
                    (
                        value.label().into(),
                        Action::ChooseNormalization(value),
                        self.settings.generation.normalization() == value,
                    )
                })
                .collect(),
            PickerKind::Spectrum(setting) => setting
                .choices()
                .map(|choice| {
                    (
                        choice.label().into_owned(),
                        Action::ChooseSpectrum(choice),
                        self.settings.spectrum.selected(setting) == choice,
                    )
                })
                .collect(),
            PickerKind::CatalogFilter => CatalogFilter::ALL
                .into_iter()
                .map(|option| {
                    (
                        option.label().into(),
                        Action::ChooseCatalogFilter(option),
                        self.settings.catalog_filter == option,
                    )
                })
                .collect(),
            PickerKind::Device(side) => self
                .devices
                .iter()
                .enumerate()
                .filter(|(_, device)| match side {
                    RouteSide::Capture => device.input_channels >= 2,
                    RouteSide::Playback => device.output_channels >= 2,
                })
                .map(|(index, device)| {
                    (
                        device.name.clone(),
                        Action::ChooseDevice(side, index),
                        endpoint(side).is_some_and(|e| e.uid == device.uid),
                    )
                })
                .collect(),
            PickerKind::Pair(side) => {
                let selected = endpoint(side).expect("channel picker has an endpoint");
                self.devices
                    .iter()
                    .find(|device| device.uid == selected.uid)
                    .into_iter()
                    .flat_map(|device| {
                        let channels = match side {
                            RouteSide::Capture => device.input_channels,
                            RouteSide::Playback => device.output_channels,
                        };
                        (0..channels.saturating_sub(1))
                            .step_by(2)
                            .map(move |first| {
                                let pair = StereoPair::try_from([first, first + 1])
                                    .expect("adjacent channels are distinct");
                                (
                                    format!("{} / {}", first + 1, first + 2),
                                    Action::ChoosePair(side, pair),
                                    selected.pair == pair,
                                )
                            })
                    })
                    .collect()
            }
            PickerKind::Graph(_) => {
                return self
                    .graph_toggles()
                    .map(|toggle| MenuOption {
                        label: toggle.label.into(),
                        action: toggle.action,
                        checks: MenuChecks::Checkbox(toggle.selected),
                    })
                    .collect();
            }
        };
        options
            .into_iter()
            .map(|(label, action, selected)| MenuOption {
                label,
                action,
                checks: MenuChecks::Radio(selected),
            })
            .collect()
    }
    fn draw_picker(
        p: &mut Painter<'_, Action>,
        picker: &mut Picker,
        options: &[MenuOption],
        window: Rect,
        chrome: WindowChrome,
    ) {
        let colors = p.colors;
        let anchor = match picker.kind {
            PickerKind::Graph(point) => Rect::from_origin_size(point, (0.0, 0.0)),
            _ => {
                p.hits
                    .iter()
                    .find(|(_, action)| Some(*action) == picker.kind.action())
                    .expect("open picker has a visible control")
                    .0
            }
        };
        let width = match picker.kind {
            PickerKind::Themes(_) | PickerKind::Presets => 264.0_f64,
            _ => 264.0_f64.max(anchor.width()),
        }
        .min(window.width() - 16.0);
        let header = 32.0;
        let footer = 32.0;
        let max_rows = ((window.height() - chrome.height - INSET - header - footer) / CONTROL)
            .floor()
            .clamp(1.0, 12.0) as usize;
        picker.visible_rows = max_rows;
        let count = options.len().max(1).min(max_rows);
        let height = header + count as f64 * CONTROL + footer;
        let x = anchor.x0.min(window.x1 - width - 8.0).max(8.0);
        let y = if anchor.y0 < chrome.height {
            chrome.height
        } else {
            (anchor.y1 + 4.0)
                .min(window.y1 - height - 8.0)
                .max(chrome.height)
        };
        let card = Rect::new(x, y, x + width, y + height);
        picker.bounds = Some(card);
        let top_radius = if anchor.y0 < chrome.height { 0.0 } else { 6.0 };
        let radii = vello::kurbo::RoundedRectRadii::new(top_radius, top_radius, 6.0, 6.0);
        p.rounded(
            card + Vec2::new(0.0, 3.0),
            radii,
            opacity(vello::peniko::Color::BLACK, 0.25),
        );
        p.rounded(card, radii, opacity(colors.bg, 0.97));
        p.outline(card, radii, 1.0, opacity(colors.fg, 0.22));
        p.hits.clear();
        picker.layout = Some(Box::new(p.field(
            Rect::new(x + 5.0, y + 5.0, x + width - 5.0, y + 27.0),
            &picker.filter,
            &options.len().to_string(),
            [Action::PickerSearch, Action::ClearPickerSearch],
            true,
            "Filter...",
        )));
        p.line(
            (x, y + header),
            (x + width, y + header),
            opacity(colors.fg, 0.22),
            1.0,
        );
        picker.scroll = picker.scroll.min(options.len().saturating_sub(count));
        for (row, option) in options.iter().skip(picker.scroll).take(count).enumerate() {
            let bounds = Rect::new(
                x,
                y + header + row as f64 * CONTROL,
                x + width,
                y + header + (row + 1) as f64 * CONTROL,
            );
            let highlighted = picker.highlighted == Some(option.action);
            let ink = if highlighted { colors.bg } else { colors.fg };
            if highlighted {
                p.rect(bounds, colors.fg);
            }
            let text_x = if let Action::ChooseTheme(_, index) = option.action {
                let palette = appearance::registry()[index].colors;
                let tile =
                    Rect::from_origin_size((x + 13.0, bounds.center().y - 8.5), (29.0, 17.0));
                p.rounded(tile, 3.0, palette.bg);
                let swatches = [
                    palette.basic[1],
                    palette.basic[2],
                    palette.basic[3],
                    palette.basic[4],
                    palette.basic[5],
                    palette.basic[6],
                    palette.fg,
                    palette.secondary,
                ];
                for (i, color) in swatches.into_iter().enumerate() {
                    let dx = tile.x0 + 3.0 + (i % 4) as f64 * 6.0;
                    let dy = tile.y0 + 3.0 + (i / 4) as f64 * 6.0;
                    p.rounded(Rect::new(dx, dy, dx + 5.0, dy + 5.0), 1.2, color);
                }
                if matches!(option.checks, MenuChecks::Radio(true)) {
                    p.rounded(
                        Rect::from_center_size((x + width - 18.0, bounds.center().y), (5.0, 5.0)),
                        2.5,
                        ink,
                    );
                }
                x + 49.0
            } else {
                p.icon(
                    option.checks.icon(),
                    Rect::from_center_size((x + 20.0, bounds.center().y), (14.0, 14.0)),
                    ink,
                );
                x + 34.0
            };
            p.text(
                &option.label,
                Rect::new(text_x, bounds.y0, x + width - 13.0, bounds.y1),
                Face::Label,
                Align::Start,
                ink,
            );
            p.hits.push((bounds, option.action));
        }
        if options.is_empty() {
            p.text(
                "No matches",
                Rect::new(x + 13.0, y + header, x + width - 13.0, y + header + CONTROL),
                Face::Label,
                Align::Start,
                colors.secondary,
            );
        }
        let footer_y = y + height - footer;
        p.line(
            (x, footer_y),
            (x + width, footer_y),
            opacity(colors.fg, 0.22),
            1.0,
        );
        p.text(
            "Arrows to choose / Enter / Esc",
            Rect::new(x + 13.0, footer_y, x + width - 13.0, y + height),
            Face::Small,
            Align::Start,
            colors.secondary,
        );
        if let Some(bar) = Scrollbar::new(
            Rect::new(x, y + header, x + width, footer_y),
            options.len() as f64 * CONTROL,
            picker.scroll as f64 * CONTROL,
        ) {
            p.scrollbar(bar, Action::Scroll(ScrollSurface::Picker, bar));
        }
    }
    fn begin_scroll(&mut self, surface: ScrollSurface, bar: Scrollbar) {
        if !self.commit_number() {
            return;
        }
        let grab = bar.grab(self.cursor);
        self.dragging = Some(Drag::Scroll { surface, bar, grab });
        self.scroll_from_cursor(surface, bar, grab);
    }
    fn scroll_from_cursor(&mut self, surface: ScrollSurface, bar: Scrollbar, grab: f64) {
        let offset = bar.offset(self.cursor.y, grab);
        match surface {
            ScrollSurface::Sidebar => self.scroll = offset,
            ScrollSurface::Picker => {
                self.picker
                    .as_mut()
                    .expect("dragging an open picker")
                    .scroll = (offset / CONTROL).round() as usize
            }
        }
        self.dirty = true;
    }
    fn reset_value_bar(&mut self, target: ValueBar) {
        match target {
            ValueBar::Volume => self.volume(Controls::default().volume.db()),
            ValueBar::SpectrumIntensity => self
                .set_number(
                    NumberField::SpectrumIntensity,
                    analysis::Intensity::default().percent(),
                )
                .expect("default intensity is valid"),
        }
    }
    fn value_bar_from_cursor(&mut self, target: ValueBar, bounds: Rect) {
        let fraction = ((self.cursor.x - bounds.x0) / bounds.width()).clamp(0.0, 1.0) as f32;
        match target {
            ValueBar::Volume => self.volume(fraction * 80.0 - 80.0),
            ValueBar::SpectrumIntensity => self
                .set_number(NumberField::SpectrumIntensity, fraction * 100.0)
                .expect("slider clamps to valid intensity"),
        }
    }
    fn help_enabled(&self) -> bool {
        self.visible()
            && self.picker.is_none()
            && self.text_focus.is_none()
            && self.dragging.is_none()
            && self.pressed.is_none()
    }
    fn help_target(&self) -> Option<help::Hint> {
        if self.help_enabled() {
            help::target(&self.hits, self.cursor, self.browser.is_preferences())
        } else {
            None
        }
    }
    fn visible(&self) -> bool {
        self.frames.kind(Instant::now()) == FrameKind::Visible
    }
    fn sync_visuals(&mut self) {
        let enabled = self.visible()
            && matches!(self.lifecycle, Lifecycle::Running { .. })
            && self.settings.spectrum.enabled;
        self.runtime.meters.spectrum.set_enabled(enabled);
        self.spectrum.configure(analysis::Settings {
            enabled,
            ..self.settings.spectrum
        });
        if !enabled {
            self.dirty |= self.spectrum.update();
        }
    }
    fn refresh_visuals(&mut self) {
        self.sync_visuals();
        if self.visible() && matches!(self.lifecycle, Lifecycle::Running { .. }) {
            self.peaks = self.runtime.meters.peaks();
            self.spectrum.update();
        }
    }
    fn poll(&mut self) {
        self.dirty |= self.theme.advance(Instant::now());
        while let Ok(event) = self.runtime.events.try_recv() {
            match event {
                Event::Spectrum(stream) => self.spectrum.attach(stream),
                Event::State(state) => {
                    if !matches!(state, Lifecycle::Running { .. }) {
                        self.spectrum.detach();
                    }
                    if matches!(
                        state,
                        Lifecycle::Stopped | Lifecycle::Recovering { .. } | Lifecycle::Failed(_)
                    ) {
                        self.awaiting = None;
                    }
                    self.lifecycle = state;
                    if !self.route_editable()
                        && matches!(
                            self.picker.as_ref().map(|picker| picker.kind),
                            Some(PickerKind::Device(_) | PickerKind::Pair(_))
                        )
                    {
                        self.dismiss_picker();
                    }
                    if let Lifecycle::Failed(reason) = &self.lifecycle {
                        self.message = reason.clone();
                    }
                }
                Event::Message(message) => {
                    self.message = message;
                }
                Event::ActivationRejected(message) => {
                    self.awaiting = None;
                    self.message = message;
                }
                Event::Activated(correction) => {
                    if let Some(view) = self.awaiting.take() {
                        if Arc::ptr_eq(&view.correction, &correction) {
                            self.active = view;
                            self.message = format!(
                                "Loaded {}. The selected profile has replaced the previous IR.",
                                self.active.correction.name
                            );
                        } else {
                            self.awaiting = Some(view);
                            self.active_from_receipt(correction);
                        }
                    } else {
                        self.active_from_receipt(correction);
                    }
                    self.settings.active = Some(self.active.hash);
                    self.save();
                }
            }
            if let Some(menu) = &self.menu {
                menu.status(&self.lifecycle, self.settings.controls);
            }
            self.dirty = true;
        }
        if let Some(job) = &self.job {
            let result = match job.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err(invalid("Preparation worker stopped unexpectedly")))
                }
            };
            if let Some(result) = result {
                let cancelled = self
                    .job
                    .as_ref()
                    .expect("pending job")
                    .cancel
                    .load(Ordering::Relaxed);
                let next = self.job.take().expect("finished job").next;
                let result = if cancelled {
                    Ok(JobResult::Nothing)
                } else {
                    result
                };
                match result {
                    Ok(JobResult::Catalog(catalog)) => {
                        self.catalog = Some(catalog);
                        self.selected = None;
                        self.message =
                            "Library refreshed. The active correction is unchanged.".into();
                    }
                    Ok(JobResult::Prepared(view, engine)) => {
                        if matches!(self.selected.as_ref().map(|selected|&selected.source),Some(Selection::Saved(hash)) if *hash==view.hash)
                            && let Origin::AutoEq { generation, .. } = &view.correction.origin
                        {
                            self.settings.generation = *generation;
                            self.save();
                        }
                        self.preview = Some((view, engine));
                        self.message =
                            "Compare the selected profile with the loaded IR, then choose Replace loaded profile.".into();
                    }
                    Ok(JobResult::Nothing) if !cancelled => {
                        self.message = "Preparation cancelled.".into()
                    }
                    Ok(JobResult::Nothing) => {}
                    Err(e) => {
                        if let Some(selected) = &mut self.selected {
                            selected.intent = SelectionIntent::Compare;
                        }
                        self.message = e.to_string();
                    }
                }
                if let Some(task) = next {
                    self.submit(task);
                }
                self.dirty = true;
            }
        }
        self.sync_visuals();
        let target = self.help_target();
        self.dirty |= self.help.update(target, Instant::now());
        self.activate_requested();
        if self
            .persist_after
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.persist_after = None;
            self.save();
        }
    }
    fn active_from_receipt(&mut self, correction: Arc<Correction>) {
        if Arc::ptr_eq(&self.active.correction, &correction) {
            return;
        }
        match View::from_managed(
            (*correction).clone(),
            self.active.hash,
            self.settings.controls,
        ) {
            Ok((view, _)) => self.active = view,
            Err(e) => self.message = e.to_string(),
        }
    }
    fn system_appearance_changed(&mut self, appearance: Appearance) {
        self.system_appearance = appearance;
        self.theme
            .show(self.settings.appearance.selected(appearance));
        self.dirty = true;
    }
    fn toggle_compact(&mut self) {
        self.dismiss_picker();
        self.text_focus = None;
        self.dragging = None;
        self.pressed = None;
        self.settings.window_mode = self.settings.window_mode.toggle();
        let dimensions = match self.settings.window_mode {
            WindowMode::Full => self.settings.window.dimensions(),
            WindowMode::Compact => self
                .settings
                .compact_window
                .map_or([720, 420], WindowSize::dimensions),
        };
        if let Some(window) = &self.window {
            let minimum = match self.settings.window_mode {
                WindowMode::Full => (880.0, layout::MIN_HEIGHT),
                WindowMode::Compact => (520.0, 300.0),
            };
            window.set_min_inner_size(Some(LogicalSize::new(minimum.0, minimum.1)));
            let _ = window.request_inner_size(LogicalSize::new(dimensions[0], dimensions[1]));
        }
        self.browser = Browser::Catalog;
        self.scroll = 0.0;
        self.save();
        self.dirty = true;
    }
    fn graph_geometry(&self) -> GraphLayout {
        match self.settings.window_mode {
            WindowMode::Full => self.geometry().graph_layout(),
            WindowMode::Compact => GraphLayout::compact(
                self.viewport(),
                self.window_chrome.expect("live chrome"),
                self.has_candidate(),
            ),
        }
    }
    fn viewport(&self) -> Size {
        self.window
            .as_ref()
            .map_or(Size::new(WIDTH, HEIGHT), |window| {
                let size = window.inner_size().to_logical::<f64>(window.scale_factor());
                Size::new(size.width, size.height)
            })
    }
    fn has_candidate(&self) -> bool {
        self.preview.is_some()
            || self.awaiting.is_some()
            || (self.selected.is_some()
                && self
                    .job
                    .as_ref()
                    .is_some_and(|job| job.next.is_some() || !job.cancel.load(Ordering::Relaxed)))
    }
    fn geometry(&self) -> Layout {
        assert_eq!(
            self.settings.window_mode,
            WindowMode::Full,
            "full layout is only used in full mode"
        );
        Layout::new(
            self.viewport(),
            self.details,
            self.has_candidate(),
            self.window_chrome
                .expect("window chrome is measured before layout"),
        )
    }
    fn scale(&self) -> f64 {
        self.window
            .as_ref()
            .map_or(1.0, |window| window.scale_factor())
    }
    fn build_scene(&mut self, size: Size, scale: f64) -> Scene {
        let chrome = self
            .window_chrome
            .expect("window chrome is measured before drawing");
        let window = size.to_rect();
        let full = (self.settings.window_mode == WindowMode::Full)
            .then(|| Layout::new(size, self.details, self.has_candidate(), chrome));
        let graph = full.as_ref().map_or_else(
            || GraphLayout::compact(size, chrome, self.has_candidate()),
            Layout::graph_layout,
        );
        let colors = self.theme.current;
        let top = chrome.height;
        let catalog_target = self.catalog_target();
        let editable_catalog_index = self.editable_catalog_index();
        let browsing_catalog = matches!(
            self.selected.as_ref().map(|selected| &selected.source),
            Some(Selection::Catalog(_))
        );
        let graph_toggles: Vec<_> = self.graph_toggles().collect();
        let menu_options = self.picker.as_ref().map(|_| self.filtered_picker_options());
        let candidate = self
            .preview
            .as_ref()
            .map(|(view, _)| view)
            .or(self.awaiting.as_ref());
        let view = candidate.unwrap_or(&self.active);
        let metadata = candidate
            .filter(|view| view.correction.measured.is_some())
            .unwrap_or(&self.active);
        let help_enabled = self.help_enabled();
        let mut p = Painter::new(
            &mut self.fonts,
            &mut self.layout,
            &self.icons,
            scale,
            self.cursor,
            self.pressed,
            colors,
        );
        let editable = matches!(self.lifecycle, Lifecycle::Stopped | Lifecycle::Failed(_));
        let running = matches!(
            self.lifecycle,
            Lifecycle::Running { .. } | Lifecycle::Starting | Lifecycle::Recovering { .. }
        );
        let transport_label = if running { "Stop" } else { "Start" };
        let transport_enabled = running || self.settings.route.is_some();
        let status = match &self.lifecycle {
            Lifecycle::Stopped => if self.settings.route.is_some() {
                "Audio stopped"
            } else {
                "Choose audio devices"
            }
            .into(),
            Lifecycle::Starting => "Starting...".into(),
            Lifecycle::Running {
                rate,
                estimated_latency_ms,
            } => format!(
                "{}  /  {} Hz  /  app delay ~{estimated_latency_ms:.1} ms",
                if self.settings.controls.correction == CorrectionMode::On {
                    "Corrected"
                } else {
                    "Bypassed"
                },
                rate.hz()
            ),
            Lifecycle::Recovering { attempt, .. } => format!("Recovering  {attempt}/3"),
            Lifecycle::Failed(_) => "Failed / check route, then Retry".into(),
        };
        p.rect(window, colors.body);
        p.rect(Rect::new(0.0, 0.0, window.x1, top), colors.bg);
        let compact = self.settings.window_mode == WindowMode::Compact;
        let mut x = chrome.controls_end + 8.0;
        if !compact {
            p.icon(
                Icon::Headphones,
                Rect::from_center_size((x + 15.0, top * 0.5), (16.0, 16.0)),
                colors.fg,
            );
            x += 38.0;
            let title_width = p.advance("IRresponsible", Face::Title);
            p.text(
                "IRresponsible",
                Rect::new(x, 0.0, x + title_width, top),
                Face::Title,
                Align::Start,
                colors.fg,
            );
            x += title_width + GAP;
        }
        p.ghost_icon(
            Rect::new(x, (top - CONTROL) * 0.5, x + CONTROL, (top + CONTROL) * 0.5),
            Icon::GearSix,
            Action::Preferences,
            true,
        );
        x += CONTROL + GAP;
        let compact_x = window.x1 - INSET - CONTROL;
        p.ghost_icon(
            Rect::new(
                compact_x,
                (top - CONTROL) * 0.5,
                compact_x + CONTROL,
                (top + CONTROL) * 0.5,
            ),
            if compact {
                Icon::ArrowsOutSimple
            } else {
                Icon::ArrowsInSimple
            },
            Action::Compact,
            true,
        );
        let bypass_x = compact_x - GAP - p.advance("Bypass", Face::Label).ceil() - INSET;
        p.chip(
            bypass_x,
            (top - CONTROL) * 0.5,
            "Bypass",
            Action::Bypass,
            self.settings.controls.correction == CorrectionMode::Bypass,
            true,
        );
        let transport_x = bypass_x - GAP - p.advance(transport_label, Face::Label).ceil() - INSET;
        let has_correction = !matches!(self.active.correction.origin, Origin::Identity);
        let named_preset = self.settings.presets.get(&self.active.hash);
        let can_save = has_correction && named_preset.is_none();
        let status_gap = if can_save { 36.0 } else { GAP };
        let preset_end = if compact {
            bypass_x - GAP
        } else {
            (x + 260.0).min(transport_x - GAP - status_gap - p.advance(&status, Face::Stat).ceil())
        };
        let preset_name = match named_preset {
            Some(name) => std::borrow::Cow::Borrowed(name.as_str()),
            None if has_correction => {
                std::borrow::Cow::Borrowed(self.active.correction.name.as_str())
            }
            None => std::borrow::Cow::Borrowed("Choose preset..."),
        };
        p.titlebar_selector(
            Rect::new(x, (top - CONTROL) * 0.5, preset_end, (top + CONTROL) * 0.5),
            &preset_name,
            Action::PresetPicker,
            matches!(
                self.picker.as_ref().map(|picker| picker.kind),
                Some(PickerKind::Presets)
            ),
        );
        if !compact {
            if can_save {
                p.ghost_icon(
                    Rect::new(
                        preset_end + 4.0,
                        (top - CONTROL) * 0.5,
                        preset_end + 28.0,
                        (top + CONTROL) * 0.5,
                    ),
                    Icon::FloppyDisk,
                    Action::SavePreset,
                    true,
                );
            }
            p.button(
                transport_x,
                (top - CONTROL) * 0.5,
                transport_label,
                Action::StartStop,
                transport_enabled,
            );
            p.text(
                &status,
                Rect::new(preset_end + status_gap, 0.0, transport_x - GAP, top),
                Face::Stat,
                Align::End,
                match self.lifecycle {
                    Lifecycle::Running { .. } => match self.settings.controls.correction {
                        CorrectionMode::On => colors.basic[2],
                        CorrectionMode::Bypass => colors.basic[1],
                    },
                    Lifecycle::Failed(_) => colors.basic[1],
                    _ => colors.secondary,
                },
            );
        }
        if let Some(g) = &full {
            p.rect(g.sidebar, colors.body);
            p.rect(
                Rect::new(SIDEBAR, top, SIDEBAR + RULE, g.status.y0),
                colors.pane(),
            );
            if !self.browser.is_preferences() {
                p.section(
                    Rect::new(0.0, top + 12.0, SIDEBAR - 38.0, top + 36.0),
                    Icon::Headphones,
                    "Headphones",
                    colors.secondary,
                );
                p.ghost_icon(
                    Rect::new(SIDEBAR - 40.0, top + 12.0, SIDEBAR - INSET, top + 36.0),
                    Icon::ArrowsClockwise,
                    Action::RefreshCatalog,
                    self.job.is_none(),
                );
                let matches: Vec<_> = self
                    .catalog
                    .as_ref()
                    .map(|catalog| {
                        catalog
                            .filtered(self.filter.text(), self.settings.catalog_filter)
                            .collect()
                    })
                    .unwrap_or_default();
                let count = matches.len();
                let listing = g.listing;
                let row_height = Layout::RESULT_HEIGHT;
                let result = |row: usize| {
                    Rect::new(
                        listing.x0,
                        listing.y0 + row as f64 * row_height,
                        listing.x1,
                        listing.y0 + (row + 1) as f64 * row_height,
                    )
                };
                let max_scroll = (count as f64 * row_height - listing.height()).max(0.0);
                self.scroll = self.scroll.clamp(0.0, max_scroll);
                let first = (self.scroll / row_height).floor() as usize;
                let offset = self.scroll % row_height;
                let visible = ((listing.height() + offset) / row_height).ceil() as usize;
                self.filter_layout = Some(p.field(
                    Rect::new(INSET, top + 52.0, SIDEBAR - INSET, top + 76.0),
                    &self.filter,
                    &count.to_string(),
                    [Action::Search, Action::ClearSearch],
                    matches!(self.text_focus, Some(TextFocus::Search)),
                    "Search headphones...",
                ));
                p.button(
                    INSET,
                    top + 84.0,
                    "Import WAV...",
                    Action::Import,
                    self.job.is_none(),
                );
                if self.job.is_some() {
                    p.text(
                        "Preparing...",
                        Rect::new(142.0, top + 84.0, SIDEBAR - INSET, top + 108.0),
                        Face::Small,
                        Align::End,
                        colors.secondary,
                    );
                }
                if self.browser == Browser::Catalog {
                    p.dropdown(
                        Rect::new(INSET, top + 116.0, SIDEBAR - INSET, top + 140.0),
                        self.settings.catalog_filter.label(),
                        Action::CatalogFilter,
                        self.catalog.as_ref().is_some_and(Catalog::inventory_known),
                    );
                }
                let list_hits = p.hits.len();
                p.scene.push_clip_layer(
                    vello::peniko::Fill::NonZero,
                    vello::kurbo::Affine::scale(scale),
                    &listing,
                );
                for (row, &(index, entry)) in matches.iter().skip(first).take(visible).enumerate() {
                    p.list_row(
                        result(row) + Vec2::new(0.0, -offset),
                        &format!(
                            "{}{}",
                            entry.name,
                            if entry.recommended { " *" } else { "" }
                        ),
                        &entry.source,
                        Action::Select(index),
                        self.selected
                            .as_ref()
                            .is_some_and(|selected| selected.source == Selection::Catalog(index)),
                    );
                }
                if count == 0 {
                    p.text(
                        if self.catalog.is_some() {
                            "No matching headphones"
                        } else {
                            "Refresh to load the library"
                        },
                        Rect::new(INSET, listing.y0, SIDEBAR - INSET, listing.y0 + ROW),
                        Face::Label,
                        Align::Start,
                        colors.secondary,
                    );
                }
                p.scene.pop_layer();
                for (bounds, _) in &mut p.hits[list_hits..] {
                    *bounds = bounds.intersect(listing);
                }
                if let Some(bar) = Scrollbar::new(listing, count as f64 * row_height, self.scroll) {
                    p.scrollbar(bar, Action::Scroll(ScrollSurface::Sidebar, bar));
                }
                p.text(
                    &format!("{count} matches / * recommended"),
                    Rect::new(INSET, g.status.y0 - 30.0, SIDEBAR - INSET, g.status.y0),
                    Face::Small,
                    Align::Start,
                    colors.secondary,
                );
            }
            p.section(
                g.section(top + 4.0),
                Icon::PlugsConnected,
                "Audio route",
                colors.secondary,
            );
            p.ghost_icon(
                Rect::new(g.main.x1 - 40.0, top + 4.0, g.main.x1 - INSET, top + 28.0),
                Icon::ArrowsClockwise,
                Action::RefreshDevices,
                editable,
            );
            let label = |endpoint: Option<&Endpoint>| {
                endpoint.map_or_else(|| "Choose device".into(), |endpoint| endpoint.label.clone())
            };
            let columns = g.route_columns();
            for (column, name, endpoint, device_action, pair_action) in [
                (
                    columns[0],
                    "Capture",
                    self.settings.route.as_ref().map(|r| &r.capture),
                    Action::Capture,
                    Action::CapturePair,
                ),
                (
                    columns[1],
                    "Playback",
                    self.settings.route.as_ref().map(|r| &r.playback),
                    Action::Playback,
                    Action::PlaybackPair,
                ),
            ] {
                p.text(
                    name,
                    Rect::new(column.x0, column.y0 - 22.0, column.x1, column.y0 - 2.0),
                    Face::Small,
                    Align::Start,
                    colors.secondary,
                );
                let (device, pair) = Layout::split_route(column);
                p.dropdown(device, &label(endpoint), device_action, editable);
                let pair_label = endpoint.map_or_else(
                    || "1 / 2".into(),
                    |endpoint| {
                        let [left, right] = endpoint.pair.indices();
                        format!("{} / {}", left + 1, right + 1)
                    },
                );
                p.dropdown(
                    pair,
                    &pair_label,
                    pair_action,
                    editable && endpoint.is_some(),
                );
            }
            p.icon(
                Icon::ArrowRight,
                Rect::from_center_size(
                    ((columns[0].x1 + columns[1].x0) * 0.5, columns[0].center().y),
                    (16.0, 16.0),
                ),
                colors.secondary,
            );
            let route_action = if !self.settings_healthy {
                Some(("Recover settings", Action::RecoverSettings))
            } else if matches!(self.lifecycle, Lifecycle::Failed(_)) {
                Some(("Retry", Action::Retry))
            } else {
                None
            };
            if let Some((label, action)) = route_action {
                let width = p.advance(label, Face::Label).ceil() + INSET;
                p.button(
                    g.main.x1 - 40.0 - GAP - width,
                    top + 4.0,
                    label,
                    action,
                    true,
                );
            }
            p.rect(
                Rect::new(
                    g.main.x0,
                    top + Layout::ROUTE_SEPARATOR_TOP,
                    g.main.x1,
                    top + Layout::ROUTE_SEPARATOR_TOP + RULE,
                ),
                colors.pane(),
            );
            p.section(
                g.section(top + Layout::PROFILE_HEADER_TOP),
                Icon::Waveform,
                &format!("Loaded / {}", self.active.correction.name),
                colors.fg,
            );
            let row = g.row(top + Layout::MODES_TOP);
            let selected_entry = editable_catalog_index.map(|index| {
                &self
                    .catalog
                    .as_ref()
                    .expect("resolved catalog entry")
                    .entries[index]
            });
            let reference_label = selected_entry
                .and_then(|entry| entry.reference.as_ref())
                .and_then(|reference| reference.target(Target::Reference))
                .map_or("Reference target", |name| name.label());
            let mut x = row.x0;
            for (target, label, action) in [
                (Target::AutoEq, "AutoEq", Action::AutoEq),
                (Target::Harman, "Harman", Action::Harman),
                (Target::Reference, reference_label, Action::Reference),
                (Target::Flat, "Literal Flat", Action::Flat),
            ] {
                let available = selected_entry
                    .is_some_and(|entry| entry.supports(target, self.active.correction.rate));
                if available || (selected_entry.is_none() && view.correction.target == target) {
                    x =
                        p.chip(
                            x,
                            row.y0,
                            label,
                            action,
                            if browsing_catalog {
                                catalog_target.includes(target)
                            } else {
                                view.correction.target == target
                            },
                            available && self.job.is_none(),
                        )
                        .x1 + GAP;
                }
            }
            if matches!(view.correction.origin, Origin::Imported { .. }) {
                p.chip(
                    x,
                    row.y0,
                    "Custom WAV",
                    Action::Import,
                    true,
                    self.job.is_none(),
                );
            }
            {
                let source = match &view.correction.origin {
                    Origin::AutoEq { reference, .. } => reference.clone(),
                    Origin::PublishedAutoEq { provider, .. } => {
                        format!("Published AutoEq correction / {provider}")
                    }
                    Origin::Imported { impulse } => format!(
                        "Custom WAV / original {} Hz / conversion delay {} frames",
                        impulse.original_rate,
                        impulse.conversion.delay()
                    ),
                    Origin::Identity => {
                        "Choose a profile on the left to compare its response with the loaded IR."
                            .into()
                    }
                };
                let mut source_row = g.row(top + Layout::SOURCE_TOP);
                if selected_entry.is_some_and(|entry| {
                    entry.supports(view.correction.target, self.active.correction.rate)
                }) && matches!(
                    view.correction.target,
                    Target::Harman | Target::Reference | Target::Flat
                ) {
                    p.toggle(
                        Rect::new(
                            source_row.x1 - 156.0,
                            source_row.y0 - 3.0,
                            source_row.x1,
                            source_row.y1 + 3.0,
                        ),
                        "AutoEq treble",
                        Action::TrebleExtension,
                        self.settings.generation.treble() == TrebleExtension::AutoEq,
                        true,
                    );
                    source_row.x1 -= 156.0 + GAP;
                }
                p.text(
                    &source,
                    source_row,
                    Face::Small,
                    Align::Start,
                    colors.secondary,
                );
            }
        }
        {
            let g = &graph;
            p.rounded(g.graph, if compact { 0.0 } else { RADIUS }, colors.bg);
            let max_hz = *self.active.response[0]
                .frequency
                .last()
                .expect("response frequencies");
            let ticks: &[i32] = if g.plot.height() < 100.0 {
                &[-24, 0, 24]
            } else {
                &[-24, -12, 0, 12, 24]
            };
            for &db in ticks {
                let y = g.plot.y0 + f64::from(24 - db) / 48.0 * g.plot.height();
                p.line(
                    (g.plot.x0, y),
                    (g.plot.x1, y),
                    if db == 0 { colors.ghost } else { colors.pane() },
                    1.0,
                );
                p.text(
                    &format!("{db:+}"),
                    Rect::new(g.graph.x0 + 2.0, y - 12.0, g.plot.x0 - GAP, y + 12.0),
                    Face::SmallValue,
                    Align::End,
                    colors.secondary,
                );
            }
            for hz in [
                20.0_f32, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0,
            ] {
                if hz > max_hz {
                    continue;
                }
                let x =
                    g.plot.x0 + f64::from((hz / 20.0).ln() / (max_hz / 20.0).ln()) * g.plot.width();
                p.line((x, g.plot.y0), (x, g.plot.y1), colors.pane(), 1.0);
                p.text(
                    &if hz >= 1000.0 {
                        format!("{}k", hz / 1000.0)
                    } else {
                        format!("{hz:.0}")
                    },
                    Rect::new(x - 17.0, g.plot.y1 + 2.0, x + 17.0, g.plot.y1 + 22.0),
                    Face::SmallValue,
                    Align::Center,
                    colors.secondary,
                );
            }
            spectrum::draw(
                &mut p,
                &self.spectrum,
                self.settings.spectrum,
                g.plot,
                max_hz,
                scale,
            );
            if self.show_measured
                && let Some(curve) = &metadata.correction.measured
            {
                p.curve(
                    &sample_curve(curve, &self.active.response[0]),
                    g.plot,
                    colors.secondary,
                    1.2,
                );
            }
            if self.show_target
                && let Some(curve) = &metadata.correction.target_curve
            {
                p.curve(
                    &sample_curve(curve, &self.active.response[0]),
                    g.plot,
                    colors.basic[4],
                    1.2,
                );
            }
            if self.show_prediction
                && let Some(measured) = &metadata.correction.measured
            {
                let mut prediction = metadata.response[0].clone();
                for (&f, db) in prediction.frequency.iter().zip(&mut prediction.db) {
                    *db += measured.at(f);
                }
                p.curve(&prediction, g.plot, colors.basic[1], 1.3);
            }
            if self.show_loaded {
                p.curve(&self.active.response[0], g.plot, colors.basic[3], 1.5);
                if self.active.correction.channels[0] != self.active.correction.channels[1] {
                    p.curve(
                        &self.active.response[1],
                        g.plot,
                        opacity(colors.basic[3], 0.65),
                        1.2,
                    );
                }
            }
            if self.show_candidate
                && let Some(candidate) = candidate
            {
                p.curve(&candidate.response[0], g.plot, colors.basic[2], 1.7);
                if candidate.correction.channels[0] != candidate.correction.channels[1] {
                    p.curve(
                        &candidate.response[1],
                        g.plot,
                        opacity(colors.basic[2], 0.65),
                        1.2,
                    );
                }
            }
            if g.plot.contains(self.cursor)
                && !g.toast.is_some_and(|toast| toast.contains(self.cursor))
            {
                let f = 20.0
                    * (max_hz / 20.0).powf(((self.cursor.x - g.plot.x0) / g.plot.width()) as f32);
                p.line(
                    (self.cursor.x, g.plot.y0),
                    (self.cursor.x, g.plot.y1),
                    colors.secondary,
                    0.7,
                );
                let readout = if let Some(candidate) = candidate {
                    format!(
                        "{f:.0} Hz / loaded {:+.2} / selected {:+.2} dB",
                        self.active.response[0].at(f),
                        candidate.response[0].at(f)
                    )
                } else {
                    format!(
                        "{f:.0} Hz / loaded L {:+.2} / R {:+.2} dB",
                        self.active.response[0].at(f),
                        self.active.response[1].at(f)
                    )
                };
                p.text(
                    &readout,
                    Rect::new(
                        g.plot.x0 + GAP,
                        g.graph.y0,
                        g.plot.x1 - GAP,
                        g.graph.y0 + CONTROL,
                    ),
                    Face::SmallValue,
                    Align::End,
                    colors.fg,
                );
            }
            let mut x = g.legend.x0;
            for toggle in &graph_toggles {
                x =
                    p.legend_chip(
                        Point::new(x, g.legend.y0),
                        toggle.label,
                        toggle.action,
                        toggle.selected,
                        true,
                        toggle.color,
                    )
                    .x1 + GAP;
            }
            if let Some(toast) = g.toast {
                p.rounded(toast, 6.0, colors.bg);
                p.outline(toast, 6.0, 1.0, opacity(colors.basic[2], 0.35));
                p.rounded(
                    Rect::new(
                        toast.x0 + 12.0,
                        toast.center().y - 5.0,
                        toast.x0 + 22.0,
                        toast.center().y + 5.0,
                    ),
                    2.0,
                    colors.basic[2],
                );
                let name = candidate
                    .map(|view| view.correction.name.as_str())
                    .or_else(
                        || match self.selected.as_ref().map(|selected| &selected.source) {
                            Some(Selection::Catalog(index)) => self
                                .catalog
                                .as_ref()
                                .map(|catalog| catalog.entries[*index].name.as_str()),
                            Some(Selection::Saved(hash)) => {
                                self.settings.presets.get(hash).map(String::as_str)
                            }
                            None => None,
                        },
                    )
                    .unwrap_or("Selected profile");
                let replacing = self.awaiting.is_some();
                let ready = self.preview.is_some();
                let action_label = if replacing && ready {
                    "Finishing previous..."
                } else if replacing {
                    "Replacing..."
                } else if ready {
                    "Replace loaded profile"
                } else {
                    "Preparing..."
                };
                let width = p.advance(action_label, Face::Label).ceil() + INSET;
                let button_x = toast.x1 - 40.0 - GAP - width;
                let title = Rect::new(
                    toast.x0 + 32.0,
                    toast.y0 + 4.0,
                    button_x - GAP,
                    toast.y0 + 24.0,
                );
                p.text(name, title, Face::Label, Align::Start, colors.fg);
                let detail = if replacing && ready {
                    "Previous replacement in progress; this profile is only selected.".into()
                } else if replacing {
                    "Waiting for the audio engine to confirm replacement".into()
                } else if ready {
                    let provider: std::borrow::Cow<'_, str> = match &view.correction.origin {
                        Origin::PublishedAutoEq { provider, .. } => provider.as_str().into(),
                        Origin::AutoEq { path, .. } => percent_encoding::percent_decode_str(
                            path.split('/').next().expect("source path"),
                        )
                        .decode_utf8_lossy(),
                        Origin::Imported { .. } => "Custom WAV".into(),
                        Origin::Identity => "Identity".into(),
                    };
                    format!("{provider} / {:?}", view.correction.target)
                } else {
                    "Loading the selected profile; loaded IR is unchanged".into()
                };
                p.text(
                    &detail,
                    Rect::new(title.x0, toast.y0 + 24.0, title.x1, toast.y1 - 4.0),
                    Face::Small,
                    Align::Start,
                    colors.secondary,
                );
                p.button(
                    button_x,
                    toast.center().y - 12.0,
                    action_label,
                    Action::ReplaceLoaded,
                    ready && !replacing,
                );
                p.ghost_icon(
                    Rect::new(
                        toast.x1 - 32.0,
                        toast.center().y - 12.0,
                        toast.x1 - 8.0,
                        toast.center().y + 12.0,
                    ),
                    Icon::Backspace,
                    Action::Cancel,
                    !replacing,
                );
            }
        }
        if let Some(g) = &full {
            p.section(
                g.listening_header(),
                Icon::SlidersHorizontal,
                "Listening",
                colors.secondary,
            );
            let listen = g.listening_header();
            p.toggle(
                Rect::new(
                    g.main.x1 - INSET - 128.0,
                    listen.y0 - 3.0,
                    g.main.x1 - INSET,
                    listen.y1 + 3.0,
                ),
                "Level match",
                Action::Compensation,
                self.settings.controls.comparison == Comparison::PinkNoise,
                true,
            );
            p.value_bar(
                g.volume(),
                "Volume",
                &format!("{:.1} dB", self.settings.controls.volume.db()),
                f64::from((self.settings.controls.volume.db() + 80.0) / 80.0),
                Action::ValueBar(ValueBar::Volume),
            );
            let trim_y = g.listening_row(1).y0 + 3.0;
            p.text(
                "Comparison trim",
                Rect::new(
                    g.main.x0 + INSET,
                    trim_y,
                    g.main.x0 + 150.0,
                    trim_y + CONTROL,
                ),
                Face::Label,
                Align::Start,
                colors.fg,
            );
            Self::draw_number(
                &mut p,
                Rect::new(
                    g.main.x1 - 144.0,
                    trim_y,
                    g.main.x1 - INSET,
                    trim_y + CONTROL,
                ),
                NumberField::ComparisonTrim,
                &format!("{:+.1} dB", self.settings.controls.trim.db()),
                &mut self.text_focus,
            );
            p.text(
                &format!(
                    "IN {:+.1} / OUT {:+.1} dBFS",
                    gain_db(self.peaks.0),
                    gain_db(self.peaks.1)
                ),
                Rect::new(
                    g.main.x0 + 154.0,
                    trim_y,
                    g.main.x1 - 160.0,
                    trim_y + CONTROL,
                ),
                Face::SmallValue,
                Align::End,
                if self.peaks.1 > 1.0 {
                    colors.basic[1]
                } else {
                    colors.secondary
                },
            );
            p.rect(
                Rect::new(g.main.x0, g.details.y0 - 2.0, g.main.x1, g.details.y0 - 0.5),
                colors.pane(),
            );
            p.section(
                g.details_header,
                Icon::Waveform,
                "Details",
                colors.secondary,
            );
            p.hits.push((g.details_header, Action::Details));
            p.icon(
                if self.details {
                    Icon::CaretDown
                } else {
                    Icon::CaretRight
                },
                Rect::new(
                    g.main.x1 - 30.0,
                    g.details_header.y0 + 4.0,
                    g.main.x1 - 14.0,
                    g.details_header.y0 + 20.0,
                ),
                colors.secondary,
            );
            p.text(
                &format!(
                    "Loaded headroom {:+.1} dB",
                    self.active.analysis.headroom_db
                ),
                Rect::new(
                    g.main.x0 + 132.0,
                    g.details_header.y0,
                    g.main.x1 - 40.0,
                    g.details_header.y1,
                ),
                Face::SmallValue,
                Align::End,
                colors.secondary,
            );
            if self.details {
                let correction = &view.correction;
                let target_label = match &correction.origin {
                    Origin::AutoEq {
                        target, generation, ..
                    } => format!(
                        "{target} / treble {}",
                        if generation.treble() == TrebleExtension::AutoEq {
                            "extended"
                        } else {
                            "limited"
                        }
                    ),
                    _ => format!("{:?}", correction.target),
                };
                let lines = [
                    format!(
                        "{target_label} / {} Hz / {} taps / boost {:+.1} dB",
                        correction.rate.hz(),
                        correction.channels[0].len(),
                        view.analysis.max_boost_db
                    ),
                    format!(
                        "{}: headroom {:+.1} / wet {:+.1} / dry {:+.1} dB{}",
                        if self.preview.is_some() {
                            "Preview"
                        } else {
                            "Active"
                        },
                        view.analysis.headroom_db,
                        view.analysis.wet_db,
                        view.analysis.dry_db,
                        match self.lifecycle {
                            Lifecycle::Running {
                                estimated_latency_ms,
                                ..
                            } => format!(" / app ~{estimated_latency_ms:.1} ms, devices extra"),
                            _ => String::new(),
                        }
                    ),
                    format!(
                        "Queues {}:{} / drift {:+.0} ppm / overloads {} / xruns {}",
                        self.runtime.meters.capture_queue.load(Ordering::Relaxed),
                        self.runtime.meters.playback_queue.load(Ordering::Relaxed),
                        self.runtime.meters.drift(),
                        self.runtime.meters.overloads.load(Ordering::Relaxed),
                        self.runtime.meters.backend_xruns.load(Ordering::Relaxed)
                    ),
                ];
                for (index, text) in lines.iter().enumerate() {
                    p.text(
                        text,
                        g.detail_row(index),
                        Face::SmallValue,
                        Align::Start,
                        colors.secondary,
                    );
                }
            }
            p.rect(g.status, colors.bg);
            p.text(
                &self.message,
                Rect::new(INSET, g.status.y0, g.status.x1 - INSET, g.status.y1),
                Face::Small,
                Align::Start,
                if matches!(self.lifecycle, Lifecycle::Failed(_)) {
                    colors.basic[1]
                } else {
                    colors.secondary
                },
            );
        }
        if self.browser.is_preferences() {
            let panel_bottom = full.as_ref().map_or(window.y1, |layout| layout.status.y0);
            let pane = Rect::new(0.0, top, SIDEBAR, panel_bottom);
            p.hits
                .retain(|(bounds, _)| bounds.y1 <= top || bounds.intersect(pane).area() == 0.0);
            p.rect(pane, colors.body);
            p.rect(
                Rect::new(SIDEBAR, top, SIDEBAR + RULE, panel_bottom),
                colors.pane(),
            );
            let tabs = [
                (
                    "General",
                    Icon::GearSix,
                    Action::BrowseGeneral,
                    Browser::General,
                ),
                (
                    "IR settings",
                    Icon::SlidersHorizontal,
                    Action::BrowseGeneration,
                    Browser::Generation,
                ),
                (
                    "Spectrum",
                    Icon::Waveform,
                    Action::BrowseSpectrum,
                    Browser::Spectrum,
                ),
                (
                    "Presets",
                    Icon::FloppyDisk,
                    Action::BrowsePresets,
                    Browser::Presets,
                ),
            ];
            let tab_width = SIDEBAR / tabs.len() as f64;
            for (i, (label, icon, action, page)) in tabs.into_iter().enumerate() {
                let x = i as f64 * tab_width;
                p.tab(
                    Rect::new(x, top, x + tab_width, top + 48.0),
                    icon,
                    label,
                    action,
                    self.browser == page,
                );
            }
            p.rect(
                Rect::new(0.0, top + 48.0, SIDEBAR, top + 48.0 + RULE),
                colors.pane(),
            );
            if self.browser == Browser::Generation {
                let content = Rect::new(0.0, top + 60.0, SIDEBAR, panel_bottom - 12.0);
                let settings = self.settings.generation;
                let parameters: Vec<_> = GenerationParameter::ALL
                    .into_iter()
                    .filter(|parameter| match parameter {
                        GenerationParameter::HighFull | GenerationParameter::HighUnity => {
                            settings.treble() == TrebleExtension::Limited
                        }
                        GenerationParameter::LowFull | GenerationParameter::LowUnity => {
                            settings.bass() == BassExtension::Tapered
                        }
                        GenerationParameter::CutLimit => settings.cut_limit().is_some(),
                        _ => true,
                    })
                    .collect();
                let height = 8.0 * 32.0 + parameters.len() as f64 * 32.0 + 112.0;
                let max_scroll = (height - content.height()).max(0.0);
                self.scroll = self.scroll.clamp(0.0, max_scroll);
                let first_hit = p.hits.len();
                p.scene.push_clip_layer(
                    vello::peniko::Fill::NonZero,
                    vello::kurbo::Affine::scale(scale),
                    &content,
                );
                let mut y = content.y0 - self.scroll;
                for (label, value, action) in [
                    (
                        "FIR length",
                        format!("{} taps", settings.length().frames()),
                        Action::GenerationLength,
                    ),
                    (
                        "Alignment",
                        settings.alignment().label().into(),
                        Action::GenerationAlignment,
                    ),
                    (
                        "Output gain",
                        settings.normalization().label().into(),
                        Action::GenerationNormalization,
                    ),
                ] {
                    p.text(
                        label,
                        Rect::new(INSET, y, 132.0, y + CONTROL),
                        Face::Label,
                        Align::Start,
                        colors.fg,
                    );
                    p.dropdown(
                        Rect::new(132.0, y, SIDEBAR - INSET, y + CONTROL),
                        &value,
                        action,
                        true,
                    );
                    y += 32.0;
                }
                p.chip(
                    INSET,
                    y,
                    "Extend treble (AutoEq)",
                    Action::TrebleExtension,
                    settings.treble() == TrebleExtension::AutoEq,
                    true,
                );
                y += 32.0;
                for (label, action, selected) in [
                    (
                        "Taper bass",
                        Action::GenerationBass,
                        settings.bass() == BassExtension::Tapered,
                    ),
                    (
                        "Limit cuts",
                        Action::GenerationCutLimit,
                        settings.cut_limit().is_some(),
                    ),
                    (
                        "Concha interference",
                        Action::GenerationConcha,
                        settings.concha() == ConchaInterference::Present,
                    ),
                ] {
                    p.chip(INSET, y, label, action, selected, true);
                    y += 32.0;
                }
                for parameter in parameters {
                    let label = parameter.label();
                    p.text(
                        label,
                        Rect::new(INSET, y, 132.0, y + CONTROL),
                        Face::Label,
                        Align::Start,
                        colors.fg,
                    );
                    let target = NumberField::Generation(parameter);
                    let value = settings.value(parameter);
                    let formatted = if value < 5.0 {
                        format!("{value:.3}")
                    } else {
                        format!("{value:.0}")
                    };
                    Self::draw_number(
                        &mut p,
                        Rect::new(132.0, y, SIDEBAR - INSET, y + CONTROL),
                        target,
                        &formatted,
                        &mut self.text_focus,
                    );
                    y += 32.0;
                }
                p.button(
                    INSET,
                    y,
                    "Restore defaults",
                    Action::GenerationDefaults,
                    settings != settings.reset_parameters()
                        || self
                            .preview
                            .as_ref()
                            .map_or(&self.active, |(view, _)| view)
                            .correction
                            .requires_regeneration(),
                );
                y += 40.0;
                for text in [
                    "Applies to in-app target modes.",
                    "Edits prepare a comparison preview.",
                    "Stored presets keep their current audio.",
                    "Replace loaded applies the preview.",
                ] {
                    p.text(
                        text,
                        Rect::new(INSET, y, SIDEBAR - INSET, y + 20.0),
                        Face::Small,
                        Align::Start,
                        colors.secondary,
                    );
                    y += 20.0;
                }
                p.scene.pop_layer();
                for (bounds, _) in &mut p.hits[first_hit..] {
                    *bounds = bounds.intersect(content);
                }
                if let Some(bar) = Scrollbar::new(content, height, self.scroll) {
                    p.scrollbar(bar, Action::Scroll(ScrollSurface::Sidebar, bar));
                }
            } else if self.browser == Browser::Spectrum {
                let content = Rect::new(0.0, top + 60.0, SIDEBAR, panel_bottom - 12.0);
                let height = 424.0;
                self.scroll = self.scroll.clamp(0.0, (height - content.height()).max(0.0));
                let first_hit = p.hits.len();
                p.scene.push_clip_layer(
                    vello::peniko::Fill::NonZero,
                    vello::kurbo::Affine::scale(scale),
                    &content,
                );
                let mut y = content.y0 - self.scroll;
                p.toggle(
                    Rect::new(INSET, y, SIDEBAR - INSET, y + ROW),
                    "Live spectrum backdrop",
                    Action::SpectrumEnabled,
                    self.settings.spectrum.enabled,
                    true,
                );
                y += 40.0;
                for setting in SpectrumSetting::ALL {
                    p.text(
                        setting.label(),
                        Rect::new(INSET, y, 108.0, y + CONTROL),
                        Face::Label,
                        Align::Start,
                        colors.fg,
                    );
                    p.dropdown(
                        Rect::new(108.0, y, SIDEBAR - INSET, y + CONTROL),
                        &self.settings.spectrum.selected(setting).label(),
                        Action::SpectrumPicker(setting),
                        true,
                    );
                    y += 36.0;
                }
                p.value_bar(
                    Rect::new(INSET, y, SIDEBAR - 124.0, y + CONTROL),
                    "Intensity",
                    "",
                    f64::from(self.settings.spectrum.intensity.alpha()),
                    Action::ValueBar(ValueBar::SpectrumIntensity),
                );
                Self::draw_number(
                    &mut p,
                    Rect::new(SIDEBAR - 116.0, y, SIDEBAR - INSET, y + CONTROL),
                    NumberField::SpectrumIntensity,
                    &format!("{:.1}%", self.settings.spectrum.intensity.percent()),
                    &mut self.text_focus,
                );
                y += 36.0;
                y += 8.0;
                for text in [
                    "Stereo average power / Hann window.",
                    "Input: before IR, gain and bypass.",
                    "Output: after IR, gain and bypass.",
                    "Spectrum scale: dBFS on the right.",
                    "IR curves retain their dB scale.",
                    "Refresh target / display may limit FPS.",
                    "Changes apply immediately.",
                    "Analysis pauses while hidden.",
                ] {
                    p.text(
                        text,
                        Rect::new(INSET, y, SIDEBAR - INSET, y + 20.0),
                        Face::Small,
                        Align::Start,
                        colors.secondary,
                    );
                    y += 20.0;
                }
                p.scene.pop_layer();
                for (bounds, _) in &mut p.hits[first_hit..] {
                    *bounds = bounds.intersect(content);
                }
                if let Some(bar) = Scrollbar::new(content, height, self.scroll) {
                    p.scrollbar(bar, Action::Scroll(ScrollSurface::Sidebar, bar));
                }
            } else if self.browser == Browser::General {
                let slots: &[ThemeSlot] = if self.settings.appearance.mode == ThemeMode::System {
                    &[ThemeSlot::Light, ThemeSlot::Dark]
                } else {
                    &[ThemeSlot::Fixed]
                };
                let content = Rect::new(0.0, top + 60.0, SIDEBAR, panel_bottom - 12.0);
                let status = self.login.as_ref().ok().copied();
                let needs_settings = status.is_some_and(login::Status::needs_settings);
                let height =
                    156.0 + slots.len() as f64 * 62.0 + if needs_settings { 36.0 } else { 0.0 };
                let max_scroll = (height - content.height()).max(0.0);
                self.scroll = self.scroll.clamp(0.0, max_scroll);
                let first_hit = p.hits.len();
                p.scene.push_clip_layer(
                    vello::peniko::Fill::NonZero,
                    vello::kurbo::Affine::scale(scale),
                    &content,
                );
                let mut y = content.y0 - self.scroll;
                p.toggle(
                    Rect::new(INSET, y, SIDEBAR - INSET, y + ROW),
                    "Launch at login",
                    Action::LaunchAtLogin,
                    status.is_some_and(login::Status::requested),
                    status.and_then(login::Status::change).is_some(),
                );
                y += 32.0;
                let caption = match &self.login {
                    Ok(status) => status.caption(),
                    Err(error) => error.as_str(),
                };
                p.text(
                    caption,
                    Rect::new(INSET, y, SIDEBAR - INSET, y + 20.0),
                    Face::Small,
                    Align::Start,
                    colors.secondary,
                );
                y += 28.0;
                if needs_settings {
                    p.button(INSET, y, "Open Login Items...", Action::LoginSettings, true);
                    y += 36.0;
                }
                p.text(
                    "Appearance",
                    Rect::new(INSET, y, SIDEBAR - INSET, y + CONTROL),
                    Face::Title,
                    Align::Start,
                    colors.fg,
                );
                y += 32.0;
                p.toggle(
                    Rect::new(INSET, y, SIDEBAR - INSET, y + ROW),
                    "Follow macOS light/dark",
                    Action::FollowSystem,
                    self.settings.appearance.mode == ThemeMode::System,
                    true,
                );
                y += 40.0;
                for &slot in slots {
                    p.text(
                        slot.label(),
                        Rect::new(INSET, y, SIDEBAR - INSET, y + CONTROL),
                        Face::Label,
                        Align::Start,
                        colors.fg,
                    );
                    y += 26.0;
                    p.dropdown(
                        Rect::new(INSET, y, SIDEBAR - INSET, y + CONTROL),
                        self.settings.appearance.theme(slot).name(),
                        Action::ThemePicker(slot),
                        true,
                    );
                    y += 36.0;
                }
                p.text(
                    match self.system_appearance {
                        Appearance::Light => "macOS currently uses light appearance.",
                        Appearance::Dark => "macOS currently uses dark appearance.",
                    },
                    Rect::new(INSET, y, SIDEBAR - INSET, y + CONTROL),
                    Face::Small,
                    Align::Start,
                    colors.secondary,
                );
                p.scene.pop_layer();
                for (bounds, _) in &mut p.hits[first_hit..] {
                    *bounds = bounds.intersect(content);
                }
                if let Some(bar) = Scrollbar::new(content, height, self.scroll) {
                    p.scrollbar(bar, Action::Scroll(ScrollSurface::Sidebar, bar));
                }
            } else {
                let terms: Vec<_> = self
                    .filter
                    .text()
                    .split_whitespace()
                    .map(str::to_lowercase)
                    .collect();
                let saved: Vec<_> = self
                    .settings
                    .presets
                    .iter()
                    .filter(|(_, name)| terms.iter().all(|term| name.to_lowercase().contains(term)))
                    .collect();
                let count = saved.len();
                self.filter_layout = Some(p.field(
                    Rect::new(INSET, top + 60.0, SIDEBAR - INSET, top + 84.0),
                    &self.filter,
                    &count.to_string(),
                    [Action::Search, Action::ClearSearch],
                    matches!(self.text_focus, Some(TextFocus::Search)),
                    "Search presets...",
                ));
                if has_correction {
                    p.button(
                        INSET,
                        top + 92.0,
                        if named_preset.is_some() {
                            "Rename current preset..."
                        } else {
                            "Save loaded IR as preset..."
                        },
                        Action::SavePreset,
                        true,
                    );
                } else {
                    p.text(
                        "Load an IR to create a preset",
                        Rect::new(INSET, top + 92.0, SIDEBAR - INSET, top + 116.0),
                        Face::Small,
                        Align::Start,
                        colors.secondary,
                    );
                }
                let listing = Rect::new(0.0, top + 128.0, SIDEBAR, panel_bottom - 36.0);
                self.scroll = self
                    .scroll
                    .clamp(0.0, (count as f64 * ROW - listing.height()).max(0.0));
                let first = (self.scroll / ROW).floor() as usize;
                let offset = self.scroll % ROW;
                let visible = ((listing.height() + offset) / ROW).ceil() as usize;
                let first_hit = p.hits.len();
                p.scene.push_clip_layer(
                    vello::peniko::Fill::NonZero,
                    vello::kurbo::Affine::scale(scale),
                    &listing,
                );
                for (row, &(hash, name)) in saved.iter().skip(first).take(visible).enumerate() {
                    let y = listing.y0 + row as f64 * ROW - offset;
                    let bounds = Rect::new(0.0, y, SIDEBAR, y + ROW);
                    p.list_item(
                        bounds,
                        name,
                        Action::Recall(*hash),
                        match self.selected.as_ref().map(|selected| &selected.source) {
                            Some(Selection::Saved(selected)) => selected == hash,
                            _ => self.active.hash == *hash,
                        },
                        &[
                            (Icon::PencilSimple, Action::RenamePreset(*hash)),
                            (Icon::X, Action::DeletePreset(*hash)),
                        ],
                    );
                }
                if count == 0 {
                    p.text(
                        if self.settings.presets.is_empty() {
                            "No presets yet"
                        } else {
                            "No matching presets"
                        },
                        Rect::new(INSET, listing.y0, SIDEBAR - INSET, listing.y0 + CONTROL),
                        Face::Label,
                        Align::Start,
                        colors.secondary,
                    );
                }
                p.scene.pop_layer();
                for (bounds, _) in &mut p.hits[first_hit..] {
                    *bounds = bounds.intersect(listing);
                }
                if let Some(bar) = Scrollbar::new(listing, count as f64 * ROW, self.scroll) {
                    p.scrollbar(bar, Action::Scroll(ScrollSurface::Sidebar, bar));
                }
                p.text(
                    &format!("{count} presets / pencil to rename / X to remove"),
                    Rect::new(INSET, panel_bottom - 30.0, SIDEBAR - INSET, panel_bottom),
                    Face::Small,
                    Align::Start,
                    colors.secondary,
                );
            }
        }
        if let Some(TextFocus::PresetName {
            source,
            field,
            layout,
        }) = &mut self.text_focus
        {
            let width = 320.0_f64.min(window.width() - 32.0);
            let x = (window.width() - width) * 0.5;
            let y = top + 8.0;
            let card = Rect::new(x, y, x + width, y + 124.0);
            p.hits.clear();
            p.rounded(card, 6.0, colors.bg);
            p.outline(card, 6.0, 1.0, opacity(colors.fg, 0.22));
            p.text(
                if self.settings.presets.contains_key(source) {
                    "Rename preset"
                } else {
                    "Save preset"
                },
                Rect::new(x + 13.0, y + 8.0, x + width - 13.0, y + 32.0),
                Face::Title,
                Align::Start,
                colors.fg,
            );
            *layout = Some(Box::new(p.entry(
                Rect::new(x + 13.0, y + 40.0, x + width - 13.0, y + 64.0),
                field,
                Action::PresetName,
            )));
            let save = p.button(
                x + 13.0,
                y + 84.0,
                if self.settings.presets.contains_key(source) {
                    "Rename"
                } else {
                    "Save"
                },
                Action::CommitPresetName,
                true,
            );
            p.button(
                save.x1 + GAP,
                y + 84.0,
                "Cancel",
                Action::CancelPresetName,
                true,
            );
        }
        if let Some(picker) = &mut self.picker {
            Self::draw_picker(
                &mut p,
                picker,
                menu_options.as_ref().expect("open menu has options"),
                window,
                chrome,
            );
        }
        let hint = if help_enabled {
            help::target(&p.hits, self.cursor, self.browser.is_preferences())
        } else {
            None
        };
        self.help.draw(
            &mut p,
            hint,
            self.cursor,
            Size::new(window.width(), window.height()),
            top,
        );
        self.hits = p.hits;
        self.dirty = false;
        p.scene
    }
    fn draw(&mut self) -> Result<Presentation> {
        let scene = self.build_scene(self.viewport(), self.scale());
        self.graphics.as_mut().expect("drawing owns graphics").draw(
            &scene,
            self.theme.current.bg,
            self.window.as_ref().expect("drawing owns a window"),
        )
    }
}

fn search_key(text: &str) -> String {
    text.nfd()
        .filter(|ch| !is_combining_mark(*ch))
        .flat_map(char::to_lowercase)
        .collect()
}

fn sample_curve(curve: &Curve, grid: &Curve) -> Curve {
    Curve {
        frequency: grid.frequency.clone(),
        db: grid.frequency.iter().map(|&f| curve.at(f)).collect(),
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let outcome = (|| -> Result<()> {
            let [width, height] = match self.settings.window_mode {
                WindowMode::Full => self.settings.window.dimensions(),
                WindowMode::Compact => self
                    .settings
                    .compact_window
                    .map_or([720, 420], WindowSize::dimensions),
            };
            let minimum = match self.settings.window_mode {
                WindowMode::Full => (880.0, layout::MIN_HEIGHT),
                WindowMode::Compact => (520.0, 300.0),
            };
            let attributes = Window::default_attributes()
                .with_title("IRresponsible")
                .with_inner_size(LogicalSize::new(width, height))
                .with_min_inner_size(LogicalSize::new(minimum.0, minimum.1));
            #[cfg(target_os = "macos")]
            let attributes = {
                use winit::platform::macos::WindowAttributesExtMacOS;
                attributes
                    .with_titlebar_transparent(true)
                    .with_fullsize_content_view(true)
                    .with_title_hidden(true)
            };
            let window = Arc::new(
                event_loop
                    .create_window(attributes)
                    .map_err(|e| invalid(e.to_string()))?,
            );
            self.system_appearance = match window.theme() {
                Some(winit::window::Theme::Light) => Appearance::Light,
                _ => Appearance::Dark,
            };
            self.theme = Theme::new(self.settings.appearance.selected(self.system_appearance));
            self.window_chrome = Some(window_chrome::from_window(&window)?);
            self.graphics = Some(Graphics::new(window.clone())?);
            self.menu = Some(MenuBar::new()?);
            self.frames
                .resized(window.inner_size().into(), Instant::now());
            self.window = Some(window);
            self.initialize();
            Ok(())
        })();
        if let Err(e) = outcome {
            eprintln!("{e}");
            event_loop.exit();
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                if let Some(window) = &self.window {
                    window.set_visible(false);
                    self.frames
                        .occlusion_changed(Occlusion::Occluded, Instant::now());
                }
            }
            WindowEvent::ThemeChanged(theme) => self.system_appearance_changed(match theme {
                winit::window::Theme::Light => Appearance::Light,
                winit::window::Theme::Dark => Appearance::Dark,
            }),
            WindowEvent::Occluded(occluded) => {
                self.frames.occlusion_changed(
                    if occluded {
                        Occlusion::Occluded
                    } else {
                        Occlusion::Visible
                    },
                    Instant::now(),
                );
                if self.visible() {
                    self.dirty = true;
                }
            }
            WindowEvent::Resized(size) => {
                let area = SurfaceArea::from(size);
                self.frames.resized(area, Instant::now());
                if area == SurfaceArea::Empty {
                    return;
                }
                if let Some(window) = &self.window {
                    let logical: winit::dpi::LogicalSize<u32> =
                        size.to_logical(window.scale_factor());
                    if let Ok(size) = WindowSize::try_from([logical.width, logical.height]) {
                        match self.settings.window_mode {
                            WindowMode::Full => self.settings.window = size,
                            WindowMode::Compact => self.settings.compact_window = Some(size),
                        };
                        self.persist_after = Some(Instant::now() + Duration::from_millis(300));
                    }
                }
                if let Some(window) = &self.window {
                    match window_chrome::from_window(window) {
                        Ok(chrome) => self.window_chrome = Some(chrome),
                        Err(error) => self.message = error.to_string(),
                    }
                }
                if let Some(g) = &mut self.graphics {
                    g.context
                        .resize_surface(&mut g.surface, size.width, size.height);
                }
                self.dirty = true;
            }
            WindowEvent::RedrawRequested => {
                if self.frames.take_redraw_permit() == RedrawPermit::Idle || !self.visible() {
                    return;
                }
                self.refresh_visuals();
                match self.draw() {
                    Ok(Presentation::Presented) => {}
                    Ok(Presentation::Unavailable) => {
                        self.frames.surface_unavailable(Instant::now());
                        self.dirty = true;
                    }
                    Err(error) => {
                        self.message = error.to_string();
                        self.frames.surface_unavailable(Instant::now());
                        self.dirty = true;
                    }
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::CursorMoved {
                position: PhysicalPosition { x, y },
                ..
            } => {
                let scale = self.scale();
                self.cursor = Point::new(x / scale, y / scale);
                if let Some(picker) = &mut self.picker {
                    let hit = self
                        .hits
                        .iter()
                        .rev()
                        .find(|(bounds, _)| bounds.contains(self.cursor))
                        .map(|(_, action)| *action)
                        .filter(|action| {
                            !matches!(
                                action,
                                Action::PickerSearch
                                    | Action::ClearPickerSearch
                                    | Action::Scroll(..)
                            )
                        });
                    if let Some(hit) = hit {
                        picker.highlighted = Some(hit);
                        if let Action::ChooseTheme(_, index) = hit {
                            self.theme.show(&appearance::registry()[index].id);
                        }
                    }
                }
                match self.dragging {
                    Some(Drag::ValueBar { target, bounds }) => {
                        self.value_bar_from_cursor(target, bounds)
                    }
                    Some(Drag::Scroll { surface, bar, grab }) => {
                        self.scroll_from_cursor(surface, bar, grab)
                    }
                    Some(Drag::TextSelection) => {
                        if let Some(at) = self
                            .focused_layout()
                            .map(|layout| layout.caret_at(self.cursor))
                        {
                            self.focused_field_mut()
                                .expect("selection drag has focus")
                                .place(at, true);
                        }
                    }
                    None => {}
                }
                if let Some(window) = &self.window {
                    let hit = self
                        .hits
                        .iter()
                        .rev()
                        .find(|(bounds, _)| bounds.contains(self.cursor))
                        .map(|(_, action)| *action);
                    window.set_cursor(match hit {
                        Some(Action::ValueBar(_)) => winit::window::CursorIcon::EwResize,
                        Some(
                            Action::Search
                            | Action::EditNumber(_)
                            | Action::PickerSearch
                            | Action::PresetName,
                        ) => winit::window::CursorIcon::Text,
                        _ => winit::window::CursorIcon::Default,
                    });
                }
                self.dirty = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } if self.graph_geometry().graph.contains(self.cursor)
                && !(self.browser.is_preferences() && self.cursor.x < SIDEBAR) =>
            {
                if self.commit_number() {
                    self.text_focus = None;
                    self.open_picker(PickerKind::Graph(self.cursor));
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let hit = self
                    .hits
                    .iter()
                    .rev()
                    .find(|(bounds, _)| bounds.contains(self.cursor))
                    .map(|(_, action)| *action);
                if state == ElementState::Pressed {
                    self.pressed = hit;
                    match hit {
                        Some(Action::Scroll(surface, bar)) => self.begin_scroll(surface, bar),
                        Some(Action::ValueBar(target)) => {
                            if self.modifiers.alt_key() {
                                self.reset_value_bar(target);
                                self.pressed = None;
                            } else {
                                self.action(Action::ValueBar(target));
                            }
                        }
                        Some(
                            Action::Search
                            | Action::EditNumber(_)
                            | Action::PickerSearch
                            | Action::PresetName,
                        ) => {
                            let action = hit.expect("text field action");
                            let already_number = matches!((action, &self.text_focus), (Action::EditNumber(target), Some(TextFocus::Number {target: current, ..})) if target == *current);
                            let already_entry = matches!(
                                (action, &self.text_focus),
                                (Action::PickerSearch, Some(TextFocus::Picker))
                                    | (Action::PresetName, Some(TextFocus::PresetName { .. }))
                            );
                            if (action == Action::Search || !already_number) && !already_entry {
                                self.action(action);
                            }
                            if (action == Action::Search
                                && !matches!(self.text_focus, Some(TextFocus::Search)))
                                || matches!(action, Action::EditNumber(target) if !matches!(self.text_focus, Some(TextFocus::Number {target: current,..}) if current == target))
                            {
                                self.pressed = None;
                                return;
                            }
                            let target = match action {
                                Action::Search => ClickTarget::Filter,
                                Action::PickerSearch => ClickTarget::Picker,
                                Action::PresetName => ClickTarget::PresetName,
                                Action::EditNumber(target) => ClickTarget::Number(target),
                                _ => unreachable!("text field action"),
                            };
                            let count = self.record_click(target);
                            let range = self.focused_layout().map(|layout| match count {
                                ClickCount::Single => {
                                    let at = layout.caret_at(self.cursor);
                                    at..at
                                }
                                ClickCount::Double => layout.word_at(self.cursor),
                                ClickCount::Triple => {
                                    0..self
                                        .focused_field()
                                        .expect("focused selection")
                                        .text()
                                        .len()
                                }
                            });
                            if let Some(range) = range {
                                let extend = self.modifiers.shift_key();
                                let field = self.focused_field_mut().expect("focused selection");
                                if matches!(count, ClickCount::Single) {
                                    field.place(range.start, extend);
                                } else {
                                    field.select(range);
                                }
                            }
                            self.dragging = Some(Drag::TextSelection);
                        }
                        Some(_) => {}
                        None if self.picker.is_some() => {
                            self.dismiss_picker();
                        }
                        None if self.cursor.y < self.window_chrome.expect("live chrome").height
                            && self.cursor.x
                                >= self.window_chrome.expect("live chrome").controls_end =>
                        {
                            let count = self.record_click(ClickTarget::Titlebar);
                            if let Some(window) = &self.window {
                                if matches!(count, ClickCount::Double) {
                                    window.set_maximized(!window.is_maximized());
                                } else if let Err(error) = window.drag_window() {
                                    self.message = error.to_string();
                                }
                            }
                        }
                        None => {
                            if self.commit_number() {
                                self.text_focus = None;
                            }
                        }
                    }
                } else {
                    if self.dragging.is_none()
                        && let Some(action) = self.pressed.take()
                        && hit == Some(action)
                    {
                        self.click(action);
                    }
                    self.dragging = None;
                    self.pressed = None;
                }
                self.dirty = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.dirty |= self.help.clear();
                self.last_click = None;
                let pixels = match delta {
                    MouseScrollDelta::LineDelta(_, rows) => f64::from(rows) * CONTROL,
                    MouseScrollDelta::PixelDelta(point) => point.y / self.scale(),
                };
                if let Some(picker) = &mut self.picker {
                    if picker
                        .bounds
                        .is_some_and(|bounds| bounds.contains(self.cursor))
                    {
                        let rows = (pixels.abs() / CONTROL).ceil() as usize;
                        picker.scroll = if pixels > 0.0 {
                            picker.scroll.saturating_sub(rows)
                        } else {
                            picker.scroll.saturating_add(rows)
                        };
                        self.dirty = true;
                    }
                } else {
                    let region = if self.browser.is_preferences() {
                        Some(Rect::new(
                            0.0,
                            self.window_chrome.expect("live chrome").height + 60.0,
                            SIDEBAR,
                            self.viewport().height,
                        ))
                    } else if self.settings.window_mode == WindowMode::Full {
                        Some(self.geometry().listing)
                    } else {
                        None
                    };
                    if region.is_some_and(|bounds| bounds.contains(self.cursor))
                        && self.commit_number()
                    {
                        self.scroll = (self.scroll - pixels).max(0.0);
                        self.dirty = true;
                    }
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if self.dragging.is_none() {
                    self.cursor = Point::new(-1.0, -1.0);
                    self.dirty = true;
                }
            }
            WindowEvent::Focused(true) if self.browser == Browser::General => self.refresh_login(),
            WindowEvent::Focused(false) => {
                self.pressed = None;
                self.dragging = None;
                self.commit_number();
                self.dismiss_picker();
                self.text_focus = None;
                self.dirty = true;
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                self.dirty = true;
            }
            WindowEvent::DroppedFile(path) if self.job.is_none() => {
                self.selected = None;
                let rate = self.current_rate();
                let controls = self.settings.controls;
                let store = self.store.clone();
                self.job(move |_| {
                    let (view, engine) = View::prepare(
                        Correction::read_wav(&path, rate, &store)?,
                        controls,
                        &store,
                    )?;
                    Ok(JobResult::Prepared(view, engine))
                });
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                self.last_click = None;
                let command = self.modifiers.super_key() || self.modifiers.control_key();
                match &event.logical_key {
                    Key::Character(c) if command && c.eq_ignore_ascii_case("q") => {
                        event_loop.exit();
                        return;
                    }
                    Key::Character(c) if command && c.eq_ignore_ascii_case("w") => {
                        if let Some(window) = &self.window {
                            window.set_visible(false);
                            self.frames
                                .occlusion_changed(Occlusion::Occluded, Instant::now());
                        }
                        return;
                    }
                    Key::Character(c) if command && c.as_str() == "," => {
                        self.action(Action::Preferences);
                        return;
                    }
                    Key::Character(c) if command && c.eq_ignore_ascii_case("s") => {
                        self.action(Action::SavePreset);
                        return;
                    }
                    Key::Character(c)
                        if command && c.eq_ignore_ascii_case("f") && self.picker.is_some() =>
                    {
                        self.text_focus = Some(TextFocus::Picker);
                        self.picker
                            .as_mut()
                            .expect("open picker")
                            .filter
                            .select_all();
                        self.dirty = true;
                        return;
                    }
                    Key::Character(c) if command && c.eq_ignore_ascii_case("f") => {
                        self.action(Action::Search);
                        if matches!(self.text_focus, Some(TextFocus::Search)) {
                            self.filter.select_all();
                        }
                        self.dirty = true;
                        return;
                    }
                    _ => {}
                }
                if (self.text_focus.is_some() || self.picker.is_some())
                    && self.text_key(&event.logical_key, event.text.as_deref())
                {
                    return;
                }
                match &event.logical_key {
                    Key::Character(c) if command && c.eq_ignore_ascii_case("b") => {
                        self.action(Action::Bypass)
                    }
                    Key::Named(NamedKey::ArrowDown) if self.picker.is_none() => {
                        self.navigate_catalog(Navigation::Next)
                    }
                    Key::Named(NamedKey::ArrowUp) if self.picker.is_none() => {
                        self.navigate_catalog(Navigation::Previous)
                    }
                    Key::Named(NamedKey::Escape) => {
                        if self.picker.is_some() {
                            self.dismiss_picker();
                        } else {
                            if self.text_focus.is_some() {
                                self.text_focus = None;
                            } else if self.browser.is_preferences() {
                                self.action(Action::Preferences);
                            } else {
                                self.action(Action::Cancel);
                            }
                        }
                        self.dirty = true;
                    }
                    _ => {}
                }
            }

            _ => {}
        }
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.poll();
        if self.text_focus.is_some() && self.visible() {
            self.dirty = true;
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if let Some(menu) = &self.menu {
                if event.id == *menu.show.id() {
                    if let Some(window) = &self.window {
                        window.set_visible(true);
                        window.focus_window();
                        self.frames
                            .occlusion_changed(Occlusion::Visible, Instant::now());
                        self.dirty = true;
                    }
                } else if event.id == *menu.quit.id() {
                    event_loop.exit();
                } else if event.id == *menu.bypass.id() {
                    self.action(Action::Bypass);
                } else if event.id == *menu.louder.id() {
                    self.volume(self.settings.controls.volume.db() + 2.0);
                } else if event.id == *menu.quieter.id() {
                    self.volume(self.settings.controls.volume.db() - 2.0);
                } else if event.id == *menu.stop.id() {
                    self.command(Command::Stop);
                }
            }
        }
        let now = Instant::now();
        if self.frames.is_due(now) {
            if self.frames.kind(now) == FrameKind::Visible
                && (self.dirty
                    || matches!(self.lifecycle, Lifecycle::Running { .. })
                    || self.theme.is_transitioning())
            {
                self.frames
                    .request_visible_frame(now, self.settings.spectrum.frame_rate);
            } else {
                self.frames.maintenance_finished(now);
            }
        }
        if self.frames.redraw_is_pending() && self.frames.kind(now) == FrameKind::Visible {
            self.window
                .as_ref()
                .expect("a drawable frame owns a window")
                .request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.frames.deadline(now)));
    }
    fn exiting(&mut self, _: &ActiveEventLoop) {
        self.commit_number();
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Relaxed);
        }
        self.save();
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    use irresponsible::frame_schedule::FrameRate;
    fn app() -> (tempfile::TempDir, App) {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::at(directory.path()).unwrap();
        let revision = irresponsible::catalog::UPSTREAM_REVISION;
        let index = store
            .put(include_bytes!("../tests/fixtures/autoeq-index.md"))
            .unwrap();
        let recommended = store
            .put(include_bytes!("../tests/fixtures/autoeq-recommended.md"))
            .unwrap();
        let inventory = store
            .put(include_bytes!("../tests/fixtures/autoeq-inventory.json"))
            .unwrap();
        store.write("catalog.json", &serde_json::to_vec(&serde_json::json!({"revision": revision, "index":index, "recommended":recommended, "inventory":inventory})).unwrap()).unwrap();
        let fixtures: &[(&str, &[u8])] = &[
            (
                "crinacle/GRAS%2043AG-7%20over-ear/Beyerdynamic%20DT%20990%20Pro%20(250%20Ohm)/Beyerdynamic%20DT%20990%20Pro%20(250%20Ohm)%20minimum%20phase%2044100Hz.wav",
                include_bytes!("../tests/fixtures/dt990-pro-250-44100.wav"),
            ),
            (
                "oratory1990/over-ear/Beyerdynamic%20DT%20990%20Pro/Beyerdynamic%20DT%20990%20Pro%20minimum%20phase%2044100Hz.wav",
                include_bytes!("../tests/fixtures/dt990-pro-oratory-44100.wav"),
            ),
            (
                "oratory1990/over-ear/Beyerdynamic%20DT%20990%20Pro/Beyerdynamic%20DT%20990%20Pro.csv",
                include_bytes!("../tests/fixtures/dt990-pro.csv"),
            ),
            (
                "Auriculares%20Argentina/over-ear/Beyerdynamic%20DT%20990%20Pro%20(250%20Ohm)/Beyerdynamic%20DT%20990%20Pro%20(250%20Ohm)%20minimum%20phase%2044100Hz.wav",
                include_bytes!("../tests/fixtures/dt990-argentina-44100.wav"),
            ),
            (
                "Auriculares%20Argentina/over-ear/Beyerdynamic%20DT%20990%20Pro%20(250%20Ohm)/Beyerdynamic%20DT%20990%20Pro%20(250%20Ohm).csv",
                include_bytes!("../tests/fixtures/dt990-argentina.csv"),
            ),
        ];
        for (path, bytes) in fixtures {
            let key = ContentHash::of(format!("{revision}/results/{path}").as_bytes());
            let hash = store.put(bytes).unwrap();
            store
                .write(
                    &format!("asset-{}.json", key.as_str()),
                    &serde_json::to_vec(&hash).unwrap(),
                )
                .unwrap();
        }
        let mut app = App::new(store).unwrap();
        app.frames.resized(SurfaceArea::NonEmpty, Instant::now());
        app.window_chrome = Some(WindowChrome {
            height: 32.0,
            controls_end: 81.0,
        });
        (directory, app)
    }
    fn wait_for_preview(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.job.is_some() && Instant::now() < deadline {
            app.poll();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(app.preview.is_some(), "{}", app.message);
    }
    fn wait_for_loaded(app: &mut App, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while (app.active.correction.name != name || app.awaiting.is_some())
            && Instant::now() < deadline
        {
            app.poll();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(app.active.correction.name, name, "{}", app.message);
        assert!(app.awaiting.is_none());
    }
    #[test]
    fn repeated_single_click_reuses_preview_and_double_click_loads_it() {
        let (_directory, mut app) = app();
        let index = app
            .catalog
            .as_ref()
            .unwrap()
            .search("DT 990 Pro oratory")
            .next()
            .unwrap()
            .0;
        app.click(Action::Select(index));
        wait_for_preview(&mut app);
        let prepared = app.preview.as_ref().unwrap().0.correction.clone();
        app.last_click = None;
        app.click(Action::Select(index));
        assert!(app.job.is_none());
        assert!(Arc::ptr_eq(
            &prepared,
            &app.preview.as_ref().unwrap().0.correction
        ));
        assert_eq!(app.active.correction.name, "No correction");
        app.click(Action::Select(index));
        assert!(Arc::ptr_eq(
            &prepared,
            &app.awaiting.as_ref().unwrap().correction
        ));
        wait_for_loaded(&mut app, "Beyerdynamic DT 990 Pro");
        assert!(Arc::ptr_eq(&prepared, &app.active.correction));
    }
    #[test]
    fn double_click_during_preparation_loads_the_latest_entry() {
        let (_directory, mut app) = app();
        let catalog = app.catalog.as_ref().unwrap();
        let first = catalog.search("DT 990 Pro oratory").next().unwrap().0;
        let last = catalog
            .search("DT 990 Pro (250 Ohm) Auriculares Argentina")
            .next()
            .unwrap()
            .0;
        app.click(Action::Select(first));
        app.click(Action::Select(last));
        app.click(Action::Select(last));
        assert_eq!(app.active.correction.name, "No correction");
        wait_for_loaded(&mut app, "Beyerdynamic DT 990 Pro (250 Ohm)");
        assert!(
            matches!(&app.active.correction.origin, Origin::PublishedAutoEq { provider, .. } if provider == "Auriculares Argentina")
        );
    }
    #[test]
    fn catalog_target_persists_across_selections_and_unsupported_entries() {
        let (_directory, mut app) = app();
        let catalog = app.catalog.as_ref().unwrap();
        let oratory = catalog.search("DT 990 Pro oratory").next().unwrap().0;
        let argentina = catalog
            .search("DT 990 Pro (250 Ohm) Auriculares Argentina")
            .next()
            .unwrap()
            .0;
        let published = catalog
            .search("DT 990 Pro (250 Ohm) crinacle")
            .next()
            .unwrap()
            .0;
        app.action(Action::Select(oratory));
        wait_for_preview(&mut app);
        app.action(Action::Flat);
        wait_for_preview(&mut app);
        app.action(Action::Select(argentina));
        wait_for_preview(&mut app);
        assert_eq!(
            app.preview.as_ref().unwrap().0.correction.target,
            Target::Flat
        );
        app.action(Action::Harman);
        wait_for_preview(&mut app);
        app.action(Action::Select(published));
        assert!(app.preview.is_none() && app.job.is_none());
        assert_eq!(app.settings.catalog_target, Some(CatalogTarget::Reference));
        assert!(app.message.contains("cannot use"));
        app.action(Action::Select(oratory));
        wait_for_preview(&mut app);
        assert_eq!(
            app.preview.as_ref().unwrap().0.correction.target,
            Target::Harman
        );
        assert_eq!(
            app.store.load().unwrap().catalog_target,
            Some(CatalogTarget::Reference)
        );
        assert_eq!(app.active.correction.name, "No correction");
    }
    #[test]
    fn an_unset_catalog_mode_inherits_the_loaded_profiles_target() {
        let (_directory, mut app) = app();
        let index = app
            .catalog
            .as_ref()
            .unwrap()
            .search("DT 990 Pro oratory")
            .next()
            .unwrap()
            .0;
        app.action(Action::Select(index));
        wait_for_preview(&mut app);
        app.action(Action::Harman);
        wait_for_preview(&mut app);
        app.settings.active = Some(app.preview.as_ref().unwrap().0.hash);
        app.settings.catalog_target = None;
        app.save();
        let restored = App::new(app.store.clone()).unwrap();
        assert_eq!(restored.active.correction.target, Target::Harman);
        assert_eq!(restored.catalog_target(), CatalogTarget::Reference);
    }

    #[test]
    fn double_click_on_a_saved_profile_loads_after_preparation() {
        let (_directory, mut app) = app();
        let index = app
            .catalog
            .as_ref()
            .unwrap()
            .search("DT 990 Pro oratory")
            .next()
            .unwrap()
            .0;
        app.action(Action::Select(index));
        wait_for_preview(&mut app);
        let hash = app.preview.as_ref().unwrap().0.hash;
        app.settings
            .presets
            .insert(hash, "Beyerdynamic DT 990 Pro".into());
        app.action(Action::BrowsePresets);
        app.click(Action::Recall(hash));
        app.click(Action::Recall(hash));
        wait_for_loaded(&mut app, "Beyerdynamic DT 990 Pro");
        assert_eq!(app.settings.active, Some(hash));
    }

    #[test]
    fn argentina_harman_and_treble_options_prepare_comparisons_without_replacing_audio() {
        let (_directory, mut app) = app();
        let index = app
            .catalog
            .as_ref()
            .unwrap()
            .search("DT 990 Pro (250 Ohm) Auriculares Argentina")
            .next()
            .unwrap()
            .0;
        app.action(Action::Select(index));
        wait_for_preview(&mut app);
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        for action in [Action::AutoEq, Action::Harman, Action::Flat] {
            assert!(app.hits.iter().any(|(_, hit)| *hit == action));
        }
        app.action(Action::Harman);
        wait_for_preview(&mut app);
        let extended = app.preview.as_ref().unwrap().0.response[0].at(18000.0);
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        assert!(
            app.hits
                .iter()
                .any(|(_, action)| *action == Action::TrebleExtension)
        );
        app.action(Action::TrebleExtension);
        wait_for_preview(&mut app);
        let ir = &app.preview.as_ref().unwrap().0;
        assert!(matches!(
            ir.correction.origin,
            Origin::AutoEq { generation, .. } if generation.treble() == TrebleExtension::Limited
        ));
        assert!((ir.response[0].at(18000.0) - extended).abs() > 1.0);
        assert_eq!(
            app.store.load().unwrap().generation.treble(),
            TrebleExtension::Limited
        );
        assert_eq!(app.active.correction.name, "No correction");
    }

    #[test]
    fn non_harman_rigs_show_the_documented_reference_target_name() {
        let (_directory, mut app) = app();
        let index = app
            .catalog
            .as_ref()
            .unwrap()
            .entries
            .iter()
            .position(|entry| entry.path == "HypetheSonics/over-ear/Audeze%20LCD-4")
            .unwrap();
        app.selected = Some(SelectedProfile::compare(Selection::Catalog(index)));
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        assert!(
            app.hits
                .iter()
                .any(|(_, action)| *action == Action::Reference)
        );
        assert!(!app.hits.iter().any(|(_, action)| *action == Action::Harman));
        assert!(app.hits.iter().any(|(_, action)| *action == Action::Flat));
    }

    #[test]
    fn graph_controls_follow_assets_and_bypass_lives_in_the_titlebar() {
        let (_directory, mut app) = app();
        assert!(!app.details, "Details starts collapsed");
        let endpoint = |id: &str| Endpoint {
            uid: DeviceUid::new(cpal::DeviceId::new(cpal::default_host().id(), id)),
            label: id.to_owned(),
            pair: StereoPair::default(),
        };
        app.settings.route = Some(Route {
            capture: endpoint("test-capture"),
            playback: endpoint("test-playback"),
        });
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        let bypass = app
            .hits
            .iter()
            .find(|(_, action)| *action == Action::Bypass)
            .unwrap()
            .0;
        assert!(bypass.y0 >= 0.0 && bypass.y1 <= 32.0);
        let transport: Vec<_> = app
            .hits
            .iter()
            .filter(|(_, action)| *action == Action::StartStop)
            .map(|(rect, _)| *rect)
            .collect();
        assert_eq!(transport.len(), 1);
        assert!(
            transport
                .iter()
                .any(|rect| rect.y0 >= 0.0 && rect.y1 <= 32.0 && rect.x1 + GAP <= bypass.x0)
        );
        for action in [
            Action::ShowCandidate,
            Action::ShowMeasured,
            Action::ShowTarget,
            Action::ShowPrediction,
        ] {
            assert!(!app.hits.iter().any(|(_, hit)| *hit == action));
        }
        let index = app
            .catalog
            .as_ref()
            .unwrap()
            .search("DT 990 Pro (250 Ohm) crinacle")
            .next()
            .unwrap()
            .0;
        app.action(Action::Select(index));
        wait_for_preview(&mut app);
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        assert!(app.hits.iter().any(|(_, action)| *action == Action::AutoEq));
        assert!(
            !app.hits
                .iter()
                .any(|(_, action)| matches!(action, Action::Harman | Action::Flat))
        );
        assert!(
            app.hits
                .iter()
                .any(|(_, action)| *action == Action::ShowCandidate)
        );
        assert!(!app.hits.iter().any(|(_, action)| matches!(
            action,
            Action::ShowMeasured | Action::ShowTarget | Action::ShowPrediction
        )));
        let replace = app
            .hits
            .iter()
            .find(|(_, action)| *action == Action::ReplaceLoaded)
            .unwrap()
            .0;
        assert_eq!(replace.intersect(app.geometry().graph), replace);
        app.action(Action::Bypass);
        assert_eq!(app.settings.controls.correction, CorrectionMode::Bypass);
        app.action(Action::Bypass);
        assert_eq!(app.settings.controls.correction, CorrectionMode::On);
        let index = app
            .catalog
            .as_ref()
            .unwrap()
            .search("DT 990 Pro oratory")
            .next()
            .unwrap()
            .0;
        app.action(Action::Select(index));
        wait_for_preview(&mut app);
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        assert!(app.hits.iter().any(|(_, action)| *action == Action::Harman));
        assert!(app.hits.iter().any(|(_, action)| *action == Action::Flat));
    }

    #[test]
    fn dismissing_comparison_keeps_catalog_targets_and_allows_another_preview() {
        let (_directory, mut app) = app();
        let index = app
            .catalog
            .as_ref()
            .unwrap()
            .search("DT 990 Pro oratory")
            .next()
            .unwrap()
            .0;
        let active = app.active.correction.clone();
        app.action(Action::Select(index));
        wait_for_preview(&mut app);
        app.action(Action::Cancel);
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        assert!(app.preview.is_none());
        assert!(!app.has_candidate());
        assert!(Arc::ptr_eq(&active, &app.active.correction));
        for target in [Action::AutoEq, Action::Harman, Action::Flat] {
            assert!(
                app.hits.iter().any(|(_, action)| *action == target),
                "dismissing the toast must keep the selected profile's target buttons"
            );
        }
        assert!(
            app.selected
                .as_ref()
                .is_some_and(|selected| selected.source == Selection::Catalog(index))
        );
        app.action(Action::AutoEq);
        wait_for_preview(&mut app);
        assert_eq!(
            app.preview.as_ref().unwrap().0.correction.name,
            "Beyerdynamic DT 990 Pro"
        );
        assert!(Arc::ptr_eq(&active, &app.active.correction));
    }

    #[test]
    fn dismissing_inflight_comparison_hides_toast_and_allows_queued_preview() {
        let (_directory, mut app) = app();
        let index = app
            .catalog
            .as_ref()
            .unwrap()
            .search("DT 990 Pro oratory")
            .next()
            .unwrap()
            .0;
        app.action(Action::Select(index));
        app.action(Action::Cancel);
        assert!(app.job.is_some(), "dismissal must not block on the worker");
        assert!(
            app.selected
                .as_ref()
                .is_some_and(|selected| selected.source == Selection::Catalog(index))
        );
        assert!(
            !app.has_candidate(),
            "a cancelled preparation must not keep the toast visible"
        );
        app.action(Action::AutoEq);
        assert!(
            app.has_candidate(),
            "the queued replacement preview must show its toast"
        );
        wait_for_preview(&mut app);
        assert_eq!(
            app.preview.as_ref().unwrap().0.correction.name,
            "Beyerdynamic DT 990 Pro"
        );
        assert_eq!(app.active.correction.name, "No correction");
    }

    #[test]
    fn catalog_requirement_is_saved_and_used_by_keyboard_navigation_while_running() {
        let (_directory, mut app) = app();
        app.action(Action::ChooseCatalogFilter(CatalogFilter::Both));
        assert_eq!(
            app.store.load().unwrap().catalog_filter,
            CatalogFilter::Both
        );
        app.navigate_catalog(Navigation::Next);
        let Some(Selection::Catalog(index)) =
            app.selected.as_ref().map(|selected| &selected.source)
        else {
            panic!("keyboard selected an entry")
        };
        let entry = &app.catalog.as_ref().unwrap().entries[*index];
        assert!(entry.has_measurements() && entry.has_autoeq());
        wait_for_preview(&mut app);
        app.lifecycle = Lifecycle::Running {
            rate: Rate::Hz44100,
            estimated_latency_ms: 12.0,
        };
        app.action(Action::CatalogFilter);
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        for filter in CatalogFilter::ALL {
            assert!(
                app.hits
                    .iter()
                    .any(|(_, action)| *action == Action::ChooseCatalogFilter(filter))
            );
        }
    }
    #[test]
    fn latest_profile_selection_wins_and_replace_has_a_confirmed_result() {
        let (_directory, mut app) = app();
        let catalog = app.catalog.as_ref().unwrap();
        let first = catalog.entries.iter().position(|entry| entry.path == "crinacle/GRAS%2043AG-7%20over-ear/Beyerdynamic%20DT%20990%20Pro%20(250%20Ohm)").unwrap();
        let last = catalog
            .entries
            .iter()
            .position(|entry| entry.path == "oratory1990/over-ear/Beyerdynamic%20DT%20990%20Pro")
            .unwrap();
        app.action(Action::Select(first));
        app.action(Action::Select(last));
        wait_for_preview(&mut app);
        assert_eq!(
            app.preview.as_ref().unwrap().0.correction.name,
            "Beyerdynamic DT 990 Pro"
        );
        assert_eq!(app.active.correction.name, "No correction");
        app.action(Action::ReplaceLoaded);
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.awaiting.is_some() && Instant::now() < deadline {
            app.poll();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            app.active.correction.name, "Beyerdynamic DT 990 Pro",
            "{}",
            app.message
        );
        assert!(app.awaiting.is_none());
    }
    #[test]
    fn settings_help_covers_labels_and_controls_without_intercepting_input() {
        let (_directory, mut app) = app();
        app.action(Action::BrowseGeneration);
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        let target = NumberField::Generation(GenerationParameter::Smoothing);
        let control = app
            .hits
            .iter()
            .find(|(_, hit)| *hit == Action::EditNumber(target))
            .unwrap()
            .0;
        app.cursor = Point::new(INSET + 4.0, control.center().y);
        let label_hint = app.help_target().expect("the setting label offers help");
        app.cursor = control.center();
        assert!(app.help_target() == Some(label_hint));
        let now = Instant::now();
        app.help.update(Some(label_hint), now);
        assert!(app.help.update(Some(label_hint), now + help::DELAY));
        let hits = app.hits.clone();
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        assert!(
            app.hits == hits,
            "help is an overlay with no clickable area"
        );
        app.action(Action::EditNumber(target));
        assert!(app.help_target().is_none());
        app.text_key(&Key::Named(NamedKey::Escape), None);
        app.action(Action::GenerationLength);
        assert!(app.help_target().is_none());
        app.dismiss_picker();
        app.dragging = Some(Drag::TextSelection);
        assert!(app.help_target().is_none());
    }
    #[test]
    fn generation_edits_validate_and_prepare_without_replacing_the_playing_profile() {
        let (_directory, mut app) = app();
        let index = app
            .catalog
            .as_ref()
            .unwrap()
            .search("DT 990 Pro oratory")
            .next()
            .unwrap()
            .0;
        app.action(Action::Select(index));
        wait_for_preview(&mut app);
        app.action(Action::Harman);
        wait_for_preview(&mut app);
        let playing = app.active.correction.clone();
        app.action(Action::BrowseGeneration);
        app.action(Action::ChooseLength(FirLength::Taps4096));
        let target = NumberField::Generation(GenerationParameter::Smoothing);
        app.action(Action::EditNumber(target));
        app.text_key(&Key::Character("1/12".into()), Some("1/12"));
        app.text_key(&Key::Named(NamedKey::Enter), None);
        wait_for_preview(&mut app);
        assert!(Arc::ptr_eq(&playing, &app.active.correction));
        assert!(app.awaiting.is_none());
        assert_eq!(
            app.preview.as_ref().unwrap().0.correction.channels[0].len(),
            4096
        );
        assert_eq!(
            app.store.load().unwrap().generation,
            app.settings.generation
        );
        app.action(Action::GenerationBass);
        app.action(Action::GenerationConcha);
        app.action(Action::GenerationCutLimit);
        wait_for_preview(&mut app);
        assert_eq!(app.settings.generation.bass(), BassExtension::Tapered);
        assert_eq!(
            app.settings.generation.concha(),
            ConchaInterference::Present
        );
        assert_eq!(app.settings.generation.cut_limit(), Some(18.0));
        assert_eq!(
            app.store.load().unwrap().generation,
            app.settings.generation
        );
        assert!(Arc::ptr_eq(&playing, &app.active.correction));
        let valid = app.settings.generation;
        app.action(Action::EditNumber(NumberField::Generation(
            GenerationParameter::LowUnity,
        )));
        app.text_key(&Key::Character("30".into()), Some("30"));
        app.text_key(&Key::Named(NamedKey::Enter), None);
        assert_eq!(app.settings.generation, valid);
        assert!(app.focused_field().is_some());
        app.text_key(&Key::Named(NamedKey::Escape), None);
        app.action(Action::TrebleExtension);
        app.build_scene(Size::new(880.0, 760.0), 1.0);
        assert!(
            app.hits
                .iter()
                .any(|(_, action)| *action == Action::EditNumber(target))
        );
        assert!(app.hits.iter().any(|(_, action)| *action
            == Action::EditNumber(NumberField::Generation(GenerationParameter::HighUnity))));
        app.action(Action::GenerationDefaults);
        wait_for_preview(&mut app);
        assert_eq!(
            app.settings.generation,
            GenerationSettings::default().with_treble(TrebleExtension::Limited)
        );
        assert_eq!(
            app.store.load().unwrap().generation,
            app.settings.generation
        );
        assert!(
            matches!(app.preview.as_ref().unwrap().0.correction.origin, Origin::AutoEq { generation, .. } if generation == app.settings.generation)
        );
        assert!(Arc::ptr_eq(&playing, &app.active.correction));
        app.build_scene(Size::new(880.0, 760.0), 1.0);
        assert!(
            !app.hits
                .iter()
                .any(|(_, action)| *action == Action::GenerationDefaults)
        );
        app.action(Action::TrebleExtension);
        app.action(Action::ChooseLength(FirLength::Taps2048));
        app.action(Action::GenerationDefaults);
        wait_for_preview(&mut app);
        assert_eq!(app.settings.generation, GenerationSettings::default());
        app.action(Action::Search);
        assert!(app.browser == Browser::Catalog);
    }
    #[test]
    fn named_presets_are_searchable_and_recall_through_audio_confirmation() {
        let (_directory, mut app) = app();
        let first = app
            .catalog
            .as_ref()
            .unwrap()
            .search("DT 990 Pro oratory")
            .next()
            .unwrap()
            .0;
        app.action(Action::Select(first));
        wait_for_preview(&mut app);
        app.action(Action::ReplaceLoaded);
        wait_for_loaded(&mut app, "Beyerdynamic DT 990 Pro");
        let saved = app.active.hash;
        app.action(Action::SavePreset);
        app.text_key(
            &Key::Character("Late night listening".into()),
            Some("Late night listening"),
        );
        app.text_key(&Key::Named(NamedKey::Enter), None);
        assert_eq!(
            app.store.load().unwrap().presets[&saved],
            "Late night listening"
        );
        let second = app
            .catalog
            .as_ref()
            .unwrap()
            .search("DT 990 Pro (250 Ohm) Auriculares Argentina")
            .next()
            .unwrap()
            .0;
        app.action(Action::Select(second));
        app.request_load();
        app.action(Action::SavePreset);
        app.text_key(
            &Key::Character("Late night listening".into()),
            Some("Late night listening"),
        );
        wait_for_loaded(&mut app, "Beyerdynamic DT 990 Pro (250 Ohm)");
        assert!(
            matches!(&app.text_focus,Some(TextFocus::PresetName {source,..}) if *source==saved)
        );
        app.text_key(&Key::Named(NamedKey::Enter), None);
        assert_eq!(app.settings.presets[&saved], "Late night listening");
        assert!(!app.settings.presets.contains_key(&app.active.hash));
        let playing = app.active.correction.clone();
        app.action(Action::RenamePreset(saved));
        app.text_key(
            &Key::Character("Night listening".into()),
            Some("Night listening"),
        );
        app.text_key(&Key::Named(NamedKey::Enter), None);
        assert!(Arc::ptr_eq(&playing, &app.active.correction));
        app.lifecycle = Lifecycle::Running {
            rate: Rate::Hz48000,
            estimated_latency_ms: 12.0,
        };
        app.action(Action::PresetPicker);
        app.text_key(&Key::Character("night".into()), Some("night"));
        assert_eq!(app.filtered_picker_options().len(), 1);
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        let picker = app.picker.as_ref().unwrap();
        assert_eq!(picker.layout.as_ref().unwrap().content.height(), 22.0);
        app.text_key(&Key::Named(NamedKey::Enter), None);
        wait_for_loaded(&mut app, "Beyerdynamic DT 990 Pro");
        assert_eq!(app.active.hash, saved);
        assert_eq!(app.active.correction.rate, Rate::Hz48000);
        assert_eq!(app.settings.presets[&saved], "Night listening");
        let prepared = Arc::new(
            app.active
                .correction
                .at_rate(Rate::Hz44100, &app.store)
                .unwrap(),
        );
        app.active_from_receipt(prepared);
        assert_eq!(app.active.hash, saved);
        app.action(Action::BrowsePresets);
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        for action in [
            Action::BrowseGeneral,
            Action::BrowseGeneration,
            Action::BrowsePresets,
        ] {
            let bounds = app.hits.iter().find(|(_, hit)| *hit == action).unwrap().0;
            assert_eq!(bounds.y0, app.window_chrome.unwrap().height);
            assert_eq!(bounds.height(), 48.0);
        }
        assert!(
            app.hits
                .iter()
                .filter(|(_, action)| *action == Action::Preferences)
                .all(|(bounds, _)| bounds.y1 <= app.window_chrome.unwrap().height)
        );
        let remove = app
            .hits
            .iter()
            .find(|(_, action)| *action == Action::DeletePreset(saved))
            .unwrap()
            .0;
        let row = app
            .hits
            .iter()
            .find(|(_, action)| matches!(action, Action::Recall(_)))
            .unwrap()
            .0;
        assert_eq!(remove.center().y, row.center().y);
        assert_eq!(row.height(), ROW);
        let playing = app.active.correction.clone();
        app.action(Action::DeletePreset(saved));
        assert!(Arc::ptr_eq(&playing, &app.active.correction));
        assert_eq!(app.settings.active, Some(saved));
        assert!(!app.store.load().unwrap().presets.contains_key(&saved));
        assert!(app.store.get(&saved).is_ok());
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        assert!(
            app.hits
                .iter()
                .any(|(_, action)| *action == Action::SavePreset)
        );
    }
    #[test]
    fn compact_mode_keeps_audio_and_only_draws_graph_controls_below_the_titlebar() {
        let (_directory, mut app) = app();
        let full_size = app.settings.window.dimensions();
        let playing = app.active.correction.clone();
        app.action(Action::Compact);
        assert_eq!(app.store.load().unwrap().window_mode, WindowMode::Compact);
        for size in [Size::new(520.0, 300.0), Size::new(720.0, 420.0)] {
            app.build_scene(size, 1.0);
            for action in [
                Action::PresetPicker,
                Action::Bypass,
                Action::Compact,
                Action::Preferences,
            ] {
                let bounds = app.hits.iter().find(|(_, a)| *a == action).unwrap().0;
                assert!(bounds.y1 <= 32.0);
            }
            let preset = app
                .hits
                .iter()
                .find(|(_, a)| *a == Action::PresetPicker)
                .unwrap()
                .0;
            let bypass = app
                .hits
                .iter()
                .find(|(_, a)| *a == Action::Bypass)
                .unwrap()
                .0;
            assert!(preset.width() > 200.0 && preset.x1 + GAP <= bypass.x0);
            assert!(!app.hits.iter().any(|(_, a)| matches!(
                a,
                Action::Capture
                    | Action::Playback
                    | Action::ValueBar(ValueBar::Volume)
                    | Action::Details
                    | Action::Select(_)
            )));
            let graph = GraphLayout::compact(size, app.window_chrome.unwrap(), false);
            assert_eq!(
                graph.graph,
                Rect::new(
                    0.0,
                    app.window_chrome.unwrap().height,
                    size.width,
                    size.height
                )
            );
            assert!(graph.plot.width() > 400.0 && graph.plot.height() > 100.0);
        }
        assert!(Arc::ptr_eq(&playing, &app.active.correction));
        app.action(Action::Compact);
        assert_eq!(app.settings.window_mode, WindowMode::Full);
        assert_eq!(app.settings.window.dimensions(), full_size);
    }
    #[test]
    fn spectrum_refresh_runs_on_permitted_frames_and_pauses_while_hidden() {
        let (_directory, mut app) = app();
        app.lifecycle = Lifecycle::Running {
            rate: Rate::Hz48000,
            estimated_latency_ms: 28.0,
        };
        let (mut tap, stream) = analysis::channel(Rate::Hz48000);
        app.spectrum.attach(stream);
        for _ in 0..4096 {
            tap.push([0.25; 2], [0.125; 2]);
        }
        app.poll();
        assert!(
            !app.spectrum.ready(),
            "event-loop polling must not run the FFT ahead of a permitted draw"
        );
        let now = Instant::now();
        app.frames.request_visible_frame(now, FrameRate::Fps60);
        assert_eq!(app.frames.take_redraw_permit(), RedrawPermit::Requested);
        app.refresh_visuals();
        assert!(app.spectrum.ready());
        assert_eq!(app.frames.take_redraw_permit(), RedrawPermit::Idle);
        app.frames.occlusion_changed(Occlusion::Occluded, now);
        app.poll();
        assert!(!app.runtime.meters.spectrum.enabled() && !app.spectrum.ready());
    }
    #[test]
    fn spectrum_intensity_supports_dragging_text_entry_reset_and_saved_fractions() {
        let (_directory, mut app) = app();
        let playing = app.active.correction.clone();
        app.action(Action::BrowseSpectrum);
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        let bounds = app
            .hits
            .iter()
            .find(|(_, hit)| *hit == Action::ValueBar(ValueBar::SpectrumIntensity))
            .unwrap()
            .0;
        app.cursor = Point::new(bounds.x0 + bounds.width() * 0.3725, bounds.center().y);
        app.action(Action::ValueBar(ValueBar::SpectrumIntensity));
        assert!((app.settings.spectrum.intensity.percent() - 37.25).abs() < 0.001);
        app.cursor.x = bounds.x1 + 100.0;
        app.value_bar_from_cursor(ValueBar::SpectrumIntensity, bounds);
        assert_eq!(app.settings.spectrum.intensity.percent(), 100.0);
        app.dragging = None;
        app.action(Action::EditNumber(NumberField::SpectrumIntensity));
        app.text_key(&Key::Character("23.75".into()), Some("23.75"));
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        assert!(app.focused_layout().is_some());
        app.text_key(&Key::Named(NamedKey::Enter), None);
        assert_eq!(app.settings.spectrum.intensity.percent(), 23.75);
        app.action(Action::ChooseSpectrum(SpectrumChoice::FrameRate(
            FrameRate::Fps120,
        )));
        let stored = app.store.load().unwrap().spectrum;
        assert_eq!(stored.intensity.percent(), 23.75);
        assert_eq!(stored.frame_rate, FrameRate::Fps120);
        app.reset_value_bar(ValueBar::SpectrumIntensity);
        assert_eq!(
            app.settings.spectrum.intensity,
            analysis::Intensity::default()
        );
        assert!(Arc::ptr_eq(&playing, &app.active.correction));
        assert!(app.preview.is_none() && app.awaiting.is_none() && app.job.is_none());
    }
    #[test]
    fn spectrum_preferences_apply_live_persist_and_pause_without_changing_audio() {
        let (_directory, mut app) = app();
        let playing = app.active.correction.clone();
        app.lifecycle = Lifecycle::Running {
            rate: Rate::Hz48000,
            estimated_latency_ms: 28.0,
        };
        app.action(Action::BrowseSpectrum);
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        for setting in SpectrumSetting::ALL {
            assert!(
                app.hits
                    .iter()
                    .any(|(_, hit)| *hit == Action::SpectrumPicker(setting))
            );
        }
        app.action(Action::SpectrumPicker(SpectrumSetting::Resolution));
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        assert_eq!(app.filtered_picker_options().len(), 4);
        app.action(Action::ChooseSpectrum(SpectrumChoice::Resolution(
            analysis::Resolution::Samples8192,
        )));
        app.action(Action::ChooseSpectrum(SpectrumChoice::Source(
            analysis::Source::Input,
        )));
        assert!(app.picker.is_none());
        assert_eq!(app.store.load().unwrap().spectrum, app.settings.spectrum);
        let (mut tap, stream) = analysis::channel(Rate::Hz48000);
        app.spectrum.attach(stream);
        let feed = |tap: &mut analysis::Tap| {
            for _ in 0..8192 {
                tap.push([0.25, -0.25], [0.125, -0.125]);
            }
        };
        feed(&mut tap);
        app.poll();
        app.refresh_visuals();
        assert!(app.spectrum.ready());
        assert!(app.runtime.meters.spectrum.enabled());
        assert!(app.spectrum.bins().next().unwrap().1 > -13.0);
        app.action(Action::SpectrumEnabled);
        app.poll();
        app.refresh_visuals();
        assert!(!app.spectrum.ready());
        assert!(!app.runtime.meters.spectrum.enabled());
        assert!(!app.store.load().unwrap().spectrum.enabled);
        app.action(Action::SpectrumEnabled);
        feed(&mut tap);
        app.poll();
        app.refresh_visuals();
        assert!(app.spectrum.ready());
        app.frames
            .occlusion_changed(Occlusion::Occluded, Instant::now());
        app.poll();
        app.refresh_visuals();
        assert!(!app.spectrum.ready());
        assert!(!app.runtime.meters.spectrum.enabled());
        app.frames
            .occlusion_changed(Occlusion::Visible, Instant::now());
        app.poll();
        app.refresh_visuals();
        feed(&mut tap);
        app.poll();
        app.refresh_visuals();
        assert!(app.spectrum.ready());
        assert!(Arc::ptr_eq(&playing, &app.active.correction));
        assert!(matches!(
            app.lifecycle,
            Lifecycle::Running {
                rate: Rate::Hz48000,
                ..
            }
        ));
        assert!(app.awaiting.is_none() && app.preview.is_none() && app.job.is_none());
    }
    #[test]
    fn login_preferences_reflect_system_states_without_changing_playback() {
        let (_directory, mut app) = app();
        let playing = app.active.correction.clone();
        app.lifecycle = Lifecycle::Running {
            rate: Rate::Hz44100,
            estimated_latency_ms: 30.5,
        };
        app.action(Action::BrowseGeneral);
        for status in [
            login::Status::Disabled,
            login::Status::Enabled,
            login::Status::ApprovalRequired,
            login::Status::NotFound,
            login::Status::Unbundled,
            login::Status::Unsupported,
        ] {
            app.login = Ok(status);
            app.build_scene(Size::new(880.0, 760.0), 1.0);
            assert_eq!(
                app.hits
                    .iter()
                    .any(|(_, action)| *action == Action::LaunchAtLogin),
                status.change().is_some()
            );
            assert_eq!(
                app.hits
                    .iter()
                    .any(|(_, action)| *action == Action::LoginSettings),
                status.needs_settings()
            );
            assert!(Arc::ptr_eq(&playing, &app.active.correction));
            assert!(matches!(app.lifecycle, Lifecycle::Running { .. }));
        }
        app.login = Err("Login status unavailable".into());
        app.build_scene(Size::new(880.0, 760.0), 1.0);
        assert!(
            !app.hits
                .iter()
                .any(|(_, action)| *action == Action::LaunchAtLogin)
        );
    }
    #[test]
    fn preferences_keep_os_theme_slots_and_fixed_appearance_independent() {
        let (_directory, mut app) = app();
        let dark = appearance::registry()
            .iter()
            .position(|entry| entry.id.name() == "Dracula")
            .unwrap();
        let light = appearance::registry()
            .iter()
            .position(|entry| entry.id.name() == "Breadog")
            .unwrap();
        app.action(Action::Preferences);
        app.action(Action::ChooseTheme(ThemeSlot::Dark, dark));
        app.action(Action::ChooseTheme(ThemeSlot::Light, light));
        app.system_appearance_changed(Appearance::Light);
        app.theme.advance(Instant::now() + Duration::from_secs(1));
        assert_eq!(
            app.theme.current.bg,
            appearance::registry()[light].colors.bg
        );
        app.action(Action::FollowSystem);
        app.system_appearance_changed(Appearance::Dark);
        app.theme.advance(Instant::now() + Duration::from_secs(1));
        assert_eq!(
            app.theme.current.bg,
            appearance::registry()[light].colors.bg
        );
        app.action(Action::FollowSystem);
        app.theme.advance(Instant::now() + Duration::from_secs(1));
        assert_eq!(app.theme.current.bg, appearance::registry()[dark].colors.bg);
        app.action(Action::ThemePicker(ThemeSlot::Dark));
        app.text_key(&Key::Character("rose pine".into()), Some("rose pine"));
        assert_eq!(app.filtered_picker_options().len(), 3);
        app.action(Action::ClearPickerSearch);
        app.text_key(&Key::Character("Aura".into()), Some("Aura"));
        assert_eq!(app.filtered_picker_options().len(), 1);
        app.text_key(&Key::Named(NamedKey::ArrowDown), None);
        app.text_key(&Key::Named(NamedKey::Escape), None);
        app.theme.advance(Instant::now() + Duration::from_secs(1));
        assert_eq!(app.theme.current.bg, appearance::registry()[dark].colors.bg);
        app.action(Action::ThemePicker(ThemeSlot::Dark));
        for _ in 0..appearance::registry().len() {
            app.navigate_picker(Navigation::Next);
        }
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        let last = appearance::registry().len() - 1;
        assert!(
            app.hits
                .iter()
                .any(|(_, action)| *action == Action::ChooseTheme(ThemeSlot::Dark, last))
        );
        app.text_key(&Key::Named(NamedKey::Escape), None);
        let restored = app.store.load().unwrap();
        assert_eq!(restored.appearance.dark.name(), "Dracula");
        assert_eq!(restored.appearance.light.name(), "Breadog");
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        let capture = app
            .hits
            .iter()
            .find(|(_, a)| *a == Action::Capture)
            .unwrap()
            .0;
        let playback = app
            .hits
            .iter()
            .find(|(_, a)| *a == Action::Playback)
            .unwrap()
            .0;
        assert_eq!(capture.y0, playback.y0);
        assert!(capture.x1 < playback.x0);
        assert!(!app.hits.iter().any(|(_, a)| matches!(a, Action::Select(_))));
    }
    #[test]
    fn restored_presets_regenerate_from_the_managed_recipe_without_catalog_assets() {
        let (_directory, mut app) = app();
        let index = app
            .catalog
            .as_ref()
            .unwrap()
            .search("DT 990 Pro oratory")
            .next()
            .unwrap()
            .0;
        app.action(Action::Select(index));
        wait_for_preview(&mut app);
        app.action(Action::Harman);
        wait_for_preview(&mut app);
        app.action(Action::ReplaceLoaded);
        wait_for_loaded(&mut app, "Beyerdynamic DT 990 Pro");
        app.action(Action::SavePreset);
        app.text_key(&Key::Named(NamedKey::Enter), None);
        let mut restored = App::new(app.store.clone()).unwrap();
        restored.catalog = None;
        let playing = restored.active.correction.clone();
        restored.action(Action::ChooseLength(FirLength::Taps2048));
        wait_for_preview(&mut restored);
        assert_eq!(
            restored.preview.as_ref().unwrap().0.correction.channels[0].len(),
            2048
        );
        assert!(Arc::ptr_eq(&playing, &restored.active.correction));
        restored.action(Action::ChooseAlignment(Alignment::OneKhz));
        wait_for_preview(&mut restored);
        let candidate = &restored.preview.as_ref().unwrap().0.correction;
        assert!(candidate.measured.as_ref().unwrap().at(1000.0).abs() < 0.001);
        assert!(
            matches!(candidate.origin,Origin::AutoEq {generation,..} if generation.alignment()==Alignment::OneKhz)
        );
        let previous = restored.preview.as_ref().unwrap().0.response[0].clone();
        restored
            .set_number(
                NumberField::Generation(GenerationParameter::BoostLimit),
                0.0,
            )
            .unwrap();
        wait_for_preview(&mut restored);
        let current = &restored.preview.as_ref().unwrap().0.response[0];
        assert!(
            previous
                .frequency
                .iter()
                .zip(&previous.db)
                .any(|(&hz, &db)| (current.at(hz) - db).abs() > 1.0)
        );
    }
    #[test]
    fn scrollbar_dragging_reaches_the_last_sidebar_and_menu_rows() {
        let (_directory, mut app) = app();
        app.filter.clear();
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        let bar = app
            .hits
            .iter()
            .find_map(|(_, action)| {
                if let Action::Scroll(ScrollSurface::Sidebar, bar) = action {
                    Some(*bar)
                } else {
                    None
                }
            })
            .unwrap();
        app.cursor = Point::new(bar.rail.center().x, bar.rail.y1);
        app.begin_scroll(ScrollSurface::Sidebar, bar);
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        let last = app
            .catalog
            .as_ref()
            .unwrap()
            .filtered("", app.settings.catalog_filter)
            .last()
            .unwrap()
            .0;
        assert!(
            app.hits
                .iter()
                .any(|(_, action)| *action == Action::Select(last))
        );
        app.dragging = None;
        app.action(Action::Preferences);
        app.action(Action::ThemePicker(ThemeSlot::Dark));
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        let bar = app
            .hits
            .iter()
            .find_map(|(_, action)| {
                if let Action::Scroll(ScrollSurface::Picker, bar) = action {
                    Some(*bar)
                } else {
                    None
                }
            })
            .unwrap();
        app.cursor = Point::new(bar.rail.center().x, bar.rail.y1);
        app.begin_scroll(ScrollSurface::Picker, bar);
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        assert!(app.hits.iter().any(|(_, action)| *action
            == Action::ChooseTheme(ThemeSlot::Dark, appearance::registry().len() - 1)));
    }
    #[test]
    fn restored_presets_keep_supported_target_modes_editable() {
        let (_directory, mut app) = app();
        let index = app
            .catalog
            .as_ref()
            .unwrap()
            .search("DT 990 Pro oratory")
            .next()
            .unwrap()
            .0;
        app.action(Action::Select(index));
        wait_for_preview(&mut app);
        app.action(Action::ReplaceLoaded);
        wait_for_loaded(&mut app, "Beyerdynamic DT 990 Pro");
        app.action(Action::SavePreset);
        app.text_key(&Key::Named(NamedKey::Enter), None);
        let mut restored = App::new(app.store.clone()).unwrap();
        restored.window_chrome = app.window_chrome;
        let playing = restored.active.correction.clone();
        restored.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        for action in [Action::AutoEq, Action::Harman, Action::Flat] {
            assert!(
                restored.hits.iter().any(|(_, hit)| *hit == action),
                "restoring a preset must retain each supported target control"
            );
        }
        restored.action(Action::Flat);
        wait_for_preview(&mut restored);
        assert_eq!(
            restored.preview.as_ref().unwrap().0.correction.target,
            Target::Flat
        );
        restored.action(Action::Harman);
        wait_for_preview(&mut restored);
        assert_eq!(
            restored.preview.as_ref().unwrap().0.correction.target,
            Target::Harman
        );
        assert!(Arc::ptr_eq(&playing, &restored.active.correction));
    }
    #[test]
    fn comparison_trim_text_entry_commits_clamps_and_rejects_invalid_numbers() {
        let (_directory, mut app) = app();
        app.action(Action::EditNumber(NumberField::ComparisonTrim));
        app.text_key(&Key::Character("-3.25".into()), Some("-3.25"));
        app.text_key(&Key::Named(NamedKey::Enter), None);
        assert_eq!(app.settings.controls.trim.db(), -3.25);
        assert!(app.text_focus.is_none());
        for invalid in ["NaN", "inf", "-"] {
            app.action(Action::EditNumber(NumberField::ComparisonTrim));
            app.text_key(&Key::Character(invalid.into()), Some(invalid));
            app.text_key(&Key::Named(NamedKey::Enter), None);
            assert_eq!(app.settings.controls.trim.db(), -3.25);
            assert!(matches!(app.text_focus, Some(TextFocus::Number { .. })));
            app.text_key(&Key::Named(NamedKey::Escape), None);
            assert!(app.text_focus.is_none());
        }
        app.action(Action::EditNumber(NumberField::ComparisonTrim));
        app.text_key(&Key::Character("99".into()), Some("99"));
        app.text_key(&Key::Named(NamedKey::Tab), None);
        assert_eq!(app.settings.controls.trim.db(), 12.0);
        app.action(Action::EditNumber(NumberField::ComparisonTrim));
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        assert!(app.focused_layout().is_some());
        app.text_key(&Key::Character("2.5".into()), Some("2.5"));
        app.action(Action::Search);
        assert_eq!(app.settings.controls.trim.db(), 2.5);
        assert!(matches!(app.text_focus, Some(TextFocus::Search)));
    }

    #[test]
    fn menus_use_radio_choices_and_checkboxes_for_available_graph_curves() {
        let (_directory, mut app) = app();
        let filters = app.picker_options(PickerKind::CatalogFilter);
        assert_eq!(
            filters
                .iter()
                .filter(|option| matches!(option.checks, MenuChecks::Radio(true)))
                .count(),
            1
        );
        let graph = app.picker_options(PickerKind::Graph(Point::new(400.0, 350.0)));
        assert_eq!(graph.len(), 1);
        assert!(matches!(graph[0].checks, MenuChecks::Checkbox(true)));
        app.action(graph[0].action);
        let graph = app.picker_options(PickerKind::Graph(Point::new(400.0, 350.0)));
        assert!(matches!(graph[0].checks, MenuChecks::Checkbox(false)));
        app.open_picker(PickerKind::Graph(Point::new(400.0, 350.0)));
        app.build_scene(Size::new(WIDTH, HEIGHT), 1.0);
        assert!(
            app.hits
                .iter()
                .any(|(_, action)| *action == Action::ShowLoaded)
        );
    }

    #[test]
    fn launch_starts_the_configured_route_and_reports_missing_devices() {
        let (_directory, mut app) = app();
        let endpoint = |id: &str| Endpoint {
            uid: DeviceUid::new(cpal::DeviceId::new(cpal::default_host().id(), id)),
            label: id.to_owned(),
            pair: StereoPair::default(),
        };
        app.settings.route = Some(Route {
            capture: endpoint("missing-autostart-capture"),
            playback: endpoint("missing-autostart-playback"),
        });
        app.initialize();
        assert!(matches!(app.lifecycle, Lifecycle::Starting));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(app.lifecycle, Lifecycle::Failed(_)) && Instant::now() < deadline {
            app.poll();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            matches!(&app.lifecycle, Lifecycle::Failed(reason) if reason.contains("missing-autostart-capture"))
        );
    }

    #[test]
    fn launch_without_a_catalog_starts_one_cancellable_refresh() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new(Store::at(directory.path()).unwrap()).unwrap();
        assert!(app.catalog.is_none());
        app.settings.route = None;
        app.initialize();
        assert!(app.job.is_some());
        app.action(Action::Cancel);
        drop(app);
        drop(directory);
    }
    #[test]
    fn filter_accepts_the_named_space_key() {
        let (_directory, mut app) = app();
        app.filter = TextField::new("DT".into());
        app.text_focus = Some(TextFocus::Search);
        assert!(app.text_key(&Key::Named(NamedKey::Space), Some(" ")));
        app.text_key(&Key::Character("990".into()), Some("990"));
        assert_eq!(app.filter.text(), "DT 990");
    }
    #[test]
    fn clicking_the_recommended_250_ohm_profile_prepares_a_graph_without_activation() {
        let (_directory, mut app) = app();
        let index = app
            .catalog
            .as_ref()
            .unwrap()
            .entries
            .iter()
            .position(|entry| {
                entry.name == "Beyerdynamic DT 990 Pro (250 Ohm)" && entry.recommended
            })
            .unwrap();
        let active = app.active.correction.clone();
        app.action(Action::Select(index));
        assert!(
            app.job.is_some(),
            "a profile click must start preparing its graph"
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.job.is_some() && Instant::now() < deadline {
            app.poll();
            std::thread::sleep(Duration::from_millis(2));
        }
        let (preview, _) = app
            .preview
            .as_ref()
            .expect("published profile must load without a missing CSV request");
        assert_eq!(preview.correction.name, "Beyerdynamic DT 990 Pro (250 Ohm)");
        assert!(
            preview.response[0]
                .db
                .windows(2)
                .any(|pair| (pair[0] - pair[1]).abs() > 0.01)
        );
        assert!(
            Arc::ptr_eq(&active, &app.active.correction),
            "selection must preserve loaded audio until replacement is requested"
        );
    }
}
