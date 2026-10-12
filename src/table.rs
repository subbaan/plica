//! Markdown pipe tables: recognition, a logical model mapped to exact
//! source spans, and the edits that change them.
//!
//! The text stays canonical. A `Table` is derived from the rope: every cell
//! knows the char range it occupies between its pipes (`span`) and the
//! trimmed text inside (`content`). Edits are computed as replacements of
//! those spans in the old text, applied as one transaction, after which the
//! table is simply recognised again.
//!
//! Grammar (GitHub-flavoured Markdown, conservatively):
//! - A header row, then a delimiter row with the same number of cells
//!   (`---`, `:--`, `--:`, `:-:` per cell), then body rows.
//! - Every row contains an unescaped `|` (`\|` is a pipe inside a cell, also
//!   inside code spans, as on GitHub). A line without one ends the table, as
//!   do blank lines, lines indented 4+ spaces or by a tab, and fences.
//! - Tables inside fenced code blocks (``` or ~~~) are not tables.
//! - Ragged rows are kept as they are; formatting pads short rows and gives
//!   extra cells their own column, so no text is ever dropped.

use ropey::Rope;
use std::ops::Range;

/// Replacements `(at, delete_len, insert)` in the old text, applied as one
/// transaction by `Buffer::edit`.
pub type Ops = Vec<(usize, usize, String)>;

/// Lines longer than this are never table rows (keeps huge single-line
/// files from being scanned on every keystroke).
const MAX_ROW_CHARS: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

/// Which text format a table is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Markdown,
    /// `+---+` rules and `|` bars.
    Ascii,
    /// Box-drawing rules and `│` bars.
    Unicode,
}

impl Style {
    pub fn label(self) -> &'static str {
        match self {
            Style::Markdown => "Markdown",
            Style::Ascii => "ASCII",
            Style::Unicode => "Unicode box",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Header,
    Delimiter,
    Body,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// Chars between the pipes (or the line edge), padding included.
    pub span: Range<usize>,
    /// `span` without surrounding whitespace. For an empty cell, an empty
    /// range just after the first padding space.
    pub content: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub line: usize,
    pub kind: RowKind,
    /// The line's chars, terminator excluded.
    pub start: usize,
    pub end: usize,
    pub cells: Vec<Cell>,
    /// This row's own leading / trailing pipe.
    pub lead: bool,
    pub trail: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// Header, delimiter, then body rows, one per line.
    pub rows: Vec<Row>,
    pub aligns: Vec<Align>,
    /// Style, taken from the header row: leading / trailing pipe, a space
    /// inside the pipes, and the indentation.
    pub lead: bool,
    pub trail: bool,
    pub spaced: bool,
    /// The delimiter row has spaces inside its pipes (`| --- |`).
    pub delim_spaced: bool,
    pub indent: String,
    pub style: Style,
    /// First and last line of the table's text (bordered tables: the outer
    /// rules, which are not rows).
    pub extent: (usize, usize),
    /// Rule layout of a bordered table (None for Markdown).
    pub border: Option<crate::table_border::Border>,
    /// The line ending inside the table.
    pub le: String,
}

/// One row split into cells: char ranges relative to the line.
struct Parts {
    cells: Vec<Range<usize>>,
    lead: bool,
    trail: bool,
    indent: usize,
}

fn split_row(chars: &[char]) -> Option<Parts> {
    let indent = chars.iter().take_while(|&&c| c == ' ').count();
    if indent > 3 || chars.get(indent) == Some(&'\t') {
        return None;
    }
    let mut end = chars.len();
    while end > indent && chars[end - 1].is_whitespace() {
        end -= 1;
    }
    if end == indent || is_fence(&chars[indent..end]) {
        return None;
    }
    let mut pipes = Vec::new();
    let mut i = indent;
    while i < end {
        match chars[i] {
            '\\' => i += 2,
            '|' => {
                pipes.push(i);
                i += 1;
            }
            _ => i += 1,
        }
    }
    let (&first, &last) = (pipes.first()?, pipes.last()?);
    let lead = first == indent;
    let trail = last == end - 1 && !(pipes.len() == 1 && lead);
    let inner = &pipes[usize::from(lead)..pipes.len() - usize::from(trail)];
    let mut cells = Vec::with_capacity(inner.len() + 1);
    let mut start = if lead { indent + 1 } else { indent };
    for &p in inner {
        cells.push(start..p);
        start = p + 1;
    }
    cells.push(start..if trail { end - 1 } else { chars.len() });
    Some(Parts { cells, lead, trail, indent })
}

fn is_fence(t: &[char]) -> bool {
    t.len() >= 3 && (t[..3] == ['`'; 3] || t[..3] == ['~'; 3])
}

fn line_chars(rope: &Rope, line: usize) -> Vec<char> {
    let l = rope.line(line);
    if l.len_chars() > MAX_ROW_CHARS {
        // Never a row: a lone `x` has no pipe.
        return vec!['x'];
    }
    let n = crate::buffer::line_content_len(l);
    l.chars().take(n).collect()
}

fn delimiter_align(t: &[char]) -> Option<Align> {
    let (l, r) = (t.first() == Some(&':'), t.len() > 1 && t.last() == Some(&':'));
    let dashes = &t[usize::from(l)..t.len() - usize::from(r)];
    if dashes.is_empty() || dashes.iter().any(|&c| c != '-') {
        return None;
    }
    Some(match (l, r) {
        (true, true) => Align::Center,
        (true, false) => Align::Left,
        (false, true) => Align::Right,
        (false, false) => Align::None,
    })
}

fn trim(chars: &[char], r: Range<usize>) -> Range<usize> {
    let (mut s, mut e) = (r.start, r.end);
    while s < e && chars[s].is_whitespace() {
        s += 1;
    }
    while e > s && chars[e - 1].is_whitespace() {
        e -= 1;
    }
    if s == e {
        // Empty: just after one padding space, if there is one.
        let p = (r.start + 1).min(r.end);
        return p..p;
    }
    s..e
}

fn make_row(rope: &Rope, line: usize, kind: RowKind, chars: &[char], parts: &Parts) -> Row {
    let start = rope.line_to_char(line);
    let cells = parts
        .cells
        .iter()
        .map(|r| {
            let c = trim(chars, r.clone());
            Cell { span: start + r.start..start + r.end, content: start + c.start..start + c.end }
        })
        .collect();
    Row { line, kind, start, end: start + chars.len(), cells, lead: parts.lead, trail: parts.trail }
}

/// Is `line` inside a fenced code block? Scans the text before it (bytes,
/// so it is fast even for large files; the caller caches the answer).
pub fn in_fence(rope: &Rope, line: usize) -> bool {
    let limit = rope.line_to_byte(line.min(rope.len_lines()));
    let mut open: Option<(u8, usize)> = None;
    let (mut indent, mut run_char, mut run, mut at_start) = (0usize, 0u8, 0usize, true);
    let mut marker: Option<(u8, usize)> = None;
    let mut seen = 0usize;
    let end_line = |marker: &mut Option<(u8, usize)>, open: &mut Option<(u8, usize)>| {
        if let Some((c, n)) = marker.take() {
            match *open {
                None => *open = Some((c, n)),
                Some((oc, on)) if oc == c && n >= on => *open = None,
                Some(_) => {}
            }
        }
    };
    'outer: for chunk in rope.chunks() {
        for &b in chunk.as_bytes() {
            if seen >= limit {
                break 'outer;
            }
            seen += 1;
            if b == b'\n' {
                if run >= 3 {
                    marker = Some((run_char, run));
                }
                end_line(&mut marker, &mut open);
                (indent, run, at_start) = (0, 0, true);
                continue;
            }
            if !at_start {
                continue;
            }
            match b {
                b' ' if run == 0 && indent < 3 => indent += 1,
                b'`' | b'~' if run == 0 || b == run_char => {
                    run_char = b;
                    run += 1;
                }
                _ => {
                    if run >= 3 {
                        marker = Some((run_char, run));
                    }
                    at_start = false;
                }
            }
        }
    }
    open.is_some()
}

