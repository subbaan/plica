//! Watches a palette file for changes, robust to atomic replacement.
//!
//! Tools like ThemeWall may rewrite the file in place or replace it via
//! rename. Watching the file's inode would lose track after a rename, so we
//! watch the **parent directory** (and the symlink target's directory, if the
//! path is a symlink) and filter events by file name. The caller debounces.

use notify::event::{AccessKind, AccessMode, ModifyKind};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

pub struct PaletteWatcher {
    _watcher: RecommendedWatcher,
}

/// Message sent when the watched file may have changed.
pub struct PaletteTouched;

impl PaletteWatcher {
    pub fn new(path: &Path, tx: Sender<PaletteTouched>) -> notify::Result<Self> {
        let mut names: Vec<PathBuf> = vec![path.to_path_buf()];
        if let Ok(real) = std::fs::canonicalize(path)
            && real != path
        {
            names.push(real);
        }
        let targets = names.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            // Ignore open/read/close-nowrite: our own reload reads the file,
            // and reacting to that would loop forever.
            if !matches!(
                ev.kind,
                EventKind::Create(_)
                    | EventKind::Modify(_)
                    | EventKind::Remove(_)
                    | EventKind::Access(AccessKind::Close(AccessMode::Write))
                    | EventKind::Any
            ) {
                return;
            }
            if let EventKind::Modify(ModifyKind::Metadata(_)) = ev.kind {
                return;
            }
            let hit = ev.paths.iter().any(|p| {
                targets.iter().any(|t| p == t || (p.file_name() == t.file_name() && p.parent() == t.parent()))
            });
            if hit {
                let _ = tx.send(PaletteTouched);
            }
        })?;
        let mut watched_dirs: Vec<PathBuf> = Vec::new();
        for n in &names {
            let dir = n.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
            if !watched_dirs.iter().any(|d| d == dir) {
                watcher.watch(dir, RecursiveMode::NonRecursive)?;
                watched_dirs.push(dir.to_path_buf());
            }
        }
        Ok(Self { _watcher: watcher })
    }
}
