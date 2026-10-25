//! Remappable commands and the key chords bound to them.
//!
//! Motion and basic editing keys (arrows, Home/End, Backspace, Enter, Tab,
//! typing) are fixed; everything with a name below can be rebound in the
//! config file's `[keys]` table.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;
use std::fmt;

macro_rules! commands {
    ($($variant:ident => $name:literal, $desc:literal, [$($chord:literal),*];)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Command { $($variant),* }

        impl Command {
            pub const ALL: &'static [Command] = &[$(Command::$variant),*];

            /// Name used in the config file.
            pub fn name(self) -> &'static str {
                match self { $(Command::$variant => $name),* }
            }

            pub fn describe(self) -> &'static str {
                match self { $(Command::$variant => $desc),* }
            }

            pub fn from_name(s: &str) -> Option<Command> {
                match s { $($name => Some(Command::$variant),)* _ => None }
            }

            fn default_chords(self) -> &'static [&'static str] {
                match self { $(Command::$variant => &[$($chord),*]),* }
            }
        }
    };
}

commands! {
    Save => "save", "Save", ["ctrl+s"];
    SaveAs => "save_as", "Save as", ["ctrl+shift+s", "f12"];
    Open => "open", "Open", ["ctrl+o"];
    RecentFiles => "recent_files", "Recent files (reopen where you left off)", ["alt+r"];
    NewTab => "new_tab", "New tab", ["ctrl+n"];
    CloseTab => "close_tab", "Close tab", ["ctrl+w"];
    Quit => "quit", "Quit", ["ctrl+q"];
    Undo => "undo", "Undo", ["ctrl+z"];
    Redo => "redo", "Redo", ["ctrl+y", "ctrl+shift+z"];
    Copy => "copy", "Copy", ["ctrl+c"];
    Cut => "cut", "Cut", ["ctrl+x"];
    Paste => "paste", "Paste", ["ctrl+v"];
    SelectAll => "select_all", "Select all", ["ctrl+a"];
    Find => "find", "Find", ["ctrl+f"];
    Replace => "replace", "Replace", ["ctrl+h", "ctrl+r"];
    FindNext => "find_next", "Find next", ["f3"];
    FindPrev => "find_prev", "Find previous", ["shift+f3"];
    GotoLine => "goto_line", "Go to line", ["ctrl+g"];
    NextTab => "next_tab", "Next tab", ["ctrl+pagedown", "ctrl+tab"];
    PrevTab => "prev_tab", "Previous tab", ["ctrl+pageup", "ctrl+shift+tab"];
    ToggleWrap => "toggle_wrap", "Soft wrap on/off", ["alt+z"];
    ToggleLineNumbers => "toggle_line_numbers", "Line numbers on/off", ["alt+l"];
    ToggleWhitespace => "toggle_whitespace", "Show whitespace on/off", ["alt+w"];
    ToggleMouse => "toggle_mouse", "Give the mouse to the terminal", ["alt+m"];
    ToggleOverwrite => "toggle_overwrite", "Overwrite (overtype) mode on/off", ["insert"];
    ScrollUp => "scroll_up", "Scroll up a line", ["ctrl+up"];
    ScrollDown => "scroll_down", "Scroll down a line", ["ctrl+down"];
    Help => "help", "Help", ["f1"];
    Preferences => "preferences", "Preferences", ["f9", "ctrl+,"];
    HighlightWord => "highlight_word", "Highlight word in this document (again: remove)", ["alt+h", "f8"];
    Encoding => "encoding", "Reopen or save in another encoding", ["alt+e", "f7"];
    TableMenu => "table_menu", "Table menu (format, rows, columns)", ["alt+t", "f6"];
    FormatTable => "format_table", "Format the table under the caret", [];
    Syntax => "syntax", "Language for syntax colours", ["f4"];
}

/// A key plus modifiers, normalised so equivalent events compare equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

