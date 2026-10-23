use super::chrome::{Align, Face, Painter, RADIUS, opacity};
use super::{Action, GenerationParameter, NumberField, SpectrumSetting, ThemeSlot, ValueBar};
use std::time::{Duration, Instant};
use vello::kurbo::{Point, Rect, Size};

pub(super) const DELAY: Duration = Duration::from_millis(400);

impl Action {
    fn help(self) -> Option<&'static str> {
        Some(match self {
            Self::GenerationLength => "Filter length. More taps resolve bass more precisely.",
            Self::GenerationAlignment => {
                "Matches measurement and target levels before calculating correction."
            }
            Self::GenerationNormalization => {
                "Raw keeps calculated gain; Peak caps the filter response at -0.2 dB."
            }
            Self::TrebleExtension => "Extends treble correction and adds AutoEq-style rolloff.",
            Self::GenerationDefaults => {
                "Resets IR parameters while keeping AutoEq treble unchanged."
            }
            Self::EditNumber(field) | Self::NudgeNumber(field, _) => field.help(),
            Self::ValueBar(ValueBar::SpectrumIntensity) => NumberField::SpectrumIntensity.help(),
            Self::ValueBar(ValueBar::Volume) => {
                "Listening level after processing. Alt-click restores -18 dB."
            }
            Self::Compensation => {
                "Matches corrected and bypass levels using a pink-noise reference."
            }
            Self::Bypass => "Compares uncorrected audio while keeping the route running.",
            Self::SpectrumEnabled => "Show the live audio spectrum behind the IR curves.",
            Self::SpectrumPicker(setting) => match setting {
                SpectrumSetting::Source => {
                    "Input is before correction and gain; Output is after processing."
                }
                SpectrumSetting::Resolution => {
                    "More samples give finer bass detail, with a longer analysis window."
                }
                SpectrumSetting::Smoothing => {
                    "Time averaging. Longer values steady the display but react more slowly."
                }
                SpectrumSetting::Range => {
                    "Visible levels in dBFS. This does not change audio gain."
                }
                SpectrumSetting::FrameRate => {
                    "Display refresh target. Higher rates use more CPU/GPU; VSync follows the display."
                }
            },
            Self::LaunchAtLogin => "Open IRresponsible automatically when you sign in to macOS.",
            Self::LoginSettings => {
                "Manage login items and approve IRresponsible in System Settings."
            }
            Self::FollowSystem => {
                "Automatically switches between your saved light and dark themes."
            }
            Self::ThemePicker(slot) => match slot {
                ThemeSlot::Light => "Theme to use when macOS has a light appearance.",
                ThemeSlot::Dark => "Theme to use when macOS has a dark appearance.",
                ThemeSlot::Fixed => "Theme to use regardless of macOS appearance.",
            },
            Self::Capture => "Device receiving Mac audio, usually BlackHole.",
            Self::Playback => "Device sending processed audio to your headphones.",
            Self::CapturePair => "The stereo channels captured from the input device.",
            Self::PlaybackPair => "The stereo output channels used for playback.",
            Self::CatalogFilter => {
                "Choose which correction and measurement assets entries must provide."
            }
            Self::AutoEq => "Loads the published AutoEq impulse, preserving its coefficients.",
            Self::Harman | Self::Reference => {
                "Generates correction toward the measurement rig's reference target."
            }
            Self::Flat => "Targets a flat response in this measurement rig's reference.",
            _ => return None,
        })
    }
}
impl NumberField {
    fn help(self) -> &'static str {
        match self {
            Self::ComparisonTrim => {
                "Adjusts corrected audio relative to bypass for loudness comparisons."
            }
            Self::SpectrumIntensity => {
                "Spectrum fill strength. Drag or type a percentage; Alt-click resets."
            }
            Self::Generation(parameter) => match parameter {
                GenerationParameter::Smoothing => {
                    "Correction averaging width in octaves. Higher values reduce narrow adjustments."
                }
                GenerationParameter::TrebleSmoothing => {
                    "Averaging width above 'Treble from'. Higher values soften treble corrections."
                }
                GenerationParameter::TrebleStart => {
                    "Frequency where treble smoothing replaces normal smoothing."
                }
                GenerationParameter::BoostLimit => {
                    "Maximum correction boost. Lower values need less headroom."
                }
                GenerationParameter::CutLimit => {
                    "Maximum correction cut, entered as a positive dB value."
                }
                GenerationParameter::SlopeLimit => {
                    "Maximum correction change per octave. Lower values produce gentler curves."
                }
                GenerationParameter::LowUnity => "Below this frequency, correction is zero dB.",
                GenerationParameter::LowFull => {
                    "Full bass correction starts here; below it, correction fades out."
                }
                GenerationParameter::HighFull => {
                    "Treble correction starts fading out above this frequency."
                }
                GenerationParameter::HighUnity => "Above this frequency, correction is zero dB.",
            },
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub struct Hint {
    bounds: Rect,
    text: &'static str,
}

pub fn target(hits: &[(Rect, Action)], cursor: Point, preferences: bool) -> Option<Hint> {
    if let Some((bounds, action)) = hits
        .iter()
        .rev()
        .find(|(bounds, _)| bounds.contains(cursor))
    {
        return hint_for(hits, *bounds, *action, preferences);
    }
    // Setting labels share their control's explanation without becoming click targets.
    if preferences && (super::INSET..super::SIDEBAR - super::INSET).contains(&cursor.x) {
        hits.iter()
            .rev()
            .filter_map(|(bounds, action)| {
                if bounds.x0 >= super::SIDEBAR || bounds.area() == 0.0 {
                    return None;
                }
                hint_for(hits, *bounds, *action, true)
            })
            .find(|hint| hint.bounds.contains(cursor))
    } else {
        None
    }
}
fn hint_for(
    hits: &[(Rect, Action)],
    bounds: Rect,
    action: Action,
    preferences: bool,
) -> Option<Hint> {
    let text = action.help()?;
    let mut group = hits
        .iter()
        .filter(|(other, action)| {
            other.area() > 0.0
                && (other.x0 < super::SIDEBAR) == (bounds.x0 < super::SIDEBAR)
                && other.y0 < bounds.y1
                && other.y1 > bounds.y0
                && action.help() == Some(text)
        })
        .fold(bounds, |union, (other, _)| union.union(*other));
    if preferences && group.x0 < super::SIDEBAR {
        group.x0 = super::INSET;
        group.x1 = super::SIDEBAR - super::INSET;
        if matches!(action, Action::ThemePicker(_)) {
            group.y0 -= 26.0;
        }
    }
    Some(Hint {
        bounds: group,
        text,
    })
}

enum Hover {
    Waiting { hint: Hint, since: Instant },
    Shown(Hint),
}
#[derive(Default)]
pub struct Help(Option<Hover>);
impl Help {
    pub fn clear(&mut self) -> bool {
        let shown = self.shown().is_some();
        self.0 = None;
        shown
    }
    pub fn update(&mut self, target: Option<Hint>, now: Instant) -> bool {
        let shown = self.shown().is_some();
        match (&self.0, target) {
            (Some(Hover::Waiting { hint, since }), Some(next)) if *hint == next => {
                if now.duration_since(*since) >= DELAY {
                    self.0 = Some(Hover::Shown(next));
                    return true;
                }
            }
            (Some(Hover::Shown(hint)), Some(next)) if *hint == next => {}
            (_, Some(hint)) => {
                self.0 = Some(Hover::Waiting { hint, since: now });
                return shown;
            }
            (_, None) => {
                self.0 = None;
                return shown;
            }
        }
        false
    }
    fn shown(&self) -> Option<Hint> {
        match self.0 {
            Some(Hover::Shown(hint)) => Some(hint),
            _ => None,
        }
    }
    pub fn draw(
        &self,
        p: &mut Painter<'_, Action>,
        current: Option<Hint>,
        cursor: Point,
        size: Size,
        chrome_height: f64,
    ) {
        let Some(hint) = self.shown().filter(|hint| Some(*hint) == current) else {
            return;
        };
        let room = (size.width - 32.0).min(300.0) - 16.0;
        let mut lines = Vec::new();
        let mut line = String::new();
        for word in hint.text.split_whitespace() {
            let candidate = if line.is_empty() {
                word.into()
            } else {
                format!("{line} {word}")
            };
            if !line.is_empty() && p.advance(&candidate, Face::Label) > room {
                lines.push(std::mem::take(&mut line));
                line = word.into();
            } else {
                line = candidate;
            }
        }
        if !line.is_empty() {
            lines.push(line);
        }
        let width = lines
            .iter()
            .map(|line| p.advance(line, Face::Label))
            .fold(0.0_f64, f64::max)
            + 16.0;
        let height = lines.len() as f64 * 18.0 + 16.0;
        let x = if cursor.x + 16.0 + width <= size.width - 8.0 {
            cursor.x + 16.0
        } else {
            cursor.x - width - 16.0
        }
        .clamp(8.0, size.width - width - 8.0);
        let y = if cursor.y + 4.0 + height <= size.height - 8.0 {
            cursor.y + 4.0
        } else {
            cursor.y - height - 4.0
        }
        .clamp(chrome_height + 8.0, size.height - height - 8.0);
        let bounds = Rect::from_origin_size((x, y), (width, height));
        p.rounded(
            bounds + vello::kurbo::Vec2::new(0.0, 2.0),
            RADIUS,
            opacity(p.colors.bg, 0.3),
        );
        p.rounded(bounds, RADIUS, p.colors.bg);
        p.outline(bounds, RADIUS, 1.0, p.colors.ghost_intense);
        for (i, line) in lines.iter().enumerate() {
            let top = y + 8.0 + i as f64 * 18.0;
            p.text(
                line,
                Rect::new(x + 8.0, top, x + width - 8.0, top + 18.0),
                Face::Label,
                Align::Start,
                p.colors.fg,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn help_waits_for_dwell_and_restarts_after_the_target_changes() {
        let hint = Hint {
            bounds: Rect::new(0.0, 0.0, 100.0, 24.0),
            text: "First",
        };
        let second = Hint {
            text: "Second",
            ..hint
        };
        let start = Instant::now();
        let mut help = Help::default();
        assert!(!help.update(Some(hint), start));
        assert!(!help.update(Some(hint), start + DELAY - Duration::from_nanos(1)));
        assert!(help.update(Some(hint), start + DELAY));
        assert!(help.shown().is_some());
        assert!(help.update(Some(second), start + DELAY));
        assert!(help.shown().is_none());
        assert!(!help.update(Some(second), start + DELAY * 2 - Duration::from_nanos(1)));
        assert!(help.update(Some(second), start + DELAY * 2));
        assert!(help.update(None, start + DELAY * 2));
        assert!(help.shown().is_none());
    }
}
