//! Text model: a rope plus grapheme-aware navigation and transactional undo.
//!
//! Offset conventions (see docs/adr/0001-language-and-tui-stack.md):
//! - Positions inside Plica are **char indices into the rope** (`usize`). They
//!   are always kept on grapheme-cluster boundaries by the navigation helpers.
//! - Byte offsets exist only at the IO boundary (`document.rs`).
//! - Terminal display cells exist only in `layout.rs`.

use ropey::{Rope, RopeSlice};
use std::time::{Duration, Instant};
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

/// A single primitive edit, recorded so it can be inverted.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Edit {
    Insert { at: usize, text: String },
    Delete { at: usize, text: String },
}

impl Edit {
    fn change(&self) -> Change {
        match self {
            Edit::Insert { at, text } => Change { at: *at, removed: 0, inserted: text.chars().count() },
            Edit::Delete { at, text } => Change { at: *at, removed: text.chars().count(), inserted: 0 },
        }
    }

    /// The change made by undoing this edit.
    fn inverse_change(&self) -> Change {
        let c = self.change();
        Change { at: c.at, removed: c.inserted, inserted: c.removed }
    }
}

/// One user-visible undo step: a group of edits plus cursor state around it.
#[derive(Debug, Clone)]
struct Transaction {
    id: u64,
    edits: Vec<Edit>,
    sel_before: Selection,
    sel_after: Selection,
    kind: EditKind,
    at: Instant,
}

/// Used to decide whether consecutive edits coalesce into one undo step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    Typing,
    Deleting,
    /// Never coalesces (paste, cut, replace, newline...).
    Other,
}

/// Anchor/head selection. When `anchor == head` there is just a caret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn caret(at: usize) -> Self {
        Self { anchor: at, head: at }
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }
    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }
}

const COALESCE_WINDOW: Duration = Duration::from_millis(1500);

pub struct Buffer {
    rope: Rope,
    pub sel: Selection,
    /// Preferred display column for vertical movement; set by layout-aware code.
    pub preferred_col: Option<usize>,
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    next_tx_id: u64,
    /// Id of the undo-stack top when last saved; dirty == top differs.
    saved_top: Option<u64>,
    /// When true the next edit always starts a fresh undo step.
    break_coalescing: bool,
    /// Unique per buffer, and bumped on every content change: together a
    /// cache key for derived data (search matches).
    id: u64,
    version: u64,
    /// The text each recent version replaced, oldest first, so derived
    /// views can update incrementally (`changes_since`).
    log: std::collections::VecDeque<(u64, Change)>,
}

/// One replacement in char positions: `removed` chars at `at` in the old
/// text became `inserted` chars. Several combine into one that covers them all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    pub at: usize,
    pub removed: usize,
    pub inserted: usize,
}

impl Change {
    /// The single replacement covering `self` followed by `next`.
    fn then(self, next: Change) -> Change {
        let at = self.at.min(next.at);
        let end = (self.at + self.inserted).max(next.at + next.removed);
        Change { at, removed: end - self.inserted + self.removed - at, inserted: end - next.removed + next.inserted - at }
    }
}

/// What changed in a buffer since a cache key was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Since {
    Same,
    Changed(Change),
    /// Another buffer, or too long ago to tell.
    Unknown,
}

const LOG_LEN: usize = 256;

fn compose(edits: impl Iterator<Item = Change>) -> Option<Change> {
    edits.reduce(Change::then)
}

