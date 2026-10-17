//! Editing inside Markdown tables: typing into cells (with the column
//! re-padded as it grows), Tab / Shift+Tab between cells, Enter for a new
//! row, and the Format table command.
//!
//! Table keys apply whenever the caret is in a recognised table. Esc (with
//! nothing selected) turns them off for that table, so Tab, Enter and
//! Backspace edit plain text again; Esc again, or leaving the table, turns
//! them back on. Edits that don't fit a single cell (a selection across
//! cells, typing in the delimiter row) fall through to plain editing.

use crate::app::{App, TableAction};
use crate::buffer::{EditKind, Selection};
use crate::keymap::Command;
use crate::table::{RowKind, Style, Table};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::rc::Rc;

/// A rectangle of selected cells in one table, valid for one buffer
/// version. Rows are table rows (the delimiter row is never data).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellSel {
    pub key: (u64, u64),
    /// The table's first line (identifies it).
    pub table: usize,
    pub anchor: (usize, usize),
    pub head: (usize, usize),
}

impl CellSel {
    /// (first row, first col, last row, last col), inclusive.
    pub fn rect(&self) -> (usize, usize, usize, usize) {
        let (a, h) = (self.anchor, self.head);
        (a.0.min(h.0), a.1.min(h.1), a.0.max(h.0), a.1.max(h.1))
    }

    pub fn contains(&self, r: usize, c: usize) -> bool {
        let (r0, c0, r1, c1) = self.rect();
        r != 1 && (r0..=r1).contains(&r) && (c0..=c1).contains(&c)
    }
}

impl App {
    /// The cell selection, if still valid, with its table.
    pub fn cell_sel_live(&self) -> Option<(Rc<Table>, CellSel)> {
        let cs = self.cell_sel?;
        if cs.key != self.tab.buf.cache_key() {
            return None;
        }
        let t = self.tab.table_at(cs.table)?;
        (t.first_line() == cs.table).then_some((t, cs))
    }

    /// Select cells from `anchor` to `head`; the text caret goes to the
    /// head cell (no text selection is shown meanwhile).
    pub fn set_cell_sel(&mut self, t: &Table, anchor: (usize, usize), head: (usize, usize)) {
        let clamp = |(r, c): (usize, usize)| {
            let r = r.min(t.rows.len() - 1);
            let r = if r == 1 { 2.min(t.rows.len() - 1) } else { r };
            (r, c.min(t.cols().saturating_sub(1)))
        };
        let (anchor, head) = (clamp(anchor), clamp(head));
        self.cell_sel = Some(CellSel { key: self.tab.buf.cache_key(), table: t.first_line(), anchor, head });
        let row = &t.rows[head.0];
        let pos = row.cells.get(head.1).map_or(row.end, |c| c.content.start);
        self.tab.buf.sel = Selection::caret(pos);
        self.tab.buf.break_undo_group();
        self.table_typed_at = None;
    }

    /// Selected cells as tab-separated text (pipes unescaped).
    pub fn cell_sel_text(&self) -> Option<(String, usize)> {
        let (t, cs) = self.cell_sel_live()?;
        let rope = self.tab.buf.rope();
        let (r0, c0, r1, c1) = cs.rect();
        let mut lines = Vec::new();
        for r in (r0..=r1).filter(|&r| r != 1) {
            let row = &t.rows[r];
            let cells: Vec<String> = (c0..=c1)
                .map(|c| {
                    row.cells.get(c).map_or_else(String::new, |cell| {
                        let v = rope.slice(cell.content.clone()).to_string();
                        // Only Markdown escapes pipes.
                        if t.style == Style::Markdown { crate::table::unescape(&v) } else { v }
                    })
                })
                .collect();
            lines.push(cells.join("\t"));
        }
        let n = lines.len() * (c1 - c0 + 1);
        Some((lines.join("\n") + "\n", n))
    }

    /// Empty the selected cells (Delete, Backspace, Cut), as one undo step.
    pub fn clear_cell_sel(&mut self) -> bool {
        let Some((t, cs)) = self.cell_sel_live() else { return false };
        let rope = self.tab.buf.rope();
        let mut grid = t.grid(rope);
        let mut touched = vec![false; grid.len()];
        let (r0, c0, r1, c1) = cs.rect();
        for r in (r0..=r1).filter(|&r| r != 1) {
            let g = if r == 0 { 0 } else { r - 1 };
            for c in c0..=c1 {
                if let Some(v) = grid[g].get_mut(c) {
                    v.clear();
                    touched[g] = true;
                }
            }
        }
        let ops = t.rewrite(rope, &grid, &touched, self.tab.doc.line_ending.as_str());
        let caret = self.tab.buf.sel;
        self.tab.buf.edit(ops, EditKind::Other, Some(caret), false);
        if let Some(nt) = self.tab.table_at(cs.table) {
            self.set_cell_sel(&nt, cs.anchor, cs.head);
        }
        true
    }

    /// Paste a grid of cells (tab-separated or Markdown rows) with its top
    /// left corner at the selected cells or the caret's cell. Rows and
    /// columns are added as needed; nothing pasted is dropped.
    fn table_paste_grid(&mut self, t: &Table, text: &str) -> bool {
        let mut data = crate::table::parse_paste(text);
        if t.style != Style::Markdown {
            // Bordered tables hold cell text as is.
            data = data.into_iter().map(|r| r.iter().map(|v| crate::table::unescape(v)).collect()).collect();
        }
        if data.is_empty() {
            return false;
        }
        let rope = self.tab.buf.rope();
        let (r, c) = match self.cell_sel_live() {
            Some((_, cs)) => {
                let (r0, c0, _, _) = cs.rect();
                (r0, c0)
            }
            None => match t.cell_at(rope, self.tab.buf.sel.head) {
                Some(rc) => rc,
                None => return false,
            },
        };
        let g0 = match r {
            0 => 0,
            1 => 1,
            r => r - 1,
        };
        let mut grid = t.grid(rope);
        let mut touched = vec![false; grid.len()];
        for (i, vals) in data.iter().enumerate() {
            let g = g0 + i;
            if g >= grid.len() {
                grid.push(Vec::new());
                touched.push(true);
            }
            if grid[g].len() < c + vals.len() {
                grid[g].resize(c + vals.len(), String::new());
            }
            for (j, v) in vals.iter().enumerate() {
                grid[g][c + j] = v.clone();
            }
            touched[g] = true;
        }
        let ops = t.rewrite(rope, &grid, &touched, self.tab.doc.line_ending.as_str());
        self.tab.buf.edit(ops, EditKind::Other, None, false);
        let rows = data.len();
        let cols = data.iter().map(Vec::len).max().unwrap_or(1);
        if let Some(nt) = self.tab.table_at(t.first_line()) {
            let tr = |g: usize| if g == 0 { 0 } else { g + 1 };
            self.set_cell_sel(&nt, (tr(g0), c), (tr(g0 + rows - 1), c + cols - 1));
        }
        self.info(format!("Pasted {rows} × {cols} cells (Ctrl+Z undoes)"));
        true
    }

