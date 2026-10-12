//! User configuration: `$XDG_CONFIG_HOME/plica/config.toml`.
//!
//! Plica only writes this file when the user presses Save in Preferences
//! (see prefs.rs, which keeps their comments). Loading is resilient: each setting is
//! validated on its own, and a bad value or unknown key produces a warning
//! while that setting keeps its default. A syntax error falls back to all
//! defaults (with a warning). Plica always starts.

use crate::highlight::{Colour, Group, Highlights};
use crate::keymap::{Chord, Command, Keymap};
use crate::theme::{Builtin, OverrideRole};
use std::path::{Path, PathBuf};
use toml::{Table, Value};

pub const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndentMode {
    /// Detect per file (tabs unless space indentation dominates).
    Auto,
    Tabs,
    Spaces,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeChoice {
    /// The palette file if one is configured or found, else Dracula.
    Auto,
    Palette,
    Builtin(Builtin),
    Terminal,
}

impl ThemeChoice {
    /// Every choice, in the order Preferences cycles through them.
    pub fn all() -> Vec<ThemeChoice> {
        let mut v = vec![ThemeChoice::Auto, ThemeChoice::Palette];
        v.extend(Builtin::ALL.map(ThemeChoice::Builtin));
        v.push(ThemeChoice::Terminal);
        v
    }

    pub fn name(self) -> &'static str {
        match self {
            ThemeChoice::Auto => "auto",
            ThemeChoice::Palette => "palette",
            ThemeChoice::Builtin(b) => b.config_name(),
            ThemeChoice::Terminal => "terminal",
        }
    }

    pub fn from_name(s: &str) -> Option<ThemeChoice> {
        Self::all().into_iter().find(|c| c.name() == s)
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub tab_width: usize,
    pub wrap: bool,
    pub line_numbers: bool,
    pub show_whitespace: bool,
    pub indent: IndentMode,
    pub mouse: bool,
    pub primary_selection: bool,
    pub theme: ThemeChoice,
    pub palette: Option<PathBuf>,
    pub high_contrast: bool,
    pub colour_overrides: Vec<(OverrideRole, Colour)>,
    pub recovery: bool,
    pub keymap_overrides: Vec<(Command, Vec<Chord>)>,
    pub highlights: Highlights,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tab_width: 4,
            wrap: true,
            line_numbers: true,
            show_whitespace: false,
            indent: IndentMode::Auto,
            mouse: true,
            primary_selection: true,
            theme: ThemeChoice::Auto,
            palette: None,
            high_contrast: false,
            colour_overrides: Vec::new(),
            recovery: true,
            keymap_overrides: Vec::new(),
            highlights: Highlights::default(),
        }
    }
}

impl Config {
    pub fn keymap(&self) -> Keymap {
        let mut km = Keymap::defaults();
        for (cmd, chords) in &self.keymap_overrides {
            km.set(*cmd, chords.clone());
        }
        km
    }
}

pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))?;
    Some(base.join("plica").join("config.toml"))
}

/// Load from `path`. A missing file is not an error (defaults, no warnings).
pub fn load(path: &Path) -> (Config, Vec<String>) {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Config::default(), Vec::new()),
        Err(e) => (Config::default(), vec![format!("cannot read {}: {e}; using defaults", path.display())]),
    }
}

