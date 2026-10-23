//! Cordex's preferences/browser metrics, in logical points. Only the final
//! scene transform applies the display scale.
use irresponsible::appearance::Colors;
use parley::{
    FontContext, FontWeight, LayoutContext, StyleProperty, layout::PositionedLayoutItem,
    style::FontFamily,
};
use serde::Deserialize;
use std::{borrow::Cow, collections::BTreeMap, sync::Arc};
use vello::{
    Scene,
    kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, RoundedRectRadii, Stroke},
    peniko::{Blob, Color, Fill},
};

pub const CONTROL: f64 = 24.0;
pub const ROW: f64 = 30.0;
pub const INSET: f64 = 16.0;
pub const GAP: f64 = 8.0;
pub const RADIUS: f64 = 4.0;
pub const RULE: f64 = 1.5;
pub const SIDEBAR: f64 = 282.0;
pub const ICON: f64 = 16.0;

pub fn opacity(color: Color, alpha: f32) -> Color {
    color.with_alpha(color.components[3] * alpha)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Icon {
    Headphones,
    PlugsConnected,
    Waveform,
    SlidersHorizontal,
    Backspace,
    CaretDown,
    CaretRight,
    ArrowsClockwise,
    Plus,
    Minus,
    CheckSquare,
    Square,
    RadioButton,
    Circle,
    ArrowRight,
    GearSix,
    ArrowsInSimple,
    ArrowsOutSimple,
    FloppyDisk,
    X,
    Palette,
    PencilSimple,
}

#[derive(Clone, Copy)]
pub enum MenuChecks {
    Checkbox(bool),
    Radio(bool),
}
impl MenuChecks {
    pub fn icon(self) -> Icon {
        match self {
            Self::Checkbox(true) => Icon::CheckSquare,
            Self::Checkbox(false) => Icon::Square,
            Self::Radio(true) => Icon::RadioButton,
            Self::Radio(false) => Icon::Circle,
        }
    }
}

struct IconFill {
    path: BezPath,
    alpha: f32,
}
pub struct Icons(BTreeMap<Icon, Vec<IconFill>>);
impl Icons {
    pub fn new() -> Self {
        #[derive(Deserialize)]
        struct Paths {
            tint: Option<String>,
            solid: String,
        }
        let paths: BTreeMap<Icon, Paths> =
            serde_json::from_str(include_str!("../../assets/icons.json"))
                .expect("embedded icon catalogue is valid");
        Self(
            paths
                .into_iter()
                .map(|(icon, paths)| {
                    let fills = paths
                        .tint
                        .iter()
                        .map(|path| (path.as_str(), 0.2))
                        .chain(paths.solid.lines().map(|path| (path, 1.0)))
                        .map(|(path, alpha)| IconFill {
                            path: BezPath::from_svg(path).expect("embedded Phosphor path is valid"),
                            alpha,
                        })
                        .collect();
                    (icon, fills)
                })
                .collect(),
        )
    }
}

pub fn fonts() -> FontContext {
    let mut fonts = FontContext::new();
    for data in [
        include_bytes!("../../assets/fonts/InterDisplay-Regular.ttf").as_slice(),
        include_bytes!("../../assets/fonts/InterDisplay-Medium.ttf").as_slice(),
        include_bytes!("../../assets/fonts/subset-Iosevka-Regular-Extended.ttf").as_slice(),
    ] {
        fonts
            .collection
            .register_fonts(Blob::new(Arc::new(data.to_vec())), None);
    }
    fonts
}

pub struct FieldLayout {
    layout: parley::Layout<[u8; 4]>,
    origin: Point,
    pub content: Rect,
}
impl FieldLayout {
    pub fn caret_at(&self, point: Point) -> usize {
        parley::editing::Cursor::from_point(
            &self.layout,
            (point.x - self.origin.x) as f32,
            (self.content.center().y - self.origin.y) as f32,
        )
        .index()
    }
    pub fn word_at(&self, point: Point) -> std::ops::Range<usize> {
        parley::editing::Selection::word_from_point(
            &self.layout,
            (point.x - self.origin.x) as f32,
            (self.content.center().y - self.origin.y) as f32,
        )
        .text_range()
    }
}
pub const LIST_ROW_HEIGHT: f64 = 48.0;
pub fn list_text_regions(bounds: Rect) -> (Rect, Rect) {
    (
        Rect::new(
            bounds.x0 + INSET,
            bounds.y0 + 8.0,
            bounds.x1 - INSET,
            bounds.y0 + 24.0,
        ),
        Rect::new(
            bounds.x0 + INSET,
            bounds.y0 + 26.0,
            bounds.x1 - INSET,
            bounds.y1 - 8.0,
        ),
    )
}

#[derive(Clone, Copy)]
pub enum Face {
    Title,
    Stat,
    Label,
    Value,
    Small,
    SmallValue,
}
impl Face {
    fn size(self) -> f32 {
        match self {
            Self::Title | Self::Label | Self::Value => 12.0,
            Self::Stat => 11.5,
            Self::Small | Self::SmallValue => 10.0,
        }
    }
    fn family(self) -> &'static str {
        match self {
            Self::Title | Self::Stat | Self::Label | Self::Small => "Inter Display",
            Self::Value | Self::SmallValue => "Iosevka",
        }
    }
    fn tracking(self) -> f32 {
        match self {
            Self::Value | Self::SmallValue => -self.size() * 0.1,
            Self::Title | Self::Stat | Self::Label | Self::Small => 0.0,
        }
    }
}
#[derive(Clone, Copy)]
pub enum Align {
    Start,
    Center,
    End,
}
#[derive(Clone, Copy)]
enum State {
    Active,
    Pressed,
    Hovered,
    Idle,
}

