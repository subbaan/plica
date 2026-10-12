//! Display layout: maps buffer char positions to terminal cells and back.
//!
//! Layout is computed per logical line, on demand, only for lines that are
//! visible or that the caret moves through. Widths come from ratatui's own
//! `CellWidth` so hit-testing can never disagree with what is rendered.

use crate::buffer::{line_content_len, next_grapheme_boundary};
use ratatui::buffer::CellWidth;
use ropey::Rope;
use std::borrow::Cow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutOpts {
    pub tab_width: usize,
    /// Soft wrap at this many columns; `None` = no wrap (horizontal scroll).
    pub wrap_width: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlyphKind {
    Text,
    Tab,
    /// Control or invisible char shown with a substitute symbol.
    Substitute,
}

#[derive(Debug, Clone)]
pub struct Glyph {
    /// Char range in the rope covered by this grapheme.
    pub start: usize,
    pub end: usize,
    /// Display column within its visual row (0 = first text column).
    pub col: usize,
    pub width: usize,
    pub symbol: Cow<'static, str>,
    pub kind: GlyphKind,
}

#[derive(Debug, Clone)]
pub struct VisualRow {
    pub line: usize,
    pub row_in_line: usize,
    /// Char range of this row; for the last row `end` is the line's content end.
    pub start: usize,
    pub end: usize,
    pub glyphs: Vec<Glyph>,
    pub is_last: bool,
}

impl VisualRow {
    pub fn width(&self) -> usize {
        self.glyphs.last().map_or(0, |g| g.col + g.width)
    }

    /// Display column of a char position that lies inside this row.
    pub fn col_of(&self, pos: usize) -> usize {
        for g in &self.glyphs {
            if pos <= g.start {
                return g.col;
            }
            if pos < g.end {
                return g.col; // inside a grapheme (should not happen once snapped)
            }
        }
        self.width()
    }

    /// Nearest char position for a display column (click / vertical motion).
    pub fn pos_at_col(&self, col: usize) -> usize {
        for g in &self.glyphs {
            if col < g.col + g.width {
                // Clicking the right half of a wide glyph or tab lands after it.
                if g.width > 1 && col - g.col >= g.width.div_ceil(2) {
                    return g.end;
                }
                return g.start;
            }
        }
        if self.is_last {
            self.end
        } else {
            // Past the end of a wrapped row: stay on this row (before its last
            // glyph) since `end` would display at the start of the next row.
            self.glyphs.last().map_or(self.start, |g| g.start)
        }
    }

    pub fn contains_caret(&self, pos: usize) -> bool {
        pos >= self.start && (pos < self.end || (self.is_last && pos == self.end))
    }
}

/// Display width of `s` laid out from column 0 with tabs counting 1 (for
/// aligning table cells, whose real column isn't known).
pub fn text_width(s: &str) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    if s.bytes().all(|b| (0x20..0x7f).contains(&b) || b == b'\t') {
        // Printable ASCII (tabs count 1 here): one cell per byte.
        return s.len();
    }
    s.graphemes(true).map(|g| shape(g, 0, 1).1).sum()
}

/// Shape a grapheme into (symbol, width, kind) for a given starting column.
fn shape(g: &str, col: usize, tab_width: usize) -> (Cow<'static, str>, usize, GlyphKind) {
    let mut chars = g.chars();
    let first = chars.next().unwrap_or(' ');
    if g == "\t" {
        let w = tab_width - (col % tab_width);
        return (Cow::Borrowed(" "), w, GlyphKind::Tab);
    }
    if let Some(b) = crate::encoding::raw_byte(first) {
        // A byte kept from a file that wasn't valid in its encoding.
        return (Cow::Owned(format!("\\x{b:02X}")), 4, GlyphKind::Substitute);
    }
    if first.is_control() && chars.next().is_none() {
        let sym: Cow<'static, str> = match first as u32 {
            0..=0x1f => Cow::Owned(format!("^{}", char::from_u32(first as u32 + 0x40).unwrap_or('?'))),
            0x7f => Cow::Borrowed("^?"),
            _ => Cow::Borrowed("\u{fffd}"),
        };
        let w = sym.as_ref().cell_width() as usize;
        return (sym, w, GlyphKind::Substitute);
    }
    if g.contains(char::is_control) {
        return (Cow::Borrowed("\u{fffd}"), 1, GlyphKind::Substitute);
    }
    let w = g.cell_width() as usize;
    if w == 0 {
        // Lone combining mark or invisible format char: show a dotted circle.
        return (Cow::Owned(format!("\u{25cc}{g}")), 1, GlyphKind::Substitute);
    }
    (Cow::Owned(g.to_string()), w, GlyphKind::Text)
}