impl Table {
    /// The table containing `line`, if any. Costs O(table size) plus a
    /// fence scan of the text above it.
    pub fn at(rope: &Rope, line: usize) -> Option<Table> {
        if line >= rope.len_lines() {
            return None;
        }
        if let Some(t) = crate::table_border::at(rope, line) {
            return Some(t);
        }
        let first = line_chars(rope, line);
        split_row(&first)?;
        let mut a = line;
        while a > 0 && split_row(&line_chars(rope, a - 1)).is_some() {
            a -= 1;
        }
        let mut b = line;
        while b + 1 < rope.len_lines() && split_row(&line_chars(rope, b + 1)).is_some() {
            b += 1;
        }
        // The first header + delimiter pair in the block starts the table;
        // pipe lines above it are prose.
        let mut header = None;
        let mut prev = split_row(&line_chars(rope, a)).map(|p| (line_chars(rope, a), p));
        for k in a + 1..=b.min(line + 1) {
            let chars = line_chars(rope, k);
            let parts = split_row(&chars).expect("block line");
            let (pc, pp) = prev.take().expect("previous line");
            let aligns: Option<Vec<Align>> = parts.cells.iter().map(|r| delimiter_align(&chars[trim(&chars, r.clone())])).collect();
            if let Some(aligns) = aligns
                && aligns.len() == pp.cells.len()
            {
                header = Some((k - 1, pc, pp, chars, parts, aligns));
                break;
            }
            prev = Some((chars, parts));
        }
        let (h, hc, hp, dc, dp, aligns) = header?;
        if line < h || in_fence(rope, h) {
            return None;
        }
        let spaced = hp.cells.iter().any(|r| r.end > r.start && hc[r.start] == ' ');
        let delim_spaced = dp.cells.iter().any(|r| r.end > r.start && dc[r.start] == ' ');
        let mut rows = vec![make_row(rope, h, RowKind::Header, &hc, &hp), make_row(rope, h + 1, RowKind::Delimiter, &dc, &dp)];
        for l in h + 2..=b {
            let chars = line_chars(rope, l);
            let parts = split_row(&chars).expect("block line");
            rows.push(make_row(rope, l, RowKind::Body, &chars, &parts));
        }
        let le = line_ending_of(rope, h);
        Some(Table {
            rows,
            aligns,
            lead: hp.lead,
            trail: hp.trail,
            spaced,
            delim_spaced,
            indent: " ".repeat(hp.indent),
            style: Style::Markdown,
            extent: (h, b),
            border: None,
            le: le.to_string(),
        })
    }

    pub fn first_line(&self) -> usize {
        self.extent.0
    }

    pub fn last_line(&self) -> usize {
        self.extent.1
    }

    /// Is the first row a header (shown bold)? Always for Markdown.
    pub fn has_header(&self) -> bool {
        self.border.is_none_or(|b| b.header)
    }

    fn bordered(&self) -> Option<crate::table_border::Border> {
        self.border.filter(|_| self.style != Style::Markdown)
    }

    /// Re-render a bordered table from `grid` / `aligns`.
    fn re_render(&self, rope: &Rope, grid: &[Vec<String>], aligns: &[Align]) -> (Ops, Vec<Vec<usize>>) {
        self.replace_with(rope, self.style, self.border.expect("bordered"), grid, aligns)
    }

    /// Grid row of table row `r` (header 0, body rows from 1).
    fn grid_index(r: usize) -> usize {
        r.saturating_sub(1)
    }

    /// The whole table rewritten in another format, as one replacement.
    /// Converting to Markdown makes the first row the header.
    pub fn convert(&self, rope: &Rope, to: Style) -> Ops {
        let grid = self.grid(rope);
        let border = self.border.unwrap_or(crate::table_border::Border { header: true, header_double: false, row_rules: false });
        let grid: Vec<Vec<String>> = match (self.style, to) {
            (Style::Markdown, Style::Markdown) => grid,
            (Style::Markdown, _) => grid.iter().map(|r| r.iter().map(|v| unescape(v)).collect()).collect(),
            (_, Style::Markdown) => grid.iter().map(|r| r.iter().map(|v| escape(v)).collect()).collect(),
            _ => grid,
        };
        let border = if self.style == Style::Markdown { crate::table_border::Border { header: true, ..border } } else { border };
        self.replace_with(rope, to, border, &grid, &self.aligns).0
    }

