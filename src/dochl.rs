//! Document-specific highlight groups, stored outside the documents in
//! `documents.toml` next to `config.toml`, keyed by each file's canonical
//! path (so a symlinked path finds the same rules).
//!
//! The file on disk is the source of truth: every change re-reads it,
//! updates only the one document's entry with `toml_edit` (other entries and
//! comments are kept), and writes it atomically. Moving or renaming a file
//! outside Plica loses its rules (Save As carries them over).

use crate::highlight::{Colour, Group, Role};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, value};

pub const MARKED: &str = "Marked";

const HEADER: &str = "\
# Plica: highlight groups for individual documents, by full path.
# Same group format as [[highlights.group]] in config.toml. Edit freely;
# Plica only rewrites the entry of the document you change.
";

#[derive(Debug, Default, Clone)]
pub struct DocRules {
    pub file: Option<PathBuf>,
    map: HashMap<PathBuf, Vec<Group>>,
}

pub fn key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

impl DocRules {
    /// Load `file` (missing = empty). Warnings name the bad entries.
    pub fn load(file: Option<PathBuf>) -> (Self, Vec<String>) {
        let Some(f) = file else { return (Self::default(), Vec::new()) };
        let (map, w) = match std::fs::read_to_string(&f) {
            Ok(t) => parse(&t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (HashMap::new(), Vec::new()),
            Err(e) => (HashMap::new(), vec![format!("cannot read {}: {e}", f.display())]),
        };
        let w = w.into_iter().map(|m| format!("{}: {m}", f.file_name().unwrap_or_default().to_string_lossy())).collect();
        (Self { file: Some(f), map }, w)
    }

    pub fn groups(&self, doc: &Path) -> &[Group] {
        self.map.get(&key(doc)).map_or(&[], Vec::as_slice)
    }

    /// Replace one document's groups (empty removes its entry) and write.
    pub fn set(&mut self, doc: &Path, groups: Vec<Group>) -> Result<()> {
        let file = self.file.clone().context("no config location (HOME unset)")?;
        let k = key(doc);
        let existing = match std::fs::read_to_string(&file) {
            Ok(t) => Some(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => anyhow::bail!("cannot read {}: {e}", file.display()),
        };
        let mut d: DocumentMut = match &existing {
            Some(t) => t.parse().map_err(|e| anyhow::anyhow!("{} has a syntax error; fix it first ({e})", file.display()))?,
            None => HEADER.parse().expect("header parses"),
        };
        write_entry(&mut d, &k, &groups);
        let text = d.to_string();
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut out = crate::document::Document::untitled();
        out.path = Some(file.clone());
        out.save(&text)?;
        // Pick up the whole file again (it may have been edited elsewhere).
        let (map, _) = parse(&text);
        self.map = map;
        Ok(())
    }

    /// Toggle `term` in the document's "Marked" group. Returns true if the
    /// term is now highlighted.
    pub fn toggle_marked(&mut self, doc: &Path, term: &str) -> Result<bool> {
        // Start from what is on disk now.
        let (fresh, _) = Self::load(self.file.clone());
        let mut groups = fresh.groups(doc).to_vec();
        let i = match groups.iter().position(|g| g.name == MARKED) {
            Some(i) => i,
            None => {
                groups.insert(0, marked_group());
                0
            }
        };
        let g = &mut groups[i];
        let on = if let Some(j) = g.terms.iter().position(|t| t.eq_ignore_ascii_case(term)) {
            g.terms.remove(j);
            false
        } else {
            g.terms.push(term.to_string());
            true
        };
        if g.terms.is_empty() {
            groups.remove(i);
        }
        self.set(doc, groups)?;
        Ok(on)
    }

    /// Copy a document's rules to a new path (Save As).
    pub fn carry_over(&mut self, from: &Path, to: &Path) -> Result<()> {
        let g = self.groups(from).to_vec();
        if g.is_empty() || key(from) == key(to) || !self.groups(to).is_empty() {
            return Ok(());
        }
        self.set(to, g)
    }
}

fn marked_group() -> Group {
    Group { bg: Some(Colour::Role(Role::Accent)), case_sensitive: false, whole_word: true, ..Group::new(MARKED) }
}

pub fn parse(text: &str) -> (HashMap<PathBuf, Vec<Group>>, Vec<String>) {
    let mut map = HashMap::new();
    let mut w = Vec::new();
    let table: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            let first = e.to_string().lines().next().unwrap_or("syntax error").to_string();
            return (map, vec![format!("syntax error ({first}); document highlights off")]);
        }
    };
    for (k, v) in &table {
        if k != "document" {
            w.push(format!("unknown setting `{k}`"));
            continue;
        }
        let Some(docs) = v.as_array() else {
            w.push("`document` should be [[document]] tables".into());
            continue;
        };
        for (i, d) in docs.iter().enumerate() {
            let Some(path) = d.get("path").and_then(|p| p.as_str()).filter(|p| p.starts_with('/')) else {
                w.push(format!("document entry {} needs an absolute `path`", i + 1));
                continue;
            };
            let mut groups = Vec::new();
            if let Some(gs) = d.get("group").and_then(|g| g.as_array()) {
                for (j, g) in gs.iter().enumerate() {
                    if let Some(g) = crate::config::parse_group(g, j, &mut w) {
                        groups.push(g);
                    }
                }
            }
            map.insert(PathBuf::from(path), groups);
        }
    }
    (map, w)
}