const MODS: KeyModifiers = KeyModifiers::CONTROL.union(KeyModifiers::ALT).union(KeyModifiers::SHIFT);

impl Chord {
    pub fn from_event(k: &KeyEvent) -> Self {
        let mut mods = k.modifiers & MODS;
        let code = match k.code {
            // Shift+letter arrives as an uppercase char (with or without SHIFT).
            KeyCode::Char(c) if c.is_uppercase() => {
                mods |= KeyModifiers::SHIFT;
                KeyCode::Char(c.to_lowercase().next().unwrap_or(c))
            }
            KeyCode::BackTab => {
                mods |= KeyModifiers::SHIFT;
                KeyCode::Tab
            }
            c => c,
        };
        Self { code, mods }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim().to_ascii_lowercase();
        if s.is_empty() {
            return Err("empty key".into());
        }
        let parts: Vec<&str> = s.split('+').collect();
        // "ctrl++" means Ctrl and the plus key.
        let (mods_part, key) = if s.ends_with("++") {
            (&parts[..parts.len() - 2], "+")
        } else {
            (&parts[..parts.len() - 1], parts[parts.len() - 1])
        };
        let mut mods = KeyModifiers::NONE;
        for m in mods_part {
            mods |= match *m {
                "ctrl" | "control" => KeyModifiers::CONTROL,
                "alt" | "meta" => KeyModifiers::ALT,
                "shift" => KeyModifiers::SHIFT,
                other => return Err(format!("unknown modifier `{other}`")),
            };
        }
        let code = match key {
            "enter" | "return" => KeyCode::Enter,
            "tab" => KeyCode::Tab,
            "esc" | "escape" => KeyCode::Esc,
            "space" => KeyCode::Char(' '),
            "backspace" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "insert" | "ins" => KeyCode::Insert,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" => KeyCode::PageDown,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            f if f.starts_with('f') && f.len() > 1 && f[1..].chars().all(|c| c.is_ascii_digit()) => {
                let n: u8 = f[1..].parse().map_err(|_| format!("bad function key `{f}`"))?;
                if !(1..=24).contains(&n) {
                    return Err(format!("bad function key `{f}`"));
                }
                KeyCode::F(n)
            }
            k if k.chars().count() == 1 => KeyCode::Char(k.chars().next().unwrap()),
            other => return Err(format!("unknown key `{other}`")),
        };
        Ok(Self { code, mods })
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.contains(KeyModifiers::CONTROL) {
            f.write_str("Ctrl+")?;
        }
        if self.mods.contains(KeyModifiers::ALT) {
            f.write_str("Alt+")?;
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            f.write_str("Shift+")?;
        }
        match self.code {
            KeyCode::Char(' ') => f.write_str("Space"),
            KeyCode::Char(c) => write!(f, "{}", c.to_ascii_uppercase()),
            KeyCode::F(n) => write!(f, "F{n}"),
            KeyCode::PageUp => f.write_str("PgUp"),
            KeyCode::PageDown => f.write_str("PgDn"),
            KeyCode::Up => f.write_str("↑"),
            KeyCode::Down => f.write_str("↓"),
            KeyCode::Left => f.write_str("←"),
            KeyCode::Right => f.write_str("→"),
            KeyCode::Esc => f.write_str("Esc"),
            other => write!(f, "{other:?}"),
        }
    }
}

pub struct Keymap {
    map: HashMap<Chord, Command>,
    /// Chords per command, in priority order (for help text).
    by_command: HashMap<Command, Vec<Chord>>,
}

impl Keymap {
    pub fn defaults() -> Self {
        let mut km = Self { map: HashMap::new(), by_command: HashMap::new() };
        for &c in Command::ALL {
            let chords = c.default_chords().iter().map(|s| Chord::parse(s).expect("valid default chord")).collect();
            km.set(c, chords);
        }
        km
    }

