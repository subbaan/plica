# Changelog

## 0.3.5 (2026-10-12): select, copy and paste table cells

- **Cell selection:** drag from one cell into another, or use **Shift+↑/↓**, or **Shift+←/→** at the edge of a cell's text, to select a rectangle of cells. The table menu can also select the whole row, column or table. The status bar shows e.g. `2×3 cells`.
- **Ctrl+C** copies the selected cells as **tab-separated text**, which pastes straight into a spreadsheet; `\|` becomes a plain `|`. Ctrl+X cuts them, and Delete/Backspace empties them, both as one undo step. Typing replaces the active cell's text, and Esc or any other key goes back to ordinary selection. For a plain-text copy of the table's source, select it with table keys off (Esc).
- **Pasting into a table:** tab-separated text (from a spreadsheet, or copied cells) fills cells starting at the caret's cell or the selected cells. Rows and columns are added when the data reaches past the table, so nothing is dropped. Pasted Markdown table rows work the same way (their `---` rows are skipped), and several lines without tabs fill one column downwards. `|` in pasted data is escaped. A table that was lined up stays lined up; in one that wasn't, only the rows you pasted into change. The pasted cells are left selected; one undo removes the paste.

## 0.3.4 (2026-10-12): build fix

- No user-visible change. Fixes a lint error (an overly complex type in the table code) that 0.3.3 was published with: `make check` failed and the commit went out anyway. `make check` and `make e2e` pass again.

## 0.3.3 (2026-10-12): table rows and columns

- The **table menu** (Alt+T, F6, or **right-click in a table**) now has: insert row above / below, delete row, move row up / down, insert column left / right, delete column, move column left / right, and align a column left / centre / right / none. Type to filter, e.g. "col right".
- **Alt+↑ / Alt+↓** move the current row up or down within the table.
- Each action is a single undo step and puts the caret in the cell it affected. Other cells keep their exact text and padding; a new column takes the table's style. Changing a column's alignment re-pads its cells if the column was lined up, and otherwise changes only the `---` row.
- The header and `---` rows can't be deleted or moved, and a table always keeps at least one column. Plica says so instead of breaking the table.
- Fix (inside tables): a typed word and the spaces after it now undo in the same steps as ordinary typing. Previously an empty cell's padding could split every keystroke into its own undo step.

## 0.3.2 (2026-10-12): Markdown tables, first slice

The start of Phase 4. Plica now recognises Markdown pipe tables (a header row followed by a row like `|---|:--:|`) and edits them cell by cell. The file stays ordinary Markdown.

- **Typing in a cell re-pads its column** so the pipes stay lined up; the rest of the document is untouched. A column that wasn't lined up to begin with is left as it is (only the cell's text changes). Undo restores the previous layout. You can turn this off under Preferences → Tables, or with `[tables] realign = false`.
- **Tab / Shift+Tab** move to the next / previous cell and select its text, so typing replaces it. **Tab in the last cell adds a row.** **Enter** adds a row below the current one.
- **Backspace and Delete stop at the cell's edge**, so a pipe is never deleted by accident. Typing `|` in a cell inserts `\|`, which keeps it as cell text.
- **Esc** (with nothing selected) turns table keys off for that table, so Tab, Enter and Backspace edit plain text there. Esc again, or moving to another table, turns them back on. The status bar shows `Table` or `Table (keys off)`.
- **Table menu: Alt+T or F6** (rebindable as `table_menu`). For now it has **Format table**, which lines up every row, keeps alignment colons and pipe style, and pads short rows without dropping any text. It is also available as the `format_table` command (unbound by default).
- Pipes and the delimiter row are drawn in the muted colour and the header row in bold.
- Not tables: anything inside fenced code blocks, lines indented 4+ spaces, and prose that merely contains a `|`.

## 0.3.1 (2026-10-12): text encodings

