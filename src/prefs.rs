//! Preferences dialog: live preview of settings, Save (to config.toml,
//! preserving the user's comments and layout), Apply (this session only)
//! or Cancel (revert).

use crate::app::App;
use crate::colours;
use crate::config::{self, IndentMode, ThemeChoice};
use crate::keymap::Command;
use crate::tab::detect_indent;
use crate::term;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, value};

/// Everything the Preferences dialog can change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub tab_width: usize,
    pub wrap: bool,
    pub line_numbers: bool,
    pub show_whitespace: bool,
    pub indent: IndentMode,
    pub mouse: bool,
    pub primary_selection: bool,
    pub theme: ThemeChoice,
    pub palette: Option<PathBuf>,
    pub recovery: bool,
}

impl Settings {
    pub fn from_config(c: &config::Config) -> Self {
        Self {
            tab_width: c.tab_width,
            wrap: c.wrap,
            line_numbers: c.line_numbers,
            show_whitespace: c.show_whitespace,
            indent: c.indent,
            mouse: c.mouse,
            primary_selection: c.primary_selection,
            theme: c.theme,
            palette: c.palette.clone(),
            recovery: c.recovery,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    TabWidth,
    Wrap,
    LineNumbers,
    Whitespace,
    Indent,
    Mouse,
    Primary,
    Theme,
    Palette,
    Recovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Editor,
    Interaction,
    Appearance,
    Files,
    Keys,
    Tables,
    Highlights,
}

impl Section {
    pub const ALL: [Section; 7] = [
        Section::Editor,
        Section::Interaction,
        Section::Appearance,
        Section::Files,
        Section::Keys,
        Section::Tables,
        Section::Highlights,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Editor => "Editor",
            Section::Interaction => "Interaction",
            Section::Appearance => "Appearance",
            Section::Files => "Files & Recovery",
            Section::Keys => "Keybindings",
            Section::Tables => "Tables",
            Section::Highlights => "Highlights",
        }
    }

    pub fn rows(self) -> &'static [Row] {
        match self {
            Section::Editor => &[Row::TabWidth, Row::Wrap, Row::LineNumbers, Row::Whitespace, Row::Indent],
            Section::Interaction => &[Row::Mouse, Row::Primary],
            Section::Appearance => &[Row::Theme, Row::Palette],
            Section::Files => &[Row::Recovery],
            _ => &[],
        }
    }
}