    /// Shift+arrows in a table: extend the cell selection, or start one
    /// when the text selection would leave the cell. Returns false to let
    /// ordinary text selection happen.
    fn table_shift_arrow(&mut self, t: &Table, code: KeyCode) -> bool {
        let step = |t: &Table, (r, c): (usize, usize)| -> Option<(usize, usize)> {
            let last_row = t.rows.len() - 1;
            let last_col = t.cols().saturating_sub(1);
            match code {
                KeyCode::Up => (r > 0).then(|| if r == 2 { 0 } else { r - 1 }).map(|r| (r, c)),
                KeyCode::Down => (r < last_row).then(|| if r == 0 { 2.min(last_row) } else { r + 1 }).map(|r| (r, c)),
                KeyCode::Left => (c > 0).then(|| (r, c - 1)),
                KeyCode::Right => (c < last_col).then(|| (r, c + 1)),
                _ => None,
            }
        };
        if let Some((_, cs)) = self.cell_sel_live() {
            if let Some(h) = step(t, cs.head) {
                self.set_cell_sel(t, cs.anchor, h);
            }
            return true;
        }
        let rope = self.tab.buf.rope();
        let sel = self.tab.buf.sel;
        let Some((r, c)) = t.cell_at(rope, sel.head) else { return false };
        if r == 1 {
            return false;
        }
        let cell = &t.rows[r].cells[c];
        let leaving = match code {
            KeyCode::Up | KeyCode::Down => true,
            KeyCode::Left => sel.head <= cell.content.start,
            KeyCode::Right => sel.head >= cell.content.end,
            _ => false,
        };
        match step(t, (r, c)) {
            Some(h) if leaving => {
                self.set_cell_sel(t, (r, c), h);
                true
            }
            _ => false,
        }
    }

    /// Select a whole row, column or table of cells (table menu).
    pub fn select_cells(&mut self, what: TableAction) {
        let Some(t) = self.caret_table() else { return };
        let rope = self.tab.buf.rope();
        let Some((r, c)) = t.cell_at(rope, self.tab.buf.sel.head) else { return };
        let last_row = t.rows.len() - 1;
        let last_col = t.cols().saturating_sub(1);
        let r = if r == 1 { 0 } else { r };
        let (a, h) = match what {
            TableAction::SelectRow => ((r, 0), (r, last_col)),
            TableAction::SelectCol => ((0, c), (last_row, c)),
            _ => ((0, 0), (last_row, last_col)),
        };
        self.set_cell_sel(&t, a, h);
        self.info("Cells selected: Ctrl+C copies them as tab-separated text, Delete empties them");
    }

    /// The table under the caret.
    pub fn caret_table(&self) -> Option<Rc<Table>> {
        self.tab.table_at(self.tab.buf.line_of(self.tab.buf.sel.head))
    }

    /// Table keys are live: the caret is in a table and they weren't
    /// switched off there with Esc.
    pub fn table_keys_live(&self) -> Option<Rc<Table>> {
        let t = self.caret_table()?;
        (self.table_keys_off != Some(t.first_line())).then_some(t)
    }

    /// Show a one-line hint when the caret enters a table.
    pub fn note_table_context(&mut self) {
        let now = self.caret_table().map(|t| t.first_line());
        if now != self.table_context {
            if self.table_keys_off.is_some() && self.table_keys_off != now {
                self.table_keys_off = None;
            }
            if now.is_some() && self.table_keys_off.is_none() {
                let menu = self.keymap.label(Command::TableMenu);
                self.info(format!("Table: Tab next cell · Enter new row · {menu} menu · Esc plain keys"));
            }
            self.table_context = now;
        }
    }

