//! Modal dialogs: a message plus buttons, driven by keyboard or mouse.
//!
//! Keys: ←/→ or Tab/Shift+Tab move focus, Enter/Space activates, Esc
//! cancels, and each button's underlined letter (plain or with Alt)
//! activates it directly.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Cancel,
    /// Save the active tab, then close it.
    SaveThenClose,
    DiscardThenClose,
    /// Save every unsaved tab (asking for names as needed), then quit.
    SaveAllThenQuit,
    SaveThenQuit,
    DiscardAndQuit,
    /// Save over a file that changed on disk.
    Overwrite,
    /// Replace the buffer with the disk version (undoable).
    ReloadFromDisk,
    /// Keep the buffer; the next save will ask again.
    KeepMine,
    SaveAs,
    /// Save As onto an existing file, confirmed.
    Replace(PathBuf),
    RestoreRecovery,
    DiscardRecovery,
    /// Keep recovery snapshots for the next start.
    RecoveryLater,
    /// Save over a file that wasn't decoded exactly, confirmed.
    SaveInexact,
    /// Switch the document to UTF-8 and save (its encoding lacks a char).
    SaveUtf8,
    /// The same for a Save As to this path.
    SaveAsUtf8(PathBuf),
    /// Pick an encoding to read the file again with.
    EncReopen,
    /// Pick an encoding to save the file in.
    EncSave,
}

#[derive(Debug, Clone)]
pub struct Button {
    pub label: String,
    /// Index into `label` of the mnemonic char (shown underlined).
    pub mnemonic: Option<usize>,
    pub action: Action,
}

impl Button {
    /// `label` marks its mnemonic with `&`, e.g. "&Save", "Do&n't Save".
    pub fn new(label: &str, action: Action) -> Self {
        let mnemonic = label.find('&');
        Self { label: label.replace('&', ""), mnemonic, action }
    }

    fn key(&self) -> Option<char> {
        self.mnemonic.and_then(|i| self.label[i..].chars().next()).map(|c| c.to_ascii_lowercase())
    }
}

#[derive(Debug, Clone)]
pub struct Dialog {
    pub title: String,
    pub lines: Vec<String>,
    pub buttons: Vec<Button>,
    pub focus: usize,
}

impl Dialog {
    pub fn new(title: impl Into<String>, lines: Vec<String>, buttons: Vec<Button>) -> Self {
        Self { title: title.into(), lines, buttons, focus: 0 }
    }

    /// Returns the chosen action, or None if the key only moved focus.
    pub fn on_key(&mut self, k: KeyEvent) -> Option<Action> {
        let n = self.buttons.len();
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::Esc => Some(Action::Cancel),
            KeyCode::Enter | KeyCode::Char(' ') => Some(self.buttons[self.focus].action.clone()),
            KeyCode::Right | KeyCode::Down => {
                self.focus = (self.focus + 1) % n;
                None
            }
            KeyCode::Tab if !shift => {
                self.focus = (self.focus + 1) % n;
                None
            }
            KeyCode::Left | KeyCode::Up | KeyCode::BackTab | KeyCode::Tab => {
                self.focus = (self.focus + n - 1) % n;
                None
            }
            KeyCode::Char(c) => {
                let c = c.to_ascii_lowercase();
                self.buttons.iter().find(|b| b.key() == Some(c)).map(|b| b.action.clone())
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyEventKind, KeyEventState};

    fn k(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent { code, modifiers: m, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }

    fn sample() -> Dialog {
        Dialog::new(
            "t",
            vec![],
            vec![
                Button::new("&Save", Action::SaveThenClose),
                Button::new("Do&n't Save", Action::DiscardThenClose),
                Button::new("&Cancel", Action::Cancel),
            ],
        )
    }

    #[test]
    fn mnemonics_focus_and_cancel() {
        let mut d = sample();
        assert_eq!(d.buttons[1].label, "Don't Save");
        assert_eq!(d.on_key(k(KeyCode::Char('n'), KeyModifiers::NONE)), Some(Action::DiscardThenClose));
        assert_eq!(d.on_key(k(KeyCode::Char('S'), KeyModifiers::SHIFT)), Some(Action::SaveThenClose));
        assert_eq!(d.on_key(k(KeyCode::Char('s'), KeyModifiers::ALT)), Some(Action::SaveThenClose));
        assert_eq!(d.on_key(k(KeyCode::Right, KeyModifiers::NONE)), None);
        assert_eq!(d.on_key(k(KeyCode::Enter, KeyModifiers::NONE)), Some(Action::DiscardThenClose));
        assert_eq!(d.on_key(k(KeyCode::Left, KeyModifiers::NONE)), None);
        assert_eq!(d.on_key(k(KeyCode::Left, KeyModifiers::NONE)), None);
        assert_eq!(d.focus, 2, "wraps around");
        assert_eq!(d.on_key(k(KeyCode::Esc, KeyModifiers::NONE)), Some(Action::Cancel));
        assert_eq!(d.on_key(k(KeyCode::Char('x'), KeyModifiers::NONE)), None);
    }
}