- **Files in other encodings open and save back unchanged.** Plica detects the encoding when it opens a file and saves in that same encoding, so a Windows-1252, Shift_JIS or UTF-16 file stays one. Supported: UTF-8 (with or without BOM), UTF-16 LE/BE (with or without BOM), and about 35 legacy encodings: Western, Central European, Cyrillic, Greek, Turkish, Hebrew, Arabic, Baltic, Vietnamese, Thai, Japanese, Chinese and Korean.
- Detection: a byte-order mark decides; otherwise valid UTF-8 is UTF-8, UTF-16 without a BOM is recognised, and anything else is guessed with Firefox's detector (chardetng). The status bar shows the encoding, with `?` after a guess.
- **Encoding menu:** click the encoding in the status bar, or press **Alt+E** / **F7** (rebindable as `encoding`). **Reopen with…** reads the file again in another encoding when the text looks wrong (undo brings the old text back). **Save with…** writes it in another encoding: UTF-8 with or without BOM, UTF-16, or any legacy one. Both open a list you can filter by typing ("cyr", "japan", "1252") and choose from with the arrows or a click.
- **No silent damage:**
  - If you type a character the file's encoding can't store (say ✓ in a Windows-1252 file), saving stops before writing anything, selects that character, and offers to save as UTF-8 instead.
  - Bytes that aren't valid in the file's encoding (a stray Latin-1 byte in a UTF-8 file, for example) are shown as `\xE9` and written back exactly as they were.
  - Every non-UTF-8 file is checked when it opens: re-encoding the text must give back the same bytes. If it doesn't, the status bar shows ⚠ and saving over the original asks first (Save As is always allowed).
  - Binary files and UTF-32 are still refused, with a clear message.
- `--encoding NAME` opens files in a given encoding instead of detecting it (e.g. `--encoding latin1`, `shift_jis`, `utf-16le`).
- `[files] fallback_encoding` (also under Preferences → Files & Recovery → Other encodings) uses a fixed encoding instead of guessing for files that aren't UTF-8 or UTF-16.
- Crash-recovery snapshots remember the document's encoding, so restored work saves the way it was opened.

## 0.3.0 (2026-10-12): Phase 3, shared colours and highlighting

Completes Phase 3 (text field cursor 0.2.1, highlight groups 0.2.2, document highlights 0.2.3, Highlights in Preferences 0.2.4, themes / high contrast / overrides 0.2.5, and this release). Manual gate checklist: `docs/phase3-checklist.md`.

