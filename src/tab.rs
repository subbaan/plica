//! One open document: its buffer, file identity and view position.

use crate::buffer::Buffer;
use crate::document::{self, Document};
use std::path::Path;

pub struct Tab {
    pub buf: Buffer,
    pub doc: Document,
    /// What Tab inserts / indents with: "\t" or N spaces, detected per file.
    pub indent_unit: String,
    /// Viewport: first visible visual row is row `top_row` of line `top_line`.
    pub top_line: usize,
    pub top_row: usize,
    pub scroll_x: usize,
    /// Set when the caret moved and the viewport must follow it.
    pub follow_caret: bool,
}

impl Tab {
    pub fn new(buf: Buffer, doc: Document, tab_width: usize) -> Self {
        let indent_unit = detect_indent(&buf, tab_width);
        Self { buf, doc, indent_unit, top_line: 0, top_row: 0, scroll_x: 0, follow_caret: true }
    }

    pub fn untitled(tab_width: usize) -> Self {
        Self::new(Buffer::new(), Document::untitled(), tab_width)
    }

    pub fn open(path: &Path, tab_width: usize) -> anyhow::Result<(Self, bool)> {
        let l = document::load(path)?;
        Ok((Self::new(Buffer::from_str(&l.text), l.doc, tab_width), l.is_new))
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