pub fn parse(text: &str) -> (Config, Vec<String>) {
    let mut cfg = Config::default();
    let mut w = Vec::new();
    let table: Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            let msg = e.to_string();
            let first = msg.lines().next().unwrap_or("syntax error").to_string();
            return (cfg, vec![format!("config syntax error ({first}); using defaults")]);
        }
    };

    for (key, val) in &table {
        match key.as_str() {
            "version" => match val.as_integer() {
                Some(v) if v == SCHEMA_VERSION => {}
                Some(v) if v > SCHEMA_VERSION => w.push(format!(
                    "config version {v} is newer than this Plica understands ({SCHEMA_VERSION}); reading what it can"
                )),
                _ => w.push(format!("`version` should be {SCHEMA_VERSION}")),
            },
            "editor" => section(val, "editor", &mut w, |k, v, w| match k {
                "tab_width" => known(int(v, "editor.tab_width", 1, 16, w).map(|n| cfg.tab_width = n as usize)),
                "wrap" => known(boolean(v, "editor.wrap", w).map(|b| cfg.wrap = b)),
                "line_numbers" => known(boolean(v, "editor.line_numbers", w).map(|b| cfg.line_numbers = b)),
                "show_whitespace" => known(boolean(v, "editor.show_whitespace", w).map(|b| cfg.show_whitespace = b)),
                "indent" => known(choice(v, "editor.indent", &["auto", "tabs", "spaces"], w)
                    .map(|s| {
                        cfg.indent = match s {
                            "tabs" => IndentMode::Tabs,
                            "spaces" => IndentMode::Spaces,
                            _ => IndentMode::Auto,
                        }
                    })
                    ),
                _ => false,
            }),
            "interaction" => section(val, "interaction", &mut w, |k, v, w| match k {
                "mouse" => known(boolean(v, "interaction.mouse", w).map(|b| cfg.mouse = b)),
                "primary_selection" => {
                    known(boolean(v, "interaction.primary_selection", w).map(|b| cfg.primary_selection = b))
                }
                _ => false,
            }),
            "appearance" => section(val, "appearance", &mut w, |k, v, w| match k {
                "theme" => {
                    let names: Vec<&str> = ThemeChoice::all().iter().map(|c| c.name()).collect();
                    known(choice(v, "appearance.theme", &names, w).and_then(ThemeChoice::from_name).map(|c| cfg.theme = c))
                }
                "high_contrast" => known(boolean(v, "appearance.high_contrast", w).map(|b| cfg.high_contrast = b)),
                "colours" | "colors" => {
                    match v.as_table() {
                        Some(t) => {
                            for (role, c) in t {
                                let Some(r) = OverrideRole::from_name(role) else {
                                    w.push(format!(
                                        "unknown colour role `appearance.colours.{role}` (roles: {})",
                                        OverrideRole::ALL.map(|r| r.name()).join(", ")
                                    ));
                                    continue;
                                };
                                match c.as_str().map(Colour::parse) {
                                    Some(Ok(c)) => cfg.colour_overrides.push((r, c)),
                                    Some(Err(e)) => w.push(format!("`appearance.colours.{role}`: {e}")),
                                    None => w.push(format!("`appearance.colours.{role}` should be a colour string")),
                                }
                            }
                        }
                        None => w.push("`appearance.colours` should be a table".into()),
                    }
                    true
                }
                "palette" => match v.as_str() {
                    Some(s) if !s.trim().is_empty() => {
                        cfg.palette = Some(expand_tilde(s.trim()));
                        true
                    }
                    _ => {
                        w.push("`appearance.palette` should be a file path".into());
                        true
                    }
                },
                _ => false,
            }),
            "files" => section(val, "files", &mut w, |k, v, w| match k {
                "recovery" => known(boolean(v, "files.recovery", w).map(|b| cfg.recovery = b)),
                _ => false,
            }),
            "highlights" => section(val, "highlights", &mut w, |k, v, w| match k {
                "enabled" => known(boolean(v, "highlights.enabled", w).map(|b| cfg.highlights.enabled = b)),
                "samples" => known(boolean(v, "highlights.samples", w).map(|b| cfg.highlights.samples = b)),
                "group" => {
                    match v.as_array() {
                        Some(a) => {
                            for (i, item) in a.iter().enumerate() {
                                if let Some(g) = parse_group(item, i, w) {
                                    cfg.highlights.groups.push(g);
                                }
                            }
                        }
                        None => w.push("`highlights.group` should be [[highlights.group]] tables".into()),
                    }
                    true
                }
                _ => false,
            }),
            "keys" => match val.as_table() {
                Some(t) => {
                    for (name, v) in t {
                        let Some(cmd) = Command::from_name(name) else {
                            w.push(format!("unknown command `keys.{name}`"));
                            continue;
                        };
                        let list: Vec<&Value> = match v {
                            Value::String(_) => vec![v],
                            Value::Array(a) => a.iter().collect(),
                            _ => {
                                w.push(format!("`keys.{name}` should be a key or a list of keys"));
                                continue;
                            }
                        };
                        let mut chords = Vec::new();
                        for item in list {
                            match item.as_str().map(Chord::parse) {
                                Some(Ok(c)) => chords.push(c),
                                Some(Err(e)) => w.push(format!("`keys.{name}`: {e}")),
                                None => w.push(format!("`keys.{name}` entries should be strings")),
                            }
                        }
                        cfg.keymap_overrides.push((cmd, chords));
                    }
                }
                None => w.push("`keys` should be a table".into()),
            },
            other => w.push(format!("unknown setting `{other}`")),
        }
    }
    (cfg, w)
}