    /// Handle a key in table context. Returns false to let normal editing
    /// handle it.
    pub fn table_key(&mut self, k: &KeyEvent) -> bool {
        let Some(t) = self.caret_table() else { return false };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let sel_empty = self.tab.buf.sel.is_empty();
        if self.table_keys_off == Some(t.first_line()) {
            if k.code == KeyCode::Esc && sel_empty && !ctrl && !alt {
                self.table_keys_off = None;
                self.info("Table keys on");
                return true;
            }
            return false;
        }
        if self.cell_sel_live().is_some() {
            match k.code {
                KeyCode::Esc => {
                    self.cell_sel = None;
                    return true;
                }
                KeyCode::Delete | KeyCode::Backspace => return self.clear_cell_sel(),
                KeyCode::Char(_) if !ctrl && !alt => {
                    // Typing goes into the head cell, replacing its text.
                    if let Some((t2, cs)) = self.cell_sel_live() {
                        self.cell_sel = None;
                        self.select_cell_in(&t2, cs.head.0, cs.head.1);
                    }
                }
                _ => {}
            }
        }
        // Enter at the very start of the table, or the very end of it (past
        // the closing pipe or border), breaks the line like in plain text:
        // that's how to push a table down or get below it.
        if k.code == KeyCode::Enter && !ctrl && !alt && !shift && sel_empty && self.at_table_edge(&t) {
            return false;
        }
        // On a bordered table's rule line, editing keys would break the
        // border: they do nothing there (Esc still gives plain keys).
        let on_rule = t.style != Style::Markdown && {
            let rope = self.tab.buf.rope();
            let line = rope.char_to_line(self.tab.buf.sel.head);
            t.row_of_line(line).is_none_or(|r| t.rows[r].kind == RowKind::Delimiter)
        };
        if on_rule && !ctrl && !alt && matches!(k.code, KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete | KeyCode::Enter | KeyCode::Tab) {
            if k.code == KeyCode::Tab || k.code == KeyCode::Enter {
                // Tab/Enter from a rule: go to the next row's first cell.
                let line = self.tab.buf.line_of(self.tab.buf.sel.head);
                if let Some(r) = (0..t.rows.len()).find(|&r| t.rows[r].line > line && t.rows[r].kind != RowKind::Delimiter) {
                    self.select_cell_in(&t, r, 0);
                    self.tab.follow_caret = true;
                }
            } else {
                self.info("Borders are drawn by Plica: put the caret in a cell (Esc for plain keys)");
            }
            return true;
        }
        let done = match k.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right if shift && !ctrl && !alt => self.table_shift_arrow(&t, k.code),
            KeyCode::Esc if sel_empty && !ctrl && !alt => {
                self.table_keys_off = Some(t.first_line());
                self.info("Table keys off: Tab, Enter and Backspace edit plain text here (Esc: on again)");
                true
            }
            KeyCode::Tab if !ctrl && !alt && !shift => self.table_step(&t, true),
            KeyCode::BackTab | KeyCode::Tab if !ctrl && !alt => self.table_step(&t, false),
            KeyCode::Enter if !ctrl && !alt && !shift => self.table_new_row(&t),
            KeyCode::Up if alt && !ctrl && !shift => {
                self.table_action(TableAction::RowUp);
                true
            }
            KeyCode::Down if alt && !ctrl && !shift => {
                self.table_action(TableAction::RowDown);
                true
            }
            KeyCode::Char(c) if !ctrl && !alt => self.table_type(&t, &c.to_string(), self.overwrite),
            KeyCode::Backspace if !shift => self.table_erase(&t, false, ctrl || alt),
            KeyCode::Delete if !shift => self.table_erase(&t, true, ctrl || alt),
            _ => false,
        };
        if done {
            self.tab.follow_caret = true;
        }
        done
    }

    /// Is the caret just outside the table: at the start of its first line,
    /// or at the end of its last line past the closing pipe / border?
    fn at_table_edge(&self, t: &Table) -> bool {
        let buf = &self.tab.buf;
        let head = buf.sel.head;
        if head == buf.line_start(t.first_line()) {
            return true;
        }
        if head != buf.line_end(t.last_line()) {
            return false;
        }
        // The last line is a bottom border, or a row ending with a pipe
        // (without one, its end is also the end of the last cell's text).
        match t.row_of_line(t.last_line()) {
            Some(r) => t.rows[r].trail && t.rows[r].cells.last().is_some_and(|c| c.span.end < head),
            None => true,
        }
    }

    /// Paste into a table: one line goes into the cell; several lines or
    /// tab-separated values fill cells from the caret's cell.
    pub fn table_paste(&mut self, text: &str) -> bool {
        let Some(t) = self.table_keys_live() else { return false };
        if text.contains(['\n', '\r', '\t']) || self.cell_sel_live().is_some() {
            return self.table_paste_grid(&t, text);
        }
        self.table_type(&t, text, false)
    }

    /// The cell holding the selection (not in the delimiter row). A caret
    /// on a row's edge pipe or indentation belongs to the edge cell; a
    /// selection must lie within one cell's span.
    fn sel_cell(&self, t: &Table) -> Option<(usize, usize)> {
        let rope = self.tab.buf.rope();
        let sel = self.tab.buf.sel;
        let (r, c) = t.cell_at(rope, sel.head)?;
        let cell = &t.rows[r].cells[c];
        let inside = sel.is_empty() || (sel.start() >= cell.span.start && sel.end() <= cell.span.end);
        (inside && t.rows[r].kind != RowKind::Delimiter).then_some((r, c))
    }

    /// Where an edit at the caret applies. A caret in the cell's padding
    /// counts as the content's edge, unless Plica put it there itself
    /// while typing (so typed trailing spaces aren't lost).
    fn edit_point(&self, t: &Table, r: usize, c: usize) -> usize {
        let cell = &t.rows[r].cells[c];
        let head = self.tab.buf.sel.head;
        if self.table_typed_at == Some((self.tab.buf.cache_key(), head)) {
            return head.clamp(cell.content.start, cell.span.end);
        }
        head.clamp(cell.content.start, cell.content.end)
    }

    /// Replace chars `a..b` of cell (r, c) with `ins`, re-padding the column.
    fn cell_replace(&mut self, t: &Table, (r, c): (usize, usize), (a, b): (usize, usize), ins: &str, kind: EditKind) -> bool {
        let rope = self.tab.buf.rope();
        let span = t.rows[r].cells[c].span.clone();
        let full: Vec<char> = rope.slice(span.clone()).chars().collect();
        let (ra, rb) = (a - span.start, b - span.start);
        let left: String = full[..ra].iter().collect::<String>() + ins;
        let right: String = full[rb..].iter().collect();
        let left_trimmed = left.trim_start();
        // Whitespace after the edit point is padding unless text follows it.
        // A pipe that would split the cell is escaped (so typing `|` gives
        // `\|`, and a backslash typed before `\|` can't break the row).
        // (Markdown only: bordered tables split by column, so `|` is text.)
        let md = t.style == Style::Markdown;
        let safe = |s: &str| if md { crate::table::sanitize(s) } else { s.to_string() };
        let new = safe(&if right.trim().is_empty() { left_trimmed.to_string() } else { format!("{left_trimmed}{}", right.trim_end()) });
        let caret_off = safe(left_trimmed).chars().count().min(new.chars().count());
        let (ops, start) = t.edit_cell(rope, r, c, &new, self.table_realign);
        let caret = start + caret_off;
        // Like plain typing: a typed space starts a new undo step.
        let word_start = kind == EditKind::Typing && !ins.is_empty() && ins.chars().all(char::is_whitespace);
        self.tab.buf.edit(ops, kind, Some(Selection::caret(caret)), word_start);
        self.table_typed_at = Some((self.tab.buf.cache_key(), caret));
        true
    }

    /// Type `text` into the caret's cell; `overtype` replaces the grapheme
    /// after the caret, within the cell's text only (never padding or a pipe).
    fn table_type(&mut self, t: &Table, text: &str, overtype: bool) -> bool {
        let Some((r, c)) = self.sel_cell(t) else { return false };
        let sel = self.tab.buf.sel;
        let (a, b) = if sel.is_empty() {
            let p = self.edit_point(t, r, c);
            let end = t.rows[r].cells[c].content.end;
            (p, if overtype && p < end { self.tab.buf.next_grapheme(p).min(end) } else { p })
        } else {
            (sel.start(), sel.end())
        };
        self.cell_replace(t, (r, c), (a, b), text, EditKind::Typing)
    }

    fn table_erase(&mut self, t: &Table, forward: bool, word: bool) -> bool {
        let Some((r, c)) = self.sel_cell(t) else { return false };
        let sel = self.tab.buf.sel;
        if !sel.is_empty() {
            return self.cell_replace(t, (r, c), (sel.start(), sel.end()), "", EditKind::Deleting);
        }
        let content = t.rows[r].cells[c].content.clone();
        let p = self.edit_point(t, r, c);
        let buf = &self.tab.buf;
        let (a, b) = if forward {
            let end = if word { buf.word_right(p) } else { buf.next_grapheme(p) };
            (p, end.min(content.end.max(p)))
        } else {
            let start = if word { buf.word_left(p) } else { buf.prev_grapheme(p) };
            (start.max(content.start), p)
        };
        if a >= b {
            // At the cell's edge: never delete a pipe.
            self.info(if forward { "End of cell (Esc for plain keys)" } else { "Start of cell (Esc for plain keys)" });
            return true;
        }
        self.cell_replace(t, (r, c), (a, b), "", EditKind::Deleting)
    }

    /// Select cell (r, c)'s content (or put the caret where it goes).
    fn select_cell(&mut self, r: usize, c: usize) {
        let Some(t) = self.caret_table() else { return };
        self.select_cell_in(&t, r, c);
    }

    fn select_cell_in(&mut self, t: &Table, r: usize, c: usize) {
        let Some(cell) = t.rows.get(r).and_then(|row| row.cells.get(c.min(row.cells.len().saturating_sub(1)))) else { return };
        self.tab.buf.sel = Selection { anchor: cell.content.start, head: cell.content.end };
        self.tab.buf.break_undo_group();
        self.table_typed_at = None;
    }

    /// Tab / Shift+Tab: the next or previous cell in reading order
    /// (skipping the delimiter row). Tab in the last cell adds a row.
    fn table_step(&mut self, t: &Table, forward: bool) -> bool {
        let rope = self.tab.buf.rope();
        let Some((r, c)) = t.cell_at(rope, self.tab.buf.sel.head) else { return false };
        let body = |i: usize| t.rows[i].kind != RowKind::Delimiter;
        let target = if forward {
            if c + 1 < t.rows[r].cells.len() && body(r) {
                Some((r, c + 1))
            } else {
                (r + 1..t.rows.len()).find(|&i| body(i)).map(|i| (i, 0))
            }
        } else if c > 0 && body(r) {
            Some((r, c - 1))
        } else {
            (0..r).rev().find(|&i| body(i)).map(|i| (i, t.rows[i].cells.len() - 1))
        };
        match target {
            Some((r, c)) => self.select_cell(r, c),
            None if forward => {
                let last = t.rows.len() - 1;
                self.insert_row_after(t, last, 0);
            }
            None => {}
        }
        true
    }

    fn table_new_row(&mut self, t: &Table) -> bool {
        let rope = self.tab.buf.rope();
        let Some((r, _)) = t.cell_at(rope, self.tab.buf.sel.head) else { return false };
        // Rows go below the delimiter at the earliest; typing continues in
        // the new row's first cell, like Tab at the end of a row.
        self.insert_row_after(t, r.max(1), 0);
        true
    }

    /// Insert an empty row after row `r` and put the caret in its column `c`.
    fn insert_row_after(&mut self, t: &Table, r: usize, c: usize) {
        let ops = t.insert_row(self.tab.buf.rope(), r + 1, self.tab.doc.line_ending.as_str());
        let caret = self.tab.buf.sel;
        self.tab.buf.edit(ops, EditKind::Other, Some(caret), false);
        if let Some(nt) = self.tab.table_at(t.first_line())
            && let Some(row) = nt.rows.get(r + 1)
            && let Some(cell) = row.cells.get(c).or(row.cells.last())
        {
            self.tab.buf.sel = Selection::caret(cell.content.start);
        }
        self.tab.buf.break_undo_group();
        self.table_typed_at = None;
    }

    /// Run a table-menu action on the table under the caret, as one undo
    /// step, then put the caret in the cell it concerns.
    pub fn table_action(&mut self, a: TableAction) {
        match a {
            TableAction::Help => return self.table_help(),
            TableAction::CopyTsv => {
                self.select_cells(TableAction::SelectAll);
                if let Some((tsv, n)) = self.cell_sel_text() {
                    let dest = self.clipboard.copy(&tsv);
                    self.info(format!("Copied {n} cells to {dest} as tab-separated text: paste into a spreadsheet"));
                }
                return;
            }
            TableAction::New(style) => return self.insert_table(style),
            TableAction::Convert(style) => return self.convert_table(style),
            TableAction::Format => return self.format_table(),
            TableAction::SelectRow | TableAction::SelectCol | TableAction::SelectAll => return self.select_cells(a),
            _ => {}
        }
        let Some(t) = self.caret_table() else {
            self.warn("The caret isn't in a Markdown table");
            return;
        };
        let rope = self.tab.buf.rope();
        let Some((r, c)) = t.cell_at(rope, self.tab.buf.sel.head) else { return };
        let le = self.tab.doc.line_ending.as_str();
        let body_r = r.max(2);
        // (ops, target cell, message when refused)
        let (ops, target, refused): (Option<crate::table::Ops>, (usize, usize), &str) = match a {
            TableAction::Format | TableAction::SelectRow | TableAction::SelectCol | TableAction::SelectAll | TableAction::New(_) | TableAction::Convert(_) | TableAction::Help | TableAction::CopyTsv => unreachable!(),
            TableAction::RowAbove => (Some(t.insert_row(rope, body_r, le)), (body_r, c), ""),
            TableAction::RowBelow => (Some(t.insert_row(rope, r.max(1) + 1, le)), (r.max(1) + 1, c), ""),
            TableAction::DeleteRow => (t.delete_row(rope, r), (r.min(t.rows.len().saturating_sub(2)).max(2), c), "Only body rows can be deleted (not the header or the --- row)"),
            TableAction::RowUp | TableAction::RowDown => match t.move_row(rope, r, a == TableAction::RowUp) {
                Some((ops, to)) => (Some(ops), (to, c), ""),
                None => (None, (r, c), "Can't move this row further"),
            },
            TableAction::ColLeft => (Some(t.insert_col_in(rope, c)), (r, c), ""),
            TableAction::ColRight => (Some(t.insert_col_in(rope, c + 1)), (r, c + 1), ""),
            TableAction::DeleteCol => (t.delete_col_in(rope, c), (r, c.saturating_sub(usize::from(c + 1 >= t.rows[0].cells.len()))), "A table needs at least one column"),
            TableAction::MoveColLeft | TableAction::MoveColRight => match t.move_col(rope, c, a == TableAction::MoveColLeft) {
                Some((ops, to)) => (Some(ops), (r, to), ""),
                None => (None, (r, c), "Can't move this column further"),
            },
            TableAction::Align(al) => (Some(t.set_align(rope, c, al)), (r, c), ""),
        };
        let Some(ops) = ops else {
            self.warn(refused);
            return;
        };
        if ops.is_empty() {
            return;
        }
        let first = t.first_line();
        self.tab.buf.edit(ops, EditKind::Other, None, false);
        self.tab.buf.break_undo_group();
        self.table_typed_at = None;
        if let Some(nt) = self.tab.table_at(first)
            && let Some(row) = nt.rows.get(target.0.min(nt.rows.len() - 1))
            && let Some(cell) = row.cells.get(target.1).or(row.cells.last())
        {
            self.tab.buf.sel = Selection::caret(cell.content.start);
        }
        self.tab.follow_caret = true;
    }

    /// A short guide to tables (from the table menus).
    pub fn table_help(&mut self) {
        let menu = self.keymap.label(Command::TableMenu);
        let lines = vec![
            "Markdown: a header row, then a |---|---| row.".to_string(),
            "Plain text: full borders, +---+ or ┌───┐.".into(),
            String::new(),
            format!("{menu}, right-click, Table button: insert,"),
            "rows and columns, align, convert formats.".into(),
            "Tab / Shift+Tab: next / previous cell; Tab at".into(),
            "the end and Enter add a row.".into(),
            "Typing keeps the columns lined up.".into(),
            "Select cells: drag across them, Shift+arrows at".into(),
            "a cell's edge, or click the --- under a column.".into(),
            "Ctrl+C copies cells for a spreadsheet (or use".into(),
            "Copy for a spreadsheet in this menu); paste".into(),
            "spreadsheet cells into a table to fill it.".into(),
            "Enter at the very start or end of a table".into(),
            "moves it down / opens a line below it.".into(),
            "Esc: plain keys in this table.".into(),
        ];
        self.dialog = Some(crate::dialog::Dialog::new("How tables work", lines, vec![crate::dialog::Button::new("&OK", crate::dialog::Action::Cancel)]));
    }

    /// A click on the `---` row (or a bordered table's header rule) selects
    /// the column below it.
    pub fn click_selects_column(&mut self, pos: usize) -> bool {
        let line = self.tab.buf.line_of(pos);
        let Some(t) = self.tab.table_at(line) else { return false };
        let Some(r) = t.row_of_line(line) else { return false };
        let row = &t.rows[r];
        if row.kind != RowKind::Delimiter || row.cells.is_empty() {
            return false;
        }
        let c = row.cells.iter().position(|cell| pos <= cell.span.end).unwrap_or(row.cells.len() - 1);
        let last_row = t.rows.len() - 1;
        self.set_cell_sel(&t, (0, c), (last_row, c));
        true
    }

    /// Rewrite the table under the caret in another format (one undo step).
    pub fn convert_table(&mut self, to: Style) {
        let Some(t) = self.caret_table() else { return };
        if t.style == to {
            return;
        }
        let rope = self.tab.buf.rope();
        let spot = t.cell_at(rope, self.tab.buf.sel.head).filter(|(r, _)| t.rows[*r].kind != RowKind::Delimiter);
        let ops = t.convert(rope, to);
        let caret = self.tab.buf.sel;
        self.tab.buf.edit(ops, EditKind::Other, Some(Selection::caret(caret.head.min(rope.len_chars()))), false);
        self.tab.buf.break_undo_group();
        if let Some(nt) = self.tab.table_at(t.first_line()) {
            let (r, c) = spot.unwrap_or((0, 0));
            self.select_cell_in(&nt, r, c);
            self.tab.buf.sel = Selection::caret(self.tab.buf.sel.start());
        }
        let note = if t.style != Style::Markdown && to == Style::Markdown && !t.has_header() { " (its first row is now the header)" } else { "" };
        self.info(format!("Converted to a {} table{note} (Ctrl+Z undoes)", to.label()));
        self.tab.follow_caret = true;
    }

    /// Insert a new empty 3×2 table (header + one row) at the caret, on its
    /// own lines, with the caret in the first header cell.
    pub fn insert_table(&mut self, style: Style) {
        let le = self.tab.doc.line_ending.as_str().to_string();
        let grid = vec![vec![String::new(); 3]; 2];
        let aligns = [crate::table::Align::None; 3];
        let border = crate::table_border::Border { header: true, header_double: false, row_rules: false };
        let (text, starts) = match style {
            Style::Markdown => crate::table::render_markdown("", &le, &grid, &aligns),
            _ => crate::table_border::render(style, border, "", &le, &grid, &aligns),
        };
        let buf = &self.tab.buf;
        let line = buf.line_of(buf.sel.head);
        let (ls, le_pos) = (buf.line_start(line), buf.line_end(line));
        let blank = buf.rope().slice(ls..le_pos).chars().all(char::is_whitespace);
        // On a blank line, there; otherwise after the line, with a blank
        // line between (a Markdown table can't follow a paragraph line).
        let (at, del, prefix, suffix) = if blank {
            let next_blank = line + 1 >= buf.len_lines() || buf.line_end(line + 1) == buf.line_start(line + 1);
            (ls, le_pos - ls, String::new(), if next_blank { String::new() } else { le.clone() })
        } else {
            (le_pos, 0, format!("{le}{le}"), String::new())
        };
        let first = starts[0][0] + at + prefix.chars().count();
        self.tab.buf.edit(vec![(at, del, format!("{prefix}{text}{suffix}"))], EditKind::Other, Some(Selection::caret(first)), false);
        self.tab.buf.break_undo_group();
        self.tab.follow_caret = true;
        self.info(format!("New {} table: type a header, Tab to the next cell, Enter for a new row", style.label()));
    }

    /// Format table: line up every row of the table under the caret, as one
    /// undo step, keeping the caret in its cell.
    pub fn format_table(&mut self) {
        let Some(t) = self.caret_table() else {
            self.warn("The caret isn't in a Markdown table");
            return;
        };
        let rope = self.tab.buf.rope();
        let head = self.tab.buf.sel.head;
        let spot = t.cell_at(rope, head).map(|(r, c)| {
            let content = &t.rows[r].cells[c].content;
            (r, c, head.saturating_sub(content.start).min(content.len()))
        });
        let ops = t.format(rope);
        if ops.is_empty() {
            self.info("Table is already formatted");
            return;
        }
        self.tab.buf.edit(ops, EditKind::Other, None, false);
        if let (Some((r, c, off)), Some(nt)) = (spot, self.tab.table_at(t.first_line()))
            && let Some(cell) = nt.rows.get(r).and_then(|row| row.cells.get(c))
        {
            self.tab.buf.sel = Selection::caret(cell.content.start + off);
        }
        self.tab.buf.break_undo_group();
        self.tab.follow_caret = true;
        self.info("Table formatted (Ctrl+Z undoes)");
    }
}