impl Row {
    pub fn label(self) -> &'static str {
        match self {
            Row::TabWidth => "Tab width",
            Row::Wrap => "Soft wrap",
            Row::LineNumbers => "Line numbers",
            Row::Whitespace => "Show whitespace",
            Row::Indent => "Indent with",
            Row::Mouse => "Mouse in Plica",
            Row::Primary => "Primary selection",
            Row::Theme => "Colours",
            Row::Palette => "Palette file",
            Row::Recovery => "Crash recovery",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Row::TabWidth => "Columns per tab stop (1–16)",
            Row::Wrap => "Wrap long lines at the window edge",
            Row::LineNumbers => "Line numbers in the gutter",
            Row::Whitespace => "Tabs as →, spaces as ·",
            Row::Indent => "auto detects each file's style",
            Row::Mouse => "Off: the terminal keeps the mouse",
            Row::Primary => "Select to copy, middle-click to paste",
            Row::Theme => "auto: palette file if found, else Dracula",
            Row::Palette => "Enter to edit, Delete to clear (Kitty format)",
            Row::Recovery => "Snapshot unsaved work for crash recovery",
        }
    }

    pub fn value(self, s: &Settings) -> String {
        let b = |v: bool| if v { "[x] on".to_string() } else { "[ ] off".to_string() };
        match self {
            Row::TabWidth => format!("‹ {} ›", s.tab_width),
            Row::Wrap => b(s.wrap),
            Row::LineNumbers => b(s.line_numbers),
            Row::Whitespace => b(s.show_whitespace),
            Row::Indent => format!(
                "‹ {} ›",
                match s.indent {
                    IndentMode::Auto => "auto",
                    IndentMode::Tabs => "tabs",
                    IndentMode::Spaces => "spaces",
                }
            ),
            Row::Mouse => b(s.mouse),
            Row::Primary => b(s.primary_selection),
            Row::Theme => format!(
                "‹ {} ›",
                match s.theme {
                    ThemeChoice::Auto => "auto",
                    ThemeChoice::Palette => "palette",
                    ThemeChoice::Dracula => "dracula",
                    ThemeChoice::Terminal => "terminal",
                }
            ),
            Row::Palette => s.palette.as_ref().map_or_else(|| "(none)".into(), |p| tilde(p)),
            Row::Recovery => b(s.recovery),
        }
    }

    /// Step the value: `dir` is +1 / -1 (booleans toggle either way).
    pub fn change(self, s: &mut Settings, dir: i32) {
        fn cycle<T: Copy + PartialEq>(all: &[T], cur: T, dir: i32) -> T {
            let i = all.iter().position(|x| *x == cur).unwrap_or(0) as i32;
            all[(i + dir).rem_euclid(all.len() as i32) as usize]
        }
        match self {
            Row::TabWidth => s.tab_width = (s.tab_width as i32 + dir).clamp(1, 16) as usize,
            Row::Wrap => s.wrap = !s.wrap,
            Row::LineNumbers => s.line_numbers = !s.line_numbers,
            Row::Whitespace => s.show_whitespace = !s.show_whitespace,
            Row::Indent => s.indent = cycle(&[IndentMode::Auto, IndentMode::Tabs, IndentMode::Spaces], s.indent, dir),
            Row::Mouse => s.mouse = !s.mouse,
            Row::Primary => s.primary_selection = !s.primary_selection,
            Row::Theme => {
                s.theme = cycle(
                    &[ThemeChoice::Auto, ThemeChoice::Palette, ThemeChoice::Dracula, ThemeChoice::Terminal],
                    s.theme,
                    dir,
                )
            }
            Row::Recovery => s.recovery = !s.recovery,
            // Edited through a text prompt instead.
            Row::Palette => {}
        }
    }
}

fn tilde(p: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(rest) = p.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    p.display().to_string()
}

/// Clickable parts of the dialog, recorded at render time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefsHit {
    Section(usize),
    Row(usize),
    Dec(usize),
    Inc(usize),
    Save,
    Apply,
    Cancel,
}

pub struct PrefsState {
    pub section: usize,
    pub row: usize,
    pub keys_scroll: usize,
    /// Settings when the dialog opened (Cancel restores these).
    pub original: Settings,
}

impl App {
    pub fn current_settings(&self) -> Settings {
        Settings {
            tab_width: self.tab_width,
            wrap: self.wrap,
            line_numbers: self.line_numbers,
            show_whitespace: self.show_whitespace,
            indent: self.indent_mode,
            mouse: self.mouse_enabled,
            primary_selection: self.primary_selection,
            theme: self.theme_choice,
            palette: self.palette_setting.clone(),
            recovery: self.recovery_enabled,
        }
    }