    /// Columns: the most cells in any row.
    pub fn cols(&self) -> usize {
        self.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0)
    }

    /// Char positions of row `r`'s cell-separating pipes.
    pub fn pipes(&self, r: usize) -> Vec<usize> {
        let cells = &self.rows[r].cells;
        let mut v = Vec::with_capacity(cells.len() + 1);
        if self.rows[r].lead {
            v.push(cells[0].span.start - 1);
        }
        for (i, c) in cells.iter().enumerate() {
            if i + 1 < cells.len() || self.rows[r].trail {
                v.push(c.span.end);
            }
        }
        v
    }

    pub fn row_of_line(&self, line: usize) -> Option<usize> {
        if self.style == Style::Markdown {
            return line.checked_sub(self.first_line()).filter(|&i| i < self.rows.len());
        }
        // Rules aren't rows; the virtual separator never matches (the
        // header, at the same line, comes first).
        self.rows.iter().position(|r| r.line == line && !(r.kind == RowKind::Delimiter && r.cells.is_empty()))
    }

    /// (row, column) of the cell holding char position `pos`. A position on
    /// a pipe belongs to the cell before it; the line's edges to the first
    /// and last cell.
    pub fn cell_at(&self, rope: &Rope, pos: usize) -> Option<(usize, usize)> {
        let r = self.row_of_line(rope.char_to_line(pos))?;
        let cells = &self.rows[r].cells;
        let c = cells.iter().position(|c| pos <= c.span.end).unwrap_or(cells.len() - 1);
        Some((r, c))
    }

    fn text(rope: &Rope, r: &Range<usize>) -> String {
        rope.slice(r.clone()).to_string()
    }

    fn align(&self, c: usize) -> Align {
        self.aligns.get(c).copied().unwrap_or(Align::None)
    }

    /// Padding spaces left and right of the content in column `c`.
    fn sides(&self, c: usize, ncols: usize) -> (usize, usize) {
        let left = self.spaced && (c > 0 || self.lead);
        let right = self.spaced && !(c + 1 == ncols && !self.trail);
        (usize::from(left), usize::from(right))
    }

    /// The narrowest content width that leaves the delimiter 3 dashes.
    fn min_inner(&self, c: usize, ncols: usize) -> usize {
        let (l, r) = self.sides(c, ncols);
        if self.delim_spaced { 3 } else { 3usize.saturating_sub(l + r) }
    }

    /// Widest content per column (at least `min_inner`), optionally with
    /// one cell's content replaced.
    fn inner_widths(&self, rope: &Rope, over: Option<(usize, usize, &str)>) -> Vec<usize> {
        let ncols = self.cols();
        (0..ncols)
            .map(|c| {
                self.rows
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r.kind != RowKind::Delimiter)
                    .filter_map(|(i, r)| match over {
                        Some((or, oc, t)) if or == i && oc == c => Some(text_width(t)),
                        _ => r.cells.get(c).map(|cell| text_width(&Self::text(rope, &cell.content))),
                    })
                    .max()
                    .unwrap_or(0)
                    .max(self.min_inner(c, ncols))
            })
            .collect()
    }

    /// Left and right padding that places `text` in `width` cells.
    fn pad_split(text: &str, width: usize, align: Align) -> (usize, usize) {
        let gap = width.saturating_sub(text_width(text));
        match align {
            Align::Right => (gap, 0),
            Align::Center => (gap / 2, gap - gap / 2),
            Align::None | Align::Left => (0, gap),
        }
    }

    fn dashes(width: usize, align: Align) -> String {
        let w = width.max(3);
        match align {
            Align::None => "-".repeat(w),
            Align::Left => format!(":{}", "-".repeat(w - 1)),
            Align::Right => format!("{}:", "-".repeat(w - 1)),
            Align::Center => format!(":{}:", "-".repeat(w - 2)),
        }
    }

    /// Cell `c` of a row of `kind` with content `inner` cells wide, in the
    /// table's spacing. The last cell of a table without a trailing pipe
    /// gets no trailing spaces.
    fn cell_text(&self, kind: RowKind, c: usize, ncols: usize, content: &str, inner: usize) -> String {
        self.cell_text_as(kind, c, ncols, content, inner, self.align(c))
    }

    fn cell_text_as(&self, kind: RowKind, c: usize, ncols: usize, content: &str, inner: usize, align: Align) -> String {
        let (l, r) = self.sides(c, ncols);
        let s = match kind {
            RowKind::Delimiter if self.delim_spaced => format!("{}{}{}", " ".repeat(l), Self::dashes(inner, align), " ".repeat(r)),
            RowKind::Delimiter => Self::dashes(l + inner + r, align),
            _ => {
                let (pl, pr) = Self::pad_split(content, inner, align);
                format!("{}{content}{}", " ".repeat(l + pl), " ".repeat(pr + r))
            }
        };
        if c + 1 == ncols && !self.trail { s.trim_end().to_string() } else { s }
    }

    fn row_text(&self, kind: RowKind, contents: &[String], widths: &[usize]) -> String {
        let n = widths.len();
        let cells: Vec<String> = (0..n).map(|c| self.cell_text(kind, c, n, contents.get(c).map_or("", String::as_str), widths[c])).collect();
        format!("{}{}{}{}", self.indent, if self.lead { "|" } else { "" }, cells.join("|"), if self.trail { "|" } else { "" })
    }

    /// Re-align every row (the Format table command). Rows already in
    /// shape are left alone; short rows gain empty cells.
    pub fn format(&self, rope: &Rope) -> Ops {
        if self.bordered().is_some() {
            return self.re_render(rope, &self.grid(rope), &self.aligns).0;
        }
        let widths = self.inner_widths(rope, None);
        self.rows
            .iter()
            .filter_map(|r| {
                let contents: Vec<String> = r.cells.iter().map(|c| Self::text(rope, &c.content)).collect();
                let new = self.row_text(r.kind, &contents, &widths);
                (Self::text(rope, &(r.start..r.end)) != new).then(|| (r.start, r.end - r.start, new))
            })
            .collect()
    }

    /// True if column `c` lines up: every row that has the cell gives it
    /// the same display width.
    pub fn column_aligned(&self, rope: &Rope, c: usize) -> bool {
        let mut w = None;
        for r in &self.rows {
            if let Some(cell) = r.cells.get(c) {
                let cw = text_width(&Self::text(rope, &cell.span));
                if *w.get_or_insert(cw) != cw {
                    return false;
                }
            }
        }
        w.is_some()
    }

    /// Replace cell (r, c)'s content with `new`. With `realign`, if the rest
    /// of the column lines up, the column is re-padded to fit: it grows as
    /// needed, and shrinks back only if it had no slack before. Returns the
    /// replacements (positions in the old text) and where the new content
    /// starts in the new text.
    pub fn edit_cell(&self, rope: &Rope, r: usize, c: usize, new: &str, realign: bool) -> (Ops, usize) {
        if self.bordered().is_some() {
            // A bordered table is always kept lined up.
            let mut grid = self.grid(rope);
            let g = Self::grid_index(r);
            if grid[g].len() <= c {
                grid[g].resize(c + 1, String::new());
            }
            grid[g][c] = new.to_string();
            let (ops, starts) = self.re_render(rope, &grid, &self.aligns);
            return (ops, starts[g][c]);
        }
        let ncols = self.cols();
        let cell = &self.rows[r].cells[c];
        let last_open = c + 1 == ncols && !self.trail;
        if !realign || last_open || !self.column_aligned(rope, c) {
            let ops = vec![(cell.content.start, cell.content.len(), new.to_string())];
            return (ops, cell.content.start);
        }
        let (l, rr) = self.sides(c, ncols);
        // The column's current inner width, from any other row.
        let other = self.rows.iter().enumerate().find_map(|(i, row)| (i != r).then(|| row.cells.get(c)).flatten()).expect("aligned column");
        let inner_old = text_width(&Self::text(rope, &other.span)).saturating_sub(l + rr);
        let widest_old = self.inner_widths(rope, None)[c];
        let needed = self.inner_widths(rope, Some((r, c, new)))[c];
        let inner = if widest_old < inner_old { needed.max(inner_old) } else { needed };
        let mut ops = Vec::new();
        let mut delta: isize = 0;
        let mut start_new = cell.content.start;
        for (i, row) in self.rows.iter().enumerate() {
            let Some(cl) = row.cells.get(c) else { continue };
            if inner == inner_old && i != r {
                continue;
            }
            let content = if i == r { new.to_string() } else { Self::text(rope, &cl.content) };
            let text = self.cell_text(row.kind, c, ncols, &content, inner);
            if i == r {
                let (pl, _) = Self::pad_split(new, inner, self.align(c));
                start_new = (cl.span.start as isize + delta) as usize + l + pl;
            }
            if Self::text(rope, &cl.span) != text {
                delta += text.chars().count() as isize - cl.span.len() as isize;
                ops.push((cl.span.start, cl.span.len(), text));
            }
        }
        (ops, start_new)
    }

    /// Insert an empty row before row `at` (2 ..= rows.len(); body rows
    /// only). `le` is the document's line ending.
    pub fn insert_row(&self, rope: &Rope, at: usize, le: &str) -> Ops {
        if self.bordered().is_some() {
            let mut grid = self.grid(rope);
            let g = Self::grid_index(at.clamp(2, self.rows.len())).clamp(1, grid.len());
            grid.insert(g, vec![String::new(); self.rows[0].cells.len()]);
            return self.re_render(rope, &grid, &self.aligns).0;
        }
        let row = self.empty_row(rope);
        let at = at.clamp(2, self.rows.len());
        if at < self.rows.len() {
            vec![(self.rows[at].start, 0, format!("{row}{le}"))]
        } else {
            vec![(self.rows[at - 1].end, 0, format!("{le}{row}"))]
        }
    }

    /// Delete body row `r`, line terminator included.
    pub fn delete_row(&self, rope: &Rope, r: usize) -> Option<Ops> {
        let row = self.rows.get(r).filter(|row| row.kind == RowKind::Body)?;
        if self.bordered().is_some() {
            let mut grid = self.grid(rope);
            grid.remove(Self::grid_index(r));
            return Some(self.re_render(rope, &grid, &self.aligns).0);
        }
        let op = if row.line + 1 < rope.len_lines() {
            let next = rope.line_to_char(row.line + 1);
            (row.start, next - row.start, String::new())
        } else {
            // Last line of the file: take the terminator before it instead.
            let prev = self.rows[r - 1].end;
            (prev, row.end - prev, String::new())
        };
        Some(vec![op])
    }

    /// Swap body row `r` with its neighbour above (`up`) or below.
    pub fn move_row(&self, rope: &Rope, r: usize, up: bool) -> Option<(Ops, usize)> {
        let t = if up { r.checked_sub(1)? } else { r + 1 };
        let (a, b) = (self.rows.get(r)?, self.rows.get(t)?);
        if a.kind != RowKind::Body || b.kind != RowKind::Body {
            return None;
        }
        if self.bordered().is_some() {
            let mut grid = self.grid(rope);
            grid.swap(Self::grid_index(r), Self::grid_index(t));
            return Some((self.re_render(rope, &grid, &self.aligns).0, t));
        }
        let ta = Self::text(rope, &(a.start..a.end));
        let tb = Self::text(rope, &(b.start..b.end));
        Some((vec![(a.start, a.end - a.start, tb), (b.start, b.end - b.start, ta)], t))
    }

    /// Insert an empty column at index `at` (0 ..= columns) in every row
    /// that reaches it. Other cells keep their exact text.
    pub fn insert_col(&self, at: usize) -> Ops {
        if self.bordered().is_some() {
            unreachable!("bordered tables use insert_col_in")
        }
        let ncols = self.cols() + 1;
        let mut ops = Vec::new();
        for row in &self.rows {
            let n = row.cells.len();
            if at > n {
                continue;
            }
            let inner = self.min_inner(at, ncols);
            let cell = self.cell_text_as(row.kind, at, ncols, "", inner, Align::None);
            if at == n && !row.trail {
                // Appending to a row without a trailing pipe: a lone `|` at
                // the end would only be a trailing pipe, so the new cell
                // gets one on both sides.
                let open = self.cell_text_as(row.kind, at, ncols + 1, "", self.min_inner(at, ncols + 1), Align::None);
                ops.push((row.end, 0, format!("|{open}|")));
            } else if at == n {
                // After the trailing pipe.
                ops.push((row.cells[n - 1].span.end + 1, 0, format!("{cell}|")));
            } else {
                ops.push((row.cells[at].span.start, 0, format!("{cell}|")));
            }
        }
        ops
    }

    /// Delete column `c` (and one pipe next to it) from every row that has
    /// it. Refused for a single-column table.
    pub fn delete_col(&self, c: usize) -> Option<Ops> {
        if self.rows[0].cells.len() < 2 || c >= self.rows[0].cells.len() {
            return None;
        }
        if self.bordered().is_some() {
            unreachable!("bordered tables use delete_col_in")
        }
        let mut ops = Vec::new();
        for row in &self.rows {
            let n = row.cells.len();
            let Some(cell) = row.cells.get(c) else { continue };
            if n == 1 {
                ops.push((cell.span.start, cell.span.len(), String::new()));
            } else if c + 1 < n {
                // The cell and the pipe after it.
                ops.push((cell.span.start, cell.span.len() + 1, String::new()));
            } else {
                // Last cell: the pipe before it and the cell.
                ops.push((cell.span.start - 1, cell.span.len() + 1, String::new()));
            }
        }
        Some(ops)
    }

    /// Swap column `c` with its left (`left`) or right neighbour in every
    /// row that has both.
    pub fn move_col(&self, rope: &Rope, c: usize, left: bool) -> Option<(Ops, usize)> {
        let t = if left { c.checked_sub(1)? } else { c + 1 };
        if t >= self.rows[0].cells.len() {
            return None;
        }
        if self.bordered().is_some() {
            let mut grid = self.grid(rope);
            for row in &mut grid {
                if row.len() > c.max(t) {
                    row.swap(c, t);
                }
            }
            let mut aligns = self.aligns.clone();
            aligns.resize(self.cols(), Align::None);
            aligns.swap(c, t);
            return Some((self.re_render(rope, &grid, &aligns).0, t));
        }
        let mut ops = Vec::new();
        for row in &self.rows {
            let (Some(a), Some(b)) = (row.cells.get(c), row.cells.get(t)) else { continue };
            // Whole spans swap, so lined-up columns stay lined up.
            let (ta, tb) = (Self::text(rope, &a.span), Self::text(rope, &b.span));
            ops.push((a.span.start, a.span.len(), tb));
            ops.push((b.span.start, b.span.len(), ta));
        }
        Some((ops, t))
    }

    /// Set column `c`'s alignment: the delimiter cell gets the colons, and
    /// if the column lines up its cells are re-padded (same width).
    pub fn set_align(&self, rope: &Rope, c: usize, align: Align) -> Ops {
        if self.bordered().is_some() {
            let mut aligns = self.aligns.clone();
            aligns.resize(self.cols().max(c + 1), Align::None);
            aligns[c] = align;
            return self.re_render(rope, &self.grid(rope), &aligns).0;
        }
        let t = self;
        let ncols = t.cols();
        let (l, r) = t.sides(c, ncols);
        let aligned = t.column_aligned(rope, c);
        let mut ops = Vec::new();
        for row in &t.rows {
            let Some(cell) = row.cells.get(c) else { continue };
            let old = Self::text(rope, &cell.span);
            let new = if aligned && !(c + 1 == ncols && !t.trail) {
                let inner = text_width(&old).saturating_sub(l + r).max(t.min_inner(c, ncols));
                t.cell_text_as(row.kind, c, ncols, &Self::text(rope, &cell.content), inner, align)
            } else if row.kind == RowKind::Delimiter {
                // Keep the delimiter's width and padding, swap the dashes.
                let dash = old.trim();
                let lead = old.len() - old.trim_start().len();
                let trail = old.len() - old.trim_end().len();
                format!("{}{}{}", " ".repeat(lead), Self::dashes(dash.chars().count(), align), " ".repeat(trail))
            } else {
                continue;
            };
            if new != old {
                ops.push((cell.span.start, cell.span.len(), new));
            }
        }
        ops
    }

    /// Insert an empty column at `at`, any table style.
    pub fn insert_col_in(&self, rope: &Rope, at: usize) -> Ops {
        if self.bordered().is_none() {
            return self.insert_col(at);
        }
        let mut grid = self.grid(rope);
        for row in &mut grid {
            if row.len() >= at {
                row.insert(at, String::new());
            }
        }
        let mut aligns = self.aligns.clone();
        aligns.resize(self.cols(), Align::None);
        aligns.insert(at.min(aligns.len()), Align::None);
        self.re_render(rope, &grid, &aligns).0
    }

    /// Delete column `c`, any table style (None for the only column).
    pub fn delete_col_in(&self, rope: &Rope, c: usize) -> Option<Ops> {
        if self.bordered().is_none() {
            return self.delete_col(c);
        }
        if self.rows[0].cells.len() < 2 || c >= self.rows[0].cells.len() {
            return None;
        }
        let mut grid = self.grid(rope);
        for row in &mut grid {
            if row.len() > c {
                row.remove(c);
            }
        }
        let mut aligns = self.aligns.clone();
        if c < aligns.len() {
            aligns.remove(c);
        }
        Some(self.re_render(rope, &grid, &aligns).0)
    }

    /// Cell contents as a grid: the header, then the body rows (the
    /// delimiter row is not data).
    pub fn grid(&self, rope: &Rope) -> Vec<Vec<String>> {
        self.rows.iter().filter(|r| r.kind != RowKind::Delimiter).map(|r| r.cells.iter().map(|c| Self::text(rope, &c.content)).collect()).collect()
    }

    /// Table row of grid row `g`.
    pub fn table_row(g: usize) -> usize {
        if g == 0 { 0 } else { g + 1 }
    }

    /// Write `grid` (header + body rows, possibly longer or wider than the
    /// table) back. Rows marked in `touched`, rows past the end, and every
    /// row when columns were added are rewritten; if the table was lined
    /// up before, all rows are re-padded to keep it so. Other rows keep
    /// their exact text.
    pub fn rewrite(&self, rope: &Rope, grid: &[Vec<String>], touched: &[bool], le: &str) -> Ops {
        if self.bordered().is_some() {
            let mut aligns = self.aligns.clone();
            aligns.resize(grid.iter().map(Vec::len).max().unwrap_or(0), Align::None);
            return self.re_render(rope, grid, &aligns).0;
        }
        let formatted = self.format(rope).is_empty();
        let old_cols = self.cols();
        let ncols = grid.iter().map(Vec::len).max().unwrap_or(0).max(old_cols);
        let widened = ncols > old_cols;
        let mut widths: Vec<usize> = (0..ncols).map(|c| self.min_inner(c, ncols)).collect();
        for row in grid {
            for (c, v) in row.iter().enumerate() {
                widths[c] = widths[c].max(text_width(v));
            }
        }
        let mut ops = Vec::new();
        // A lined-up table stays lined up; in one that wasn't, rewritten
        // rows get no extra padding either.
        let min: Vec<usize> = (0..ncols).map(|c| self.min_inner(c, ncols)).collect();
        let rewrite_row = |kind: RowKind, contents: &[String]| {
            if formatted || kind == RowKind::Delimiter {
                let w = if formatted { &widths } else { &min };
                return self.row_text(kind, contents, w);
            }
            let natural: Vec<usize> = (0..ncols).map(|c| contents.get(c).map_or(0, |v| text_width(v))).collect();
            self.row_text(kind, contents, &natural)
        };
        let body_rows = self.rows.len() - 1;
        for (g, contents) in grid.iter().enumerate() {
            let tr = Self::table_row(g);
            if g < body_rows {
                let row = &self.rows[tr];
                if !(formatted || widened || touched.get(g).copied().unwrap_or(false)) {
                    continue;
                }
                let new = rewrite_row(row.kind, contents);
                if Self::text(rope, &(row.start..row.end)) != new {
                    ops.push((row.start, row.end - row.start, new));
                }
            } else {
                let last = &self.rows[self.rows.len() - 1];
                ops.push((last.end, 0, format!("{le}{}", rewrite_row(RowKind::Body, contents))));
            }
        }
        if formatted || widened {
            let d = &self.rows[1];
            let new = rewrite_row(RowKind::Delimiter, &[]);
            if Self::text(rope, &(d.start..d.end)) != new {
                ops.push((d.start, d.end - d.start, new));
            }
        }
        // New rows were pushed in order at the same position; keep them so.
        merge_inserts(ops)
    }

    /// A new empty row (as text, no line terminator) in the table's style.
    pub fn empty_row(&self, rope: &Rope) -> String {
        let widths = self.inner_widths(rope, None);
        let n = self.rows[0].cells.len().max(1);
        self.row_text(RowKind::Body, &[], &widths[..n.min(widths.len())])
    }
}

