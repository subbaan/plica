# Phase 5 gate: manual checklist

**Gate (plan §9):** plain-text and Markdown tables share editing behaviour and are portable. No false-positive conversion of arbitrary aligned prose.

**Automated coverage:**

- **Shared behaviour:** the randomised invariant test (`table_invariants_under_random_operations`) runs the same operations on Markdown, ASCII and Unicode box tables, plus conversions between them. Those operations are typing, Backspace, Tab, Enter, row/column actions, alignment, grid paste, clearing cells and Format. Each time it checks that the text outside the table is unchanged, the table survives, no cell text is lost, typed and pasted text arrive intact, and undo/redo are exact. Six seeds × 2500 rounds passed before release.
- **No false positives:** `no_false_positives_on_aligned_prose_and_code` checks that none of these is read as a table:
  - column-aligned prose, `ls -l` and psql output, a `git log --graph`;
  - shell pipelines, side-by-side ASCII-art boxes, YAML;
  - pipe lines without a delimiter row, and unfinished borders;
  - Markdown tables inside code fences.
- **Insert and convert:** `make e2e` session 16 inserts an ASCII table with Alt+T, fills it, converts it to a box table and checks the saved file. Unit tests convert Markdown → box → ASCII → Markdown and back with the data intact.

What remains needs your eyes:

1. **Insert:** in a text file, press Alt+T (F6, or the Table button) outside any table and insert each kind. Type a header, Tab across, Enter for rows.
2. **Same keys everywhere:** in a Markdown, an ASCII and a box table, check that typing, Tab/Shift+Tab, Enter, Alt+↑/↓, Backspace at a cell edge, drag-selecting cells, Ctrl+C into a spreadsheet and pasting spreadsheet cells back all behave the same. Bordered tables keep their borders lined up as you type.
3. **Convert:** Alt+T → Convert to … through all three formats; Ctrl+Z steps back one conversion at a time.
4. **Portable:** open the saved ASCII and box tables in another editor, `cat`, or a GitHub code block. They are ordinary text and line up in a monospace font. (Box drawing needs a font with those glyphs; ASCII works everywhere.)
5. **Not tables:** open a file with text laid out in columns using spaces, command output, and ASCII-art diagrams. No `Table` appears in the status bar there, and Tab and Enter behave normally.
6. **Borders:** put the caret on a border line of a bordered table and type: nothing changes (Esc gives plain keys if you really want to edit it).
7. **Help:** Alt+T → How tables work reads sensibly at your usual window size.

Record anything odd in `ARCHITECTURE.md` (log).