    /// Apply settings live (used for preview, Apply and Cancel).
    pub fn apply_settings(&mut self, s: &Settings) {
        let old = self.current_settings();
        self.wrap = s.wrap;
        self.line_numbers = s.line_numbers;
        self.show_whitespace = s.show_whitespace;
        self.primary_selection = s.primary_selection;
        if s.tab_width != old.tab_width || s.indent != old.indent {
            self.tab_width = s.tab_width;
            self.indent_mode = s.indent;
            let opts = self.tab_opts();
            for i in 0..self.tab_count() {
                let t = self.tab_at_mut(i);
                t.indent_unit = match opts.indent {
                    IndentMode::Auto => detect_indent(&t.buf, opts.tab_width),
                    IndentMode::Tabs => "\t".into(),
                    IndentMode::Spaces => " ".repeat(opts.tab_width),
                };
            }
        }
        if s.mouse != old.mouse {
            self.mouse_enabled = s.mouse;
            let _ = term::set_mouse(s.mouse);
        }
        if s.theme != old.theme || s.palette != old.palette {
            self.theme_choice = s.theme;
            self.palette_setting = s.palette.clone();
            let (c, note) = colours::decide(s.theme, s.palette.as_deref());
            let (theme, source, warning) = colours::load(c, note);
            self.theme = theme;
            self.theme_source = source;
            self.palette_watch_dirty = true;
            if let Some(w) = warning {
                self.warn(w);
            }
        }
        if s.recovery != old.recovery {
            self.set_recovery_enabled(s.recovery);
        }
    }

    pub fn open_prefs(&mut self) {
        self.prefs = Some(PrefsState { section: 0, row: 0, keys_scroll: 0, original: self.current_settings() });
    }