- **Fix:** when the palette path is a symlink and a theme switcher re-points it, later edits to the *new* target file are now picked up too (Plica used to keep watching the old target's folder).
- New end-to-end gate tests: every interface region (editor, bars, find bar, accent, selection, highlights, an open dialog) follows a dark → light palette switch; unreadable, truncated, invalid and deleted palette files keep the last good colours and recover; rapid switching settles on the final palette.

## 0.2.5 (2026-10-12): built-in themes, high contrast, colour overrides

- **Built-in themes:** Gruvbox, Nord, Solarized Dark and Solarized Light join Dracula (`appearance.theme`, Preferences → Appearance, or `--theme NAME`). The status bar names the active theme.
- **High contrast** (`appearance.high_contrast`, or Preferences → Appearance): text, selection, search, bars and accents are pushed to 7:1 contrast (WCAG AAA), muted text to 4.5:1. Works with any palette or theme.
- **Colour overrides** in `[appearance.colours]`: set any role (foreground, background, accent, selection, search, bar, current_line, line_numbers, …) to `#rrggbb`, a palette slot (`color4`) or another role. Slots follow the live palette. Overrides are applied as written; if one makes text hard to read, Plica says so at startup and `--check-palette` lists it.
- Preferences → Appearance shows a **preview strip** of text, selection, find match, accent, bars, muted text and a highlight, so you can judge a theme before saving.

## 0.2.4 (2026-10-12): Highlights in Preferences

- **Preferences → Highlights** manages highlight groups. The list shows each group's name in its own colours, with switches for all highlighting and for the sample groups. Space switches a group on/off, Enter edits it, N adds a group, Delete removes one, Alt+↑/↓ (or Ctrl+↑/↓) changes priority. Everything is clickable.
- The group editor sets the name, words and phrases (comma-separated), text and background colour (←/→ steps through palette roles and slots with a colour swatch; Enter accepts `#rrggbb` or any colour name), bold / italic / underline, whole words, match case, file types and on/off, with a live preview of the terms.
- Changing a sample makes it yours (marked "sample, edited") without moving it in the list; deleting your copy brings the built-in sample back. Samples themselves can be switched off but not deleted.
- Changes preview in the document as you make them. Save writes them to `[[highlights.group]]` in `config.toml` (only when highlights changed, so hand-written groups keep their comments otherwise); Cancel undoes them.

## 0.2.3 (2026-10-12): document highlights

- **Alt+H** (or **F8**) highlights the selected text, or the word under the caret, everywhere in the current document; press it again on the same word to remove it. Rebindable as `highlight_word`.
- Document-specific rules are kept in `documents.toml` next to `config.toml`, keyed by the file's full path (symlinks resolve to the same file), never inside your files. They use the same group format as `[[highlights.group]]`, so you can edit them by hand; Plica rewrites only the entry of the document you change and keeps your comments.
- Document groups take priority over config groups, which take priority over the samples.
- Save As carries a document's highlights over to the new name.

## 0.2.2 (2026-10-12): word and phrase highlighting

- **Highlight groups** in `config.toml` (`[[highlights.group]]`): a list of words or phrases with a text colour, optional background, and bold / italic / underline. Whole-word (default on) and case-sensitive (default off) matching per group; `file_types = ["md"]` limits a group to some file types. Earlier groups win where matches overlap.
- Colours can be fixed (`"#rrggbb"`), a palette slot (`"color4"`) or a role (`accent`, `warning`, `search`, …). Slots and roles follow the shared palette live. A colour too faint on the background is lifted so it stays readable.
- Built-in sample groups for **TODO, FIXME, IMPORTANT, IDEA, DONE** (upper case, whole words). Turn them off with `highlights.samples = false`, or replace one by naming your group the same (e.g. `"Todo"`). `highlights.enabled = false` turns everything off.
- Highlighting is display-only: files are never changed. Only the visible lines are matched, so large files stay fast.
- `--check-palette` also lists each highlight group's colours and contrast for that palette.
- `--print-config` documents the new section.

## 0.2.1 (2026-10-12): text fields with a cursor

- The Find and Replace fields and the prompts (Open, Save As, Go to line, palette path) now have a real cursor: ←/→ (Ctrl/Alt for words), Home/End, Delete, Ctrl+Backspace / Ctrl+Delete for whole words, typing and pasting at the cursor, and clicking to place the cursor. Long text scrolls with the cursor instead of only showing the end.
- In the find bar, ←/→ and Home/End now edit the field rather than moving the document caret; ↑/↓ still go to the document.

## 0.2.0 (2026-10-12): Phase 2, preferences and polish

Completes Phase 2 (tabs 0.1.1, dialogs 0.1.2, find/replace 0.1.3, recovery 0.1.4, config 0.1.5, primary selection 0.1.6, and this release). Manual gate checklist: `docs/phase2-checklist.md`.

- **Preferences dialog** (F9, Ctrl+, where the terminal sends it, or the Prefs button): Editor, Interaction, Appearance, Files & Recovery, and a read-only Keybindings list; Tables and Highlights are placeholders for 0.4 / 0.3. Keyboard (↑/↓, ←/→, Space/Enter, Tab between sections) and mouse (click values, ‹ › arrows, buttons).
- Changes preview live, including the colour theme and palette file. **Save** writes `config.toml`, keeping your comments and layout and only adding settings that differ from the defaults; the first save keeps your original as `config.toml.bak`; a config with a syntax error is never overwritten. **Apply** keeps changes for this session; **Cancel** reverts.
- Crash recovery can be switched on or off at runtime; switching it off removes this session's snapshots.
- When colours come from `--palette`, `--builtin-theme` or `PLICA_PALETTE`, Preferences shows (and saves) that choice.
- `make perf`: large-file (35 MB) responsiveness test.

## 0.1.6 (2026-10-12): primary selection

- Selecting text sets the X11/Wayland primary selection (mouse selections on release, keyboard selections once they settle), via OSC 52 (Kitty accepts it) and xclip / xsel / wl-copy when present. Middle-click in the text pastes it at the pointer. Turn off with `interaction.primary_selection = false`.
- Help notes the tmux settings needed for mouse and clipboard when running inside tmux.
- Unit tests can no longer touch the real system clipboard.

## 0.1.5 (2026-10-12): config file and key remapping

- Config file `$XDG_CONFIG_HOME/plica/config.toml` (schema version 1): tab width, soft wrap, line numbers, visible whitespace, indent style (auto / tabs / spaces), mouse, theme (auto / palette / dracula / terminal) and palette path, crash recovery on/off.
- `plica --print-config` prints a commented default config; `plica --check-config` lists problems (exit code 1 if any); `--config PATH` uses another file.
- Every setting is validated on its own: a bad value or unknown key is a warning (shown on the status bar, naming the value you wrote) and that setting keeps its default; a syntax error falls back to defaults. Plica never writes the file.
- `[keys]` rebinds any command, e.g. `save_as = ["ctrl+shift+s", "f12"]`; `[]` unbinds. Rebinding a key that another command used moves it. Cursor movement and basic editing keys stay fixed.
- F1 help is generated from the live keymap, so it always shows your bindings.
- Colours: command line > `PLICA_PALETTE` > config > auto (Kitty palette if found, else Dracula). `NO_COLOR` applies only to the auto theme without a configured palette.

## 0.1.4 (2026-10-12): crash recovery

- Unsaved work is snapshotted automatically to `$XDG_STATE_HOME/plica/recovery/` (private: 0700 directory, 0600 files) after 2 s of idle time, or at least every 30 s while editing continues. The original file is never touched.
- Snapshots disappear when a tab is saved, undone back to its saved state, or deliberately discarded (Don't Save / Quit Without Saving).
- After a crash, power loss or kill, the next start offers Restore / Discard / Later. Restore reopens each document with the recovered text as unsaved changes; Ctrl+Z shows the version on disk. Untitled work comes back as Untitled tabs.
- SIGTERM/SIGHUP/SIGINT now flush snapshots instead of writing loose `.txt` copies, so that work is offered on the next start too.

## 0.1.3 (2026-10-12): find and replace

- New find bar (Ctrl+F) with replace mode (Ctrl+H, or Ctrl+R where the terminal sends Ctrl+H as Backspace). Shows "3 of 17"; Enter/F3 next, Shift+Enter/Shift+F3 previous, wrapping around.
- Options: Match case (Alt+C), Whole word (Alt+W), In selection (Alt+L; switched on automatically when the bar opens with a multi-line selection). All are clickable chips, as are ↑ ↓, × and the Replace / All buttons.
- Replace: Enter in the Replace field replaces the current match and moves on; Alt+A or Alt+Enter replaces all as one undo step.
- All matches in view are highlighted while the bar is open.
- **Behaviour change:** search is case-insensitive unless Match case is on (the previous smart-case rule is gone, matching Mousepad/Gedit). The last search and its options are remembered for F3.

## 0.1.2 (2026-10-12): dialogs and external changes

- Real dialogs (keyboard: underlined letter, ←/→/Tab, Enter, Esc; or click a button) replace the "press again to confirm" warnings:
  - closing an unsaved tab: Save / Don't Save / Cancel (Save on an Untitled tab goes through Save As, then closes);
  - quitting: the same for one unsaved tab, or Save All / Quit Without Saving / Cancel for several (Save All asks names for Untitled tabs one by one);
  - saving over a file changed or deleted on disk: Overwrite / Reload (lose my changes) / Save As… / Cancel;
  - Save As onto an existing file: Replace / Cancel.
- Open files are watched. A tab without unsaved edits reloads automatically when its file changes on disk, as one undoable step that keeps the caret's line and column (Ctrl+Z brings back the old text). A tab with unsaved edits shows Keep Mine / Reload; a background tab shows it when you switch to it. Plica's own saves are not mistaken for external changes. Files are also re-checked when the terminal regains focus.
- Deleting an open file shows a warning once; saving recreates it.
- File paths are stored as absolute paths.

## 0.1.1 (2026-10-12): tabs

- Multiple documents in tabs. `plica a.txt b.txt` opens one tab per file; opening a file that is already open switches to it.
- Tab bar: click to switch, × to close, ● marks unsaved changes (click it to close), middle-click closes, mouse wheel over the bar cycles tabs. When tabs overflow, ‹ › markers appear and the active tab stays visible. The title-bar buttons hide when the tabs need the room.
- Keys: Ctrl+N new tab, Ctrl+O opens into a new tab, Ctrl+W closes the tab (asks once if unsaved; closing the last tab quits), Ctrl+Q quits (asks once if any tab is unsaved), Ctrl+PageUp/PageDown and Ctrl+Tab/Ctrl+Shift+Tab switch, Alt+1..9 jump (Alt+9 = last).
- Dropping files now opens each in its own tab, even when the current tab has unsaved changes. An untouched Untitled tab is reused.
- Recovery copies on SIGTERM/SIGHUP/SIGINT are written for every unsaved tab.

## 0.1.0 (2026-10-12): Phase 1, basic editor

- **Drag and drop:** dropping a file onto the terminal opens it instead of inserting its path. Understands Kitty (plain paths per line), VTE terminals (shell-quoted paths) and `file://` URIs. Only a paste made up entirely of existing absolute paths counts as a drop; Ctrl+V never does, so a path can still be inserted as text. A dropped folder opens the Open prompt inside it; dropping into the Open/Save As prompt fills in the path. With unsaved changes the drop is refused (tabs in 0.2 will open it alongside).
- Tab / Shift+Tab indent and outdent selected lines as one undo step; the indent style (tabs or spaces) is detected per file, and Tab pads to the next tab stop in space-indented files.
- Alt+W shows whitespace (tabs `→`, spaces `·`).
- Ctrl+N new file; Ctrl+↑/↓ scroll without moving the caret.
- Click or drag in the line-number gutter selects whole lines.
- The title bar shortens long file names in the middle instead of letting the buttons cover them.
- Docs: `ARCHITECTURE.md` with a dated decision and workaround log.
- Tests: app-level unit tests (viewport, wrapped vertical motion, indent, drop, gutter); e2e adds drop, resize, failed-save and whitespace scenarios.

## 0.0.2 (2026-10-12)

- Help screen leads with conventional keys (Shift+click, Ctrl+arrows); Alt variants are listed as fallbacks for terminals that reserve them.
- Docs: Phase 0 report updated for the user's Kitty key re-plan (Kitty moved to Super).

## 0.0.1 (2026-10-12): Phase 0 spike

First runnable build. See `docs/phase0-report.md` for gate status and known limitations.

- Rope-based buffer with grapheme-cluster navigation, word motion, undo/redo with typing coalescing.
- Soft word wrap (Alt+Z) with wrap-aware caret movement; horizontal scrolling when wrap is off.
- Mouse: click to place caret, drag select with autoscroll, double/triple-click word/line, Alt+click (or Shift+click where the terminal allows it) to extend, wheel and Shift+wheel, clickable title-bar buttons and scrollbar. Alt+M hands the mouse to the terminal.
- Find (smart case, incremental, F3/Shift+F3), go to line (Ctrl+G), `plica +LINE FILE`.
- Clipboard: internal, OSC 52 and xclip/xsel/wl-copy; bracketed paste inserts literal text as one undo step.
- Safe files: UTF-8 only (other encodings refused), BOM and LF/CRLF preserved byte-for-byte, atomic save that keeps mode/owner, symlinks and hard links; external-modification guard; recovery copy written to `$XDG_STATE_HOME/plica/recovery` if killed with unsaved changes.
- Themes: Kitty-format palette file with live reload (directory watch, debounce, last-known-good palette); semantic roles with WCAG contrast repair; built-in Dracula; NO_COLOR; 256-colour fallback. `--check-palette` diagnostic.
- `-h/--help`, `-V/--version`.
