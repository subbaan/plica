//! Bordered plain-text tables (ASCII and Unicode box drawing), on the same
//! `Table` model as Markdown tables.
//!
//! ```text
//! +--------+------+        ┌────────┬──────┐
//! | Device | Type |        │ Device │ Type │
//! +========+======+        ├────────┼──────┤
//! | MPC    | Samp |        │ MPC    │ Samp │
//! +--------+------+        └────────┴──────┘
//! ```
//!
//! Grammar, deliberately strict so ordinary text is never mistaken for a
//! table (there is no border-less plain-text table format for that reason):
//! - A block of lines with the same indentation, starting with a top rule
//!   and ending with a bottom rule; every line in between is a rule or a
//!   row. A table needs at least one row.
//! - Rules: ASCII `+---+---+` (`=` for a header rule); Unicode `┌─┬─┐`,
//!   `├─┼─┤` or `╞═╪═╡`, `└─┴─┘`.
//! - Rows start and end with `|` (ASCII) or `│` (Unicode).
//! - Column boundaries are the top rule's joints, by display column. Every
//!   line must have its joint or bar at exactly those columns, so a `|`
//!   inside a cell is just text (no escaping in bordered tables).
//! - A rule right after the first row makes that row a header.
//!
//! The model presents the same shape as a Markdown table: rows are
//! `[header, separator, body…]`, where the separator is the header rule,
//! or a virtual empty row (no cells, at the header's line) when there is
//! none. The outer rules and any rules between body rows are part of the
//! table's extent but not rows. Every edit re-renders the whole table from
//! its cell grid (a bordered table must stay lined up) and writes back only
//! the text that changed.

use crate::table::{Align, Cell, Ops, Row, RowKind, Style, Table, text_width};
use ropey::Rope;
use unicode_segmentation::UnicodeSegmentation;

/// How a bordered table's rules are drawn and placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Border {
    /// The first row is a header (a rule follows it).
    pub header: bool,
    /// The header rule is drawn double (`=` / `═`).
    pub header_double: bool,
    /// A rule between every pair of body rows.
    pub row_rules: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Line {
    Top,
    Mid { double: bool },
    Bottom,
    /// ASCII rules don't say where they are.
    AsciiRule { double: bool },
    Row,
}

fn family_of(c: char) -> Option<Style> {
    match c {
        '+' | '|' => Some(Style::Ascii),
        '┌' | '├' | '╞' | '└' | '│' => Some(Style::Unicode),
        _ => None,
    }
}

/// Classify a line (indentation already removed, trailing spaces too).
fn classify(t: &[char], style: Style) -> Option<Line> {
    let (first, last) = (*t.first()?, *t.last()?);
    if t.len() < 2 {
        return None;
    }
    match style {
        Style::Ascii => {
            if first == '|' && last == '|' {
                return Some(Line::Row);
            }
            if first != '+' || last != '+' || !t.iter().all(|c| matches!(c, '+' | '-' | '=')) || t.windows(2).any(|w| w == ['+', '+']) {
                return None;
            }
            Some(Line::AsciiRule { double: t.contains(&'=') })
        }
        Style::Unicode => {
            if first == '│' && last == '│' {
                return Some(Line::Row);
            }
            let body = &t[1..t.len() - 1];
            let (fill, joint) = match first {
                '┌' => ('─', '┬'),
                '├' => ('─', '┼'),
                '╞' => ('═', '╪'),
                '└' => ('─', '┴'),
                _ => return None,
            };
            let close = match first {
                '┌' => '┐',
                '├' => '┤',
                '╞' => '╡',
                _ => '┘',
            };
            if last != close || body.is_empty() || !body.iter().all(|&c| c == fill || c == joint) {
                return None;
            }
            Some(match first {
                '┌' => Line::Top,
                '└' => Line::Bottom,
                _ => Line::Mid { double: first == '╞' },
            })
        }
        Style::Markdown => None,
    }
}

/// The line's chars without the terminator, if not too long.
fn chars_of(rope: &Rope, line: usize) -> Vec<char> {
    let l = rope.line(line);
    if l.len_chars() > 20_000 {
        return Vec::new();
    }
    let n = crate::buffer::line_content_len(l);
    l.chars().take(n).collect()
}

