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
            if !is_content_event(&ev.kind) {
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

/// Watches the directories of open documents and reports changed paths.
/// Directories are added/removed as tabs open and close (`sync`).
pub struct DocWatcher {
    watcher: RecommendedWatcher,
    dirs: std::collections::BTreeSet<PathBuf>,
}

impl DocWatcher {
    pub fn new(tx: Sender<PathBuf>) -> notify::Result<Self> {
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            if is_content_event(&ev.kind) {
                for p in ev.paths {
                    let _ = tx.send(p);
                }
            }
        })?;
        Ok(Self { watcher, dirs: Default::default() })
    }

    pub fn sync(&mut self, wanted: &std::collections::BTreeSet<PathBuf>) {
        if *wanted == self.dirs {
            return;
        }
        for d in self.dirs.difference(wanted) {
            let _ = self.watcher.unwatch(d);
        }
        for d in wanted.difference(&self.dirs) {
            // A missing directory just isn't watched; focus checks still run.
            let _ = self.watcher.watch(d, RecursiveMode::NonRecursive);
        }
        self.dirs = wanted.clone();
    }
}

/// Events that can mean the file's contents changed. Open/read/close-nowrite
/// are excluded: Plica's own reads would otherwise trigger reload loops.
fn is_content_event(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_)
            | EventKind::Modify(_)
            | EventKind::Remove(_)
            | EventKind::Access(AccessKind::Close(AccessMode::Write))
            | EventKind::Any
    ) && !matches!(kind, EventKind::Modify(ModifyKind::Metadata(_)))
}
