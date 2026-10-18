//! One open document: its buffer, file identity and view position.

use crate::buffer::Buffer;
use crate::document::{self, Document};
use crate::encoding::{self, Encoding};
use crate::table::Table;
use std::cell::RefCell;
use std::rc::Rc;
use crate::config::IndentMode;
use std::path::Path;

/// Per-document settings that come from the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabOpts {
    pub tab_width: usize,
    pub indent: IndentMode,
    /// Encoding for files that aren't UTF-8 or UTF-16 (None = guess).
    pub fallback_encoding: Option<&'static encoding_rs::Encoding>,
    /// Recognise tables (off: every line is plain text).
    pub tables: bool,
}

pub struct Tab {
    pub buf: Buffer,
    pub doc: Document,
    /// What Tab inserts / indents with: "\t" or N spaces, detected per file.
    pub indent_unit: String,
    /// Viewport: first visible visual row is row `top_row` of line `top_line`.
    pub top_line: usize,
    pub top_row: usize,
    pub scroll_x: usize,
    /// With soft wrap on, table rows don't wrap: the caret's table scrolls
    /// sideways by this much on its own.
    pub table_scroll_x: usize,
    /// Set when the caret moved and the viewport must follow it.
    pub follow_caret: bool,
    /// The file changed on disk while this (background) tab had unsaved
    /// changes; a dialog is shown when the tab becomes active.
    pub disk_conflict: bool,
    /// Process-unique id (names this tab's recovery snapshot).
    pub uid: u64,
    /// The configured fallback encoding, for reloads.
    pub fallback_encoding: Option<&'static encoding_rs::Encoding>,
    /// Recognise tables at all (Preferences → Tables).
    pub tables_enabled: bool,
    /// Tables recognised in the current buffer version.
    tables: RefCell<TableCache>,
    /// Recovery bookkeeping: buffer key last seen / snapshotted, and when.
    pub seen_key: (u64, u64),
    pub changed_at: std::time::Instant,
    pub snap_key: Option<(u64, u64)>,
    pub snap_at: std::time::Instant,
}

static NEXT_TAB_UID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Tab {
    pub fn new(buf: Buffer, doc: Document, opts: TabOpts) -> Self {
        let indent_unit = match opts.indent {
            IndentMode::Auto => detect_indent(&buf, opts.tab_width),
            IndentMode::Tabs => "\t".to_string(),
            IndentMode::Spaces => " ".repeat(opts.tab_width),
        };
        let now = std::time::Instant::now();
        let seen_key = buf.cache_key();
        Self {
            buf,
            doc,
            indent_unit,
            top_line: 0,
            top_row: 0,
            scroll_x: 0,
            table_scroll_x: 0,
            follow_caret: true,
            disk_conflict: false,
            uid: NEXT_TAB_UID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            fallback_encoding: opts.fallback_encoding,
            tables_enabled: opts.tables,
            tables: RefCell::default(),
            seen_key,
            changed_at: now,
            snap_key: None,
            snap_at: now,
        }
    }

    pub fn untitled(opts: TabOpts) -> Self {
        Self::new(Buffer::new(), Document::untitled(), opts)
    }

    /// Open `path`, decoding it as `force` if given (else detected).
    pub fn open(path: &Path, opts: TabOpts, force: Option<Encoding>) -> anyhow::Result<(Self, bool)> {
        let eo = encoding::Options { force, prefer: None, fallback: opts.fallback_encoding };
        let l = document::load_with(path, &eo)?;
        Ok((Self::new(Buffer::from_str(&l.text), l.doc, opts), l.is_new))
    }

    /// Replace the buffer with the file's current contents as one undoable
    /// step (Ctrl+Z brings the previous text back), keeping the caret's line
    /// and column where possible. The current encoding is kept unless the
    /// bytes say otherwise; `force` decodes with that encoding instead.
    pub fn reload_from_disk(&mut self, force: Option<Encoding>) -> anyhow::Result<()> {
        let path = self.doc.path.clone().ok_or_else(|| anyhow::anyhow!("no file"))?;
        let eo = encoding::Options { force, prefer: Some(self.doc.encoding), fallback: self.fallback_encoding };
        let l = document::load_with(&path, &eo)?;
        let head = self.buf.sel.head;
        let line = self.buf.line_of(head);
        let col = head - self.buf.line_start(line);
        if self.buf.rope() != &l.text[..] {
            let len = self.buf.len_chars();
            self.buf.transact(vec![(0, len, l.text)]);
            let line = line.min(self.buf.len_lines() - 1);
            let pos = (self.buf.line_start(line) + col).min(self.buf.line_end(line));
            self.buf.sel = crate::buffer::Selection::caret(self.buf.snap(pos));
        }
        self.buf.mark_saved();
        self.doc = l.doc;
        self.disk_conflict = false;
        self.follow_caret = true;
        Ok(())
    }

    /// The Markdown table containing `line`, if any. Recognised tables
    /// (and lines known not to be in one) are cached until the text changes.
    pub fn table_at(&self, line: usize) -> Option<Rc<Table>> {
        if !self.tables_enabled {
            return None;
        }
        let mut c = self.tables.borrow_mut();
        let key = self.buf.cache_key();
        if c.key != key {
            *c = TableCache { key, ..TableCache::default() };
        }
        if let Some(t) = c.tables.iter().find(|t| (t.first_line()..=t.last_line()).contains(&line)) {
            return Some(t.clone());
        }
        if c.miss_lines.contains(&line) || c.miss_ranges.iter().any(|&(a, b)| (a..=b).contains(&line)) {
            return None;
        }
        match Table::scan(self.buf.rope(), line) {
            Ok(t) => {
                let t = Rc::new(t);
                c.tables.push(t.clone());
                Some(t)
            }
            Err((a, b)) if a == b => {
                c.miss_lines.insert(a);
                None
            }
            Err(range) => {
                c.miss_ranges.push(range);
                None
            }
        }
    }

    /// An untouched "Untitled" tab that a newly opened file may replace.
    pub fn is_pristine(&self) -> bool {
        self.doc.path.is_none() && !self.buf.is_dirty() && self.buf.len_chars() == 0
    }

    /// True if this tab shows `path` (compared after resolving symlinks).
    pub fn shows(&self, path: &Path) -> bool {
        let Some(mine) = &self.doc.path else { return false };
        match (std::fs::canonicalize(mine), std::fs::canonicalize(path)) {
            (Ok(a), Ok(b)) => a == b,
            _ => mine == path,
        }
    }
}

/// Guess the file's indentation style from its first lines: tabs unless
/// space-indented lines clearly dominate. Empty files default to tabs.
pub fn detect_indent(buf: &Buffer, tab_width: usize) -> String {
    let rope = buf.rope();
    let (mut tabs, mut spaces) = (0usize, 0usize);
    for line in rope.lines().take(2000) {
        match (line.get_char(0), line.get_char(1)) {
            (Some('\t'), _) => tabs += 1,
            (Some(' '), Some(' ')) => spaces += 1,
            _ => {}
        }
    }
    if spaces > tabs { " ".repeat(tab_width) } else { "\t".to_string() }
}

#[derive(Default)]
struct TableCache {
    key: (u64, u64),
    tables: Vec<Rc<Table>>,
    /// Lines known to hold no table, and longer ranges of them (a run of
    /// pipe lines that isn't a table is recorded once, not line by line).
    miss_lines: std::collections::HashSet<usize>,
    miss_ranges: Vec<(usize, usize)>,
}
