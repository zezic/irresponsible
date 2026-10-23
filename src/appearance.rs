use crate::error::{Result, invalid};
use serde::{Deserialize, Serialize};
use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};
use unicode_normalization::UnicodeNormalization;
use vello::peniko::Color;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Appearance {
    #[default]
    Dark,
    Light,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeMode {
    #[default]
    System,
    Fixed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeSlot {
    Light,
    Dark,
    Fixed,
}
impl ThemeSlot {
    pub fn label(self) -> &'static str {
        match self {
            Self::Light => "Light appearance",
            Self::Dark => "Dark appearance",
            Self::Fixed => "Theme",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowMode {
    #[default]
    Full,
    Compact,
}
impl WindowMode {
    #[must_use]
    pub fn toggle(self) -> Self {
        match self {
            Self::Full => Self::Compact,
            Self::Compact => Self::Full,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ThemeId(String);
impl ThemeId {
    pub fn name(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for ThemeId {
    type Error = crate::error::Error;
    fn try_from(name: String) -> Result<Self> {
        let name: String = name.nfc().collect();
        if registry().iter().any(|entry| entry.id.name() == name) {
            Ok(Self(name))
        } else {
            Err(invalid(format!("Unknown theme: {name}")))
        }
    }
}
impl From<ThemeId> for String {
    fn from(id: ThemeId) -> Self {
        id.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemePreferences {
    pub mode: ThemeMode,
    pub light: ThemeId,
    pub dark: ThemeId,
    pub fixed: ThemeId,
}
impl Default for ThemePreferences {
    fn default() -> Self {
        Self {
            mode: ThemeMode::System,
            light: ThemeId("Tomorrow".into()),
            dark: ThemeId("Aura".into()),
            fixed: ThemeId("Aura".into()),
        }
    }
}
impl ThemePreferences {
    pub fn theme(&self, slot: ThemeSlot) -> &ThemeId {
        match slot {
            ThemeSlot::Light => &self.light,
            ThemeSlot::Dark => &self.dark,
            ThemeSlot::Fixed => &self.fixed,
        }
    }
    pub fn set_theme(&mut self, slot: ThemeSlot, id: ThemeId) {
        match slot {
            ThemeSlot::Light => self.light = id,
            ThemeSlot::Dark => self.dark = id,
            ThemeSlot::Fixed => self.fixed = id,
        }
    }
    pub fn selected(&self, appearance: Appearance) -> &ThemeId {
        let slot = match (self.mode, appearance) {
            (ThemeMode::Fixed, _) => ThemeSlot::Fixed,
            (_, Appearance::Light) => ThemeSlot::Light,
            (_, Appearance::Dark) => ThemeSlot::Dark,
        };
        self.theme(slot)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Colors {
    pub basic: [Color; 16],
    pub bg: Color,
    pub fg: Color,
    pub secondary: Color,
    pub ghost: Color,
    pub ghost_intense: Color,
    pub body: Color,
    pub cursor: Color,
}
impl Colors {
    pub fn pane(self) -> Color {
        mix(self.bg, self.body, 0.5)
    }
    pub fn cast(self) -> Appearance {
        let luma =
            |c: Color| 0.299 * c.components[0] + 0.587 * c.components[1] + 0.114 * c.components[2];
        if luma(self.bg) < luma(self.fg) {
            Appearance::Dark
        } else {
            Appearance::Light
        }
    }
    fn crossfade(self, next: Self, t: f32) -> Self {
        Self {
            basic: std::array::from_fn(|i| mix(self.basic[i], next.basic[i], t)),
            bg: mix(self.bg, next.bg, t),
            fg: mix(self.fg, next.fg, t),
            secondary: mix(self.secondary, next.secondary, t),
            ghost: mix(self.ghost, next.ghost, t),
            ghost_intense: mix(self.ghost_intense, next.ghost_intense, t),
            body: mix(self.body, next.body, t),
            cursor: mix(self.cursor, next.cursor, t),
        }
    }
}
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    Color::new(std::array::from_fn(|i| {
        a.components[i] + (b.components[i] - a.components[i]) * t
    }))
}
pub struct ThemeEntry {
    pub id: ThemeId,
    pub colors: Colors,
}
pub fn registry() -> &'static [ThemeEntry] {
    static REGISTRY: OnceLock<Vec<ThemeEntry>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let documents: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../assets/themes.json"))
                .expect("embedded Gogh palettes are valid");
        let mut entries: Vec<_> = documents
            .iter()
            .map(|doc| {
                let color = |key: &str| {
                    let hex = doc[key]
                        .as_str()
                        .expect("embedded palette color")
                        .trim_start_matches('#');
                    let rgb = u32::from_str_radix(hex, 16).expect("embedded hexadecimal color");
                    Color::from_rgb8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
                };
                let bg = color("background");
                let fg = color("foreground");
                let body = mix(bg, color("color_09"), 0.1);
                ThemeEntry {
                    id: ThemeId(doc["name"].as_str().expect("embedded theme name").into()),
                    colors: Colors {
                        basic: std::array::from_fn(|i| color(&format!("color_{:02}", i + 1))),
                        bg,
                        fg,
                        body,
                        secondary: mix(bg, fg, 0.5),
                        ghost: mix(body, fg, 0.1),
                        ghost_intense: mix(body, fg, 0.2),
                        cursor: color("cursor"),
                    },
                }
            })
            .collect();
        entries.sort_by(|a, b| {
            a.colors
                .cast()
                .cmp(&b.colors.cast())
                .then_with(|| a.id.name().cmp(b.id.name()))
        });
        entries
    })
}
pub fn palette(id: &ThemeId) -> Colors {
    registry()
        .iter()
        .find(|entry| entry.id == *id)
        .expect("validated theme is in registry")
        .colors
}
pub struct Theme {
    pub current: Colors,
    previous: Colors,
    next: Colors,
    selected: ThemeId,
    transition: Option<Instant>,
}
impl Theme {
    pub fn new(id: &ThemeId) -> Self {
        let colors = palette(id);
        Self {
            current: colors,
            previous: colors,
            next: colors,
            selected: id.clone(),
            transition: None,
        }
    }
    pub fn show(&mut self, id: &ThemeId) {
        if *id != self.selected {
            self.previous = self.current;
            self.next = palette(id);
            self.selected = id.clone();
            self.transition = Some(Instant::now());
        }
    }
    pub fn advance(&mut self, now: Instant) -> bool {
        let Some(start) = self.transition else {
            return false;
        };
        let t = (now.duration_since(start).as_secs_f32()
            / Duration::from_millis(200).as_secs_f32())
        .min(1.0);
        self.current = self.previous.crossfade(
            self.next,
            if t < 0.5 {
                2.0 * t * t
            } else {
                1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
            },
        );
        if t >= 1.0 {
            self.current = self.next;
            self.transition = None;
        }
        true
    }
    pub fn is_transitioning(&self) -> bool {
        self.transition.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn system_slots_and_fixed_theme_are_independent_and_survive_storage() {
        let mut preferences = ThemePreferences::default();
        let dark = ThemeId::try_from("Dracula".to_owned()).unwrap();
        let light = ThemeId::try_from("Breadog".to_owned()).unwrap();
        preferences.set_theme(ThemeSlot::Dark, dark.clone());
        preferences.set_theme(ThemeSlot::Light, light.clone());
        preferences.mode = ThemeMode::Fixed;
        preferences.set_theme(ThemeSlot::Fixed, light.clone());
        let mut restored: ThemePreferences =
            serde_json::from_slice(&serde_json::to_vec(&preferences).unwrap()).unwrap();
        assert_eq!(restored.selected(Appearance::Dark), &light);
        restored.mode = ThemeMode::System;
        assert_eq!(restored.selected(Appearance::Dark), &dark);
        assert_eq!(restored.selected(Appearance::Light), &light);
        assert!(ThemeId::try_from("Unknown palette".to_owned()).is_err());
        assert_eq!(
            ThemeId::try_from("Rose\u{301} Pine".to_owned())
                .unwrap()
                .name(),
            "Rosé Pine"
        );
    }
    #[test]
    fn registry_and_crossfade_use_cordex_surface_formulas() {
        let prefs = ThemePreferences::default();
        let mut theme = Theme::new(&prefs.dark);
        let aura = theme.current;
        assert_eq!(aura.body, mix(aura.bg, aura.basic[8], 0.1));
        assert!(
            registry()
                .windows(2)
                .all(|pair| pair[0].colors.cast() <= pair[1].colors.cast())
        );
        theme.show(&prefs.light);
        let now = Instant::now() + Duration::from_secs(1);
        assert!(theme.advance(now));
        assert!(!theme.is_transitioning());
        assert_eq!(theme.current.bg, palette(&prefs.light).bg);
    }
}