/// The terminator of `line` ("\r\n" or "\n"; "\n" for the last line).
pub fn line_ending_of(rope: &Rope, line: usize) -> &'static str {
    let l = rope.line(line);
    let n = l.len_chars();
    if n >= 2 && l.char(n - 2) == '\r' && l.char(n - 1) == '\n' { "\r\n" } else { "\n" }
}

/// A fresh Markdown table for `grid` (first row = header), lined up, with
/// the common `| a |` / `| --- |` style. Returns the text (no final line
/// break) and where each cell's content starts, per grid row.
pub fn render_markdown(indent: &str, le: &str, grid: &[Vec<String>], aligns: &[Align]) -> (String, Vec<Vec<usize>>) {
    let cols = grid.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let widths: Vec<usize> = (0..cols).map(|c| grid.iter().filter_map(|r| r.get(c)).map(|v| text_width(v)).max().unwrap_or(0).max(3)).collect();
    let align = |c: usize| aligns.get(c).copied().unwrap_or(Align::None);
    let mut out = String::new();
    let mut offset = 0usize;
    let mut starts = Vec::with_capacity(grid.len());
    let row_line = |row: &[String], offset: usize, starts: &mut Vec<Vec<usize>>| -> String {
        let mut line = format!("{indent}|");
        let mut cs = Vec::with_capacity(cols);
        for (c, &w) in widths.iter().enumerate() {
            let v = row.get(c).map_or("", String::as_str);
            let (pl, pr) = Table::pad_split(v, w, align(c));
            line.push(' ');
            line.push_str(&" ".repeat(pl));
            cs.push(offset + line.chars().count());
            line.push_str(v);
            line.push_str(&" ".repeat(pr + 1));
            line.push('|');
        }
        starts.push(cs);
        line
    };
    for (g, row) in grid.iter().enumerate() {
        let line = row_line(row, offset, &mut starts);
        offset += line.chars().count() + le.chars().count();
        out.push_str(&line);
        if g == 0 {
            let d: Vec<String> = widths.iter().enumerate().map(|(c, &w)| format!(" {} ", Table::dashes(w, align(c)))).collect();
            let dl = format!("{le}{indent}|{}|", d.join("|"));
            offset += dl.chars().count();
            out.push_str(&dl);
        }
        if g + 1 < grid.len() {
            out.push_str(le);
        }
    }
    if grid.len() == 1 {
        // (the delimiter was already added after the header)
    }
    (out, starts)
}