/// (indent, content without indent and trailing spaces).
fn split_indent(chars: &[char]) -> (usize, &[char]) {
    let indent = chars.iter().take_while(|&&c| c == ' ').count();
    let mut end = chars.len();
    while end > indent && chars[end - 1] == ' ' {
        end -= 1;
    }
    (indent, &chars[indent..end])
}

fn line_kind(chars: &[char], style: Style, indent: usize) -> Option<Line> {
    let (i, t) = split_indent(chars);
    if i != indent {
        return None;
    }
    classify(t, style)
}

/// Char offsets (in `t`) of the chars at the given display columns, or
/// None if a column falls inside a wide char or past the end.
fn at_columns(t: &[char], cols: &[usize]) -> Option<Vec<usize>> {
    if t.iter().all(|c| c.is_ascii() && !c.is_ascii_control()) {
        // One column per char.
        return cols.iter().all(|&c| c < t.len()).then(|| cols.to_vec());
    }
    let s: String = t.iter().collect();
    let mut out = Vec::with_capacity(cols.len());
    let (mut col, mut idx) = (0usize, 0usize);
    let mut want = cols.iter().peekable();
    for g in s.graphemes(true) {
        let n = g.chars().count();
        while let Some(&&c) = want.peek() {
            if c == col {
                out.push(idx);
                want.next();
            } else if c < col {
                return None;
            } else {
                break;
            }
        }
        col += text_width(g);
        idx += n;
    }
    (want.next().is_none()).then_some(out)
}

/// The bordered table containing `line`, or the range of lines around it
/// known not to hold one (so a failed block isn't re-scanned line by line).
pub fn scan(rope: &Rope, line: usize) -> Result<Table, (usize, usize)> {
    let single = Err((line, line));
    let chars = chars_of(rope, line);
    let (indent, t) = split_indent(&chars);
    let Some(style) = t.first().and_then(|&c| family_of(c)) else { return single };
    if classify(t, style).is_none() {
        return single;
    }
    // Each line is read once: `lines[i]` is line `a + i`.
    let n = rope.len_lines();
    let mut above: Vec<Vec<char>> = Vec::new();
    let mut a = line;
    while a > 0 {
        let c = chars_of(rope, a - 1);
        if line_kind(&c, style, indent).is_none() {
            break;
        }
        above.push(c);
        a -= 1;
    }
    above.reverse();
    let mut lines = above;
    lines.push(chars.clone());
    let mut b = line;
    while b + 1 < n {
        let c = chars_of(rope, b + 1);
        if line_kind(&c, style, indent).is_none() {
            break;
        }
        lines.push(c);
        b += 1;
    }
    let kinds: Vec<Line> = lines.iter().map(|c| line_kind(c, style, indent).expect("block line")).collect();
    // A failed ASCII block fails for every line in it; a Unicode block only
    // if it holds a single ┌ … └ pair (else lines may pick other pairs).
    let tops = kinds.iter().filter(|k| **k == Line::Top).count();
    let bottoms = kinds.iter().filter(|k| **k == Line::Bottom).count();
    let miss = if style == Style::Ascii || (tops <= 1 && bottoms <= 1) { (a, b) } else { (line, line) };
    build(rope, line, style, indent, &lines, &kinds, a, b).ok_or(miss)
}

