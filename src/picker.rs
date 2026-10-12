//! A filterable list popup: type to filter, ↑/↓/PgUp/PgDn to move, Enter
//! or a click to choose, Esc or a click outside to cancel.

use crate::lineedit::{LineEdit, Outcome as EditOutcome};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use std::cell::Cell;

pub struct Item<T> {
    pub label: String,
    pub detail: String,
    /// Marked as the current value (and selected when the picker opens).
    pub current: bool,
    pub value: T,
}

pub struct Picker<T> {
    pub title: String,
    pub items: Vec<Item<T>>,
    pub filter: LineEdit,
    /// Index into `visible()`.
    pub sel: usize,
    /// First visible row, adjusted at draw time to keep `sel` in view.
    pub scroll: Cell<usize>,
}

pub enum Outcome<T> {
    /// Still open.
    Open,
    Chosen(T),
    Cancel,
}

impl<T: Clone> Picker<T> {
    pub fn new(title: impl Into<String>, items: Vec<Item<T>>) -> Self {
        let sel = items.iter().position(|i| i.current).unwrap_or(0);
        Self { title: title.into(), items, filter: LineEdit::new(""), sel, scroll: Cell::new(0) }
    }

    /// Items matching every word of the filter (label or detail, any case).
    pub fn visible(&self) -> Vec<usize> {
        let words: Vec<String> = self.filter.text().split_whitespace().map(str::to_lowercase).collect();
        (0..self.items.len())
            .filter(|&i| {
                let hay = format!("{} {}", self.items[i].label, self.items[i].detail).to_lowercase();
                words.iter().all(|w| hay.contains(w.as_str()))
            })
            .collect()
    }

    fn chosen(&self, row: usize) -> Outcome<T> {
        match self.visible().get(row) {
            Some(&i) => Outcome::Chosen(self.items[i].value.clone()),
            None => Outcome::Open,
        }
    }

    pub fn on_key(&mut self, k: &KeyEvent, page: usize) -> Outcome<T> {
        let n = self.visible().len();
        let last = n.saturating_sub(1);
        match k.code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Enter => return self.chosen(self.sel),
            KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down => self.sel = (self.sel + 1).min(last),
            KeyCode::PageUp => self.sel = self.sel.saturating_sub(page.max(1)),
            KeyCode::PageDown => self.sel = (self.sel + page.max(1)).min(last),
            _ => {
                if self.filter.key(k) == EditOutcome::Changed {
                    self.sel = 0;
                }
            }
        }
        Outcome::Open
    }

    /// A click on visible row `row` (relative to the list's first row).
    pub fn click(&mut self, row: usize) -> Outcome<T> {
        self.chosen(self.scroll.get() + row)
    }

    pub fn wheel(&mut self, down: bool) {
        let n = self.visible().len();
        self.sel = if down { (self.sel + 3).min(n.saturating_sub(1)) } else { self.sel.saturating_sub(3) };
    }

    /// Keep `sel` within `rows` visible rows; returns the first shown row.
    pub fn scroll_for(&self, rows: usize) -> usize {
        let mut top = self.scroll.get();
        if self.sel < top {
            top = self.sel;
        } else if rows > 0 && self.sel >= top + rows {
            top = self.sel + 1 - rows;
        }
        top = top.min(self.visible().len().saturating_sub(rows));
        self.scroll.set(top);
        top
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyModifiers;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn picker() -> Picker<u32> {
        let items = [("UTF-8", "Unicode"), ("windows-1252", "Western European"), ("Shift_JIS", "Japanese"), ("EUC-JP", "Japanese")]
            .iter()
            .enumerate()
            .map(|(i, (l, d))| Item { label: l.to_string(), detail: d.to_string(), current: i == 1, value: i as u32 })
            .collect();
        Picker::new("Pick", items)
    }

    #[test]
    fn filter_move_and_choose() {
        let mut p = picker();
        assert_eq!(p.sel, 1, "starts on the current item");
        for c in "japan".chars() {
            p.on_key(&key(KeyCode::Char(c)), 5);
        }
        assert_eq!(p.visible(), [2, 3]);
        p.on_key(&key(KeyCode::Down), 5);
        p.on_key(&key(KeyCode::Down), 5);
        assert!(matches!(p.on_key(&key(KeyCode::Enter), 5), Outcome::Chosen(3)));
        p.filter.set("west 1252");
        assert_eq!(p.visible(), [1]);
        p.filter.set("nothing");
        p.sel = 0;
        assert!(matches!(p.on_key(&key(KeyCode::Enter), 5), Outcome::Open), "nothing to choose");
        assert!(matches!(p.on_key(&key(KeyCode::Esc), 5), Outcome::Cancel));
    }

    #[test]
    fn scrolling_keeps_selection_visible_and_clicks_map_rows() {
        let mut p = picker();
        p.sel = 3;
        assert_eq!(p.scroll_for(2), 2);
        assert!(matches!(p.click(1), Outcome::Chosen(3)));
        p.sel = 0;
        assert_eq!(p.scroll_for(2), 0);
        p.wheel(true);
        assert_eq!(p.sel, 3);
    }
}