    /// Replace the chords bound to `cmd`. A chord already used by another
    /// command is taken over (the config wins over the defaults).
    pub fn set(&mut self, cmd: Command, chords: Vec<Chord>) {
        if let Some(old) = self.by_command.remove(&cmd) {
            for ch in old {
                self.map.remove(&ch);
            }
        }
        for ch in &chords {
            if let Some(prev) = self.map.insert(*ch, cmd)
                && prev != cmd
                && let Some(list) = self.by_command.get_mut(&prev)
            {
                list.retain(|c| c != ch);
            }
        }
        self.by_command.insert(cmd, chords);
    }

    pub fn lookup(&self, k: &KeyEvent) -> Option<Command> {
        self.map.get(&Chord::from_event(k)).copied()
    }

    pub fn chords(&self, cmd: Command) -> &[Chord] {
        self.by_command.get(&cmd).map_or(&[], Vec::as_slice)
    }

    /// "Ctrl+S" or "Ctrl+Y / Ctrl+Shift+Z" or "unbound".
    pub fn label(&self, cmd: Command) -> String {
        let c = self.chords(cmd);
        if c.is_empty() {
            "unbound".into()
        } else {
            c.iter().map(Chord::to_string).collect::<Vec<_>>().join(" / ")
        }
    }
}

