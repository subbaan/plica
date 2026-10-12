//! Choosing where colours come from (palette file, Dracula, terminal
//! defaults) and loading them. Used at startup and when Preferences change
//! the theme.

use crate::app::ThemeSource;
use crate::config::ThemeChoice;
use crate::theme::{self, Adjust, Builtin, Theme};
use std::path::{Path, PathBuf};

pub enum Colours {
    Builtin(Builtin),
    Terminal,
    Palette(PathBuf),
}

/// Candidate shared desktop palette (only if it exists).
pub fn kitty_palette() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))?;
    let candidate = config.join("kitty").join("current-theme.conf");
    candidate.is_file().then_some(candidate)
}

/// The config-level decision (CLI/environment overrides are applied by the
/// caller first). Returns the choice and an optional note for the user.
pub fn decide(choice: ThemeChoice, palette: Option<&Path>) -> (Colours, Option<String>) {
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    match choice {
        ThemeChoice::Builtin(b) => (Colours::Builtin(b), None),
        ThemeChoice::Terminal => (Colours::Terminal, None),
        ThemeChoice::Palette => match palette.map(Path::to_path_buf).or_else(kitty_palette) {
            Some(p) => (Colours::Palette(p), None),
            None => (Colours::Builtin(Builtin::Dracula), Some("Theme \"palette\" but no palette file is set or found; using Dracula".into())),
        },
        ThemeChoice::Auto => match palette {
            Some(p) => (Colours::Palette(p.to_path_buf()), None),
            None if no_color => (Colours::Terminal, None),
            None => (kitty_palette().map_or(Colours::Builtin(Builtin::Dracula), Colours::Palette), None),
        },
    }
}

/// Load the chosen colours. An unusable palette falls back to Dracula but
/// stays the source (so a later fix is picked up by the watcher).
pub fn load(c: Colours, note: Option<String>, adj: &Adjust) -> (Theme, ThemeSource, Option<String>) {
    let dracula = || theme::resolve_with(&theme::dracula(), adj);
    match c {
        Colours::Builtin(b) => (theme::resolve_with(&b.palette(), adj), ThemeSource::Builtin(b), note),
        Colours::Terminal => (dracula(), ThemeSource::TerminalDefault, note),
        Colours::Palette(path) => {
            let path = std::path::absolute(&path).unwrap_or(path);
            let parsed = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| theme::parse_kitty(&t).map_err(|e| e.to_string()));
            match parsed {
                Ok(r) => (theme::resolve_with(&r.palette, adj), ThemeSource::File { path, last_error: None }, note),
                Err(e) => (
                    dracula(),
                    ThemeSource::File { path: path.clone(), last_error: Some(e.clone()) },
                    Some(format!("Palette {} unusable ({e}); using Dracula until it is fixed", path.display())),
                ),
            }
        }
    }
}