/// Lay out one logical line into one or more visual rows.
pub fn layout_line(rope: &Rope, line: usize, opts: LayoutOpts) -> Vec<VisualRow> {
    let line = line.min(rope.len_lines().saturating_sub(1));
    let line_start = rope.line_to_char(line);
    let content_len = line_content_len(rope.line(line));
    let line_end = line_start + content_len;
    let slice = rope.slice(..);

    let mut rows = Vec::new();
    let mut glyphs: Vec<Glyph> = Vec::new();
    let mut row_start = line_start;
    let mut col = 0usize;
    // Index into `glyphs` just after the last whitespace glyph (wrap opportunity).
    let mut last_break: Option<usize> = None;

    let mut pos = line_start;
    let mut gbuf = String::new();
    while pos < line_end {
        let next = next_grapheme_boundary(slice, pos).min(line_end).max(pos + 1);
        gbuf.clear();
        gbuf.extend(rope.slice(pos..next).chars());
        let (symbol, width, kind) = shape(&gbuf, col, opts.tab_width);

        if let Some(wrap) = opts.wrap_width
            && wrap > 0
            && col + width > wrap
            && !glyphs.is_empty()
        {
            // Break at the last whitespace if there is one, else hard break.
            let split = match last_break {
                Some(i) if i < glyphs.len() => i,
                _ => glyphs.len(),
            };
            let carry: Vec<Glyph> = glyphs.drain(split..).collect();
            let row_end = carry.first().map_or(pos, |g| g.start);
            rows.push(VisualRow {
                line,
                row_in_line: rows.len(),
                start: row_start,
                end: row_end,
                glyphs: std::mem::take(&mut glyphs),
                is_last: false,
            });
            row_start = row_end;
            col = 0;
            for mut g in carry {
                // Re-shape tabs: their width depends on the column.
                if g.kind == GlyphKind::Tab {
                    g.width = opts.tab_width - (col % opts.tab_width);
                }
                g.col = col;
                col += g.width;
                glyphs.push(g);
            }
            last_break = None;
            // Re-process the current grapheme against the new row (its shape
            // may depend on the column, and it may still not fit).
            continue;
        }
        push_glyph(&mut glyphs, &mut col, &mut last_break, pos, next, symbol, width, kind, &gbuf);
        pos = next;
    }
    rows.push(VisualRow {
        line,
        row_in_line: rows.len(),
        start: row_start,
        end: line_end,
        glyphs,
        is_last: true,
    });
    rows
}

#[allow(clippy::too_many_arguments)]
fn push_glyph(
    glyphs: &mut Vec<Glyph>,
    col: &mut usize,
    last_break: &mut Option<usize>,
    start: usize,
    end: usize,
    symbol: Cow<'static, str>,
    width: usize,
    kind: GlyphKind,
    raw: &str,
) {
    let is_space = raw.chars().all(|c| c == ' ' || c == '\t');
    glyphs.push(Glyph { start, end, col: *col, width, symbol, kind });
    *col += width;
    if is_space {
        *last_break = Some(glyphs.len());
    }
}

