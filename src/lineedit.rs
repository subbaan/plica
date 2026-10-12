//! A single-line text field with a cursor (find/replace, prompts).
//!
//! The cursor is a byte index into `text`, always on a grapheme boundary.
//! Widths come from ratatui's `CellWidth`, like the editor's layout, so a
//! click lands where the glyph is drawn.

use crate::buffer::is_word_char;
use ratatui::buffer::CellWidth;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::cell::Cell;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The text changed.
    Changed,
    /// Only the cursor moved.
    Moved,
    /// Not a field key; the caller may handle it.
    Ignored,
}

#[derive(Debug, Clone, Default)]
pub struct LineEdit {
    text: String,
    cur: usize,
    /// First visible byte (kept so the view only scrolls when the cursor
    /// would leave it).
    scroll: Cell<usize>,
}

/// What to draw for a field `width` cells wide.
pub struct View<'a> {
    pub shown: &'a str,
    /// Cursor column relative to the field's left edge.
    pub caret: u16,
}

fn width(s: &str) -> usize {
    s.graphemes(true).map(|g| g.cell_width() as usize).sum()
}

impl LineEdit {
    /// A field holding `s`, cursor at the end.
    pub fn new(s: impl Into<String>) -> Self {
        let text = sanitise(&s.into());
        Self { cur: text.len(), text, scroll: Cell::new(0) }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    #[cfg(test)]
    pub fn cursor(&self) -> usize {
        self.cur
    }

    /// Replace the text, cursor at the end.
    pub fn set(&mut self, s: &str) {
        *self = Self::new(s);
    }

    /// Insert at the cursor. Newlines become spaces (fields are one line).
    pub fn insert(&mut self, s: &str) {
        let s = sanitise(s);
        self.text.insert_str(self.cur, &s);
        self.cur += s.len();
    }

    fn prev(&self, i: usize) -> usize {
        self.text[..i].grapheme_indices(true).next_back().map_or(0, |(j, _)| j)
    }

    fn next(&self, i: usize) -> usize {
        self.text[i..].graphemes(true).next().map_or(i, |g| i + g.len())
    }

    fn word_left(&self, i: usize) -> usize {
        let mut j = i;
        let is_word = |k: usize| self.text[k..].chars().next().is_some_and(is_word_char);
        while j > 0 && !is_word(self.prev(j)) {
            j = self.prev(j);
        }
        while j > 0 && is_word(self.prev(j)) {
            j = self.prev(j);
        }
        j
    }

    fn word_right(&self, i: usize) -> usize {
        let mut j = i;
        let is_word = |k: usize| self.text[k..].chars().next().is_some_and(is_word_char);
        while j < self.text.len() && !is_word(j) {
            j = self.next(j);
        }
        while j < self.text.len() && is_word(j) {
            j = self.next(j);
        }
        j
    }

    pub fn key(&mut self, k: &KeyEvent) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let word = ctrl || alt;
        match k.code {
            KeyCode::Left => self.cur = if word { self.word_left(self.cur) } else { self.prev(self.cur) },
            KeyCode::Right => self.cur = if word { self.word_right(self.cur) } else { self.next(self.cur) },
            KeyCode::Home => self.cur = 0,
            KeyCode::End => self.cur = self.text.len(),
            KeyCode::Backspace => {
                let from = if word { self.word_left(self.cur) } else { self.prev(self.cur) };
                if from == self.cur {
                    return Outcome::Moved;
                }
                self.text.replace_range(from..self.cur, "");
                self.cur = from;
                return Outcome::Changed;
            }
            KeyCode::Delete => {
                let to = if word { self.word_right(self.cur) } else { self.next(self.cur) };
                if to == self.cur {
                    return Outcome::Moved;
                }
                self.text.replace_range(self.cur..to, "");
                return Outcome::Changed;
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                let mut b = [0u8; 4];
                self.insert(c.encode_utf8(&mut b));
                return Outcome::Changed;
            }
            _ => return Outcome::Ignored,
        }
        Outcome::Moved
    }