#[cfg(test)]
mod tests {
    use crate::app::tests::{app_with, key, text, type_str};
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    const T: &str = "Notes\n\n| Device | Type |\n| ------ | ---- |\n| MPC    | Samp |\n\nEnd\n";

    fn caret_at(a: &mut crate::app::App, needle: &str) {
        let at = text(a).find(needle).unwrap();
        let pos = a.tab.buf.rope().byte_to_char(at);
        a.tab.buf.sel = crate::buffer::Selection::caret(pos);
    }

    #[test]
    fn typing_grows_the_column_and_one_undo_restores_it() {
        let mut a = app_with(T, 60, 12);
        caret_at(&mut a, "MPC ");
        a.tab.buf.sel.head += 3;
        a.tab.buf.sel.anchor += 3;
        type_str(&mut a, " One Plus");
        assert_eq!(text(&a), "Notes\n\n| Device       | Type |\n| ------------ | ---- |\n| MPC One Plus | Samp |\n\nEnd\n");
        let caret = a.tab.buf.sel.head;
        assert_eq!(a.tab.buf.rope().slice(caret - 4..caret).to_string(), "Plus");
        key(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
        key(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(text(&a), T, "undo steps restore the structure");
    }

    #[test]
    fn overwrite_stays_inside_the_cells_text() {
        let mut a = app_with(T, 60, 12);
        a.overwrite = true;
        caret_at(&mut a, "Samp");
        type_str(&mut a, "Drum");
        assert!(text(&a).contains("| MPC    | Drum |\n"), "{}", text(&a));
        type_str(&mut a, "s");
        assert!(text(&a).contains("| MPC    | Drums |\n"), "past the text it inserts: {}", text(&a));
        key(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(text(&a), T);
    }

    #[test]
    fn backspace_stops_at_the_cell_edge_and_pipes_are_escaped() {
        let mut a = app_with(T, 60, 12);
        caret_at(&mut a, "Samp");
        key(&mut a, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(text(&a), T, "the pipe before the cell survives");
        type_str(&mut a, "a|b ");
        assert!(text(&a).contains("| a\\|b Samp |"), "{}", text(&a));
    }

    #[test]
    fn tab_moves_between_cells_and_adds_a_row_at_the_end() {
        let mut a = app_with(T, 60, 12);
        caret_at(&mut a, "Device");
        key(&mut a, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(a.tab.buf.selected_text(), "Type");
        key(&mut a, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(a.tab.buf.selected_text(), "MPC", "skips the delimiter row");
        key(&mut a, KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(a.tab.buf.selected_text(), "Type");
        caret_at(&mut a, "Samp");
        key(&mut a, KeyCode::Tab, KeyModifiers::NONE);
        type_str(&mut a, "Volca");
        assert!(text(&a).contains("| MPC    | Samp |\n| Volca  |      |\n"), "{}", text(&a));
        // Typing over a selected cell replaces it.
        caret_at(&mut a, "Device");
        key(&mut a, KeyCode::Tab, KeyModifiers::NONE);
        type_str(&mut a, "Kind");
        assert!(text(&a).contains("| Device | Kind |"), "{}", text(&a));
    }

    #[test]
    fn enter_adds_a_row_below_and_esc_gives_plain_keys() {
        let mut a = app_with(T, 60, 12);
        caret_at(&mut a, "Type");
        key(&mut a, KeyCode::Enter, KeyModifiers::NONE);
        type_str(&mut a, "x");
        assert!(text(&a).contains("| ---- |\n| x      |      |\n| MPC"), "first cell of the new row: {}", text(&a));
        key(&mut a, KeyCode::Esc, KeyModifiers::NONE);
        key(&mut a, KeyCode::Tab, KeyModifiers::NONE);
        assert!(text(&a).contains("x\t"), "plain Tab after Esc: {}", text(&a));
    }

    #[test]
    fn format_command_keeps_the_caret_in_its_cell() {
        let mut a = app_with("|a|bb|\n|-|-|\n|ccc|d|\n", 60, 12);
        caret_at(&mut a, "d|");
        a.format_table();
        assert_eq!(text(&a), "|a  |bb |\n|---|---|\n|ccc|d  |\n");
        assert_eq!(a.tab.buf.rope().char(a.tab.buf.sel.head), 'd');
        key(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(text(&a), "|a|bb|\n|-|-|\n|ccc|d|\n");
    }

    #[test]
    fn caret_on_an_edge_pipe_types_into_the_edge_cell() {
        let mut a = app_with(T, 60, 12);
        caret_at(&mut a, "| MPC");
        type_str(&mut a, "Q");
        assert!(text(&a).contains("| QMPC   | Samp |"), "{}", text(&a));
        caret_at(&mut a, "| Samp |");
        a.tab.buf.sel = crate::buffer::Selection::caret(a.tab.buf.sel.head + 8);
        type_str(&mut a, "!");
        assert!(text(&a).contains("| Samp! |"), "after the trailing pipe: {}", text(&a));
    }

    #[test]
    fn menu_actions_rows_and_columns() {
        use crate::app::TableAction as A;
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
        let mut a = app_with(src, 60, 12);
        caret_at(&mut a, "3");
        key(&mut a, KeyCode::Up, KeyModifiers::ALT);
        assert_eq!(text(&a), "| a | b |\n|---|---|\n| 3 | 4 |\n| 1 | 2 |\n");
        assert_eq!(a.tab.buf.rope().char(a.tab.buf.sel.head), '3', "caret moves with the row");
        a.table_action(A::DeleteRow);
        assert_eq!(text(&a), "| a | b |\n|---|---|\n| 1 | 2 |\n");
        a.table_action(A::ColRight);
        type_str(&mut a, "new");
        assert_eq!(text(&a), "| a |     | b |\n|---|-----|---|\n| 1 | new | 2 |\n");
        a.table_action(A::MoveColLeft);
        assert_eq!(text(&a), "|     | a | b |\n|-----|---|---|\n| new | 1 | 2 |\n");
        a.table_action(A::Align(crate::table::Align::Right));
        assert_eq!(text(&a), "|     | a | b |\n|----:|---|---|\n| new | 1 | 2 |\n");
        a.table_action(A::DeleteCol);
        assert_eq!(text(&a), "| a | b |\n|---|---|\n| 1 | 2 |\n");
        // Each action is one undo step.
        for _ in 0..4 {
            key(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
        }
        assert_eq!(text(&a), "| a |   | b |\n|---|---|---|\n| 1 |   | 2 |\n", "delete, align, move and the typed word undone; the inserted column is back");
    }

    #[test]
    fn right_click_in_a_table_opens_the_menu() {
        use ratatui::crossterm::event::{Event, MouseButton, MouseEvent, MouseEventKind};
        let mut a = app_with(T, 60, 12);
        a.rendered_rows = a.visible_rows(12);
        let (x, y) = (a.regions.text.x + 3, a.regions.text.y + 4);
        a.handle_event(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Right), column: x, row: y, modifiers: KeyModifiers::NONE }));
        assert!(a.picker.as_ref().is_some_and(|p| p.title == "Markdown table"));
        assert_eq!(a.tab.buf.line_of(a.tab.buf.sel.head), 4);
    }

    #[test]
    fn cell_selection_copy_cut_and_tsv_paste() {
        let src = "| k | v |\n|---|---|\n| 1 | a |\n| 2 | b |\n";
        let mut a = app_with(src, 60, 12);
        a.clipboard = crate::clipboard::Clipboard::internal_only();
        caret_at(&mut a, "1 | a");
        a.tab.buf.sel = crate::buffer::Selection::caret(a.tab.buf.sel.head + 1); // end of "1"
        key(&mut a, KeyCode::Right, KeyModifiers::SHIFT); // leaves the cell: cells
        key(&mut a, KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(a.cell_sel_live().unwrap().1.rect(), (2, 0, 3, 1));
        key(&mut a, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(a.clipboard.paste(), "1\ta\n2\tb\n");
        key(&mut a, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(text(&a), "| k | v |\n|---|---|\n|   |   |\n|   |   |\n");
        key(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(text(&a), src, "one undo step");
        // Paste a 2x3 block at "a": the table gains a column and a row.
        a.cell_sel = None;
        caret_at(&mut a, "a |");
        a.handle_event(ratatui::crossterm::event::Event::Paste("x\ty|z\nw\tv\n9\t8\n".into()));
        assert_eq!(text(&a), "| k | v |      |\n|---|---|------|\n| 1 | x | y\\|z |\n| 2 | w | v    |\n|   | 9 | 8    |\n", "{}", text(&a));
        assert_eq!(a.cell_sel_live().unwrap().1.rect(), (2, 1, 4, 2), "pasted cells are selected");
        // Copying them back gives the same data (pipes unescaped).
        key(&mut a, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(a.clipboard.paste(), "x\ty|z\nw\tv\n9\t8\n");
        // Any other key drops the cell selection.
        key(&mut a, KeyCode::Right, KeyModifiers::NONE);
        assert!(a.cell_sel_live().is_none());
    }

    #[test]
    fn select_row_column_table_from_the_menu() {
        use crate::app::TableAction as A;
        let mut a = app_with(T, 60, 12);
        a.clipboard = crate::clipboard::Clipboard::internal_only();
        caret_at(&mut a, "Samp");
        a.table_action(A::SelectCol);
        key(&mut a, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(a.clipboard.paste(), "Type\nSamp\n", "the --- row is not data");
        a.table_action(A::SelectAll);
        key(&mut a, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(a.clipboard.paste(), "Device\tType\nMPC\tSamp\n");
        // Typing with cells selected replaces the head cell's text.
        a.table_action(A::SelectRow);
        type_str(&mut a, "Q");
        assert!(text(&a).contains("| MPC    | Q    |"), "{}", text(&a));
    }

    #[test]
    fn wide_tables_scroll_sideways_instead_of_wrapping() {
        let long = "word ".repeat(10);
        let src = format!("{long}\n| alpha | beta | gamma | delta | epsilon |\n|---|---|---|---|---|\n| 1 | 2 | 3 | 4 | 5 |\n");
        let mut a = app_with(&src, 20, 8);
        assert!(a.rows_of(0).len() > 1, "prose still wraps");
        assert_eq!(a.rows_of(1).len(), 1, "table rows don't");
        caret_at(&mut a, "epsilon");
        a.tab.follow_caret = true;
        a.ensure_caret_visible();
        assert!(a.tab.table_scroll_x > 0);
        assert_eq!(a.row_scroll_x(1), a.tab.table_scroll_x);
        assert_eq!(a.row_scroll_x(0), 0, "only the table scrolls");
        // Clicking maps through the table's scroll.
        a.tab.top_line = 0;
        a.rendered_rows = a.visible_rows(8);
        a.rendered_scroll = a.rendered_rows.iter().map(|r| a.row_scroll_x(r.line)).collect();
        let row_y = a.rendered_rows.iter().position(|r| r.line == 1).unwrap() as u16 + a.regions.text.y;
        let pos = a.pos_at_screen_pub(a.regions.text.x, row_y).unwrap();
        assert_eq!(pos - a.tab.buf.line_start(1), a.tab.table_scroll_x);
        // Leaving the table resets it.
        a.tab.buf.sel = crate::buffer::Selection::caret(0);
        a.tab.follow_caret = true;
        a.ensure_caret_visible();
        assert_eq!(a.tab.table_scroll_x, 0);
    }

    #[test]
    fn insert_and_convert_tables() {
        use crate::app::TableAction as A;
        use crate::table::Style;
        let mut a = app_with("Some prose.\n", 60, 12);
        key(&mut a, KeyCode::Char('t'), KeyModifiers::ALT);
        assert_eq!(a.picker.as_ref().map(|p| p.title.as_str()), Some("Insert table"), "Alt+T outside a table offers new tables");
        a.picker = None;
        a.table_action(A::New(Style::Unicode));
        let cells = |a: &mut crate::app::App, vals: &[&str]| {
            for (i, v) in vals.iter().enumerate() {
                if i > 0 {
                    key(a, KeyCode::Tab, KeyModifiers::NONE);
                }
                type_str(a, v);
            }
        };
        cells(&mut a, &["Name", "Qty", "Note"]);
        key(&mut a, KeyCode::Tab, KeyModifiers::NONE);
        cells(&mut a, &["Pads", "16"]);
        let unicode = "Some prose.\n\n┌──────┬─────┬──────┐\n│ Name │ Qty │ Note │\n├──────┼─────┼──────┤\n│ Pads │ 16  │      │\n└──────┴─────┴──────┘\n";
        assert_eq!(text(&a), unicode, "{}", text(&a));
        // Through every format and back: the data survives.
        a.table_action(A::Convert(Style::Ascii));
        assert!(text(&a).contains("| Pads | 16  |      |\n+------+-----+------+"), "{}", text(&a));
        a.table_action(A::Convert(Style::Markdown));
        assert!(text(&a).contains("| Name | Qty | Note |\n| ---- | --- | ---- |\n"), "{}", text(&a));
        a.table_action(A::Convert(Style::Unicode));
        assert_eq!(text(&a), unicode);
        for _ in 0..2 {
            key(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
        }
        assert!(text(&a).contains("+------+"), "each conversion is one undo step: {}", text(&a));
        key(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(text(&a), unicode);
    }

    #[test]
    fn pipes_are_plain_text_in_bordered_tables() {
        let src = "+---+---+\n| a | b |\n+---+---+\n";
        let mut a = app_with(src, 60, 12);
        a.clipboard = crate::clipboard::Clipboard::internal_only();
        caret_at(&mut a, "b |");
        a.tab.buf.sel = crate::buffer::Selection::caret(a.tab.buf.sel.head + 1);
        type_str(&mut a, "|c");
        assert_eq!(text(&a), "+---+-----+\n| a | b|c |\n+---+-----+\n");
        a.table_action(crate::app::TableAction::SelectAll);
        key(&mut a, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(a.clipboard.paste(), "a\tb|c\n");
    }

    #[test]
    fn bordered_rules_are_protected_and_failed_blocks_cached() {
        let src = "+---+---+\n| a | b |\n+===+===+\n| 1 | 2 |\n+---+---+\n";
        let mut a = app_with(src, 60, 12);
        caret_at(&mut a, "+===");
        type_str(&mut a, "Q");
        key(&mut a, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(text(&a), src, "typing on a rule changes nothing");
        key(&mut a, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(a.tab.buf.selected_text(), "1", "Tab from a rule goes to the next row");
        // A long run of pipe lines that is no table is found once.
        let body: String = (0..2000).map(|i| format!("| {i} | x |\n")).collect();
        let b = app_with(&body, 60, 12);
        assert!(b.tab.table_at(5).is_none());
        let rope = b.tab.buf.rope();
        assert_eq!(crate::table::Table::scan(rope, 1000).err(), Some((0, 1999)));
    }

    #[test]
    fn clicking_the_delimiter_selects_a_column_and_help_opens() {
        let mut a = app_with(T, 60, 12);
        let pos = a.tab.buf.rope().byte_to_char(text(&a).find("| ---- |").unwrap() + 3);
        assert!(a.click_selects_column(pos));
        assert_eq!(a.cell_sel_live().unwrap().1.rect(), (0, 1, 2, 1));
        assert!(!a.click_selects_column(a.tab.buf.rope().byte_to_char(text(&a).find("MPC").unwrap())), "other rows: normal click");
        a.table_action(crate::app::TableAction::Help);
        assert_eq!(a.dialog.as_ref().map(|d| d.title.as_str()), Some("How tables work"));
    }

    #[test]
    fn enter_at_the_tables_outer_edges_breaks_the_line() {
        // A table at the very top: Enter at 1,1 pushes it down.
        let src = "| a | b |\n|---|---|\n| 1 | 2 |";
        let mut a = app_with(src, 60, 12);
        a.tab.buf.sel = crate::buffer::Selection::caret(0);
        key(&mut a, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(text(&a), format!("\n{src}"));
        // At the end of the last row, past the pipe: a line below it.
        let end = a.tab.buf.len_chars();
        a.tab.buf.sel = crate::buffer::Selection::caret(end);
        key(&mut a, KeyCode::Enter, KeyModifiers::NONE);
        type_str(&mut a, "after");
        assert_eq!(text(&a), format!("\n{src}\nafter"));
        // Inside, Enter still adds a row.
        caret_at(&mut a, "2 |");
        key(&mut a, KeyCode::Enter, KeyModifiers::NONE);
        assert!(text(&a).contains("| 1 | 2 |\n|   |   |\nafter"), "{}", text(&a));
        // Bordered: the same at the top-left of the top border and after
        // the bottom border.
        let src = "+---+\n| a |\n+---+";
        let mut a = app_with(src, 60, 12);
        a.tab.buf.sel = crate::buffer::Selection::caret(0);
        key(&mut a, KeyCode::Enter, KeyModifiers::NONE);
        let end = a.tab.buf.len_chars();
        a.tab.buf.sel = crate::buffer::Selection::caret(end);
        key(&mut a, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(text(&a), format!("\n{src}\n"));
    }

    #[test]
    fn copy_for_a_spreadsheet_and_the_hint_for_text_copies() {
        let src = "Intro\n┌───┬────┐\n│ A │ B  │\n├───┼────┤\n│ 1 │ 23 │\n└───┴────┘\nEnd\n";
        let mut a = app_with(src, 60, 12);
        a.clipboard = crate::clipboard::Clipboard::internal_only();
        caret_at(&mut a, "23");
        a.table_action(crate::app::TableAction::CopyTsv);
        assert_eq!(a.clipboard.paste(), "A\tB\n1\t23\n");
        // A plain text copy of the whole table copies the text, with a hint.
        a.cell_sel = None;
        a.tab.buf.sel = crate::buffer::Selection { anchor: 0, head: a.tab.buf.len_chars() };
        key(&mut a, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(a.clipboard.paste(), src);
        assert!(a.status.as_ref().unwrap().0.contains("Copy for a spreadsheet"));
    }

    #[test]
    fn typed_spaces_inside_a_cell_are_kept() {
        let mut a = app_with(T, 60, 12);
        caret_at(&mut a, "Samp");
        a.tab.buf.sel = crate::buffer::Selection::caret(a.tab.buf.sel.head + 4);
        type_str(&mut a, " pad x");
        assert!(text(&a).contains("| Samp pad x |"), "{}", text(&a));
    }

    /// The Phase 4 invariants, over random tables and random operations:
    /// text outside the table never changes, the table stays a table,
    /// reflowing operations lose no cell text, and every operation undoes
    /// and redoes exactly.
    #[test]
    fn table_invariants_under_random_operations() {
        use crate::app::TableAction as A;
        use crate::table::{Align, Table};
        let words = ["MPC", "Électribe", "日本語", "🎹", "a\\|b", "", "x y", "e\u{301}", "👩‍💻", "--", "`c`"];
        let mut seed = std::env::var("PLICA_FUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x9e37_79b9_u32);
        let mut rnd = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as usize % n.max(1)
        };
        let rounds = std::env::var("PLICA_FUZZ_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(150);
        for round in 0..rounds {
            let cols = 1 + rnd(4);
            let rows = rnd(4);
            let cell = |rnd: &mut dyn FnMut(usize) -> usize| words[rnd(words.len())].to_string();
            let mut lines = vec![format!("| {} |", (0..cols).map(|_| cell(&mut rnd)).collect::<Vec<_>>().join(" | "))];
            lines.push(format!("|{}|", (0..cols).map(|_| ["---", ":--", "--:", ":-:"][rnd(4)]).collect::<Vec<_>>().join("|")));
            for _ in 0..rows {
                lines.push(format!("| {} |", (0..cols).map(|_| cell(&mut rnd)).collect::<Vec<_>>().join(" | ")));
            }
            let crlf = round % 5 == 0;
            let le = if crlf { "\r\n" } else { "\n" };
            let before = format!("Intro | with a pipe{le}{le}");
            let after = format!("{le}After.{le}{le}| other | table |{le}|---|---|{le}| 1 | 2 |{le}");
            // A third of the rounds use bordered tables.
            let table_text = match round % 3 {
                0 => lines.join(le),
                k => {
                    let grid: Vec<Vec<String>> = (0..=rows).map(|_| (0..cols).map(|_| words[rnd(words.len())].to_string()).collect()).collect();
                    let border = crate::table_border::Border { header: rnd(3) > 0, header_double: rnd(2) == 0, row_rules: rnd(4) == 0 };
                    let aligns: Vec<Align> = (0..cols).map(|_| [Align::None, Align::Right, Align::Center][rnd(3)]).collect();
                    let style = if k == 1 { crate::table::Style::Ascii } else { crate::table::Style::Unicode };
                    crate::table_border::render(style, border, "", le, &grid, &aligns).0
                }
            };
            let doc = format!("{before}{table_text}{after}");
            let mut a = app_with(&doc, 40, 12);
            a.tab.doc.line_ending = if crlf { crate::document::LineEnding::CrLf } else { crate::document::LineEnding::Lf };
            if round % 3 == 0 {
                a.tab.buf.sel = crate::buffer::Selection::caret(before.chars().count());
                a.format_table();
            }
            for step in 0..12 {
                let start_line = 2;
                let t = a.tab.table_at(start_line).unwrap_or_else(|| panic!("table lost (round {round} step {step}):\n{}", text(&a)));
                let rope = a.tab.buf.rope().clone();
                let (es, ee) = t.extent_chars(&rope);
                let prefix: String = rope.slice(..es).into();
                let suffix: String = rope.slice(ee..).into();
                // Cell text as data (Markdown escapes pipes, bordered doesn't).
                let contents = |t: &Table, rope: &ropey::Rope| -> Vec<String> {
                    let md = t.style == crate::table::Style::Markdown;
                    let mut v: Vec<String> = t.grid(rope).into_iter().flatten().filter(|s| !s.is_empty()).map(|s| if md { crate::table::unescape(&s) } else { s }).collect();
                    v.sort();
                    v
                };
                let before_cells = contents(&t, &rope);
                // Caret into a random cell.
                let r = [0, 2, 3, 4][rnd(4)].min(t.rows.len() - 1);
                let r = if r == 1 { 0 } else { r };
                let c = rnd(t.rows[r].cells.len());
                a.cell_sel = None;
                a.table_keys_off = None;
                a.tab.buf.sel = crate::buffer::Selection::caret(t.rows[r].cells[c].content.start);
                let pre = text(&a);
                let old_content: String = rope.slice(t.rows[r].cells[c].content.clone()).into();
                let word = words[rnd(words.len())];
                let op = rnd(15);
                let reflow_only = matches!(op, 0 | 7 | 8 | 9 | 14);
                match op {
                    0 => a.format_table(),
                    1 => type_str(&mut a, word),
                    2 => key(&mut a, KeyCode::Backspace, KeyModifiers::NONE),
                    3 => key(&mut a, KeyCode::Tab, KeyModifiers::NONE),
                    4 => key(&mut a, KeyCode::Enter, KeyModifiers::NONE),
                    5 => a.table_action([A::RowAbove, A::RowBelow, A::DeleteRow][rnd(3)]),
                    6 => a.table_action([A::ColLeft, A::ColRight, A::DeleteCol][rnd(3)]),
                    7 => a.table_action([A::RowUp, A::RowDown][rnd(2)]),
                    8 => a.table_action([A::MoveColLeft, A::MoveColRight][rnd(2)]),
                    9 => a.table_action(A::Align([Align::Left, Align::Center, Align::Right, Align::None][rnd(4)])),
                    10 => a.handle_event(ratatui::crossterm::event::Event::Paste(format!("{word}\t{}\n{}\n", words[rnd(words.len())], words[rnd(words.len())]))),
                    11 => {
                        a.table_action(A::SelectRow);
                        key(&mut a, KeyCode::Delete, KeyModifiers::NONE);
                    }
                    12 => type_str(&mut a, "|"),
                    14 => a.table_action(A::Convert([crate::table::Style::Markdown, crate::table::Style::Ascii, crate::table::Style::Unicode][rnd(3)])),
                    _ => key(&mut a, KeyCode::Delete, KeyModifiers::NONE),
                }
                let post = text(&a);
                let ctx = format!("round {round} step {step} op {op} cell ({r},{c})\nbefore:\n{pre}\nafter:\n{post}");
                assert!(post.starts_with(&prefix), "text before the table changed: {ctx}");
                assert!(post.ends_with(&suffix), "text after the table changed: {ctx}");
                let nt = a.tab.table_at(start_line).unwrap_or_else(|| panic!("no longer a table: {ctx}"));
                if reflow_only {
                    assert_eq!(contents(&nt, a.tab.buf.rope()), before_cells, "cell text lost: {ctx}");
                }
                match op {
                    // Typed at the start of the cell's text.
                    1 => {
                        // Typed char by char, so a backslash typed before
                        // an escaped pipe gets escaped itself: compare the
                        // text without escape backslashes.
                        let got: String = a.tab.buf.rope().slice(nt.rows[r].cells[c].content.clone()).into();
                        assert_eq!(got.replace('\\', ""), format!("{word}{old_content}").replace('\\', ""), "typed text: {ctx}");
                    }
                    4 => assert_eq!(nt.rows.len(), t.rows.len() + 1, "Enter adds one row: {ctx}"),
                    // Pasted data reads back exactly.
                    10 => {
                        let got: String = a.tab.buf.rope().slice(nt.rows[r].cells[c].content.clone()).into();
                        let data = if nt.style == crate::table::Style::Markdown { crate::table::unescape(&got) } else { got };
                        assert_eq!(data, word, "pasted value: {ctx}");
                    }
                    _ => {}
                }
                if post != pre {
                    key(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
                    // Typing may have coalesced with an earlier step: undo
                    // until the text is back, at most a few steps.
                    let mut n = 1;
                    while text(&a) != pre && n < 8 && a.tab.buf.is_dirty() {
                        key(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
                        n += 1;
                    }
                    if text(&a) == pre {
                        for _ in 0..n {
                            key(&mut a, KeyCode::Char('y'), KeyModifiers::CONTROL);
                        }
                        assert_eq!(text(&a), post, "redo differs: {ctx}");
                    } else {
                        // Undo went past `pre` into earlier coalesced
                        // typing: redo everything back.
                        while a.tab.buf.redo() {}
                        assert_eq!(text(&a), post, "undo/redo round trip broken: {ctx}");
                    }
                }
            }
        }
    }
}
