//! Recent files and where you were in each: the caret, the first visible
//! line, and a language or encoding chosen by hand. Stored in
//! `$XDG_STATE_HOME/plica/recent.toml`, never in the files themselves.
//!
//! Entries are keyed by each file's canonical path. Changes collect in
//! `pending` and are merged into the file on `flush`, which re-reads it
//! first, so two Plica windows don't drop each other's entries (the newer
//! entry for a path wins). Files that disappeared are only noticed when one
//! is chosen, so listing never touches the disk for each path (old drives,
//! network automounts).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use toml_edit::{ArrayOfTables, DocumentMut, Table, value};

/// Most entries kept; the oldest go first.
pub const MAX_ENTRIES: usize = 500;

const HEADER: &str = "\
# Plica: recently opened files and where you were in them.
# Written by Plica; safe to delete. Lines and columns count from 1;
# language = \"\" means plain text was chosen.
";

/// Where the user was in a file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Spot {
    /// Caret line and column (chars into the line), from 0.
    pub line: usize,
    pub column: usize,
    /// First visible line, from 0.
    pub top: usize,
    /// A language chosen by hand: a grammar name, or "" for plain text.
    pub language: Option<String>,
    /// An encoding chosen by hand (`Encoding::config_name`).
    pub encoding: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub spot: Spot,
    /// Last opened or used (unix seconds).
    pub time: u64,
}

pub struct Recent {
    file: PathBuf,
    pending: Vec<Entry>,
    /// Remove everything on disk at the next flush.
    forget: bool,
}

pub fn default_file() -> Option<PathBuf> {
    crate::recovery::default_dir().and_then(|d| Some(d.parent()?.join("recent.toml")))
}

/// The key a file is stored under (symlinks resolved).
pub fn key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).or_else(|_| std::path::absolute(path)).unwrap_or_else(|_| path.to_path_buf())
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Recent {
    pub fn new(file: PathBuf) -> Self {
        Self { file, pending: Vec::new(), forget: false }
    }

    /// Every entry, newest first.
    pub fn list(&self) -> Vec<Entry> {
        let disk = if self.forget { Vec::new() } else { read(&self.file) };
        merge(disk, &self.pending)
    }

    pub fn get(&self, path: &Path) -> Option<Entry> {
        let k = key(path);
        if let Some(e) = self.pending.iter().find(|e| e.path == k) {
            return Some(e.clone());
        }
        self.list().into_iter().find(|e| e.path == k)
    }

    /// Record `spot` for `path` (written at the next `flush`).
    pub fn note(&mut self, path: &Path, spot: Spot) {
        let path = key(path);
        self.pending.retain(|e| e.path != path);
        self.pending.push(Entry { path, spot, time: now() });
    }

    /// Drop one entry (a file that no longer exists).
    pub fn remove(&mut self, path: &Path) -> Result<()> {
        let k = key(path);
        self.flush()?;
        let mut all = read(&self.file);
        all.retain(|e| e.path != k && e.path != path);
        write(&self.file, &all)
    }

    /// Forget every entry, including what's on disk.
    pub fn forget_all(&mut self) -> Result<()> {
        self.pending.clear();
        self.forget = true;
        self.flush()
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty() || self.forget
    }

    /// Merge pending changes into the file.
    pub fn flush(&mut self) -> Result<()> {
        if !self.has_pending() {
            return Ok(());
        }
        let mut all = self.list();
        all.truncate(MAX_ENTRIES);
        write(&self.file, &all)?;
        self.pending.clear();
        self.forget = false;
        Ok(())
    }
}

/// `disk` with `pending` applied (the newer entry per path wins; a tie
/// goes to `pending`), newest first.
fn merge(disk: Vec<Entry>, pending: &[Entry]) -> Vec<Entry> {
    let mut all = disk;
    for p in pending {
        match all.iter_mut().find(|e| e.path == p.path) {
            Some(e) if e.time <= p.time => *e = p.clone(),
            Some(_) => {}
            None => all.push(p.clone()),
        }
    }
    // Stable: equal times keep the file's order.
    all.sort_by_key(|e| std::cmp::Reverse(e.time));
    all
}

/// The entries in `file` (missing or unreadable: none). Bad entries are
/// skipped; this is Plica's own state, so there is nothing to warn about.
fn read(file: &Path) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(file) else { return Vec::new() };
    parse(&text)
}

