//! User configuration: `$XDG_CONFIG_HOME/plica/config.toml`.
//!
//! Plica never writes this file. Loading is resilient: each setting is
//! validated on its own, and a bad value or unknown key produces a warning
//! while that setting keeps its default. A syntax error falls back to all
//! defaults (with a warning). Plica always starts.

use crate::keymap::{Chord, Command, Keymap};
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
    Dracula,
    Terminal,
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
    pub recovery: bool,
    pub keymap_overrides: Vec<(Command, Vec<Chord>)>,
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
            recovery: true,
            keymap_overrides: Vec::new(),
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
                "theme" => known(choice(v, "appearance.theme", &["auto", "palette", "dracula", "terminal"], w)
                    .map(|s| {
                        cfg.theme = match s {
                            "palette" => ThemeChoice::Palette,
                            "dracula" => ThemeChoice::Dracula,
                            "terminal" => ThemeChoice::Terminal,
                            _ => ThemeChoice::Auto,
                        }
                    })
                    ),
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
        r#"# Plica configuration ({path})
# Plica only reads this file. Lines starting with # are comments.
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
theme = "auto"           # auto | palette | dracula | terminal
# palette = "~/.config/kitty/current-theme.conf"   # Kitty-format colour file, reloaded live

[files]
recovery = true          # snapshot unsaved work for crash recovery

[keys]
# Rebind commands: a key or a list of keys; [] unbinds. Examples:
# save_as = ["ctrl+shift+s", "f12"]
# quit = "ctrl+q"
# Commands and their default keys:
{keys}"#,
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