    /// Adjust the scroll so the cursor is visible in `w` cells, and return
    /// what to draw. One cell is kept free for the caret at the end.
    pub fn view(&self, w: usize) -> View<'_> {
        let w = w.max(1);
        let mut scroll = self.scroll.get().min(self.cur);
        if !self.text.is_char_boundary(scroll) {
            scroll = 0;
        }
        // Text was deleted: scroll back to use the free space.
        while scroll > 0 && width(&self.text[self.prev(scroll)..]) < w {
            scroll = self.prev(scroll);
        }
        while width(&self.text[scroll..self.cur]) >= w {
            scroll = self.next(scroll);
        }
        self.scroll.set(scroll);
        View { shown: &self.text[scroll..], caret: width(&self.text[scroll..self.cur]) as u16 }
    }

    /// Put the cursor at display column `col` of the last view (a click).
    pub fn click(&mut self, col: usize) {
        let scroll = self.scroll.get().min(self.text.len());
        let mut x = 0;
        let mut i = scroll;
        for g in self.text[scroll..].graphemes(true) {
            let gw = g.cell_width() as usize;
            if col < x + gw {
                // Right half of a wide glyph: after it.
                if gw > 1 && (col - x) * 2 >= gw {
                    i += g.len();
                }
                break;
            }
            x += gw;
            i += g.len();
        }
        self.cur = i;
    }
}

fn sanitise(s: &str) -> String {
    s.replace("\r\n", " ").replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyEventKind, KeyEventState};

    fn k(e: &mut LineEdit, code: KeyCode, mods: KeyModifiers) -> Outcome {
        e.key(&KeyEvent { code, modifiers: mods, kind: KeyEventKind::Press, state: KeyEventState::NONE })
    }
    const N: KeyModifiers = KeyModifiers::NONE;
    const C: KeyModifiers = KeyModifiers::CONTROL;

    #[test]
    fn editing_in_the_middle() {
        let mut e = LineEdit::new("/home/me/file.txt");
        k(&mut e, KeyCode::Home, N);
        assert_eq!(e.cursor(), 0);
        k(&mut e, KeyCode::Delete, N);
        assert_eq!(e.text(), "home/me/file.txt");
        k(&mut e, KeyCode::End, N);
        k(&mut e, KeyCode::Left, C);
        assert_eq!(&e.text()[e.cursor()..], "txt");
        k(&mut e, KeyCode::Backspace, C);
        assert_eq!(e.text(), "home/me/txt", "Ctrl+Backspace deletes the word before");
        k(&mut e, KeyCode::Char('x'), N);
        assert_eq!(e.text(), "home/me/xtxt");
        assert_eq!(k(&mut e, KeyCode::Char('a'), C), Outcome::Ignored);
        assert_eq!(k(&mut e, KeyCode::Up, N), Outcome::Ignored);
    }

    #[test]
    fn graphemes_and_newlines() {
        let mut e = LineEdit::new("ae\u{301}x");
        k(&mut e, KeyCode::Left, N);
        k(&mut e, KeyCode::Left, N);
        assert_eq!(e.cursor(), 1, "one step over e + combining accent");
        k(&mut e, KeyCode::Delete, N);
        assert_eq!(e.text(), "ax");
        e.insert("one\r\ntwo\n");
        assert_eq!(e.text(), "aone two x");
    }

    #[test]
    fn view_scrolls_and_click_maps_back() {
        let mut e = LineEdit::new("0123456789abcdef");
        let v = e.view(8);
        assert_eq!(v.shown, "9abcdef");
        assert_eq!(v.caret, 7);
        k(&mut e, KeyCode::Home, N);
        let v = e.view(8);
        assert!(v.shown.starts_with("0123"));
        assert_eq!(v.caret, 0);
        e.click(3);
        assert_eq!(e.cursor(), 3);
        // Short text: everything shows, click past the end goes to the end.
        let mut e = LineEdit::new("ab");
        e.view(8);
        e.click(6);
        assert_eq!(e.cursor(), 2);
        // Wide glyphs: the caret column counts cells.
        let e = LineEdit::new("日本");
        assert_eq!(e.view(10).caret, 4);
    }
}
