//! Cordex-style single-line editing. Selection is one anchor/caret pair;
//! the shaped field supplies mouse positions in UTF-8 byte indices.
use std::{ops::Range, time::Instant};
use unicode_segmentation::UnicodeSegmentation;
use winit::keyboard::{Key, ModifiersState, NamedKey};

pub struct TextField {
    text: String,
    anchor: usize,
    caret: usize,
    edited: Instant,
}
impl TextField {
    pub fn new(text: String) -> Self {
        let end = text.len();
        Self {
            text,
            anchor: end,
            caret: end,
            edited: Instant::now(),
        }
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn anchor(&self) -> usize {
        self.anchor
    }
    pub fn caret(&self) -> usize {
        self.caret
    }
    pub fn selection(&self) -> Option<Range<usize>> {
        (self.anchor != self.caret)
            .then(|| self.anchor.min(self.caret)..self.anchor.max(self.caret))
    }
    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|range| &self.text[range])
    }
    pub fn caret_visible(&self) -> bool {
        let t = self.edited.elapsed().as_secs_f32();
        t < 0.5 || (t * 1.2).fract() < 0.5
    }
    pub fn place(&mut self, at: usize, extend: bool) {
        let mut at = at.min(self.text.len());
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        self.caret = at;
        if !extend {
            self.anchor = at;
        }
        self.edited = Instant::now();
    }
    pub fn select(&mut self, range: Range<usize>) {
        self.place(range.start, false);
        self.place(range.end, true);
    }
    pub fn select_all(&mut self) {
        self.select(0..self.text.len());
    }
    pub fn clear(&mut self) {
        self.text.clear();
        self.place(0, false);
    }
    pub fn insert(&mut self, text: &str) {
        let input: String = text.chars().filter(|ch| !ch.is_control()).collect();
        let range = self.selection().unwrap_or(self.caret..self.caret);
        self.text.replace_range(range.clone(), &input);
        self.place(range.start + input.len(), false);
    }
    pub fn cut(&mut self) -> Option<String> {
        let text = self.selected_text()?.to_owned();
        self.insert("");
        Some(text)
    }
    fn previous(&self) -> usize {
        self.text[..self.caret]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(at, _)| at)
    }
    fn next(&self) -> usize {
        self.text[self.caret..]
            .graphemes(true)
            .next()
            .map_or(self.text.len(), |ch| self.caret + ch.len())
    }
    fn previous_word(&self) -> usize {
        self.text
            .unicode_word_indices()
            .take_while(|(at, _)| *at < self.caret)
            .last()
            .map_or(0, |(at, _)| at)
    }
    fn next_word(&self) -> usize {
        self.text
            .unicode_word_indices()
            .map(|(at, word)| at + word.len())
            .find(|&end| end > self.caret)
            .unwrap_or(self.text.len())
    }
    pub fn key(&mut self, key: &Key, text: Option<&str>, modifiers: ModifiersState) -> bool {
        let command = modifiers.super_key() || modifiers.control_key();
        let word = modifiers.alt_key() || modifiers.control_key();
        let extend = modifiers.shift_key();
        match key {
            Key::Character(ch) if command && ch.eq_ignore_ascii_case("a") => self.select_all(),
            Key::Named(NamedKey::ArrowLeft) => {
                let at = if command && modifiers.super_key() {
                    0
                } else if let Some(range) = self.selection().filter(|_| !extend) {
                    range.start
                } else if word {
                    self.previous_word()
                } else {
                    self.previous()
                };
                self.place(at, extend);
            }
            Key::Named(NamedKey::ArrowRight) => {
                let at = if command && modifiers.super_key() {
                    self.text.len()
                } else if let Some(range) = self.selection().filter(|_| !extend) {
                    range.end
                } else if word {
                    self.next_word()
                } else {
                    self.next()
                };
                self.place(at, extend);
            }
            Key::Named(NamedKey::Home) => self.place(0, extend),
            Key::Named(NamedKey::End) => self.place(self.text.len(), extend),
            Key::Named(NamedKey::Backspace | NamedKey::Delete) => {
                if self.selection().is_none() {
                    let at = if matches!(key, Key::Named(NamedKey::Backspace)) {
                        if modifiers.super_key() {
                            0
                        } else if word {
                            self.previous_word()
                        } else {
                            self.previous()
                        }
                    } else if modifiers.super_key() {
                        self.text.len()
                    } else if word {
                        self.next_word()
                    } else {
                        self.next()
                    };
                    self.place(at, true);
                }
                self.insert("");
            }
            Key::Named(NamedKey::Space) if !command => self.insert(" "),
            Key::Character(_) if !command => {
                if let Some(text) = text {
                    self.insert(text);
                }
            }
            Key::Character(_) if command => {}
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spaces_selection_motion_and_replacement_share_the_same_buffer() {
        let mut field = TextField::new("DT".into());
        field.key(
            &Key::Named(NamedKey::Space),
            Some(" "),
            ModifiersState::empty(),
        );
        field.insert("990 Pro");
        assert_eq!(field.text(), "DT 990 Pro");
        field.key(
            &Key::Named(NamedKey::ArrowLeft),
            None,
            ModifiersState::SHIFT,
        );
        assert_eq!(field.selected_text(), Some("o"));
        field.insert("é");
        assert_eq!(field.text(), "DT 990 Pré");
        field.key(
            &Key::Named(NamedKey::Backspace),
            None,
            ModifiersState::empty(),
        );
        assert_eq!(field.text(), "DT 990 Pr");
        field.select_all();
        field.insert("AKG K240");
        assert_eq!(field.text(), "AKG K240");
        field.select(4..8);
        assert_eq!(field.cut().as_deref(), Some("K240"));
        assert_eq!(field.text(), "AKG ");
    }
    #[test]
    fn deletion_keeps_graphemes_and_utf8_boundaries_intact() {
        let mut field = TextField::new("cafe\u{301}".into());
        field.key(
            &Key::Named(NamedKey::Backspace),
            None,
            ModifiersState::empty(),
        );
        assert_eq!(field.text(), "caf");
        field.place(0, false);
        field.key(&Key::Named(NamedKey::Delete), None, ModifiersState::empty());
        assert_eq!(field.text(), "af");
    }
}
