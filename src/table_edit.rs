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
use crate::table::{RowKind, Table};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::rc::Rc;

impl App {
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
        let done = match k.code {
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
            KeyCode::Char(c) if !ctrl && !alt => self.table_type(&t, &c.to_string()),
            KeyCode::Backspace if !shift => self.table_erase(&t, false, ctrl || alt),
            KeyCode::Delete if !shift => self.table_erase(&t, true, ctrl || alt),
            _ => false,
        };
        if done {
            self.tab.follow_caret = true;
        }
        done
    }

    /// Paste one line of text into the cell under the caret.
    pub fn table_paste(&mut self, text: &str) -> bool {
        if text.contains(['\n', '\r']) {
            return false;
        }
        match self.table_keys_live() {
            Some(t) => self.table_type(&t, text),
            None => false,
        }
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
        let lead = left.chars().count() - left_trimmed.chars().count();
        // Whitespace after the edit point is padding unless text follows it.
        let new = if right.trim().is_empty() { left_trimmed.to_string() } else { format!("{left_trimmed}{}", right.trim_end()) };
        let caret_off = (ra + ins.chars().count()).saturating_sub(lead).min(new.chars().count());
        let (ops, start) = t.edit_cell(rope, r, c, &new, self.table_realign);
        let caret = start + caret_off;
        // Like plain typing: a typed space starts a new undo step.
        let word_start = kind == EditKind::Typing && !ins.is_empty() && ins.chars().all(char::is_whitespace);
        self.tab.buf.edit(ops, kind, Some(Selection::caret(caret)), word_start);
        self.table_typed_at = Some((self.tab.buf.cache_key(), caret));
        true
    }

    fn table_type(&mut self, t: &Table, text: &str) -> bool {
        let Some((r, c)) = self.sel_cell(t) else { return false };
        // A literal pipe would split the cell: type it escaped.
        let text = text.replace('|', "\\|");
        let sel = self.tab.buf.sel;
        let (a, b) = if sel.is_empty() {
            let p = self.edit_point(t, r, c);
            (p, p)
        } else {
            (sel.start(), sel.end())
        };
        self.cell_replace(t, (r, c), (a, b), &text, EditKind::Typing)
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
        let Some((r, c)) = t.cell_at(rope, self.tab.buf.sel.head) else { return false };
        // Rows go below the delimiter at the earliest.
        self.insert_row_after(t, r.max(1), c);
        true
    }

    /// Insert an empty row after row `r` and put the caret in its column `c`.
    fn insert_row_after(&mut self, t: &Table, r: usize, c: usize) {
        let row = t.empty_row(self.tab.buf.rope());
        let at = t.rows[r].end;
        let text = format!("{}{row}", self.tab.doc.line_ending.as_str());
        self.tab.buf.edit(vec![(at, 0, text)], EditKind::Other, Some(Selection::caret(at)), false);
        let line = t.rows[r].line + 1;
        if let Some(nt) = self.tab.table_at(line)
            && let Some(cell) = nt.rows[r + 1].cells.get(c).or(nt.rows[r + 1].cells.last())
        {
            self.tab.buf.sel = Selection::caret(cell.content.start);
        }
        self.tab.buf.break_undo_group();
        self.table_typed_at = None;
    }

    /// Run a table-menu action on the table under the caret, as one undo
    /// step, then put the caret in the cell it concerns.
    pub fn table_action(&mut self, a: TableAction) {
        if a == TableAction::Format {
            return self.format_table();
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
            TableAction::Format => unreachable!(),
            TableAction::RowAbove => (Some(t.insert_row(rope, body_r, le)), (body_r, c), ""),
            TableAction::RowBelow => (Some(t.insert_row(rope, r.max(1) + 1, le)), (r.max(1) + 1, c), ""),
            TableAction::DeleteRow => (t.delete_row(rope, r), (r.min(t.rows.len().saturating_sub(2)).max(2), c), "Only body rows can be deleted (not the header or the --- row)"),
            TableAction::RowUp | TableAction::RowDown => match t.move_row(rope, r, a == TableAction::RowUp) {
                Some((ops, to)) => (Some(ops), (to, c), ""),
                None => (None, (r, c), "Can't move this row further"),
            },
            TableAction::ColLeft => (Some(t.insert_col(c)), (r, c), ""),
            TableAction::ColRight => (Some(t.insert_col(c + 1)), (r, c + 1), ""),
            TableAction::DeleteCol => (t.delete_col(c), (r, c.saturating_sub(usize::from(c + 1 >= t.rows[0].cells.len()))), "A table needs at least one column"),
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
        assert!(text(&a).contains("| ---- |\n|        | x    |\n| MPC"), "{}", text(&a));
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
        assert!(a.picker.as_ref().is_some_and(|p| p.title == "Table"));
        assert_eq!(a.tab.buf.line_of(a.tab.buf.sel.head), 4);
    }

    #[test]
    fn typed_spaces_inside_a_cell_are_kept() {
        let mut a = app_with(T, 60, 12);
        caret_at(&mut a, "Samp");
        a.tab.buf.sel = crate::buffer::Selection::caret(a.tab.buf.sel.head + 4);
        type_str(&mut a, " pad x");
        assert!(text(&a).contains("| Samp pad x |"), "{}", text(&a));
    }
}