pub struct Painter<'a, A> {
    pub colors: Colors,
    pub scene: Scene,
    pub hits: Vec<(Rect, A)>,
    fonts: &'a mut FontContext,
    layout: &'a mut LayoutContext,
    icons: &'a Icons,
    scale: f64,
    cursor: Point,
    pressed: Option<A>,
}
impl<'a, A: Copy + PartialEq> Painter<'a, A> {
    pub fn new(
        fonts: &'a mut FontContext,
        layout: &'a mut LayoutContext,
        icons: &'a Icons,
        scale: f64,
        cursor: Point,
        pressed: Option<A>,
        colors: Colors,
    ) -> Self {
        Self {
            colors,
            scene: Scene::new(),
            hits: Vec::new(),
            fonts,
            layout,
            icons,
            scale,
            cursor,
            pressed,
        }
    }
    pub fn rect(&mut self, rect: Rect, color: Color) {
        self.rounded(rect, 0.0, color);
    }
    pub fn rounded(&mut self, rect: Rect, radius: impl Into<RoundedRectRadii>, color: Color) {
        self.scene.fill(
            Fill::NonZero,
            Affine::scale(self.scale),
            color,
            None,
            &RoundedRect::from_rect(rect, radius),
        );
    }
    pub fn outline(
        &mut self,
        bounds: Rect,
        radius: impl Into<RoundedRectRadii>,
        width: f64,
        color: Color,
    ) {
        self.scene.stroke(
            &Stroke::new(width),
            Affine::scale(self.scale),
            color,
            None,
            &RoundedRect::from_rect(bounds, radius),
        );
    }
    pub fn line(&mut self, a: (f64, f64), b: (f64, f64), color: Color, width: f64) {
        self.scene.stroke(
            &Stroke::new(width),
            Affine::scale(self.scale),
            color,
            None,
            &Line::new(a, b),
        );
    }
    pub fn icon(&mut self, icon: Icon, bounds: Rect, color: Color) {
        let transform = Affine::scale(self.scale)
            * Affine::translate((bounds.x0, bounds.y0))
            * Affine::scale(bounds.width() / 256.0);
        for fill in &self.icons.0[&icon] {
            self.scene.fill(
                Fill::NonZero,
                transform,
                opacity(color, fill.alpha),
                None,
                &fill.path,
            );
        }
    }
    fn shape(&mut self, text: &str, face: Face) -> parley::Layout<[u8; 4]> {
        let mut builder = self.layout.ranged_builder(self.fonts, text, 1.0, true);
        builder.push_default(StyleProperty::FontSize(face.size()));
        builder.push_default(StyleProperty::FontWeight(if matches!(face, Face::Title) {
            FontWeight::MEDIUM
        } else {
            FontWeight::NORMAL
        }));
        builder.push_default(StyleProperty::FontFamily(FontFamily::Source(
            Cow::Borrowed(face.family()),
        )));
        builder.push_default(StyleProperty::LetterSpacing(face.tracking()));
        let mut layout = builder.build(text);
        layout.break_all_lines(None);
        layout
    }
    pub fn advance(&mut self, text: &str, face: Face) -> f64 {
        f64::from(self.shape(text, face).width())
    }
    fn fitted(&mut self, text: &str, room: f64, face: Face) -> parley::Layout<[u8; 4]> {
        let layout = self.shape(text, face);
        if f64::from(layout.width()) <= room {
            return layout;
        }
        let chars: Vec<_> = text.chars().collect();
        let mut fits = 0;
        let mut over = chars.len();
        let cut = |kept| format!("{}...", chars[..kept].iter().collect::<String>().trim_end());
        while over - fits > 1 {
            let mid = (fits + over) / 2;
            if self.advance(&cut(mid), face) <= room {
                fits = mid;
            } else {
                over = mid;
            }
        }
        self.shape(&cut(fits), face)
    }
    pub fn text(&mut self, text: &str, bounds: Rect, face: Face, align: Align, color: Color) {
        let layout = self.fitted(text, bounds.width(), face);
        let x = match align {
            Align::Start => bounds.x0,
            Align::Center => bounds.center().x - f64::from(layout.width()) * 0.5,
            Align::End => bounds.x1 - f64::from(layout.width()),
        };
        let Some(line) = layout.lines().next() else {
            return;
        };
        let baseline = bounds.center().y + f64::from(face.size()) * 0.325;
        let y = baseline - f64::from(line.metrics().baseline);
        self.scene
            .push_clip_layer(Fill::NonZero, Affine::scale(self.scale), &bounds);
        self.draw_run(&layout, Point::new(x, y), color);
        self.scene.pop_layer();
    }
    fn draw_run(&mut self, layout: &parley::Layout<[u8; 4]>, origin: Point, color: Color) {
        let Some(line) = layout.lines().next() else {
            return;
        };
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(glyphs) = item else {
                continue;
            };
            let run = glyphs.run();
            self.scene
                .draw_glyphs(run.font())
                .font_size(run.font_size())
                .normalized_coords(run.normalized_coords())
                .transform(Affine::scale(self.scale) * Affine::translate((origin.x, origin.y)))
                .brush(color)
                .draw(
                    Fill::NonZero,
                    glyphs.positioned_glyphs().map(|g| vello::Glyph {
                        id: g.id,
                        x: g.x,
                        y: g.y,
                    }),
                );
        }
    }
    fn state(&self, bounds: Rect, action: A, selected: bool) -> State {
        if selected {
            State::Active
        } else if bounds.contains(self.cursor) {
            if self.pressed == Some(action) {
                State::Pressed
            } else {
                State::Hovered
            }
        } else {
            State::Idle
        }
    }
    fn plate(&mut self, bounds: Rect, action: A, selected: bool, enabled: bool) -> Color {
        let (plate, ink) = match self.state(bounds, action, selected) {
            State::Active => (self.colors.fg, self.colors.bg),
            State::Pressed => (opacity(self.colors.fg, 0.3), self.colors.fg),
            State::Hovered => (self.colors.ghost_intense, self.colors.fg),
            State::Idle => (self.colors.ghost, self.colors.fg),
        };
        self.rounded(
            bounds,
            RADIUS,
            opacity(plate, if enabled { 1.0 } else { 0.4 }),
        );
        if enabled {
            self.hits.push((bounds, action));
        }
        opacity(ink, if enabled { 1.0 } else { 0.4 })
    }
    pub fn button(&mut self, x: f64, y: f64, text: &str, action: A, enabled: bool) -> Rect {
        let bounds = Rect::new(
            x,
            y,
            x + self.advance(text, Face::Label).ceil() + INSET,
            y + CONTROL,
        );
        let ink = self.plate(bounds, action, false, enabled);
        self.text(text, bounds, Face::Label, Align::Center, ink);
        bounds
    }
    pub fn chip(
        &mut self,
        x: f64,
        y: f64,
        text: &str,
        action: A,
        selected: bool,
        enabled: bool,
    ) -> Rect {
        let bounds = Rect::new(
            x,
            y,
            x + self.advance(text, Face::Label).ceil() + INSET,
            y + CONTROL,
        );
        let ink = self.plate(bounds, action, selected, enabled);
        self.text(text, bounds, Face::Label, Align::Center, ink);
        bounds
    }
    pub fn legend_chip(
        &mut self,
        at: Point,
        text: &str,
        action: A,
        selected: bool,
        enabled: bool,
        color: Color,
    ) -> Rect {
        let Point { x, y } = at;
        let bounds = Rect::new(
            x,
            y,
            x + self.advance(text, Face::Label).ceil() + 32.0,
            y + CONTROL,
        );
        let ink = self.plate(bounds, action, selected, enabled);
        let swatch = Rect::new(x + 8.0, y + 8.0, x + 16.0, y + 16.0);
        self.rounded(swatch, 2.0, opacity(color, if enabled { 1.0 } else { 0.4 }));
        self.outline(swatch, 2.0, 0.75, self.colors.bg);
        self.text(
            text,
            Rect::new(x + 22.0, y, bounds.x1 - 6.0, bounds.y1),
            Face::Label,
            Align::Start,
            ink,
        );
        bounds
    }
    pub fn ghost_icon(&mut self, bounds: Rect, icon: Icon, action: A, enabled: bool) {
        self.ghost_icon_ink(
            bounds,
            icon,
            action,
            (self.colors.secondary, self.colors.fg),
            enabled,
        );
    }
    fn ghost_icon_ink(
        &mut self,
        bounds: Rect,
        icon: Icon,
        action: A,
        ink: (Color, Color),
        enabled: bool,
    ) {
        let state = self.state(bounds, action, false);
        if enabled && !matches!(state, State::Idle) {
            self.rounded(
                bounds,
                RADIUS,
                if matches!(state, State::Pressed) {
                    opacity(ink.1, 0.3)
                } else {
                    opacity(ink.1, 0.2)
                },
            );
        }
        self.icon(
            icon,
            Rect::from_center_size(bounds.center(), (ICON, ICON)),
            opacity(
                if matches!(state, State::Idle) {
                    ink.0
                } else {
                    ink.1
                },
                if enabled { 1.0 } else { 0.4 },
            ),
        );
        if enabled {
            self.hits.push((bounds, action));
        }
    }
    pub fn titlebar_selector(&mut self, bounds: Rect, text: &str, action: A, open: bool) {
        let ink = match self.state(bounds, action, open) {
            State::Active => {
                self.rounded(bounds, 4.0, self.colors.fg);
                self.colors.bg
            }
            State::Pressed => {
                self.rounded(bounds, 4.0, opacity(self.colors.fg, 0.3));
                self.colors.fg
            }
            State::Hovered => {
                self.rounded(bounds, 4.0, self.colors.ghost_intense);
                self.colors.fg
            }
            State::Idle => self.colors.secondary,
        };
        self.text(
            text,
            Rect::new(bounds.x0 + GAP, bounds.y0, bounds.x1 - 24.0, bounds.y1),
            Face::Label,
            Align::Start,
            ink,
        );
        self.icon(
            Icon::CaretDown,
            Rect::from_center_size((bounds.x1 - 12.0, bounds.center().y), (ICON, ICON)),
            ink,
        );
        self.hits.push((bounds, action));
    }
    pub fn tab(&mut self, bounds: Rect, icon: Icon, text: &str, action: A, selected: bool) {
        let ink = match self.state(bounds, action, selected) {
            State::Active => {
                self.rect(bounds, self.colors.fg);
                self.colors.bg
            }
            State::Pressed => {
                self.rect(bounds, opacity(self.colors.fg, 0.3));
                self.colors.fg
            }
            State::Hovered => {
                self.rect(bounds, self.colors.ghost_intense);
                self.colors.fg
            }
            State::Idle => self.colors.secondary,
        };
        self.icon(
            icon,
            Rect::from_origin_size((bounds.center().x - 8.0, bounds.y0 + 9.0), (16.0, 16.0)),
            ink,
        );
        self.text(
            text,
            Rect::new(bounds.x0, bounds.y0 + 28.0, bounds.x1, bounds.y0 + 44.0),
            Face::Small,
            Align::Center,
            ink,
        );
        self.hits.push((bounds, action));
    }
    pub fn scrollbar(&mut self, bar: super::scrollbar::Scrollbar, action: A) {
        self.rounded(
            bar.thumb,
            2.0,
            opacity(
                self.colors.fg,
                if bar.rail.contains(self.cursor) {
                    0.6
                } else {
                    0.3
                },
            ),
        );
        self.hits.push((bar.rail, action));
    }
    pub fn dropdown(&mut self, bounds: Rect, text: &str, action: A, enabled: bool) {
        let ink = self.plate(bounds, action, false, enabled);
        self.text(
            text,
            Rect::new(bounds.x0 + GAP, bounds.y0, bounds.x1 - 24.0, bounds.y1),
            Face::Label,
            Align::Start,
            ink,
        );
        self.icon(
            Icon::CaretDown,
            Rect::from_center_size((bounds.x1 - 12.0, bounds.center().y), (ICON, ICON)),
            opacity(self.colors.secondary, if enabled { 1.0 } else { 0.4 }),
        );
    }
    pub fn section(&mut self, bounds: Rect, icon: Icon, text: &str, ink: Color) {
        self.icon(
            icon,
            Rect::new(
                bounds.x0 + 14.0,
                bounds.y0 + 4.0,
                bounds.x0 + 30.0,
                bounds.y0 + 20.0,
            ),
            ink,
        );
        self.text(
            text,
            Rect::new(bounds.x0 + 39.0, bounds.y0, bounds.x1 - INSET, bounds.y1),
            Face::Label,
            Align::Start,
            ink,
        );
    }
    fn list_ground(&mut self, bounds: Rect, action: A, selected: bool) -> (Color, Color) {
        match self.state(bounds, action, selected) {
            State::Active => {
                self.rect(bounds, self.colors.fg);
                (self.colors.bg, self.colors.bg)
            }
            State::Pressed => {
                self.rect(bounds, opacity(self.colors.fg, 0.3));
                (self.colors.fg, self.colors.secondary)
            }
            State::Hovered => {
                self.rect(bounds, self.colors.ghost_intense);
                (self.colors.fg, self.colors.secondary)
            }
            State::Idle => (self.colors.fg, self.colors.secondary),
        }
    }
    pub fn list_item(
        &mut self,
        bounds: Rect,
        text: &str,
        action: A,
        selected: bool,
        trailing: &[(Icon, A)],
    ) {
        let (ink, _) = self.list_ground(bounds, action, selected);
        let mut label = Rect::new(bounds.x0 + INSET, bounds.y0, bounds.x1 - INSET, bounds.y1);
        self.hits.push((bounds, action));
        for (index, &(icon, action)) in trailing.iter().rev().enumerate() {
            let right = bounds.x1 - INSET - index as f64 * (CONTROL + 4.0);
            let control = Rect::from_center_size(
                (right - CONTROL * 0.5, bounds.center().y),
                (CONTROL, CONTROL),
            );
            label.x1 = control.x0 - GAP;
            self.ghost_icon_ink(control, icon, action, (ink, ink), true);
        }
        self.text(text, label, Face::Label, Align::Start, ink);
    }
    pub fn list_row(&mut self, bounds: Rect, text: &str, detail: &str, action: A, selected: bool) {
        let (ink, secondary) = self.list_ground(bounds, action, selected);
        let (label, provider) = list_text_regions(bounds);
        self.text(text, label, Face::Label, Align::Start, ink);
        self.text(detail, provider, Face::Small, Align::Start, secondary);
        self.hits.push((bounds, action));
    }
    pub fn field(
        &mut self,
        bounds: Rect,
        field: &super::text_field::TextField,
        tally: &str,
        actions: [A; 2],
        focused: bool,
        placeholder: &str,
    ) -> FieldLayout {
        self.rounded(bounds, RADIUS, self.colors.ghost);
        self.hits.push((bounds, actions[0]));
        if focused {
            self.outline(bounds, RADIUS, 2.0, opacity(self.colors.fg, 0.3));
        }
        let query = field.text();
        let tally_width = self.advance(tally, Face::Small);
        let reserved = if query.is_empty() {
            GAP
        } else {
            32.0 + GAP + tally_width + GAP
        };
        let content = Rect::new(bounds.x0 + GAP, bounds.y0, bounds.x1 - reserved, bounds.y1);
        let editing = self.edit_text(content, field, Face::Label, Align::Start, focused);
        if query.is_empty() {
            self.text(
                placeholder,
                content,
                Face::Label,
                Align::Start,
                opacity(self.colors.secondary, 0.7),
            );
        }
        if !query.is_empty() {
            let clear = Rect::new(bounds.x1 - 26.0, bounds.y0, bounds.x1 - 2.0, bounds.y1);
            self.icon(
                Icon::Backspace,
                Rect::from_center_size(clear.center(), (ICON, ICON)),
                if clear.contains(self.cursor) {
                    self.colors.fg
                } else {
                    self.colors.secondary
                },
            );
            self.hits.push((clear, actions[1]));
            self.text(
                tally,
                Rect::new(content.x1 + GAP, bounds.y0, clear.x0 - 6.0, bounds.y1),
                Face::Small,
                Align::End,
                opacity(self.colors.secondary, 0.7),
            );
        }
        editing
    }
    pub fn entry(
        &mut self,
        bounds: Rect,
        field: &super::text_field::TextField,
        action: A,
    ) -> FieldLayout {
        self.rounded(bounds, RADIUS, self.colors.ghost);
        self.outline(bounds, RADIUS, 2.0, opacity(self.colors.fg, 0.3));
        self.hits.push((bounds, action));
        self.edit_text(
            Rect::new(bounds.x0 + 8.0, bounds.y0, bounds.x1 - 8.0, bounds.y1),
            field,
            Face::Label,
            Align::Start,
            true,
        )
    }
    fn edit_text(
        &mut self,
        content: Rect,
        field: &super::text_field::TextField,
        face: Face,
        align: Align,
        focused: bool,
    ) -> FieldLayout {
        use parley::{
            editing::{Cursor, Selection},
            layout::Affinity,
        };
        let query = field.text();
        let layout = self.shape(query, face);
        let caret = Cursor::from_byte_index(&layout, field.caret(), Affinity::Downstream)
            .geometry(&layout, 1.0);
        let offset = (caret.x0 - content.width() + 2.0).max(0.0);
        let baseline = layout
            .lines()
            .next()
            .map_or(0.0, |line| f64::from(line.metrics().baseline));
        let text_x = match align {
            Align::Start => content.x0,
            Align::Center => {
                content.x0 + ((content.width() - f64::from(layout.width())) * 0.5).max(0.0)
            }
            Align::End => content.x0 + (content.width() - f64::from(layout.width())).max(0.0),
        };
        let origin = Point::new(text_x - offset, content.center().y + 3.9 - baseline);
        self.scene
            .push_clip_layer(Fill::NonZero, Affine::scale(self.scale), &content);
        {
            if focused {
                let selection = Selection::new(
                    Cursor::from_byte_index(&layout, field.anchor(), Affinity::Downstream),
                    Cursor::from_byte_index(&layout, field.caret(), Affinity::Downstream),
                );
                for (rect, _) in selection.geometry(&layout) {
                    self.rect(
                        Rect::new(
                            origin.x + rect.x0,
                            content.y0 + 4.0,
                            origin.x + rect.x1,
                            content.y1 - 4.0,
                        ),
                        opacity(self.colors.fg, 0.3),
                    );
                }
            }
            self.draw_run(&layout, origin, self.colors.fg);
        }
        if focused && field.caret_visible() {
            let x = origin.x + caret.x0;
            self.line(
                (x, content.y0 + 5.0),
                (x, content.y1 - 5.0),
                self.colors.cursor,
                1.25 * 12.0 / 13.0,
            );
        }
        self.scene.pop_layer();
        FieldLayout {
            layout,
            origin,
            content,
        }
    }
    pub fn toggle(&mut self, row: Rect, text: &str, action: A, on: bool, enabled: bool) {
        let alpha = if enabled { 1.0 } else { 0.4 };
        let plate = Rect::from_center_size((row.x1 - 14.0, row.center().y), (28.0, 16.0));
        self.text(
            text,
            Rect::new(row.x0, row.y0, plate.x0 - GAP, row.y1),
            Face::Label,
            Align::Start,
            opacity(self.colors.fg, alpha),
        );
        self.rounded(
            plate,
            8.0,
            opacity(
                if on {
                    self.colors.fg
                } else if enabled && row.contains(self.cursor) {
                    self.colors.ghost_intense
                } else {
                    self.colors.ghost
                },
                alpha,
            ),
        );
        let center = (
            if on { plate.x1 - 8.0 } else { plate.x0 + 8.0 },
            plate.center().y,
        );
        self.scene.fill(
            Fill::NonZero,
            Affine::scale(self.scale),
            opacity(
                if on {
                    self.colors.bg
                } else {
                    self.colors.secondary
                },
                alpha,
            ),
            None,
            &Circle::new(center, 6.0),
        );
        if enabled {
            self.hits.push((row, action));
        }
    }
    pub fn stepper(
        &mut self,
        bounds: Rect,
        text: &str,
        actions: [A; 3],
        editing: Option<&super::text_field::TextField>,
    ) -> Option<FieldLayout> {
        self.rounded(bounds, RADIUS, self.colors.ghost);
        let down = Rect::new(
            bounds.x0 + 2.0,
            bounds.y0 + 2.0,
            bounds.x0 + 22.0,
            bounds.y1 - 2.0,
        );
        let up = Rect::new(
            bounds.x1 - 22.0,
            bounds.y0 + 2.0,
            bounds.x1 - 2.0,
            bounds.y1 - 2.0,
        );
        for (rect, icon, action) in [
            (down, Icon::Minus, actions[0]),
            (up, Icon::Plus, actions[1]),
        ] {
            if rect.contains(self.cursor) {
                self.rounded(
                    rect,
                    RADIUS,
                    if self.pressed == Some(action) {
                        opacity(self.colors.fg, 0.3)
                    } else {
                        self.colors.ghost_intense
                    },
                );
            }
            self.icon(
                icon,
                Rect::from_center_size(rect.center(), (ICON, ICON)),
                if rect.contains(self.cursor) {
                    self.colors.fg
                } else {
                    self.colors.secondary
                },
            );
            self.hits.push((rect, action));
        }
        let content = Rect::new(down.x1 + 4.0, bounds.y0, up.x0 - 4.0, bounds.y1);
        self.hits.push((content, actions[2]));
        if let Some(field) = editing {
            self.rounded(content, 3.0, self.colors.body);
            self.outline(content, 3.0, 2.0, opacity(self.colors.fg, 0.3));
            Some(self.edit_text(content.inset(-2.0), field, Face::Value, Align::Center, true))
        } else {
            self.text(text, content, Face::Value, Align::Center, self.colors.fg);
            None
        }
    }
    pub fn value_bar(&mut self, bounds: Rect, label: &str, value: &str, fraction: f64, action: A) {
        self.rounded(bounds, RADIUS, self.colors.ghost);
        self.scene.push_clip_layer(
            Fill::NonZero,
            Affine::scale(self.scale),
            &RoundedRect::from_rect(bounds, RADIUS),
        );
        self.rect(
            Rect::new(
                bounds.x0,
                bounds.y0,
                bounds.x0 + bounds.width() * fraction,
                bounds.y1,
            ),
            if bounds.contains(self.cursor) {
                self.colors.secondary
            } else {
                self.colors.ghost_intense
            },
        );
        self.scene.pop_layer();
        self.text(
            label,
            Rect::new(bounds.x0 + GAP, bounds.y0, bounds.center().x, bounds.y1),
            Face::Label,
            Align::Start,
            self.colors.fg,
        );
        self.text(
            value,
            Rect::new(bounds.center().x, bounds.y0, bounds.x1 - GAP, bounds.y1),
            Face::Value,
            Align::End,
            self.colors.fg,
        );
        self.hits.push((bounds, action));
    }
    pub fn curve(
        &mut self,
        curve: &irresponsible::correction::Curve,
        plot: Rect,
        color: Color,
        width: f64,
    ) {
        let max_hz = *curve.frequency.last().expect("validated curve");
        let mut path = BezPath::new();
        for (i, (&hz, &db)) in curve.frequency.iter().zip(&curve.db).enumerate() {
            let x = plot.x0 + f64::from((hz / 20.0).ln() / (max_hz / 20.0).ln()) * plot.width();
            let y = plot.y0 + f64::from((24.0 - db) / 48.0) * plot.height();
            if i == 0 {
                path.move_to((x, y));
            } else {
                path.line_to((x, y));
            }
        }
        self.scene
            .push_clip_layer(Fill::NonZero, Affine::scale(self.scale), &plot);
        self.scene.stroke(
            &Stroke::new(width),
            Affine::scale(self.scale),
            color,
            None,
            &path,
        );
        self.scene.pop_layer();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collapse_chevrons_are_regular_single_weight_paths() {
        let icons = Icons::new();
        assert_eq!(icons.0[&Icon::CaretDown].len(), 1);
        assert_eq!(icons.0[&Icon::CaretRight].len(), 1);
    }
    #[test]
    fn profile_text_has_equal_padding_and_the_filter_maps_visible_text_to_selection() {
        let bounds = Rect::new(0.0, 0.0, 282.0, LIST_ROW_HEIGHT);
        let (name, provider) = list_text_regions(bounds);
        assert_eq!(name.y0 - bounds.y0, bounds.y1 - provider.y1);
        assert_eq!(provider.y0 - name.y1, 2.0);
        let mut fonts = fonts();
        let mut context = LayoutContext::new();
        let icons = Icons::new();
        let mut painter = Painter::new(
            &mut fonts,
            &mut context,
            &icons,
            1.0,
            Point::ZERO,
            None::<u8>,
            irresponsible::appearance::palette(
                &irresponsible::appearance::ThemePreferences::default().dark,
            ),
        );
        let mut field = super::super::text_field::TextField::new("DT 990 Pro".into());
        field.place(0, false);
        let draw = painter.field(
            Rect::new(16.0, 0.0, 266.0, 24.0),
            &field,
            "18",
            [1, 2],
            true,
            "Search headphones...",
        );
        let x = draw.content.x0 + painter.advance("DT 9", Face::Label);
        let word = draw.word_at(Point::new(x, 12.0));
        assert_eq!(&field.text()[word], "990");
        let caret = draw.caret_at(Point::new(draw.content.x0, 12.0));
        assert_eq!(caret, 0);
    }
    #[test]
    fn embedded_figures_and_controls_match_cordex_metrics() {
        let mut fonts = fonts();
        let mut layout = LayoutContext::new();
        let icons = Icons::new();
        let mut painter = Painter::new(
            &mut fonts,
            &mut layout,
            &icons,
            2.0,
            Point::new(-1.0, -1.0),
            None::<u8>,
            irresponsible::appearance::palette(
                &irresponsible::appearance::ThemePreferences::default().dark,
            ),
        );
        assert!((painter.advance("000.0", Face::Value) - 30.0).abs() < 0.05);
        let label = painter
            .advance("Replace loaded profile", Face::Label)
            .ceil();
        let bounds = painter.button(16.0, 40.0, "Replace loaded profile", 1, true);
        assert_eq!(bounds.height(), 24.0);
        assert_eq!(bounds.width(), label + 16.0);
        assert_eq!(painter.hits[0], (bounds, 1));
    }
}