#[allow(clippy::too_many_arguments)]
fn build(rope: &Rope, line: usize, style: Style, indent: usize, lines: &[Vec<char>], kinds: &[Line], a: usize, b: usize) -> Option<Table> {
    // The table around `line`: ASCII rules don't say top or bottom, so the
    // whole block must start and end with one; Unicode tables are the
    // nearest ┌ above and └ below.
    let (top, bottom) = match style {
        Style::Ascii => {
            let rule = |k: Line| matches!(k, Line::AsciiRule { .. });
            if !rule(kinds[0]) || !rule(kinds[b - a]) {
                return None;
            }
            (a, b)
        }
        _ => {
            let top = (a..=line).rev().find(|&l| kinds[l - a] == Line::Top)?;
            let bottom = (line..=b).find(|&l| kinds[l - a] == Line::Bottom)?;
            (top, bottom)
        }
    };
    if top >= bottom {
        return None;
    }
    let kinds = &kinds[top - a..=bottom - a];
    if kinds[1..kinds.len() - 1].iter().any(|k| matches!(k, Line::Top | Line::Bottom)) {
        return None;
    }
    if !kinds.contains(&Line::Row) {
        return None;
    }
    // Column boundaries from the top rule's joints.
    let (_, tt) = split_indent(&lines[top - a]);
    let mut joints = Vec::new();
    let mut col = 0;
    for (i, ch) in tt.iter().enumerate() {
        if i == 0 || i + 1 == tt.len() || matches!(ch, '+' | '┬') {
            joints.push(col);
        }
        col += if ch.is_ascii() { 1 } else { text_width(&ch.to_string()) };
    }
    if joints.len() < 2 {
        return None;
    }
    let vbar = if style == Style::Ascii { '|' } else { '│' };
    let mut rows_raw: Vec<(usize, Vec<Cell>)> = Vec::new();
    let mut rules: Vec<(usize, Line, Vec<Cell>)> = Vec::new();
    for (k, kind) in kinds.iter().enumerate() {
        let l = top + k;
        let lc = &lines[l - a];
        let (_, t) = split_indent(lc);
        let offs = at_columns(t, &joints)?;
        if offs.len() != joints.len() || offs[offs.len() - 1] + 1 != t.len() {
            return None;
        }
        let ok = match kind {
            Line::Row => offs.iter().all(|&o| t[o] == vbar),
            _ => offs[1..offs.len() - 1].iter().all(|&o| matches!(t[o], '+' | '┬' | '┼' | '╪' | '┴')),
        };
        if !ok {
            return None;
        }
        let start = rope.line_to_char(l) + indent;
        let cells: Vec<Cell> = offs
            .windows(2)
            .map(|w| {
                let span = start + w[0] + 1..start + w[1];
                let inner = &t[w[0] + 1..w[1]];
                let s = inner.iter().take_while(|c| **c == ' ').count();
                let e = inner.len() - inner.iter().rev().take_while(|c| **c == ' ').count();
                let content = if s >= e {
                    let p = span.start + 1.min(span.len());
                    p..p
                } else {
                    span.start + s..span.start + e
                };
                Cell { span, content }
            })
            .collect();
        match kind {
            Line::Row => rows_raw.push((l, cells)),
            _ => rules.push((l, *kind, cells)),
        }
    }
    if in_fence(rope, top) {
        return None;
    }
    // Header: a rule right after the first row (that isn't the bottom).
    let first_row = rows_raw[0].0;
    let header_rule = rules.iter().find(|(l, _, _)| *l == first_row + 1 && *l != bottom);
    let header = header_rule.is_some() && rows_raw.len() > 1;
    let header_double = header_rule.is_some_and(|(_, k, _)| matches!(k, Line::Mid { double: true } | Line::AsciiRule { double: true }));
    let body: Vec<usize> = rows_raw.iter().skip(1).map(|(l, _)| *l).collect();
    let row_rules = body.len() > 1 && body.windows(2).all(|w| rules.iter().any(|(l, _, _)| *l == w[0] + 1 && w[1] == w[0] + 2));
    let le = crate::table::line_ending_of(rope, top);
    let row = |l: usize, kind: RowKind, cells: Vec<Cell>| {
        let s = rope.line_to_char(l);
        let len = lines[l - a].len();
        Row { line: l, kind, start: s, end: s + len, cells, lead: true, trail: true }
    };
    let mut rows = Vec::with_capacity(rows_raw.len() + 1);
    let mut raw = rows_raw.into_iter();
    let (hl, hc) = raw.next().expect("a row");
    rows.push(row(hl, RowKind::Header, hc));
    match header_rule.filter(|_| header) {
        Some((l, _, cells)) => rows.push(row(*l, RowKind::Delimiter, cells.clone())),
        None => {
            let h = &rows[0];
            rows.push(Row { line: h.line, kind: RowKind::Delimiter, start: h.end, end: h.end, cells: Vec::new(), lead: true, trail: true });
        }
    }
    for (l, cells) in raw {
        rows.push(row(l, RowKind::Body, cells));
    }
    let mut t = Table {
        rows,
        aligns: Vec::new(),
        lead: true,
        trail: true,
        spaced: true,
        delim_spaced: true,
        indent: " ".repeat(indent),
        style,
        extent: (top, bottom),
        border: Some(Border { header, header_double, row_rules }),
        le: le.to_string(),
    };
    t.aligns = infer_aligns(rope, &t);
    Some(t)
}