    fn prefs_rows(&self) -> &'static [Row] {
        self.prefs.as_ref().map_or(&[], |p| Section::ALL[p.section].rows())
    }

    fn prefs_change(&mut self, row: Row, dir: i32) {
        if row == Row::Palette {
            if dir == 0 {
                self.open_palette_prompt();
            }
            return;
        }
        let mut s = self.current_settings();
        row.change(&mut s, dir);
        self.apply_settings(&s);
    }

    pub fn open_palette_prompt(&mut self) {
        let input = self.palette_setting.as_ref().map(|p| tilde(p)).unwrap_or_default();
        self.prompt = Some(crate::app::Prompt { kind: crate::app::PromptKind::PalettePath, input });
    }

    /// Prompt result for the palette path ("" clears it).
    pub fn set_palette_path(&mut self, input: &str) {
        let mut s = self.current_settings();
        s.palette = (!input.trim().is_empty()).then(|| crate::app::expand_tilde(input.trim()));
        if s.palette.is_some() && s.theme == ThemeChoice::Dracula {
            s.theme = ThemeChoice::Palette;
        }
        self.apply_settings(&s);
    }

    pub fn on_prefs_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let Some(p) = &mut self.prefs else { return };
        let section = Section::ALL[p.section];
        let rows = section.rows();
        let row = rows.get(p.row).copied();
        match k.code {
            KeyCode::Esc => self.prefs_finish(PrefsHit::Cancel),
            KeyCode::Char('s') if ctrl || alt => self.prefs_finish(PrefsHit::Save),
            KeyCode::Char('a') if alt => self.prefs_finish(PrefsHit::Apply),
            KeyCode::Tab | KeyCode::PageDown => {
                p.section = (p.section + 1) % Section::ALL.len();
                p.row = 0;
            }
            KeyCode::BackTab | KeyCode::PageUp => {
                p.section = (p.section + Section::ALL.len() - 1) % Section::ALL.len();
                p.row = 0;
            }
            KeyCode::Up if section == Section::Keys => p.keys_scroll = p.keys_scroll.saturating_sub(1),
            KeyCode::Down if section == Section::Keys => {
                p.keys_scroll = (p.keys_scroll + 1).min(Command::ALL.len().saturating_sub(1))
            }
            KeyCode::Up => p.row = p.row.saturating_sub(1),
            KeyCode::Down => p.row = (p.row + 1).min(rows.len().saturating_sub(1)),
            KeyCode::Left | KeyCode::Char('-') => {
                if let Some(r) = row {
                    self.prefs_change(r, -1);
                }
            }
            KeyCode::Right | KeyCode::Char('+') => {
                if let Some(r) = row {
                    self.prefs_change(r, 1);
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(r) = row {
                    self.prefs_change(r, if r == Row::Palette { 0 } else { 1 });
                }
            }
            KeyCode::Delete | KeyCode::Backspace if row == Some(Row::Palette) => self.set_palette_path(""),
            _ => {}
        }
    }

    pub fn on_prefs_click(&mut self, hit: PrefsHit) {
        let Some(p) = &mut self.prefs else { return };
        match hit {
            PrefsHit::Section(i) => {
                p.section = i;
                p.row = 0;
            }
            PrefsHit::Row(i) | PrefsHit::Inc(i) | PrefsHit::Dec(i) => {
                p.row = i;
                let dir = match hit {
                    PrefsHit::Dec(_) => -1,
                    PrefsHit::Row(_) if self.prefs_rows().get(i) == Some(&Row::Palette) => 0,
                    _ => 1,
                };
                if let Some(&r) = self.prefs_rows().get(i) {
                    self.prefs_change(r, dir);
                }
            }
            PrefsHit::Save | PrefsHit::Apply | PrefsHit::Cancel => self.prefs_finish(hit),
        }
    }

    fn prefs_finish(&mut self, how: PrefsHit) {
        let Some(p) = self.prefs.take() else { return };
        match how {
            PrefsHit::Cancel => {
                self.apply_settings(&p.original);
                self.info("Preferences unchanged");
            }
            PrefsHit::Apply => self.info("Preferences applied for this session"),
            _ => match self.save_settings_to_config() {
                Ok(path) => self.info(format!("Preferences saved to {}", tilde(&path))),
                Err(e) => {
                    // Keep the dialog open so nothing is lost.
                    self.prefs = Some(p);
                    self.warn(format!("Could not save preferences: {e:#}"));
                }
            },
        }
    }

    /// Write the current settings into config.toml, keeping the user's
    /// comments, layout, key bindings and unknown keys.
    pub fn save_settings_to_config(&mut self) -> anyhow::Result<PathBuf> {
        let path = self.config_path.clone().ok_or_else(|| anyhow::anyhow!("no config location (HOME unset)"))?;
        let existing = match std::fs::read_to_string(&path) {
            Ok(t) => Some(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => anyhow::bail!("cannot read {}: {e}", path.display()),
        };
        let mut doc: DocumentMut = match &existing {
            Some(t) => t
                .parse()
                .map_err(|e| anyhow::anyhow!("{} has a syntax error; fix it first ({e})", path.display()))?,
            None => config::default_text().parse().expect("default config parses"),
        };
        write_settings(&mut doc, &self.current_settings(), existing.is_some());
        let text = doc.to_string();
        if existing.as_deref() == Some(text.as_str()) {
            return Ok(path);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Keep the user's own pre-Plica file once, the first time we write it.
        let bak = path.with_extension("toml.bak");
        if let Some(orig) = &existing
            && !bak.exists()
        {
            std::fs::write(&bak, orig)?;
        }
        let mut d = crate::document::Document::untitled();
        d.path = Some(path.clone());
        d.save(&text)?;
        Ok(path)
    }
}

/// Set each setting in `doc`. For an existing file, a key is only added if
/// it differs from the default (keys already present are always updated),
/// so saving doesn't fill the user's file with defaults.
pub fn write_settings(doc: &mut DocumentMut, s: &Settings, existing_file: bool) {
    let d = Settings::from_config(&config::Config::default());
    if !doc.contains_key("version") {
        doc["version"] = value(config::SCHEMA_VERSION);
    }
    let indent = |m: IndentMode| match m {
        IndentMode::Auto => "auto",
        IndentMode::Tabs => "tabs",
        IndentMode::Spaces => "spaces",
    };
    let theme = |t: ThemeChoice| match t {
        ThemeChoice::Auto => "auto",
        ThemeChoice::Palette => "palette",
        ThemeChoice::Dracula => "dracula",
        ThemeChoice::Terminal => "terminal",
    };
    let mut set = |section: &str, key: &str, v: toml_edit::Value, is_default: bool| {
        let present = doc.get(section).and_then(|t| t.get(key)).is_some();
        if existing_file && is_default && !present {
            return;
        }
        let table = doc.entry(section).or_insert_with(|| Item::Table(Table::new()));
        let Some(table) = table.as_table_like_mut() else { return };
        match table.get_mut(key).and_then(Item::as_value_mut) {
            Some(old) => {
                // Keep the line's trailing comment and spacing.
                let decor = old.decor().clone();
                *old = v;
                *old.decor_mut() = decor;
            }
            None => {
                table.insert(key, Item::Value(v));
            }
        }
    };
    set("editor", "tab_width", (s.tab_width as i64).into(), s.tab_width == d.tab_width);
    set("editor", "wrap", s.wrap.into(), s.wrap == d.wrap);
    set("editor", "line_numbers", s.line_numbers.into(), s.line_numbers == d.line_numbers);
    set("editor", "show_whitespace", s.show_whitespace.into(), s.show_whitespace == d.show_whitespace);
    set("editor", "indent", indent(s.indent).into(), s.indent == d.indent);
    set("interaction", "mouse", s.mouse.into(), s.mouse == d.mouse);
    set("interaction", "primary_selection", s.primary_selection.into(), s.primary_selection == d.primary_selection);
    set("appearance", "theme", theme(s.theme).into(), s.theme == d.theme);
    set("files", "recovery", s.recovery.into(), s.recovery == d.recovery);
    match &s.palette {
        Some(p) => set("appearance", "palette", tilde(p).into(), false),
        None => {
            if let Some(t) = doc.get_mut("appearance").and_then(Item::as_table_like_mut) {
                t.remove("palette");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_preserves_comments_and_only_adds_changed_keys() {
        let user = "# my config\nversion = 1\n\n[editor]\nwrap = true   # I like wrap\n\n[keys]\nsave = \"f2\"\n";
        let mut doc: DocumentMut = user.parse().unwrap();
        let mut s = Settings::from_config(&config::Config::default());
        s.wrap = false;
        s.tab_width = 8;
        write_settings(&mut doc, &s, true);
        let out = doc.to_string();
        assert!(out.contains("# my config"));
        assert!(out.contains("wrap = false   # I like wrap"), "{out}");
        assert!(out.contains("tab_width = 8"));
        assert!(out.contains("save = \"f2\""), "key bindings untouched");
        assert!(!out.contains("line_numbers"), "defaults not added: {out}");
        assert!(!out.contains("[interaction]"), "no empty sections added: {out}");
        // And it reads back as intended.
        let (cfg, w) = config::parse(&out);
        assert!(w.is_empty(), "{w:?}");
        assert!(!cfg.wrap);
        assert_eq!(cfg.tab_width, 8);
    }

    #[test]
    fn palette_set_and_cleared() {
        let mut doc: DocumentMut = "version = 1\n".parse().unwrap();
        let mut s = Settings::from_config(&config::Config::default());
        s.palette = Some(PathBuf::from("/etc/plica/p.conf"));
        write_settings(&mut doc, &s, true);
        assert!(doc.to_string().contains("palette = \"/etc/plica/p.conf\""));
        s.palette = None;
        write_settings(&mut doc, &s, true);
        assert!(!doc.to_string().contains("palette ="));
    }

    #[test]
    fn rows_cycle_and_clamp() {
        let mut s = Settings::from_config(&config::Config::default());
        Row::TabWidth.change(&mut s, -1);
        Row::TabWidth.change(&mut s, -10);
        assert_eq!(s.tab_width, 1, "clamped at the minimum");
        for _ in 0..20 {
            Row::TabWidth.change(&mut s, 1);
        }
        assert_eq!(s.tab_width, 16);
        Row::Theme.change(&mut s, -1);
        assert_eq!(s.theme, ThemeChoice::Terminal, "wraps backwards");
        Row::Recovery.change(&mut s, 1);
        assert!(!s.recovery);
    }
}