/// Find the visual row (index into `rows`) that displays the caret at `pos`.
pub fn row_index_for(rows: &[VisualRow], pos: usize) -> usize {
    rows.iter().position(|r| r.contains_caret(pos)).unwrap_or(rows.len().saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(wrap: Option<usize>) -> LayoutOpts {
        LayoutOpts { tab_width: 4, wrap_width: wrap }
    }

    #[test]
    fn wide_and_combining_widths() {
        let rope = Rope::from_str("a中e\u{301}👍b\n");
        let rows = layout_line(&rope, 0, opts(None));
        assert_eq!(rows.len(), 1);
        let cols: Vec<(usize, usize)> = rows[0].glyphs.iter().map(|g| (g.col, g.width)).collect();
        assert_eq!(cols, vec![(0, 1), (1, 2), (3, 1), (4, 2), (6, 1)]);
        // Char positions: a=0, 中=1, e+◌́=2..4, 👍=4, b=5
        assert_eq!(rows[0].col_of(4), 4);
        assert_eq!(rows[0].end, 6);
    }

    #[test]
    fn hit_testing_wide_glyph_halves() {
        let rope = Rope::from_str("中x");
        let row = &layout_line(&rope, 0, opts(None))[0];
        assert_eq!(row.pos_at_col(0), 0);
        assert_eq!(row.pos_at_col(1), 1); // right half of 中 -> after it
        assert_eq!(row.pos_at_col(2), 1);
        assert_eq!(row.pos_at_col(50), 2); // past end of last row
    }

    #[test]
    fn tabs_expand_to_stops() {
        let rope = Rope::from_str("a\tb\t\tc");
        let row = &layout_line(&rope, 0, opts(None))[0];
        let cols: Vec<usize> = row.glyphs.iter().map(|g| g.col).collect();
        assert_eq!(cols, vec![0, 1, 4, 5, 8, 12]);
    }

    #[test]
    fn raw_byte_placeholders_show_as_hex() {
        let rope = Rope::from_str("a\u{10FCE9}b");
        let row = &layout_line(&rope, 0, LayoutOpts { tab_width: 4, wrap_width: None })[0];
        assert_eq!(row.glyphs[1].symbol, "\\xE9");
        assert_eq!(row.glyphs[1].width, 4);
        assert_eq!(row.glyphs[2].col, 5);
    }

    #[test]
    fn control_chars_never_render_raw() {
        let rope = Rope::from_str("a\u{1b}[31mb\u{7f}");
        let row = &layout_line(&rope, 0, opts(None))[0];
        assert!(row.glyphs.iter().all(|g| !g.symbol.contains(char::is_control)));
        assert_eq!(row.glyphs[1].symbol, "^[");
    }

    #[test]
    fn word_wrap_breaks_after_space_and_round_trips() {
        let rope = Rope::from_str("hello world foo\n");
        let rows = layout_line(&rope, 0, opts(Some(8)));
        let texts: Vec<String> =
            rows.iter().map(|r| rope.slice(r.start..r.end).to_string()).collect();
        assert_eq!(texts, vec!["hello ", "world ", "foo"]);
        // Every char is covered exactly once.
        assert_eq!(rows[0].start, 0);
        for w in rows.windows(2) {
            assert_eq!(w[0].end, w[1].start);
        }
        // Caret at a wrap boundary displays at the start of the next row.
        assert_eq!(row_index_for(&rows, 6), 1);
        assert_eq!(row_index_for(&rows, 15), 2);
        // Clicking past the end of a wrapped row stays on that row.
        assert_eq!(rows[0].pos_at_col(7), 5);
    }

    #[test]
    fn hard_wrap_long_word_and_wide_glyph_not_split() {
        let rope = Rope::from_str("abc中def");
        let rows = layout_line(&rope, 0, opts(Some(4)));
        let texts: Vec<String> =
            rows.iter().map(|r| rope.slice(r.start..r.end).to_string()).collect();
        assert_eq!(texts, vec!["abc", "中de", "f"]);
        assert!(rows.iter().all(|r| r.width() <= 4));
    }

    #[test]
    fn empty_and_crlf_lines() {
        let rope = Rope::from_str("\r\nx");
        let rows = layout_line(&rope, 0, opts(Some(10)));
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].start, rows[0].end), (0, 0));
        assert!(rows[0].contains_caret(0));
    }
}