/// One `[[highlights.group]]`. A group without usable terms is dropped
/// (with a warning); other bad fields keep their defaults.
pub(crate) fn parse_group(v: &Value, i: usize, w: &mut Vec<String>) -> Option<Group> {
    let Some(t) = v.as_table() else {
        w.push(format!("`highlights.group` entry {} should be a table", i + 1));
        return None;
    };
    let mut g = Group::new(&format!("Group {}", i + 1));
    if let Some(n) = t.get("name").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
        g.name = n.trim().into();
    }
    let label = format!("highlights.group \"{}\"", g.name);
    let strings = |v: &Value, field: &str, w: &mut Vec<String>| -> Vec<String> {
        let list: Vec<&Value> = match v {
            Value::String(_) => vec![v],
            Value::Array(a) => a.iter().collect(),
            _ => {
                w.push(format!("`{label}` {field} should be a string or a list of strings"));
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        for item in list {
            match item.as_str() {
                Some(s) if !s.is_empty() => out.push(s.to_string()),
                Some(_) => {}
                None => w.push(format!("`{label}` {field}: {item} is not a string")),
            }
        }
        out
    };
    let colour = |v: &Value, field: &str, w: &mut Vec<String>| match v.as_str().filter(|s| !s.trim().is_empty()).map(Colour::parse) {
        None if v.as_str().is_some() => None,
        Some(Ok(c)) => Some(c),
        Some(Err(e)) => {
            w.push(format!("`{label}` {field}: {e}"));
            None
        }
        None => {
            w.push(format!("`{label}` {field} should be a colour string"));
            None
        }
    };
    for (k, v) in t {
        let flag = |w: &mut Vec<String>| boolean(v, &format!("{label}.{k}"), w);
        match k.as_str() {
            "name" => {}
            "terms" => g.terms = strings(v, "terms", w),
            "fg" | "foreground" => g.fg = colour(v, "fg", w),
            "bg" | "background" => g.bg = colour(v, "bg", w),
            "bold" => g.bold = flag(w).unwrap_or(g.bold),
            "italic" => g.italic = flag(w).unwrap_or(g.italic),
            "underline" => g.underline = flag(w).unwrap_or(g.underline),
            "case_sensitive" => g.case_sensitive = flag(w).unwrap_or(g.case_sensitive),
            "whole_word" => g.whole_word = flag(w).unwrap_or(g.whole_word),
            "enabled" => g.enabled = flag(w).unwrap_or(g.enabled),
            "file_types" => {
                g.file_types = strings(v, "file_types", w)
                    .into_iter()
                    .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
                    .collect()
            }
            other => w.push(format!("unknown setting `{label}.{other}`")),
        }
    }
    if g.terms.is_empty() {
        w.push(format!("`{label}` has no terms; ignored"));
        return None;
    }
    if g.fg.is_none() && g.bg.is_none() && !g.bold && !g.italic && !g.underline {
        // Nothing to show: default to the accent colour.
        g.fg = Some(Colour::Role(crate::highlight::Role::Accent));
    }
    Some(g)
}

/// A recognised key: its value was applied or a warning was recorded.
fn known<T>(_: Option<T>) -> bool {
    true
}

/// Walk a `[section]`; `f` returns false for unknown keys.
fn section(val: &Value, name: &str, w: &mut Vec<String>, mut f: impl FnMut(&str, &Value, &mut Vec<String>) -> bool) {
    let Some(t) = val.as_table() else {
        w.push(format!("`{name}` should be a table"));
        return;
    };
    for (k, v) in t {
        if !f(k, v, w) {
            w.push(format!("unknown setting `{name}.{k}`"));
        }
    }
}

fn boolean(v: &Value, name: &str, w: &mut Vec<String>) -> Option<bool> {
    let b = v.as_bool();
    if b.is_none() {
        w.push(format!("`{name}` is {v}; should be true or false"));
    }
    b
}

fn int(v: &Value, name: &str, lo: i64, hi: i64, w: &mut Vec<String>) -> Option<i64> {
    match v.as_integer() {
        Some(n) if (lo..=hi).contains(&n) => Some(n),
        _ => {
            w.push(format!("`{name}` is {v}; should be a whole number from {lo} to {hi}"));
            None
        }
    }
}

fn choice<'a>(v: &'a Value, name: &str, options: &[&str], w: &mut Vec<String>) -> Option<&'a str> {
    match v.as_str() {
        Some(s) if options.contains(&s) => Some(s),
        _ => {
            w.push(format!("`{name}` is {v}; should be one of: {}", options.join(", ")));
            None
        }
    }
}

fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return Path::new(&home).join(rest);
    }
    PathBuf::from(p)
}

/// A commented default config, for `plica --print-config`.
pub fn default_text() -> String {
    let km = Keymap::defaults();
    let mut keys = String::new();
    for &c in Command::ALL {
        let chords: Vec<String> = km.chords(c).iter().map(|ch| format!("\"{}\"", chord_config_name(ch))).collect();
        keys.push_str(&format!("# {:<20} = [{}]   # {}\n", c.name(), chords.join(", "), c.describe()));
    }
    format!(
        r##"# Plica configuration ({path})
# Plica writes this file only when you press Save in Preferences, keeping your
# comments. Lines starting with # are comments.
version = {SCHEMA_VERSION}

[editor]
tab_width = 4            # 1..16
wrap = true              # soft wrap long lines
line_numbers = true
show_whitespace = false
indent = "auto"          # auto (detect per file) | tabs | spaces

[interaction]
mouse = true             # false: leave the mouse to the terminal
primary_selection = true # select to copy, middle-click to paste (X11/Wayland)

[appearance]
theme = "auto"           # auto | palette | dracula | gruvbox | nord | solarized-dark | solarized-light | terminal
# palette = "~/.config/kitty/current-theme.conf"   # Kitty-format colour file, reloaded live
high_contrast = false    # raise text contrast to 7:1 (WCAG AAA)

# Override individual colour roles on top of any theme or palette:
# "#rrggbb", a palette slot "color0".."color15", or another role (e.g.
# "accent"). Slots and roles follow the live palette. Roles: foreground,
# background, muted, accent, accent_text, selection, selection_text, search,
# search_text, bar, bar_text, current_line, line_numbers, warning, control_chars.
# [appearance.colours]
# current_line = "color0"
# accent = "#e5a50a"

[files]
recovery = true          # snapshot unsaved work for crash recovery

[highlights]
enabled = true           # word and phrase highlighting
samples = true           # built-in TODO / FIXME / IMPORTANT / IDEA / DONE groups

# Your own groups; earlier groups win where matches overlap. A group named
# like a sample (e.g. "Todo") replaces it. Colours: "#rrggbb", a palette slot
# "color0".."color15", or a role: foreground, background, accent, selection,
# search, muted, warning. Slots and roles follow the live palette.
# [[highlights.group]]
# name = "Names"
# terms = ["Alice", "Bob", "release day"]
# fg = "color4"
# bg = "color0"          # optional background ("" = none)
# bold = true            # also: italic, underline
# whole_word = true      # default true
# case_sensitive = false # default false
# file_types = ["md", "txt"]   # default: every file
#
# Alt+H (or F8) highlights the word under the caret in the current document
# only; those rules are kept in documents.toml next to this file.

[keys]
# Rebind commands: a key or a list of keys; [] unbinds. Examples:
# save_as = ["ctrl+shift+s", "f12"]
# quit = "ctrl+q"
# Commands and their default keys:
{keys}"##,
        path = default_path().map(|p| p.display().to_string()).unwrap_or_else(|| "config.toml".into())
    )
}