fn in_fence(rope: &Rope, line: usize) -> bool {
    crate::table::in_fence(rope, line)
}

/// Bordered tables have no alignment syntax: read it from how the cells
/// are padded. A column is right- or centre-aligned if every padded cell
/// in it agrees (and at least one shows it), else left.
fn infer_aligns(rope: &Rope, t: &Table) -> Vec<Align> {
    let cols = t.cols();
    (0..cols)
        .map(|c| {
            let (mut can_l, mut can_c, mut can_r, mut evidence) = (true, true, true, false);
            for row in t.rows.iter().filter(|r| r.kind != RowKind::Delimiter) {
                let Some(cell) = row.cells.get(c) else { continue };
                if cell.content.is_empty() {
                    continue;
                }
                let lp = cell.content.start - cell.span.start - 1;
                let rp = (cell.span.end - cell.content.end).saturating_sub(1);
                let _ = rope;
                let gap = lp + rp;
                if gap == 0 {
                    continue;
                }
                can_l &= lp == 0;
                can_r &= rp == 0;
                can_c &= lp == gap / 2;
                evidence |= lp > 0;
            }
            if !evidence || can_l {
                Align::None
            } else if can_r {
                Align::Right
            } else if can_c {
                Align::Center
            } else {
                Align::None
            }
        })
        .collect()
}

/// The whole table as text (lines joined by the table's line ending, no
/// final terminator) for `grid` (first row = header), plus where each
/// cell's content starts (char offset in that text), per grid row.
pub fn render(style: Style, border: Border, indent: &str, le: &str, grid: &[Vec<String>], aligns: &[Align]) -> (String, Vec<Vec<usize>>) {
    let cols = grid.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let widths: Vec<usize> = (0..cols).map(|c| grid.iter().filter_map(|r| r.get(c)).map(|v| text_width(v)).max().unwrap_or(0).max(1)).collect();
    let align = |c: usize| aligns.get(c).copied().unwrap_or(Align::None);
    let (v, h, hd) = match style {
        Style::Unicode => ('│', '─', '═'),
        _ => ('|', '-', '='),
    };
    let rule = |pos: u8, double: bool| -> String {
        let (l, j, r) = match (style, pos, double) {
            (Style::Unicode, 0, _) => ('┌', '┬', '┐'),
            (Style::Unicode, 2, _) => ('└', '┴', '┘'),
            (Style::Unicode, _, true) => ('╞', '╪', '╡'),
            (Style::Unicode, _, false) => ('├', '┼', '┤'),
            _ => ('+', '+', '+'),
        };
        let fill = if double { hd } else { h };
        let segs: Vec<String> = widths.iter().map(|w| fill.to_string().repeat(w + 2)).collect();
        format!("{indent}{l}{}{r}", segs.join(&j.to_string()))
    };
    let mut lines: Vec<String> = vec![rule(0, false)];
    let mut starts = Vec::with_capacity(grid.len());
    let mut offset = lines[0].chars().count() + le.chars().count();
    for (g, row) in grid.iter().enumerate() {
        if g > 0 && ((g == 1 && border.header) || (g > 1 && border.row_rules) || (g == 1 && !border.header && border.row_rules)) {
            let r = rule(1, g == 1 && border.header && border.header_double);
            offset += r.chars().count() + le.chars().count();
            lines.push(r);
        }
        let mut line = format!("{indent}{v}");
        let mut cs = Vec::with_capacity(cols);
        for (c, &w) in widths.iter().enumerate() {
            let content = row.get(c).map_or("", String::as_str);
            let gap = w.saturating_sub(text_width(content));
            let (pl, pr) = match align(c) {
                Align::Right => (gap, 0),
                Align::Center => (gap / 2, gap - gap / 2),
                _ => (0, gap),
            };
            line.push(' ');
            line.push_str(&" ".repeat(pl));
            cs.push(offset + line.chars().count());
            line.push_str(content);
            line.push_str(&" ".repeat(pr + 1));
            line.push(v);
        }
        offset += line.chars().count() + le.chars().count();
        starts.push(cs);
        lines.push(line);
    }
    lines.push(rule(2, false));
    (lines.join(le), starts)
}