/// The keys as a Markdown cheat sheet (`plica --print-keys`; also
/// `docs/keys.md`, generated with the default keys). Remappable commands
/// come from `km`, so a user's config shows their own keys.
pub fn cheat_sheet(km: &Keymap) -> String {
    let mut s = String::from(
        "# Plica keys\n\n\
         Generated by `plica --print-keys` (it shows your own bindings from config.toml). \
         Remappable commands can be rebound in `[keys]`: see `plica --print-config`.\n\n\
         ## Commands (remappable)\n\n| Keys | Command | Config name |\n|---|---|---|\n",
    );
    for &c in Command::ALL {
        s.push_str(&format!("| {} | {} | `{}` |\n", km.label(c), c.describe(), c.name()));
    }
    let fixed: &[(&str, &[(&str, &str)])] = &[
        (
            "Editing and moving",
            &[
                ("Arrows, Home / End", "Move (Home toggles between the text and the line start)"),
                ("Ctrl+Home / Ctrl+End", "Start / end of the document"),
                ("Ctrl+← / → (or Alt+← / →)", "Previous / next word"),
                ("Shift + any movement", "Extend the selection"),
                ("Page Up / Page Down", "Move a page up / down"),
                ("Ctrl+Backspace / Ctrl+Delete", "Delete a word"),
                ("Tab / Shift+Tab", "Indent / outdent (the selected lines)"),
                ("Esc", "Clear the selection"),
                ("Alt+1 … Alt+9", "Go to tab 1 … 8, Alt+9 the last tab"),
                ("↓ in the Open prompt", "Recent files"),
            ],
        ),
        (
            "Find bar",
            &[
                ("Enter / Shift+Enter, F3 / Shift+F3", "Next / previous match"),
                ("Tab", "Switch between Find and Replace (replace mode)"),
                ("Enter in Replace", "Replace this match"),
                ("Alt+C", "Match case"),
                ("Alt+W", "Whole words"),
                ("Alt+L", "Only in the selection"),
                ("Alt+A", "Replace all"),
                ("Esc", "Close"),
            ],
        ),
        (
            "In a table",
            &[
                ("Tab / Shift+Tab", "Next / previous cell (Tab in the last cell adds a row)"),
                ("Enter", "New row (at the table's very start or end: a plain line break)"),
                ("Alt+↑ / Alt+↓", "Move the row up / down"),
                ("Shift+arrows at a cell's edge", "Select cells"),
                ("Ctrl+C / Ctrl+X / Delete", "Copy (tab-separated) / cut / empty the selected cells"),
                ("Esc", "Plain keys in this table (Esc again: table keys)"),
                ("Right-click", "Table menu"),
            ],
        ),
        (
            "Mouse",
            &[
                ("Click / drag", "Place the caret / select"),
                ("Double / triple click", "Select a word / line"),
                ("Shift+click or Alt+click", "Extend the selection"),
                ("Middle click", "Paste the primary selection"),
                ("Wheel, Shift+wheel", "Scroll, scroll sideways"),
                ("Line numbers", "Click or drag to select lines"),
                ("Tabs", "Click to switch, × or middle-click to close, wheel to cycle"),
                ("Drag across table cells", "Select cells"),
                ("Click the --- under a column", "Select the column"),
                ("Status bar encoding", "Encoding menu"),
                ("Drop files on the window", "Open them"),
            ],
        ),
    ];
    for (title, rows) in fixed {
        s.push_str(&format!("\n## {title}\n\n| Keys | Action |\n|---|---|\n"));
        for (k, d) in *rows {
            s.push_str(&format!("| {k} | {d} |\n"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyEventKind, KeyEventState};

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent { code, modifiers: mods, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }

    #[test]
    fn parse_and_display() {
        assert_eq!(Chord::parse("Ctrl+Shift+S").unwrap(), Chord { code: KeyCode::Char('s'), mods: KeyModifiers::CONTROL | KeyModifiers::SHIFT });
        assert_eq!(Chord::parse("f12").unwrap().code, KeyCode::F(12));
        assert_eq!(Chord::parse("ctrl++").unwrap().code, KeyCode::Char('+'));
        assert_eq!(Chord::parse("ctrl+pagedown").unwrap().to_string(), "Ctrl+PgDn");
        assert!(Chord::parse("hyper+x").is_err());
        assert!(Chord::parse("ctrl+nosuchkey").is_err());
        assert!(Chord::parse("f99").is_err());
    }

    #[test]
    fn event_normalisation() {
        let km = Keymap::defaults();
        // Uppercase char with or without SHIFT, BackTab: all normalise.
        assert_eq!(km.lookup(&ev(KeyCode::Char('S'), KeyModifiers::CONTROL | KeyModifiers::SHIFT)), Some(Command::SaveAs));
        assert_eq!(km.lookup(&ev(KeyCode::Char('S'), KeyModifiers::CONTROL)), Some(Command::SaveAs));
        assert_eq!(km.lookup(&ev(KeyCode::Char('s'), KeyModifiers::CONTROL)), Some(Command::Save));
        assert_eq!(km.lookup(&ev(KeyCode::BackTab, KeyModifiers::CONTROL | KeyModifiers::SHIFT)), Some(Command::PrevTab));
        assert_eq!(km.lookup(&ev(KeyCode::Char('s'), KeyModifiers::NONE)), None);
    }

    #[test]
    fn rebinding_takes_over_chords() {
        let mut km = Keymap::defaults();
        km.set(Command::Quit, vec![Chord::parse("ctrl+w").unwrap()]);
        assert_eq!(km.lookup(&ev(KeyCode::Char('w'), KeyModifiers::CONTROL)), Some(Command::Quit));
        assert!(km.chords(Command::CloseTab).is_empty(), "CloseTab lost its only chord");
        assert_eq!(km.lookup(&ev(KeyCode::Char('q'), KeyModifiers::CONTROL)), None, "old Quit chord freed");
        assert_eq!(km.label(Command::CloseTab), "unbound");
        assert_eq!(km.label(Command::Redo), "Ctrl+Y / Ctrl+Shift+Z");
    }

    #[test]
    fn names_round_trip() {
        for &c in Command::ALL {
            assert_eq!(Command::from_name(c.name()), Some(c));
        }
    }

    #[test]
    fn keys_doc_is_current() {
        let doc = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/keys.md")).unwrap_or_default();
        assert!(doc == cheat_sheet(&Keymap::defaults()), "docs/keys.md is out of date: run `./plica --config /dev/null --print-keys > docs/keys.md`");
    }
}