/// Several inserts at the same position (new rows appended at the end)
/// become one, in order, so applying them can't reverse them.
fn merge_inserts(ops: Ops) -> Ops {
    let mut out: Ops = Vec::with_capacity(ops.len());
    for op in ops {
        match out.last_mut() {
            Some(last) if last.0 == op.0 && last.1 == 0 && op.1 == 0 => last.2.push_str(&op.2),
            _ => out.push(op),
        }
    }
    out
}

/// Cell text as plain data, inverting `escape`: a run of backslashes
/// followed by `|` stands for half as many backslashes and a pipe (`\|`
/// is `|`, `\\\|` is `\|`), and an even run at the end for half as many.
/// Any other backslashes are left as written.
pub fn unescape(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '\\' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let n = chars[i..].iter().take_while(|&&c| c == '\\').count();
        match chars.get(i + n) {
            Some('|') => {
                out.extend(std::iter::repeat_n('\\', n / 2));
                out.push('|');
                i += n + 1;
            }
            None if n % 2 == 0 => {
                out.extend(std::iter::repeat_n('\\', n / 2));
                i += n;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', n));
                i += n;
            }
        }
    }
    out
}

/// Plain data as cell text: newlines and tabs become spaces, surrounding
/// spaces go, `|` becomes `\|`, and a run of backslashes before a pipe (or
/// at the end, where the cell's closing pipe follows) is doubled so it
/// can't pair with that pipe.
pub fn escape(s: &str) -> String {
    let flat = s.replace(['\n', '\r', '\t'], " ");
    let chars: Vec<char> = flat.trim().chars().collect();
    let mut out = String::with_capacity(chars.len() + 4);
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '|' => {
                out.push_str("\\|");
                i += 1;
            }
            '\\' => {
                let n = chars[i..].iter().take_while(|&&c| c == '\\').count();
                let doubled = matches!(chars.get(i + n), None | Some('|'));
                out.extend(std::iter::repeat_n('\\', if doubled { 2 * n } else { n }));
                i += n;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Make edited cell text safe: a bare `|` (one that would split the cell,
/// following the same scanning as `split_row`) becomes `\|`.
pub fn sanitize(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 2);
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' if i + 1 < chars.len() => {
                out.push('\\');
                out.push(chars[i + 1]);
                i += 2;
            }
            '|' => {
                out.push_str("\\|");
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Pasted text as a grid of cell texts: Markdown table rows (every line
/// has a pipe; `---` rows are dropped) or tab-separated lines. A final
/// line break doesn't add an empty row.
pub fn parse_paste(text: &str) -> Vec<Vec<String>> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let lines: Vec<&str> = text.split('\n').collect();
    let md: Option<Vec<Vec<String>>> = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let chars: Vec<char> = l.chars().collect();
            let parts = split_row(&chars)?;
            Some(parts.cells.iter().map(|r| chars[trim(&chars, r.clone())].iter().collect::<String>()).collect::<Vec<String>>())
        })
        .collect();
    match md {
        Some(rows) if !rows.is_empty() => rows
            .into_iter()
            .filter(|cells| !cells.iter().all(|c| delimiter_align(&c.chars().collect::<Vec<_>>()).is_some()))
            .collect(),
        _ => lines.iter().map(|l| l.split('\t').map(escape).collect()).collect(),
    }
}

/// Display width as the editor lays it out (tabs count 1).
pub fn text_width(s: &str) -> usize {
    crate::layout::text_width(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str, mut ops: Ops) -> String {
        let mut rope = Rope::from_str(text);
        ops.sort_by_key(|o| std::cmp::Reverse(o.0));
        for (at, del, ins) in ops {
            rope.remove(at..at + del);
            rope.insert(at, &ins);
        }
        rope.to_string()
    }

    fn table(text: &str, line: usize) -> Option<(Rope, Table)> {
        let rope = Rope::from_str(text);
        let t = Table::at(&rope, line)?;
        Some((rope, t))
    }

    const DOC: &str = "Intro | not a table\n\n| Device | Type | Status |\n|:--|:-:|--:|\n| MPC One | Sampler | Active |\n| Electribe 2 | Groovebox |\nAfter.\n";

    #[test]
    fn recognises_spans_aligns_and_bounds() {
        assert!(table(DOC, 0).is_none(), "prose with a pipe");
        assert!(table(DOC, 1).is_none());
        let (rope, t) = table(DOC, 4).unwrap();
        assert_eq!((t.first_line(), t.last_line()), (2, 5));
        assert_eq!(t.aligns, [Align::Left, Align::Center, Align::Right]);
        assert!(t.lead && t.trail && t.spaced);
        let cell = &t.rows[2].cells[0];
        assert_eq!(rope.slice(cell.content.clone()).to_string(), "MPC One");
        assert_eq!(rope.slice(cell.span.clone()).to_string(), " MPC One ");
        assert_eq!(t.rows[3].cells.len(), 2, "ragged row kept as is");
        assert!(table(DOC, 6).is_none(), "after the table");
        assert_eq!(t.cell_at(&rope, rope.line_to_char(4) + 3), Some((2, 0)));
        assert_eq!(t.cell_at(&rope, rope.line_to_char(4) + 11), Some((2, 1)));
        let l3 = rope.line_to_char(3);
        assert_eq!(t.pipes(1), [l3, l3 + 4, l3 + 8, l3 + 12]);
        let (rope, t) = table("  a | b\n  --|--\n", 0).unwrap();
        assert_eq!(t.pipes(0), [rope.line_to_char(0) + 4], "no edge pipes, indented");
    }

    #[test]
    fn escapes_code_spans_and_pipeless_edges() {
        let (rope, t) = table("a | b\n--|--\nx \\| y | `c|d`\n", 2).unwrap();
        assert!(!t.lead && !t.trail);
        let row: Vec<String> = t.rows[2].cells.iter().map(|c| rope.slice(c.content.clone()).to_string()).collect();
        // As on GitHub: \| is cell text; a pipe in a code span still splits.
        assert_eq!(row, ["x \\| y", "`c", "d`"]);
        // \\| is an escaped backslash followed by a real pipe.
        let (rope, t) = table("|a|b|\n|-|-|\n|\\\\|z|\n", 2).unwrap();
        assert_eq!(rope.slice(t.rows[2].cells[0].content.clone()).to_string(), "\\\\");
    }

    #[test]
    fn rejects_mismatch_fences_and_code() {
        assert!(table("| a | b |\n|---|\n| 1 | 2 |\n", 0).is_none(), "delimiter count differs");
        assert!(table("| a |\n| b |\n", 0).is_none(), "no delimiter row");
        assert!(table("```\n| a | b |\n|---|---|\n```\n", 1).is_none(), "fenced");
        assert!(table("~~~~\n```\n~~~~\n| a | b |\n|---|---|\n", 3).is_some(), "fence closed by a longer run");
        assert!(table("    | a | b |\n    |---|---|\n", 0).is_none(), "indented code");
        assert!(table("Title\n---\n", 0).is_none(), "setext heading");
        assert!(table("| a | b |\n| - | x |\n", 0).is_none(), "bad delimiter cell");
    }

    #[test]
    fn format_aligns_and_keeps_every_cell() {
        let src = "|a|bb|\n|:-:|--:|\n|ccc|d|e|\n|f|\n";
        let (rope, t) = table(src, 0).unwrap();
        let out = apply(src, t.format(&rope));
        // No spaces inside pipes in the source, so none are added.
        assert_eq!(out, "| a | bb|   |\n|:-:|--:|---|\n|ccc|  d|e  |\n| f |   |   |\n", "{out}");
    }

    #[test]
    fn format_spaced_table_and_unicode_widths() {
        let src = "| Name | Kanji |\n|---|---|\n| Ann | 日本 |\n| Émile | 🎹 |\n";
        let (rope, t) = table(src, 0).unwrap();
        let out = apply(src, t.format(&rope));
        assert_eq!(out, "| Name  | Kanji |\n|-------|-------|\n| Ann   | 日本  |\n| Émile | 🎹    |\n", "{out}");
        // Formatting is idempotent: nothing left to change.
        let rope2 = Rope::from_str(&out);
        assert!(Table::at(&rope2, 0).unwrap().format(&rope2).is_empty());
    }

    #[test]
    fn edit_cell_realigns_only_its_column() {
        let src = "Before\n| Device | Type |\n| ------ | ---- |\n| MPC    | Samp |\nAfter\n";
        let (rope, t) = table(src, 2).unwrap();
        assert!(t.column_aligned(&rope, 0));
        let (ops, at) = t.edit_cell(&rope, 2, 0, "MPC One Plus", true);
        let out = apply(src, ops);
        assert_eq!(out, "Before\n| Device       | Type |\n| ------------ | ---- |\n| MPC One Plus | Samp |\nAfter\n", "{out}");
        assert_eq!(Rope::from_str(&out).slice(at..at + 12).to_string(), "MPC One Plus");
        // Shrinking it back restores the original layout.
        let (rope, t) = table(&out, 2).unwrap();
        let (ops, _) = t.edit_cell(&rope, 2, 0, "MPC", true);
        assert_eq!(apply(&out, ops), src);
    }

    #[test]
    fn edit_cell_in_unaligned_column_changes_only_the_content() {
        let src = "|a|b|\n|-|-|\n|x|yy|\n";
        let (rope, t) = table(src, 0).unwrap();
        assert!(!t.column_aligned(&rope, 1));
        let (ops, at) = t.edit_cell(&rope, 2, 1, "zzz", true);
        let out = apply(src, ops);
        assert_eq!(out, "|a|b|\n|-|-|\n|x|zzz|\n");
        assert_eq!(at, src.find("yy").unwrap());
        // An empty cell gets content right after its padding space.
        let src = "| a | b |\n|---|---|\n|   | 1 |\n";
        let (rope, t) = table(src, 2).unwrap();
        let (ops, at) = t.edit_cell(&rope, 2, 0, "q", true);
        let out = apply(src, ops);
        assert_eq!(out, "| a | b |\n|---|---|\n| q | 1 |\n");
        assert_eq!(&out[at..at + 1], "q");
    }

    #[test]
    fn empty_row_matches_style() {
        let (rope, t) = table("| Device | Type |\n|:------:|-----:|\n", 0).unwrap();
        assert_eq!(t.empty_row(&rope), "|        |      |");
        let (rope, t) = table("a | b\n--|--\n", 0).unwrap();
        assert_eq!(t.empty_row(&rope), "   |");
        // The common GitHub style stays as it is when formatted.
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let (rope, t) = table(src, 0).unwrap();
        assert!(t.format(&rope).is_empty());
    }

    #[test]
    fn row_operations() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |";
        let (rope, t) = table(src, 0).unwrap();
        assert_eq!(apply(src, t.insert_row(&rope, 3, "\n")), "| a | b |\n|---|---|\n| 1 | 2 |\n|   |   |\n| 3 | 4 |");
        assert_eq!(apply(src, t.insert_row(&rope, 4, "\n")), "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n|   |   |");
        assert_eq!(apply(src, t.delete_row(&rope, 2).unwrap()), "| a | b |\n|---|---|\n| 3 | 4 |");
        assert_eq!(apply(src, t.delete_row(&rope, 3).unwrap()), "| a | b |\n|---|---|\n| 1 | 2 |", "last line, no terminator");
        assert!(t.delete_row(&rope, 0).is_none() && t.delete_row(&rope, 1).is_none(), "header and delimiter stay");
        let (ops, to) = t.move_row(&rope, 3, true).unwrap();
        assert_eq!((apply(src, ops).as_str(), to), ("| a | b |\n|---|---|\n| 3 | 4 |\n| 1 | 2 |", 2));
        assert!(t.move_row(&rope, 2, true).is_none(), "not above the delimiter");
        assert!(t.move_row(&rope, 3, false).is_none());
    }

    #[test]
    fn column_operations() {
        let src = "| a | bb |\n|---|:--:|\n| 1 | 22 |\n";
        let (rope, t) = table(src, 0).unwrap();
        assert_eq!(apply(src, t.insert_col(1)), "| a |   | bb |\n|---|---|:--:|\n| 1 |   | 22 |\n");
        assert_eq!(apply(src, t.insert_col(0)), "|   | a | bb |\n|---|---|:--:|\n|   | 1 | 22 |\n");
        assert_eq!(apply(src, t.insert_col(2)), "| a | bb |   |\n|---|:--:|---|\n| 1 | 22 |   |\n");
        assert_eq!(apply(src, t.delete_col(0).unwrap()), "| bb |\n|:--:|\n| 22 |\n");
        assert_eq!(apply(src, t.delete_col(1).unwrap()), "| a |\n|---|\n| 1 |\n");
        let (ops, to) = t.move_col(&rope, 0, false).unwrap();
        assert_eq!((apply(src, ops).as_str(), to), ("| bb | a |\n|:--:|---|\n| 22 | 1 |\n", 1));
        assert!(t.move_col(&rope, 0, true).is_none());
        let one = "| a |\n|---|\n";
        let (_, t1) = table(one, 0).unwrap();
        assert!(t1.delete_col(0).is_none(), "the only column stays");
        // Pipeless edges.
        let src = "a | b\n--|--\n1 | 2\n";
        let (_, t) = table(src, 0).unwrap();
        assert_eq!(apply(src, t.insert_col(2)), "a | b|   |\n--|--|---|\n1 | 2|   |\n");
        assert_eq!(apply(src, t.delete_col(1).unwrap()), "a \n--\n1 \n");
    }

    #[test]
    fn alignment_changes() {
        let src = "| Name  | N |\n|-------|---|\n| Ann   | 1 |\n";
        let (rope, t) = table(src, 0).unwrap();
        assert_eq!(apply(src, t.set_align(&rope, 0, Align::Right)), "|  Name | N |\n|------:|---|\n|   Ann | 1 |\n");
        assert_eq!(apply(src, t.set_align(&rope, 0, Align::Center)), "| Name  | N |\n|:-----:|---|\n|  Ann  | 1 |\n");
        // An unaligned column only changes its delimiter.
        let src = "|a|b|\n|---|-|\n|xx|y|\n";
        let (rope, t) = table(src, 0).unwrap();
        assert_eq!(apply(src, t.set_align(&rope, 0, Align::Left)), "|a|b|\n|:--|-|\n|xx|y|\n");
    }

    #[test]
    fn grid_rewrite_and_paste_parsing() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let (rope, t) = table(src, 0).unwrap();
        let mut grid = t.grid(&rope);
        assert_eq!(grid, [vec!["a", "b"], vec!["1", "2"]]);
        // Paste a 2x2 block at the body's second column: one new column
        // and one new row; the table was lined up, so it stays lined up.
        grid[1].resize(3, String::new());
        grid[1][1] = "two".into();
        grid[1][2] = "x".into();
        grid.push(vec![String::new(), "three".into(), "y".into()]);
        let out = apply(src, t.rewrite(&rope, &grid, &[false, true, true], "\n"));
        assert_eq!(out, "| a | b     |   |\n|---|-------|---|\n| 1 | two   | x |\n|   | three | y |\n", "{out}");
        // A sloppy table: only touched rows change.
        let src = "|a|b|\n|-|-|\n|1|2|\n|3|4|\n";
        let (rope, t) = table(src, 0).unwrap();
        let mut grid = t.grid(&rope);
        grid[2][0] = "9".into();
        assert_eq!(apply(src, t.rewrite(&rope, &grid, &[false, false, true], "\n")), "|a|b|\n|-|-|\n|1|2|\n|9|4|\n");
        grid[1].push("new".into());
        assert_eq!(apply(src, t.rewrite(&rope, &grid, &[false, true, false], "\n")), "|a|b||\n|---|---|---|\n|1|2|new|\n|9|4||\n", "a new column reaches every row");

        assert_eq!(parse_paste("a\tb\r\nc|d\te\n"), [vec!["a", "b"], vec!["c\\|d", "e"]]);
        assert_eq!(parse_paste("| x | y |\n|---|---|\n| 1 | \\| |\n"), [vec!["x", "y"], vec!["1", "\\|"]]);
        assert_eq!(parse_paste("one\ntwo"), [vec!["one"], vec!["two"]]);
        assert_eq!(unescape("a\\|b"), "a|b");
        assert_eq!(parse_paste("a\\|b\n"), [vec!["a\\\\\\|b"]], "a backslash before a pipe is kept as data");
    }

    #[test]
    fn escaping_round_trips_and_never_splits_a_cell() {
        let samples = ["a|b", "a\\|b", "a\\\\|b", "ends\\", "\\", "|", "||", "x\\y", "C:\\path\\", "plain"];
        for d in samples {
            let cell = escape(d);
            assert_eq!(unescape(&cell), d, "{d:?} -> {cell:?}");
            // As a lone cell between pipes (no padding) it stays one cell.
            let row: Vec<char> = format!("|{cell}|").chars().collect();
            assert_eq!(split_row(&row).unwrap().cells.len(), 1, "{d:?} -> {cell:?}");
        }
        assert_eq!(sanitize("a|b"), "a\\|b");
        assert_eq!(sanitize("a\\|b"), "a\\|b", "already escaped");
        assert_eq!(sanitize("a\\\\|b"), "a\\\\\\|b", "escaped backslash, bare pipe");
    }

    #[test]
    fn format_lines_up_combining_marks_wide_glyphs_and_zwj_emoji() {
        let src = "| a | b |\n|---|---|\n| e\u{301}te\u{301} | 中文字 |\n| 👩‍💻 | x |\n| plain text | 🎹🎹 |\n";
        let (rope, t) = table(src, 0).unwrap();
        let out = apply(src, t.format(&rope));
        let widths: Vec<usize> = out.lines().map(text_width).collect();
        assert!(widths.windows(2).all(|w| w[0] == w[1]), "rows line up on screen: {widths:?}\n{out}");
        let rope2 = Rope::from_str(&out);
        let t2 = Table::at(&rope2, 0).unwrap();
        assert!(t2.format(&rope2).is_empty());
        assert_eq!(t2.grid(&rope2)[1], ["e\u{301}te\u{301}", "中文字"]);
    }

    #[test]
    fn fence_scan() {
        let rope = Rope::from_str("a\n```rust\nx\n```\nb\n~~~\nc\n");
        let inside: Vec<bool> = (0..7).map(|l| in_fence(&rope, l)).collect();
        assert_eq!(inside, [false, false, true, true, false, false, true]);
        let rope = Rope::from_str("   ```\nin\n    ```\nstill in\n");
        assert!(in_fence(&rope, 3), "a 4-space indented line can't close a fence");
    }

    /// Formatting never loses or changes cell contents, whatever the input.
    #[test]
    fn format_preserves_contents_on_messy_input() {
        let pieces = ["|", "a", " ", "\\|", "--", ":", "日", "\\", "`", "x y", "", "|-|"];
        let mut seed = 0x2545_f491_u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as usize
        };
        for _ in 0..3000 {
            let mut lines = vec!["| h1 | h2 |".to_string(), "|---|:-:|".to_string()];
            for _ in 0..next() % 5 {
                lines.push((0..next() % 6).map(|_| pieces[next() % pieces.len()]).collect::<String>() + "|");
            }
            let src = lines.join("\n") + "\n";
            let Some((rope, t)) = table(&src, 0) else { continue };
            let before: Vec<Vec<String>> = t.rows.iter().filter(|r| r.kind != RowKind::Delimiter).map(|r| r.cells.iter().map(|c| rope.slice(c.content.clone()).to_string()).collect()).collect();
            let out = apply(&src, t.format(&rope));
            let rope2 = Rope::from_str(&out);
            let t2 = Table::at(&rope2, 0).unwrap_or_else(|| panic!("table lost:\n{src}\n=>\n{out}"));
            let after: Vec<Vec<String>> = t2.rows.iter().filter(|r| r.kind != RowKind::Delimiter).map(|r| r.cells.iter().map(|c| rope2.slice(c.content.clone()).to_string()).collect()).collect();
            for (b, a) in before.iter().zip(&after) {
                assert_eq!(&a[..b.len()], &b[..], "\n{src}\n=>\n{out}");
                assert!(a[b.len()..].iter().all(String::is_empty));
            }
            assert_eq!(t2.last_line(), t.last_line(), "rows kept:\n{src}\n=>\n{out}");
            assert!(t2.format(&rope2).is_empty(), "idempotent:\n{out}");
        }
    }
}