/// Rewrite (or add, or remove) the `[[document]]` entry for `path`.
fn write_entry(d: &mut DocumentMut, path: &Path, groups: &[Group]) {
    let p = path.to_string_lossy().into_owned();
    if !d.contains_key("document") {
        d["document"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    let Some(arr) = d["document"].as_array_of_tables_mut() else {
        // `document` is something else: replace it rather than fail.
        d["document"] = Item::ArrayOfTables(ArrayOfTables::new());
        return write_entry(d, path, groups);
    };
    let pos = arr.iter().position(|t| t.get("path").and_then(|v| v.as_str()) == Some(p.as_str()));
    if groups.is_empty() {
        if let Some(i) = pos {
            arr.remove(i);
        }
        return;
    }
    let mut gs = ArrayOfTables::new();
    for g in groups {
        gs.push(group_table(g));
    }
    match pos {
        Some(i) => {
            let t = arr.get_mut(i).expect("index from position");
            t.insert("group", Item::ArrayOfTables(gs));
        }
        None => {
            let mut t = Table::new();
            t.insert("path", value(p));
            t.insert("group", Item::ArrayOfTables(gs));
            arr.push(t);
        }
    }
}

/// A group as a TOML table, listing only non-default fields.
pub fn group_table(g: &Group) -> Table {
    let d = Group::new("");
    let mut t = Table::new();
    t.insert("name", value(&g.name));
    let mut terms = Array::new();
    for s in &g.terms {
        terms.push(s.as_str());
    }
    t.insert("terms", value(terms));
    if let Some(c) = g.fg {
        t.insert("fg", value(c.name()));
    }
    if let Some(c) = g.bg {
        t.insert("bg", value(c.name()));
    }
    for (k, v, dv) in [
        ("bold", g.bold, d.bold),
        ("italic", g.italic, d.italic),
        ("underline", g.underline, d.underline),
        ("whole_word", g.whole_word, d.whole_word),
        ("case_sensitive", g.case_sensitive, d.case_sensitive),
        ("enabled", g.enabled, d.enabled),
    ] {
        if v != dv {
            t.insert(k, value(v));
        }
    }
    if !g.file_types.is_empty() {
        let mut a = Array::new();
        for s in &g.file_types {
            a.push(s.as_str());
        }
        t.insert("file_types", value(a));
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_writes_only_that_document_and_keeps_comments() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("documents.toml");
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, "").unwrap();
        std::fs::write(&b, "").unwrap();
        let mut r = DocRules::load(Some(file.clone())).0;
        assert!(r.toggle_marked(&a, "Alice").unwrap());
        assert!(r.toggle_marked(&b, "Bob").unwrap());
        assert!(r.toggle_marked(&a, "Carol").unwrap());
        assert_eq!(r.groups(&a)[0].terms, ["Alice", "Carol"]);
        assert_eq!(r.groups(&a)[0].bg, Some(Colour::Role(Role::Accent)));
        // A hand-written comment survives later writes.
        let text = std::fs::read_to_string(&file).unwrap().replace("[[document]]", "# mine\n[[document]]");
        std::fs::write(&file, text).unwrap();
        assert!(!r.toggle_marked(&a, "alice").unwrap(), "toggling again (any case) removes");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("# mine"), "{text}");
        let (fresh, w) = DocRules::load(Some(file.clone()));
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(fresh.groups(&a)[0].terms, ["Carol"]);
        assert_eq!(fresh.groups(&b)[0].terms, ["Bob"]);
        // Removing the last term removes the entry.
        r.toggle_marked(&b, "Bob").unwrap();
        assert!(!std::fs::read_to_string(&file).unwrap().contains("b.txt"));
    }

    #[test]
    fn symlinked_path_finds_the_same_rules_and_save_as_carries_them() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("documents.toml");
        let a = dir.path().join("a.txt");
        std::fs::write(&a, "").unwrap();
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&a, &link).unwrap();
        let mut r = DocRules::load(Some(file)).0;
        r.toggle_marked(&link, "x").unwrap();
        assert_eq!(r.groups(&a).len(), 1);
        let c = dir.path().join("c.txt");
        std::fs::write(&c, "").unwrap();
        r.carry_over(&a, &c).unwrap();
        assert_eq!(r.groups(&c)[0].terms, ["x"]);
    }

    #[test]
    fn bad_entries_warn() {
        let (m, w) = parse("[[document]]\npath = \"rel.txt\"\n[[document]]\npath = \"/x\"\n[[document.group]]\nterms = []\n");
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(m.get(Path::new("/x")).is_some_and(Vec::is_empty));
    }
}