impl Table {
    /// The table's text, first line start to last line end.
    pub fn extent_chars(&self, rope: &Rope) -> (usize, usize) {
        let s = rope.line_to_char(self.extent.0);
        let e = rope.line_to_char(self.extent.1) + crate::buffer::line_content_len(rope.line(self.extent.1));
        (s, e)
    }

    /// Replace this bordered table with `grid` rendered (in `style`), as
    /// the smallest single replacement. Returns it and where each cell's
    /// content will start in the new text.
    pub fn replace_with(&self, rope: &Rope, style: Style, border: Border, grid: &[Vec<String>], aligns: &[Align]) -> (Ops, Vec<Vec<usize>>) {
        let (s, e) = self.extent_chars(rope);
        let (text, starts) = match style {
            Style::Markdown => crate::table::render_markdown(&self.indent, &self.le, grid, aligns),
            _ => render(style, border, &self.indent, &self.le, grid, aligns),
        };
        let starts = starts.into_iter().map(|r| r.into_iter().map(|o| s + o).collect()).collect();
        (minimal_op(rope, s, e, &text), starts)
    }
}

/// The smallest single replacement turning chars `s..e` into `new`.
pub fn minimal_op(rope: &Rope, s: usize, e: usize, new: &str) -> Ops {
    let old: Vec<char> = rope.slice(s..e).chars().collect();
    let new: Vec<char> = new.chars().collect();
    let pre = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let max_suf = old.len().min(new.len()) - pre;
    let suf = old.iter().rev().zip(new.iter().rev()).take(max_suf).take_while(|(a, b)| a == b).count();
    if pre == old.len() && pre == new.len() {
        return Vec::new();
    }
    vec![(s + pre, old.len() - pre - suf, new[pre..new.len() - suf].iter().collect())]
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

    const ASCII: &str = "Before | not a table\n\n+--------+------+\n| Device | Type |\n+========+======+\n| MPC    | Samp |\n| a|b    | x    |\n+--------+------+\nAfter\n";
    const UNI: &str = "┌────────┬──────┐\n│ Device │ Type │\n├────────┼──────┤\n│ 日本   │    9 │\n└────────┴──────┘\n";

    #[test]
    fn recognises_ascii_and_unicode_tables() {
        let rope = Rope::from_str(ASCII);
        let t = Table::at(&rope, 5).unwrap();
        assert_eq!((t.style, t.first_line(), t.last_line()), (Style::Ascii, 2, 7));
        assert_eq!(t.border.unwrap(), Border { header: true, header_double: true, row_rules: false });
        let grid = t.grid(&rope);
        assert_eq!(grid, [vec!["Device", "Type"], vec!["MPC", "Samp"], vec!["a|b", "x"]], "a | inside a cell is text");
        assert_eq!(t.row_of_line(2), None, "the top rule is not a row");
        assert_eq!(t.row_of_line(5), Some(2));
        assert!(Table::at(&rope, 0).is_none() && Table::at(&rope, 8).is_none());
        let rope = Rope::from_str(UNI);
        let t = Table::at(&rope, 0).unwrap();
        assert_eq!(t.style, Style::Unicode);
        assert_eq!(t.grid(&rope)[1], ["日本", "9"]);
        assert_eq!(t.aligns, [Align::None, Align::Right], "right alignment read from the padding");
    }

    #[test]
    fn rejects_broken_or_prose_lookalikes() {
        for bad in [
            "+---+---+\n| a | b |\n",                       // no bottom rule
            "+---+---+\n| a |  b |\n+---+---+\n",            // bar off its joint
            "+---+---+\n+---+---+\n",                        // no rows
            "Name      Type\nMPC       Sampler\n",           // aligned prose
            "┌───┬───┐\n│ a │ b │\n+---+---+\n",             // mixed families
            "```\n+---+\n| a |\n+---+\n```\n",               // fenced
        ] {
            let rope = Rope::from_str(bad);
            assert!((0..rope.len_lines()).all(|l| Table::at(&rope, l).is_none_or(|t| t.style == Style::Markdown)), "{bad}");
        }
    }

    #[test]
    fn editing_re_renders_and_keeps_the_rest() {
        let rope = Rope::from_str(ASCII);
        let t = Table::at(&rope, 5).unwrap();
        let (ops, at) = t.edit_cell(&rope, 2, 0, "MPC One Plus", true);
        let out = apply(ASCII, ops);
        assert_eq!(out, "Before | not a table\n\n+--------------+------+\n| Device       | Type |\n+==============+======+\n| MPC One Plus | Samp |\n| a|b          | x    |\n+--------------+------+\nAfter\n");
        assert_eq!(&out[out.char_indices().nth(at).unwrap().0..][..12], "MPC One Plus");
        let rope = Rope::from_str(&out);
        let t = Table::at(&rope, 5).unwrap();
        let (ops, _) = t.edit_cell(&rope, 2, 0, "MPC", true);
        assert_eq!(apply(&out, ops), ASCII, "shrinks back");
        let rope = Rope::from_str(ASCII);
        assert!(t.format(&rope).is_empty() || Table::at(&rope, 5).unwrap().format(&rope).is_empty(), "already formatted");
    }

    #[test]
    fn rows_columns_and_alignment() {
        let rope = Rope::from_str(UNI);
        let t = Table::at(&rope, 0).unwrap();
        let out = apply(UNI, t.insert_row(&rope, 3, "\n"));
        assert_eq!(out, "┌────────┬──────┐\n│ Device │ Type │\n├────────┼──────┤\n│ 日本   │    9 │\n│        │      │\n└────────┴──────┘\n");
        let out = apply(UNI, t.insert_col_in(&rope, 1));
        assert_eq!(out, "┌────────┬───┬──────┐\n│ Device │   │ Type │\n├────────┼───┼──────┤\n│ 日本   │   │    9 │\n└────────┴───┴──────┘\n");
        let out = apply(UNI, t.set_align(&rope, 0, Align::Center));
        assert_eq!(out, "┌────────┬──────┐\n│ Device │ Type │\n├────────┼──────┤\n│  日本  │    9 │\n└────────┴──────┘\n");
        assert!(t.delete_row(&rope, 0).is_none(), "the header stays");
        let (ops, _) = t.move_col(&rope, 0, false).unwrap();
        assert_eq!(apply(UNI, ops), "┌──────┬────────┐\n│ Type │ Device │\n├──────┼────────┤\n│    9 │ 日本   │\n└──────┴────────┘\n");
    }

    #[test]
    fn headerless_and_row_ruled_tables_keep_their_shape() {
        let src = "+---+---+\n| 1 | 2 |\n+---+---+\n| 3 | 4 |\n+---+---+\n";
        let rope = Rope::from_str(src);
        let t = Table::at(&rope, 1).unwrap();
        assert!(t.border.unwrap().header, "a rule after the first row reads as a header");
        let src = "+---+---+\n| 1 | 2 |\n| 3 | 4 |\n+---+---+\n";
        let rope = Rope::from_str(src);
        let t = Table::at(&rope, 2).unwrap();
        assert_eq!(t.border.unwrap(), Border { header: false, header_double: false, row_rules: false });
        assert_eq!(t.rows[1].cells.len(), 0, "virtual separator");
        assert!(t.format(&rope).is_empty(), "renders back to itself");
        let (ops, _) = t.edit_cell(&rope, 2, 1, "40", true);
        assert_eq!(apply(src, ops), "+---+----+\n| 1 | 2  |\n| 3 | 40 |\n+---+----+\n");
    }
}