fn chord_config_name(c: &Chord) -> String {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    let mut s = String::new();
    if c.mods.contains(KeyModifiers::CONTROL) {
        s.push_str("ctrl+");
    }
    if c.mods.contains(KeyModifiers::ALT) {
        s.push_str("alt+");
    }
    if c.mods.contains(KeyModifiers::SHIFT) {
        s.push_str("shift+");
    }
    match c.code {
        KeyCode::Char(ch) => s.push(ch),
        KeyCode::F(n) => s.push_str(&format!("f{n}")),
        KeyCode::PageUp => s.push_str("pageup"),
        KeyCode::PageDown => s.push_str("pagedown"),
        KeyCode::Tab => s.push_str("tab"),
        KeyCode::Up => s.push_str("up"),
        KeyCode::Down => s.push_str("down"),
        other => s.push_str(&format!("{other:?}").to_ascii_lowercase()),
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_text_parses_cleanly_to_defaults() {
        let (cfg, w) = parse(&default_text());
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(cfg.tab_width, 4);
        assert!(cfg.keymap_overrides.is_empty());
    }

    #[test]
    fn values_are_validated_individually() {
        let (cfg, w) = parse(
            r#"
version = 1
bogus = 1
[editor]
tab_width = 99
wrap = false
indent = "spaces"
colour = "red"
[appearance]
theme = "neon"
palette = "~/p.conf"
[keys]
save = "f2"
quit = ["ctrl+q", "hyper+q"]
frobnicate = "ctrl+k"
"#,
        );
        assert_eq!(cfg.tab_width, 4, "out of range -> default");
        assert!(!cfg.wrap, "valid value applied");
        assert_eq!(cfg.indent, IndentMode::Spaces);
        assert_eq!(cfg.theme, ThemeChoice::Auto);
        assert!(cfg.palette.as_ref().unwrap().ends_with("p.conf"));
        let km = cfg.keymap();
        assert_eq!(km.label(Command::Save), "F2");
        assert_eq!(km.label(Command::Quit), "Ctrl+Q", "bad chord dropped, good one kept");
        for needle in ["bogus", "tab_width", "colour", "neon", "hyper", "frobnicate"] {
            assert!(w.iter().any(|m| m.contains(needle)), "missing warning for {needle}: {w:?}");
        }
        assert!(!w.iter().any(|m| m.contains("unknown setting `editor.tab_width`")), "bad value is not 'unknown': {w:?}");
    }

    #[test]
    fn highlight_groups() {
        let (cfg, w) = parse(
            r##"
[highlights]
samples = false
[[highlights.group]]
name = "Names"
terms = ["Alice", "release day"]
fg = "color4"
bg = ""
bold = true
file_types = [".MD", "txt"]
[[highlights.group]]
terms = "solo"
fg = "tomato"
italic = "yes"
[[highlights.group]]
name = "Empty"
terms = []
"##,
        );
        let h = &cfg.highlights;
        assert!(!h.samples);
        assert_eq!(h.groups.len(), 2, "group without terms dropped");
        let g = &h.groups[0];
        assert_eq!(g.terms, ["Alice", "release day"]);
        assert_eq!(g.fg, Some(Colour::Slot(4)));
        assert_eq!(g.bg, None);
        assert_eq!(g.file_types, ["md", "txt"]);
        let g = &h.groups[1];
        assert_eq!(g.name, "Group 2");
        assert_eq!(g.fg, Some(Colour::Role(crate::highlight::Role::Accent)), "bad colour, nothing else: accent");
        for needle in ["tomato", "italic", "Empty"] {
            assert!(w.iter().any(|m| m.contains(needle)), "missing warning for {needle}: {w:?}");
        }
        assert_eq!(w.len(), 3, "{w:?}");
    }

    #[test]
    fn themes_contrast_and_overrides() {
        let (cfg, w) = parse(
            "[appearance]\ntheme = \"solarized-light\"\nhigh_contrast = true\n[appearance.colours]\naccent = \"color2\"\nbar = \"#123456\"\nsparkle = \"#fff\"\nmuted = \"nope\"\n",
        );
        assert_eq!(cfg.theme, ThemeChoice::Builtin(Builtin::SolarizedLight));
        assert!(cfg.high_contrast);
        assert_eq!(cfg.colour_overrides.len(), 2);
        assert_eq!(cfg.colour_overrides[0], (OverrideRole::Accent, Colour::Slot(2)));
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].contains("sparkle") || w[1].contains("sparkle"));
        let (cfg, w) = parse("[appearance]\ntheme = \"neon\"\n");
        assert_eq!(cfg.theme, ThemeChoice::Auto);
        assert!(w[0].contains("gruvbox"), "lists valid names: {w:?}");
    }

    #[test]
    fn syntax_error_falls_back_to_defaults() {
        let (cfg, w) = parse("[editor\nwrap = ");
        assert!(cfg.wrap);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("syntax error"));
    }

    #[test]
    fn newer_version_warns_but_reads() {
        let (cfg, w) = parse("version = 7\n[editor]\nwrap = false\n");
        assert!(!cfg.wrap);
        assert!(w[0].contains("newer"));
    }
}