static NEXT_BUFFER_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Buffer {
    pub fn new() -> Self {
        Self::from_str("")
    }

    pub fn from_str(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
            sel: Selection::default(),
            preferred_col: None,
            undo: Vec::new(),
            redo: Vec::new(),
            next_tx_id: 1,
            saved_top: None,
            break_coalescing: false,
            id: NEXT_BUFFER_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            version: 0,
            log: std::collections::VecDeque::new(),
        }
    }

    /// (buffer id, content version): changes whenever the text changes.
    pub fn cache_key(&self) -> (u64, u64) {
        (self.id, self.version)
    }

    /// What changed since `key` was taken from `cache_key`.
    pub fn changes_since(&self, key: (u64, u64)) -> Since {
        if key.0 != self.id || key.1 > self.version {
            return Since::Unknown;
        }
        if key.1 == self.version {
            return Since::Same;
        }
        // Versions key.1+1 ..= self.version must all be in the log.
        match self.log.front() {
            Some(&(v, _)) if v <= key.1 + 1 => {}
            _ => return Since::Unknown,
        }
        match compose(self.log.iter().filter(|(v, _)| *v > key.1).map(|(_, c)| *c)) {
            Some(c) => Since::Changed(c),
            None => Since::Unknown,
        }
    }

    fn log_change(&mut self, edits: impl Iterator<Item = Change>) {
        self.version += 1;
        if let Some(c) = compose(edits) {
            self.log.push_back((self.version, c));
            if self.log.len() > LOG_LEN {
                self.log.pop_front();
            }
        } else {
            // A version without a recorded change: older keys can't be followed.
            self.log.clear();
        }
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn is_dirty(&self) -> bool {
        self.undo.last().map(|t| t.id) != self.saved_top
    }

    pub fn mark_saved(&mut self) {
        self.saved_top = self.undo.last().map(|t| t.id);
        self.break_coalescing = true;
    }

    /// Force the next edit into a new undo step (e.g. after cursor movement).
    pub fn break_undo_group(&mut self) {
        self.break_coalescing = true;
    }

    // ---- grapheme navigation -------------------------------------------------

    pub fn next_grapheme(&self, pos: usize) -> usize {
        next_grapheme_boundary(self.rope.slice(..), pos)
    }

    pub fn prev_grapheme(&self, pos: usize) -> usize {
        prev_grapheme_boundary(self.rope.slice(..), pos)
    }

    /// Snap an arbitrary char index onto a grapheme boundary (rounding down).
    pub fn snap(&self, pos: usize) -> usize {
        let pos = pos.min(self.len_chars());
        if is_grapheme_boundary(self.rope.slice(..), pos) {
            pos
        } else {
            self.prev_grapheme(pos)
        }
    }

    pub fn line_of(&self, pos: usize) -> usize {
        self.rope.char_to_line(pos.min(self.len_chars()))
    }

    pub fn line_start(&self, line: usize) -> usize {
        self.rope.line_to_char(line.min(self.rope.len_lines().saturating_sub(1)))
    }

    /// Char index just before the line's terminator (LF, CRLF, or EOF).
    pub fn line_end(&self, line: usize) -> usize {
        let line = line.min(self.rope.len_lines().saturating_sub(1));
        let start = self.rope.line_to_char(line);
        start + line_content_len(self.rope.line(line))
    }

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// Word boundary search used for Ctrl/Alt+Left/Right and double-click.
    pub fn word_left(&self, pos: usize) -> usize {
        let mut p = pos;
        // Skip whitespace/punctuation, then word chars.
        while p > 0 && !is_word_char(self.rope.char(p - 1)) {
            p = self.prev_grapheme(p);
        }
        while p > 0 && is_word_char(self.rope.char(p - 1)) {
            p = self.prev_grapheme(p);
        }
        p
    }

    pub fn word_right(&self, pos: usize) -> usize {
        let len = self.len_chars();
        let mut p = pos;
        while p < len && !is_word_char(self.rope.char(p)) {
            p = self.next_grapheme(p);
        }
        while p < len && is_word_char(self.rope.char(p)) {
            p = self.next_grapheme(p);
        }
        p
    }

    /// Range of the word (or run of non-word chars) around `pos`.
    pub fn word_at(&self, pos: usize) -> (usize, usize) {
        let len = self.len_chars();
        if len == 0 {
            return (0, 0);
        }
        let probe = if pos >= len { len - 1 } else { pos };
        let c = self.rope.char(probe);
        if c == '\n' || c == '\r' {
            return (pos, pos);
        }
        let class = is_word_char(c);
        let mut s = probe;
        while s > 0 {
            let pc = self.rope.char(s - 1);
            if is_word_char(pc) != class || pc == '\n' {
                break;
            }
            s -= 1;
        }
        let mut e = probe;
        while e < len {
            let nc = self.rope.char(e);
            if is_word_char(nc) != class || nc == '\n' || nc == '\r' {
                break;
            }
            e += 1;
        }
        (self.snap(s), self.snap_up(e))
    }

    fn snap_up(&self, pos: usize) -> usize {
        let pos = pos.min(self.len_chars());
        if is_grapheme_boundary(self.rope.slice(..), pos) {
            pos
        } else {
            self.next_grapheme(pos)
        }
    }

    pub fn selected_text(&self) -> String {
        self.rope.slice(self.sel.start()..self.sel.end()).to_string()
    }

    // ---- editing -------------------------------------------------------------

    /// Replace the selection with `text` as one edit of the given kind.
    pub fn insert(&mut self, text: &str, kind: EditKind) {
        let before = self.sel;
        let mut edits = Vec::new();
        let start = self.sel.start();
        if !self.sel.is_empty() {
            edits.push(self.raw_delete(start, self.sel.end()));
        }
        if !text.is_empty() {
            edits.push(self.raw_insert(start, text));
        }
        let new_pos = start + text.chars().count();
        self.sel = Selection::caret(new_pos);
        self.preferred_col = None;
        // Typing over a selection starts a fresh undo step that the
        // following keystrokes then join (one Undo restores the selection).
        if !before.is_empty() {
            self.break_coalescing = true;
        }
        self.commit(edits, before, kind);
    }

    /// Overwrite mode: `text` replaces the grapheme after the caret, never
    /// the line ending (so at the end of a line it inserts). A selection is
    /// replaced like in insert mode.
    pub fn overtype(&mut self, text: &str, kind: EditKind) {
        let pos = self.sel.head;
        if !self.sel.is_empty() || pos >= self.line_end(self.line_of(pos)) {
            self.insert(text, kind);
            return;
        }
        let before = self.sel;
        let edits = vec![self.raw_delete(pos, self.next_grapheme(pos)), self.raw_insert(pos, text)];
        self.sel = Selection::caret(pos + text.chars().count());
        self.preferred_col = None;
        self.commit(edits, before, kind);
    }

    /// Delete the selection, or the range given by `range_if_empty`.
    pub fn delete(&mut self, range_if_empty: (usize, usize), kind: EditKind) {
        let before = self.sel;
        let (s, e) = if self.sel.is_empty() {
            range_if_empty
        } else {
            (self.sel.start(), self.sel.end())
        };
        if s >= e {
            return;
        }
        let edit = self.raw_delete(s, e);
        self.sel = Selection::caret(s);
        self.preferred_col = None;
        let kind = if before.is_empty() { kind } else { EditKind::Other };
        self.commit(vec![edit], before, kind);
    }

    /// Apply several non-overlapping replacements `(at, delete_len, insert)`
    /// as one undo step. Positions refer to the text before any of them;
    /// the selection is remapped through all of them.
    pub fn transact(&mut self, ops: Vec<(usize, usize, String)>) {
        self.edit(ops, EditKind::Other, None, false);
    }

    /// `transact` with an undo kind (Typing/Deleting steps coalesce with
    /// their neighbours like ordinary typing) and, optionally, the selection
    /// to leave afterwards instead of the remapped one. Used by table edits,
    /// where one keystroke also re-pads a column. `word_start` starts a new
    /// undo step for this edit (the caller knows whether the user typed a
    /// space; the padding edits here can't tell).
    pub fn edit(&mut self, mut ops: Vec<(usize, usize, String)>, kind: EditKind, sel: Option<Selection>, word_start: bool) {
        let before = self.sel;
        ops.sort_by_key(|op| std::cmp::Reverse(op.0));
        let map = |pos: usize, ops: &[(usize, usize, String)]| -> usize {
            let mut p = pos as isize;
            for (at, del, ins) in ops {
                if *at < pos {
                    let removed = (*del).min(pos - at) as isize;
                    p += ins.chars().count() as isize - removed;
                }
            }
            p.max(0) as usize
        };
        let new_sel = Selection { anchor: map(before.anchor, &ops), head: map(before.head, &ops) };
        let mut edits = Vec::new();
        for (at, del, ins) in &ops {
            if *del > 0 {
                edits.push(self.raw_delete(*at, at + del));
            }
            if !ins.is_empty() {
                edits.push(self.raw_insert(*at, ins));
            }
        }
        self.sel = sel.unwrap_or(new_sel);
        self.preferred_col = None;
        if kind == EditKind::Other || word_start {
            self.break_coalescing = true;
        }
        self.commit_with(edits, before, kind, false);
    }

    fn raw_insert(&mut self, at: usize, text: &str) -> Edit {
        self.rope.insert(at, text);
        Edit::Insert { at, text: text.to_string() }
    }

    fn raw_delete(&mut self, s: usize, e: usize) -> Edit {
        let text = self.rope.slice(s..e).to_string();
        self.rope.remove(s..e);
        Edit::Delete { at: s, text }
    }

    fn commit(&mut self, edits: Vec<Edit>, before: Selection, kind: EditKind) {
        let boundary = ends_word_boundary(&edits, kind);
        self.commit_with(edits, before, kind, boundary);
    }

    fn commit_with(&mut self, edits: Vec<Edit>, before: Selection, kind: EditKind, boundary: bool) {
        if edits.is_empty() {
            return;
        }
        self.log_change(edits.iter().map(Edit::change));
        self.redo.clear();
        let now = Instant::now();
        if !self.break_coalescing
            && kind != EditKind::Other
            && let Some(last) = self.undo.last_mut()
            && last.kind == kind
            && now.duration_since(last.at) < COALESCE_WINDOW
            && last.sel_after == before
            && !boundary
        {
            last.edits.extend(edits);
            last.sel_after = self.sel;
            last.at = now;
            return;
        }
        self.break_coalescing = false;
        let id = self.next_tx_id;
        self.next_tx_id += 1;
        self.undo.push(Transaction { id, edits, sel_before: before, sel_after: self.sel, kind, at: now });
    }

    pub fn undo(&mut self) -> bool {
        let Some(tx) = self.undo.pop() else { return false };
        for edit in tx.edits.iter().rev() {
            match edit {
                Edit::Insert { at, text } => {
                    let n = text.chars().count();
                    self.rope.remove(*at..*at + n);
                }
                Edit::Delete { at, text } => self.rope.insert(*at, text),
            }
        }
        self.sel = tx.sel_before;
        self.log_change(tx.edits.iter().rev().map(|e| e.inverse_change()));
        self.break_coalescing = true;
        self.preferred_col = None;
        self.redo.push(tx);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(tx) = self.redo.pop() else { return false };
        for edit in &tx.edits {
            match edit {
                Edit::Insert { at, text } => self.rope.insert(*at, text),
                Edit::Delete { at, text } => {
                    let n = text.chars().count();
                    self.rope.remove(*at..*at + n);
                }
            }
        }
        self.sel = tx.sel_after;
        self.log_change(tx.edits.iter().map(Edit::change));
        self.break_coalescing = true;
        self.preferred_col = None;
        self.undo.push(tx);
        true
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Typing a space after a word starts a new undo step, like most GUI editors.
fn ends_word_boundary(edits: &[Edit], kind: EditKind) -> bool {
    if kind != EditKind::Typing {
        return false;
    }
    matches!(edits.last(), Some(Edit::Insert { text, .. }) if text.chars().all(char::is_whitespace))
}

pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Number of chars in a line slice excluding its terminator.
pub fn line_content_len(line: RopeSlice) -> usize {
    let n = line.len_chars();
    if n >= 2 && line.char(n - 2) == '\r' && line.char(n - 1) == '\n' {
        n - 2
    } else if n >= 1 && matches!(line.char(n - 1), '\n' | '\r') {
        n - 1
    } else {
        n
    }
}

// ---- grapheme boundaries over rope chunks (pattern from ropey's examples) ----

pub fn next_grapheme_boundary(slice: RopeSlice, char_idx: usize) -> usize {
    if char_idx >= slice.len_chars() {
        return slice.len_chars();
    }
    let byte_idx = slice.char_to_byte(char_idx);
    let (mut chunk, mut chunk_byte_idx, mut chunk_char_idx, _) = slice.chunk_at_byte(byte_idx);
    let mut gc = GraphemeCursor::new(byte_idx, slice.len_bytes(), true);
    loop {
        match gc.next_boundary(chunk, chunk_byte_idx) {
            Ok(None) => return slice.len_chars(),
            Ok(Some(n)) => {
                let tmp = byte_to_char_in_chunk(chunk, n - chunk_byte_idx);
                return chunk_char_idx + tmp;
            }
            Err(GraphemeIncomplete::NextChunk) => {
                chunk_byte_idx += chunk.len();
                let (a, _, c, _) = slice.chunk_at_byte(chunk_byte_idx);
                chunk = a;
                chunk_char_idx = c;
            }
            Err(GraphemeIncomplete::PreContext(n)) => {
                let ctx = slice.chunk_at_byte(n - 1).0;
                gc.provide_context(ctx, n - ctx.len());
            }
            Err(_) => unreachable!(),
        }
    }
}

pub fn prev_grapheme_boundary(slice: RopeSlice, char_idx: usize) -> usize {
    if char_idx == 0 {
        return 0;
    }
    let char_idx = char_idx.min(slice.len_chars());
    let byte_idx = slice.char_to_byte(char_idx);
    let (mut chunk, mut chunk_byte_idx, mut chunk_char_idx, _) = slice.chunk_at_byte(byte_idx);
    let mut gc = GraphemeCursor::new(byte_idx, slice.len_bytes(), true);
    loop {
        match gc.prev_boundary(chunk, chunk_byte_idx) {
            Ok(None) => return 0,
            Ok(Some(n)) => {
                let tmp = byte_to_char_in_chunk(chunk, n - chunk_byte_idx);
                return chunk_char_idx + tmp;
            }
            Err(GraphemeIncomplete::PrevChunk) => {
                let (a, b, c, _) = slice.chunk_at_byte(chunk_byte_idx - 1);
                chunk = a;
                chunk_byte_idx = b;
                chunk_char_idx = c;
            }
            Err(GraphemeIncomplete::PreContext(n)) => {
                let ctx = slice.chunk_at_byte(n - 1).0;
                gc.provide_context(ctx, n - ctx.len());
            }
            Err(_) => unreachable!(),
        }
    }
}

pub fn is_grapheme_boundary(slice: RopeSlice, char_idx: usize) -> bool {
    if char_idx == 0 || char_idx >= slice.len_chars() {
        return true;
    }
    let byte_idx = slice.char_to_byte(char_idx);
    let (chunk, chunk_byte_idx, _, _) = slice.chunk_at_byte(byte_idx);
    let mut gc = GraphemeCursor::new(byte_idx, slice.len_bytes(), true);
    loop {
        match gc.is_boundary(chunk, chunk_byte_idx) {
            Ok(b) => return b,
            Err(GraphemeIncomplete::PreContext(n)) => {
                let (ctx, ctx_byte_start, _, _) = slice.chunk_at_byte(n - 1);
                gc.provide_context(ctx, ctx_byte_start);
            }
            Err(_) => unreachable!(),
        }
    }
}

fn byte_to_char_in_chunk(chunk: &str, byte: usize) -> usize {
    chunk[..byte].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grapheme_navigation_over_combining_and_emoji() {
        // e + combining acute, family emoji (ZWJ sequence), CJK, flag.
        let s = "e\u{301}👨‍👩‍👧中🇳🇿x";
        let b = Buffer::from_str(s);
        let mut stops = vec![0];
        let mut p = 0;
        while p < b.len_chars() {
            p = b.next_grapheme(p);
            stops.push(p);
        }
        // 5 graphemes + start
        assert_eq!(stops.len(), 6, "{stops:?}");
        // Walking back hits the same stops.
        let mut back = vec![p];
        while p > 0 {
            p = b.prev_grapheme(p);
            back.push(p);
        }
        back.reverse();
        assert_eq!(stops, back);
        assert!(!is_grapheme_boundary(b.rope().slice(..), 1));
        assert_eq!(b.snap(1), 0);
    }

    #[test]
    fn crlf_is_one_grapheme_and_excluded_from_line_content() {
        let b = Buffer::from_str("ab\r\ncd");
        assert_eq!(b.next_grapheme(2), 4);
        assert_eq!(b.line_end(0), 2);
        assert_eq!(b.line_start(1), 4);
        assert_eq!(b.line_end(1), 6);
    }

    #[test]
    fn typing_coalesces_and_undo_restores() {
        let mut b = Buffer::new();
        for c in ["h", "e", "l", "l", "o"] {
            b.insert(c, EditKind::Typing);
        }
        b.insert(" ", EditKind::Typing);
        b.insert("w", EditKind::Typing);
        assert_eq!(b.rope().to_string(), "hello w");
        assert!(b.undo());
        assert_eq!(b.rope().to_string(), "hello");
        assert!(b.undo());
        assert_eq!(b.rope().to_string(), "");
        assert!(!b.undo());
        assert!(b.redo());
        assert!(b.redo());
        assert_eq!(b.rope().to_string(), "hello w");
        assert_eq!(b.sel, Selection::caret(7));
    }

    #[test]
    fn overtype_replaces_graphemes_but_not_line_endings() {
        let mut b = Buffer::from_str("#.é\u{301}.\r\nxy");
        b.sel = Selection::caret(1);
        for c in ["@", "~", "~", "~", "~"] {
            b.overtype(c, EditKind::Typing);
        }
        // e + combining accent is one grapheme; past the line's end it inserts.
        assert_eq!(b.rope().to_string(), "#@~~~~\r\nxy");
        assert_eq!(b.sel, Selection::caret(6));
        assert!(b.undo());
        assert_eq!(b.rope().to_string(), "#.é\u{301}.\r\nxy");
        assert_eq!(b.sel, Selection::caret(1));
        // A selection is replaced as in insert mode.
        b.sel = Selection { anchor: 0, head: 2 };
        b.overtype("Z", EditKind::Typing);
        assert_eq!(b.rope().to_string(), "Zé\u{301}.\r\nxy");
    }

    #[test]
    fn replacing_selection_is_one_step_and_dirty_tracks() {
        let mut b = Buffer::from_str("hello world");
        assert!(!b.is_dirty());
        b.sel = Selection { anchor: 6, head: 11 };
        b.insert("there", EditKind::Typing);
        assert_eq!(b.rope().to_string(), "hello there");
        assert!(b.is_dirty());
        b.insert("!", EditKind::Typing);
        b.undo();
        assert_eq!(b.rope().to_string(), "hello world");
        assert_eq!(b.sel, Selection { anchor: 6, head: 11 });
        assert!(!b.is_dirty());
    }

    #[test]
    fn large_paste_is_single_undo_step() {
        let mut b = Buffer::from_str("x");
        b.sel = Selection::caret(1);
        let big = "line\n".repeat(100_000);
        b.insert(&big, EditKind::Other);
        assert_eq!(b.len_lines(), 100_001);
        b.undo();
        assert_eq!(b.rope().to_string(), "x");
    }

    #[test]
    fn transact_indents_atomically_and_maps_selection() {
        let mut b = Buffer::from_str("aa\nbb\ncc\n");
        b.sel = Selection { anchor: 1, head: 7 }; // inside "aa" .. inside "cc"
        b.transact(vec![(0, 0, "\t".into()), (3, 0, "\t".into()), (6, 0, "\t".into())]);
        assert_eq!(b.rope().to_string(), "\taa\n\tbb\n\tcc\n");
        assert_eq!(b.sel, Selection { anchor: 2, head: 10 });
        // Outdent: delete the tabs again (positions in current text).
        b.transact(vec![(0, 1, String::new()), (4, 1, String::new()), (8, 1, String::new())]);
        assert_eq!(b.rope().to_string(), "aa\nbb\ncc\n");
        assert_eq!(b.sel, Selection { anchor: 1, head: 7 });
        b.undo();
        assert_eq!(b.rope().to_string(), "\taa\n\tbb\n\tcc\n");
        b.undo();
        assert_eq!(b.rope().to_string(), "aa\nbb\ncc\n");
        assert!(!b.is_dirty());
    }

    #[test]
    fn word_motion() {
        let b = Buffer::from_str("foo  bar_baz, qux");
        assert_eq!(b.word_right(0), 3);
        assert_eq!(b.word_right(3), 12);
        assert_eq!(b.word_left(12), 5);
        assert_eq!(b.word_at(7), (5, 12));
    }

    /// Text outside a reported change is the same before and after it.
    #[test]
    fn changes_since_covers_every_edit() {
        let check = |b: &Buffer, old: &str, key| {
            let new = b.rope().to_string();
            let o: Vec<char> = old.chars().collect();
            let n: Vec<char> = new.chars().collect();
            match b.changes_since(key) {
                Since::Same => assert_eq!(old, new),
                Since::Changed(c) => {
                    assert_eq!(o.len() - c.removed + c.inserted, n.len(), "{c:?}");
                    assert_eq!(o[..c.at], n[..c.at], "{c:?}");
                    assert_eq!(o[c.at + c.removed..], n[c.at + c.inserted..], "{c:?}");
                }
                Since::Unknown => panic!("lost track"),
            }
        };
        let mut seed = 7u64;
        let mut rnd = |m: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as usize % m.max(1)
        };
        let mut b = Buffer::from_str("hello\nworld\nthird line\n");
        for _ in 0..300 {
            let old = b.rope().to_string();
            let key = b.cache_key();
            for _ in 0..1 + rnd(3) {
                let len = b.len_chars();
                match rnd(5) {
                    0 => {
                        b.undo();
                    }
                    1 => {
                        b.redo();
                    }
                    2 => {
                        let (a, z) = (rnd(len + 1), rnd(len + 1));
                        b.sel = Selection { anchor: a, head: z };
                        b.delete((a.min(z), a.max(z)), EditKind::Deleting);
                    }
                    3 => {
                        let at = rnd(len + 1);
                        let ops = vec![(at, rnd(len - at + 1), "x\ny".to_string()), (0, 0, "Z".to_string())];
                        let ops = if at == 0 { ops[..1].to_vec() } else { ops };
                        b.transact(ops);
                    }
                    _ => {
                        b.sel = Selection::caret(rnd(len + 1));
                        b.insert(["a", "\n", "bc"][rnd(3)], EditKind::Typing);
                    }
                }
            }
            check(&b, &old, key);
        }
        assert_eq!(Buffer::new().changes_since(b.cache_key()), Since::Unknown);
    }
}
