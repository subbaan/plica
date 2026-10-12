# Phase 4 gate: manual checklist

**Gate (plan §9):** all table invariants and round-trip preservation tests pass; Markdown tables remain readable in ordinary text editors.

**Automated coverage:**

- `cargo test table` has parser, format, edit, row/column, grid, paste and escaping tests. It also runs a randomised invariant test (`table_invariants_under_random_operations`) of typing, Backspace, Tab, Enter, row/column insert/delete/move, alignment, grid paste, clearing cells, typed pipes and Format, on random tables with Unicode, escaped pipes, empty cells and CRLF line endings. After every operation it checks:
  - the text before and after the table is byte-identical, and a second table below is untouched;
  - the table is still a table;
  - reflowing operations lose no cell text;
  - typed and pasted text arrive intact;
  - undo and redo are exact.

  Run it harder with `PLICA_FUZZ_ROUNDS=3000 PLICA_FUZZ_SEED=123 cargo test --release table_invariants`.
- `make e2e` session 15 drives a real pty: column realign on typing, undo, Tab and Enter adding rows, Esc for plain keys, the table menu (keyboard and right-click), Alt+↓, drag-selecting cells, copying them as tab-separated text, a tab-separated paste that extends the table, save and reopen, and a wide table scrolling sideways.

What remains needs your eyes and your other applications.

Run in **Kitty** (and spot-check **XFCE Terminal**) on copies of real Markdown files:

1. **Realign (plan scenario 7):** in a lined-up table, type a much longer value into a cell. Only that column widens, neighbouring cells keep their text, and Undo steps back through consistent layouts.
2. **Spreadsheet round trip (scenario 8):** copy a block of cells from LibreOffice Calc (or any spreadsheet), click a cell in a table and paste. Rows and columns are added as needed. Then drag across some cells (or Alt+T → Select table), press Ctrl+C and paste into the spreadsheet: same values. Save, reopen, compare.
3. **Narrow window (scenario 9):** make the window narrower than a table. Rows don't wrap (› at the edge), Tab to a far column scrolls that table sideways (‹), the prose around it doesn't move, and the cell contents are unchanged after saving.
4. **Unicode (scenario 12):** a table with emoji, CJK text and accented letters (including decomposed ones). Format it with Alt+T → Format table: the pipes line up on screen, and clicks land in the cell under the pointer.
5. **Readable elsewhere:** open the saved file in another editor (Mousepad, `cat`, GitHub preview): ordinary aligned Markdown, with no Plica markers.
6. **Escape hatch:** in a table, press Esc and check that Tab, Enter and Backspace now edit plain text; Esc again turns table keys back on. Right-click shows the table menu.
7. **Not tables:** code blocks containing pipe tables, and prose with a `|`, behave as ordinary text (no `Table` in the status bar).

Record anything odd in `ARCHITECTURE.md` (log).