fn parse(text: &str) -> Vec<Entry> {
    let Ok(table) = text.parse::<toml::Table>() else { return Vec::new() };
    let Some(files) = table.get("file").and_then(|f| f.as_array()) else { return Vec::new() };
    let num = |t: &toml::Value, k: &str| t.get(k).and_then(|v| v.as_integer()).map_or(0, |n| n.max(0) as usize);
    let text = |t: &toml::Value, k: &str| t.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let mut out: Vec<Entry> = Vec::new();
    for f in files {
        let Some(path) = text(f, "path").filter(|p| p.starts_with('/')) else { continue };
        let path = PathBuf::from(path);
        if out.iter().any(|e| e.path == path) {
            continue;
        }
        let spot = Spot {
            line: num(f, "line").saturating_sub(1),
            column: num(f, "column").saturating_sub(1),
            top: num(f, "top").saturating_sub(1),
            language: text(f, "language"),
            encoding: text(f, "encoding").filter(|e| !e.is_empty()),
        };
        out.push(Entry { path, spot, time: num(f, "time") as u64 });
    }
    out
}

fn render(entries: &[Entry]) -> String {
    let mut d: DocumentMut = HEADER.parse().expect("header parses");
    let mut arr = ArrayOfTables::new();
    for e in entries {
        let mut t = Table::new();
        t.insert("path", value(e.path.to_string_lossy().into_owned()));
        t.insert("line", value(e.spot.line as i64 + 1));
        t.insert("column", value(e.spot.column as i64 + 1));
        t.insert("top", value(e.spot.top as i64 + 1));
        if let Some(l) = &e.spot.language {
            t.insert("language", value(l.as_str()));
        }
        if let Some(enc) = &e.spot.encoding {
            t.insert("encoding", value(enc.as_str()));
        }
        t.insert("time", value(e.time as i64));
        arr.push(t);
    }
    d.insert("file", toml_edit::Item::ArrayOfTables(arr));
    d.to_string()
}

fn write(file: &Path, entries: &[Entry]) -> Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let mut out = crate::document::Document::untitled();
    out.path = Some(file.to_path_buf());
    out.save(&render(entries))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, line: usize, time: u64) -> Entry {
        Entry { path: PathBuf::from(path), spot: Spot { line, ..Spot::default() }, time }
    }

    #[test]
    fn round_trip_keeps_every_field() {
        let e = Entry {
            path: PathBuf::from("/home/me/notes \"x\".md"),
            spot: Spot { line: 119, column: 4, top: 100, language: Some("Rust".into()), encoding: Some("windows-1252".into()) },
            time: 1_760_000_000,
        };
        let plain = Entry { spot: Spot { language: Some(String::new()), ..Spot::default() }, ..entry("/tmp/a.txt", 0, 5) };
        let text = render(&[e.clone(), plain.clone()]);
        assert_eq!(parse(&text), vec![e, plain]);
        assert!(text.contains("line = 120"), "lines count from 1 on disk:\n{text}");
    }

    #[test]
    fn bad_entries_are_skipped() {
        let text = "[[file]]\npath = \"relative.txt\"\n[[file]]\nline = 3\n[[file]]\npath = \"/ok\"\nline = -4\n";
        assert_eq!(parse(text), vec![entry("/ok", 0, 0)]);
        assert!(parse("not toml [").is_empty());
    }

    #[test]
    fn newer_entry_wins_and_list_is_newest_first() {
        let disk = vec![entry("/a", 1, 30), entry("/b", 2, 20), entry("/c", 3, 10)];
        let pending = [entry("/b", 9, 40), entry("/a", 7, 5), entry("/d", 4, 15)];
        let m = merge(disk, &pending);
        let order: Vec<_> = m.iter().map(|e| (e.path.to_str().unwrap(), e.spot.line)).collect();
        assert_eq!(order, [("/b", 9), ("/a", 1), ("/d", 4), ("/c", 3)]);
    }

    #[test]
    fn flush_merges_with_what_another_window_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state/recent.toml");
        let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let mut one = Recent::new(file.clone());
        let mut two = Recent::new(file.clone());
        one.note(&a, Spot { line: 3, ..Spot::default() });
        two.note(&b, Spot { line: 5, ..Spot::default() });
        one.flush().unwrap();
        two.flush().unwrap();
        let r = Recent::new(file.clone());
        assert_eq!(r.get(&a).unwrap().spot.line, 3);
        assert_eq!(r.get(&b).unwrap().spot.line, 5);
        assert_eq!(r.list().len(), 2);

        let mut r = r;
        r.remove(&a).unwrap();
        assert!(r.get(&a).is_none());
        r.forget_all().unwrap();
        assert!(Recent::new(file).list().is_empty());
    }

    #[test]
    fn list_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("recent.toml");
        let many: Vec<Entry> = (0..MAX_ENTRIES as u64 + 20).map(|i| entry(&format!("/f{i}"), 0, i)).collect();
        write(&file, &many).unwrap();
        let mut r = Recent::new(file.clone());
        r.note(&dir.path().join("new.txt"), Spot::default());
        r.flush().unwrap();
        let l = Recent::new(file).list();
        assert_eq!(l.len(), MAX_ENTRIES);
        assert!(l[0].path.ends_with("new.txt"));
        assert!(!l.iter().any(|e| e.path == Path::new("/f0")), "the oldest went first");
    }
}
