# Plica architecture

How Plica is put together, and a dated log of design decisions and **workarounds**, each with the problem it solves. Scope and phases: `PLICA_IMPLEMENTATION_PLAN_v0.1.0.md`. Stack choice: `docs/adr/`. User-facing changes: `CHANGELOG.md`.

**Keep this file current:** when a change makes a design decision, works around a terminal/library/OS quirk, or fixes a bug whose cause isn't obvious from the code, add a log entry (newest first) and update the overview if the structure changed.

## Overview

```
main.rs ── CLI (files → tabs), palette selection, event loop, debounces, recovery offer/flush
  │
  ├─ term.rs ─────── raw mode / alt screen / mouse / paste / keyboard flags; restore() on exit, panic, signal
  ├─ watch.rs ────── notify watchers: palette dir, open documents' dirs → mpsc → main (debounced)
  │
  └─ app.rs ──────── App state: tabs + Theme + shared UI state; key/mouse/paste commands
        ├─ tab.rs ───── Tab = Buffer + Document + indent style + viewport
        ├─ buffer.rs ── ropey Rope, grapheme navigation, Selection, transactions, undo/redo
        ├─ layout.rs ── per-line VisualRow/Glyph layout: wrap, tabs, widths, hit-testing
        ├─ document.rs ─ load/decode, atomic save, line-ending detection, disk stamps
        ├─ encoding.rs ─ encoding detection, lossless decode/encode, raw-byte placeholders
        ├─ clipboard.rs ─ internal + OSC 52 + wl-copy/xclip/xsel
        ├─ drop.rs ──── recognise file drops delivered as pastes
        ├─ dialog.rs ── modal dialogs (buttons, mnemonics, focus) → Action
        ├─ find.rs ──── search engine, match cache, find/replace bar commands
        ├─ dochl.rs ─── per-document highlight rules (documents.toml)
        ├─ highlight.rs ─ word/phrase groups, colour specs, per-line matching
        ├─ lineedit.rs ─ single-line text field (cursor, word motion, scroll, click)
        ├─ picker.rs ── filterable list popup (encoding and table menus)
        ├─ table.rs ─── Markdown table recognition, TableModel with source spans, format/edit ops
        ├─ table_edit.rs ─ App: typing in cells, Tab/Enter navigation, Format table
        ├─ recovery.rs ─ snapshot writer thread, orphan scan, restore data
        ├─ keymap.rs ── Command table, chord parsing/normalisation, Keymap
        ├─ config.rs ── config.toml loading/validation, defaults text
        ├─ prefs.rs ─── Preferences model/dialog, live apply, comment-preserving save
        ├─ prefs_hl.rs ─ Preferences → Highlights page (group list and editor)
        ├─ colours.rs ─ colour-source decision and loading (startup + Preferences)
        └─ theme.rs ─── parse_kitty → Palette → resolve → Theme (semantic roles, contrast repair)
ui.rs ── draws App into ratatui's cell buffer; Painter maps roles → truecolour / 256 / plain
```

### Core invariants

- **Text is the canonical model.** Highlights and tables are views computed from the rope; on-disk bytes are the rope encoded in the document's encoding (plus a remembered BOM); `encoding.rs` guarantees that round trip or marks the document inexact.
- **Three offset spaces, never mixed:** char index into the rope (all internal positions, snapped to grapheme boundaries) · bytes (only in `document.rs` and search) · display cells (only in `layout.rs`/`ui.rs`).
- **One width function:** layout uses ratatui's `CellWidth`, the same function the renderer uses.
- **Hit-testing uses what was drawn:** `App::rendered_rows` holds the rows from the last frame.
- **Nothing walks the whole document per keystroke** except find when the query or document changes (O(n), cached otherwise).
- **Every UI region asks the Theme for a semantic role**, so a palette swap recolours everything.

### Event loop

`main.rs` polls crossterm (250 ms idle, 50 ms while a status message or palette reload is pending), drains all queued events before one redraw, checks the signal flag, and applies a palette reload 150 ms after the last watcher event. Redraws go through ratatui's diff, so only changed cells are sent to the terminal.

### Viewport

The top of the view is `(top_line, top_row)`: logical line plus visual row within it. That makes wrapped scrolling independent of document size. `ensure_caret_visible` counts rows from the top only as far as the viewport height.

## Decision and workaround log

### 2026-10-12 · 0.3.3

- **Structural table edits touch as little as possible.** A row insert, delete or move rewrites only those lines. A column insert or delete edits one span per row and leaves every other cell's exact text and padding alone. A column move swaps whole spans, so lined-up columns stay lined up. Only Format rewrites rows wholesale. Header and delimiter rows are never deleted or moved; a single-column table refuses to lose its column.
- **Appending a column to a row without a trailing pipe adds a pipe on both sides.** In GFM a lone `|` at the end of a row is only a trailing pipe, not a new cell.
- **Undo grouping for table typing is decided by the caller.** *Bug found by test:* `ends_word_boundary` looked at the last primitive edit, which for a table keystroke can be another row's padding (all spaces), so every keystroke became its own undo step. `Buffer::edit` now takes `word_start` from the caller: a typed space starts a new step, as in plain typing.
- **Right-click in a table opens the table menu.** The menu is the shared `Picker` (filterable), so all actions are reachable without memorising keys. Alt+↑/↓ (move row) are fixed keys inside tables only, because Alt+arrows elsewhere still move the caret.

### 2026-10-12 · 0.3.2

- **Tables are recognised locally and cached per buffer version.** `Table::at(rope, line)` expands from the line over contiguous "pipe lines" (non-blank, indent < 4, an unescaped `|`, not a fence line). The first header + delimiter pair with matching cell counts starts the table; pipe lines above it are prose. The cost is O(table) plus a fence scan of the text above. `Tab::table_at` caches found tables, and lines known not to be in one, until the text changes. Lines over 20 000 chars are never rows, so huge single-line files aren't rescanned on every keystroke.
- **Fence detection scans bytes, not lines** (`table::in_fence`): a state machine over the rope's chunks up to the table's first line. It is cheap for any realistic file, and cached together with the table.
- **Pipes follow GitHub:** `\|` is cell text, but a pipe inside a code span still splits the cell. Typing `|` in a cell inserts `\|` so the model and the file agree.
- **Edits are span replacements in the old text, then re-recognition.** `Table::edit_cell` returns `(at, len, text)` ops plus where the new content starts; `Buffer::edit` applies them as one transaction with an undo kind. Typing coalesces with neighbouring keystrokes as usual, so one keystroke and its column re-padding always undo together. The table is never edited in place.
- **Realigning is minimal and style-preserving.** Only the edited column changes, and only if every row of it had the same display width before (otherwise only the cell's text is replaced, so sloppy tables aren't silently reformatted). The column grows as needed, and shrinks back only if it had no slack before (hand-added padding is kept). The style comes from the table itself: leading/trailing pipes, spaces inside pipes, and spaced or unspaced delimiters, so the common `| a |` / `|---|` style formats to itself. *Bug found by test:* an earlier version treated delimiter cells like content cells and re-padded `|---|` to `| --- |`, reformatting the whole column on a one-letter edit.
- **Typed trailing spaces survive re-padding.** After re-parsing, a space typed at the end of a cell's text looks exactly like padding. `App::table_typed_at` remembers where the last table edit left the caret (and in which buffer version): a caret there is trusted, while a caret clicked into padding is pulled back to the end of the text.
- **Edge carets belong to the edge cell.** *Bug found by the stress test:* with the caret at line start (before the leading pipe), typed text went outside the table (`Q| item`). An empty selection now always maps to `cell_at`'s cell, and edits clamp into its content.
- **Measured:** typing in the last row of a 50 000-row (2.5 MB) table takes about 80–110 ms per keystroke (re-recognition plus the column check over all rows). Fine for now; the cached model could later be updated incrementally if it matters.

### 2026-10-12 · 0.3.1

- **Encodings: encoding_rs + chardetng.** Both are Mozilla's (Firefox's decoder and detector), pure Rust, MIT/Apache. encoding_rs covers the WHATWG set (UTF-8, UTF-16, ~35 legacy encodings); chardetng guesses legacy encodings well from short text. iconv-style breadth (EBCDIC, CP437, UTF-32) was left out: rare for text files, and each would need its own encoder.
- **Detection order** (`encoding::detect`): BOM → BOM-less UTF-16 (zero-byte pattern in the first 8 KiB) → valid UTF-8 → NUL bytes = binary, refused → the document's previous encoding (reload) → mostly-valid UTF-8 → the configured fallback or chardetng's guess. *UTF-16 before UTF-8:* ASCII text in UTF-16 is valid UTF-8 (full of NULs), so the UTF-8 check alone takes it.
- **Valid UTF-8 is never copied:** `detect` takes the `Vec<u8>` and turns it into the `String` in place (the BOM is drained first). Only other encodings go through the decoder and the verification below. The 35 MB perf file is unchanged; a 37 MB Windows-1252 file opens in about 0.8 s.
- **Every non-UTF-8 decode is verified by re-encoding.** The text is encoded again and compared with the file's bytes. Some decodes don't round-trip: Shift_JIS has duplicate mappings, and GBK's decoder reads GB18030's four-byte forms but its encoder can't write them, so GBK retries as GB18030. A mismatch marks the document `inexact`: it shows ⚠ and saving over the original asks first (Save As doesn't), rather than refusing the file.
- **Undecodable bytes become placeholder chars** U+10FC00 + byte (Supplementary Private Use Area B), drawn as `\xNN` (layout `Substitute`, 4 cells) and written back as the raw byte. This keeps a UTF-8 file with one stray Latin-1 byte editable and byte-exact. Placeholders count as bytes only when the document was decoded with some (`Encoding::raw`). If a file genuinely contains those code points, the round-trip check fails, so it is caught at load. "Mostly UTF-8" means at least as many valid multi-byte chars as invalid bytes. Latin-1 text almost never forms valid UTF-8 sequences, so it is still guessed as legacy.
- **Saving never substitutes.** encoding_rs's ordinary `encode` writes HTML numeric references (`&#10003;`) for unmappable chars. Plica uses the `_without_replacement` encoder, which fails with `encoding::Unmappable` (char + byte offset) before anything is written. The app selects that char and offers "Save as UTF-8"; the save chain (close tab / quit) continues after converting. UTF-16 is encoded by hand, because encoding_rs only decodes it.
- **Reload keeps a chosen legacy encoding for pure-ASCII files** (`Options::prefer`). Otherwise a Windows-1250 document that happens to be ASCII would reload as UTF-8, and newly typed č would silently change the file's encoding. A file that becomes real UTF-8 still reloads as UTF-8.
- **The picker is generic** (`Picker<T>`, filter by every word in label + detail). It is built for the encoding menus and ready for later lists.

### 2026-10-12 · 0.3.0

- **The palette watcher is rebuilt when the palette path's resolved target changes.** *Bug found by the gate test:* `PaletteWatcher` records the symlink's target at creation and watches that folder. After a theme switcher re-pointed the link to a file in another folder, in-place edits to the new target were never seen. `reload_palette` now compares `canonicalize(path)` with `App::palette_real` (set when the watcher is made) and flags `palette_watch_dirty`, so the main loop rebuilds the watcher for the new folder.
- **Gate evidence is automated** (e2e session 13): region colours are read back from the rendered cells, not from internal state, so "every region updates" is checked the way a user sees it.

### 2026-10-12 · 0.2.5

- **`theme::Adjust` (high contrast + role overrides) is applied wherever a theme is resolved**: startup (`colours::load`), Preferences, and every live palette reload (`App::adjust`). Overrides resolve against the palette's own theme before overriding, so `"color4"` or `"accent"` follow palette changes and can't loop.
- **Overrides are applied as written, not contrast-repaired.** Automatic repair would silently change a colour the user picked; instead `contrast_warnings` reports pairs below target (status bar at startup, `--check-palette`). Built-in roles are still always repaired.
- **`readable_pair`: when no text colour can reach the target on a background, nudge the background too.** *Found by the new built-in theme test:* at 7:1, black on Solarized yellow (`#b58900`) only reaches 6.5:1, so search matches failed high contrast. Only kicks in when the text colour alone can't meet the target, so normal palettes (including the user's ThemeWall file) resolve exactly as before.
- **Built-in palettes come from each project's published values** (all MIT): Gruvbox dark medium, Nord, Solarized with the canonical terminal slot layout. Solarized Light provides a light palette for the contrast tests.
- **`ThemeChoice::Builtin(Builtin)`** replaces the single Dracula variant; `ThemeChoice::all()` is the one list for config validation, the Preferences cycle and `--theme`.

### 2026-10-12 · 0.2.4

- **A user group named like a sample takes the sample's slot in priority** (`Highlights::ordered`, with `Origin::User / Sample / Override`). *Bug found by e2e:* the first version listed copies with the user's groups, so switching a sample off made its row jump up the list under the mouse. Keeping the slot keeps the list stable, and "replace a sample" now means exactly that. Only plain user groups can be reordered.
- **Preferences edit `App::highlights` directly for live preview**; `Settings` carries a `Highlights` clone so the existing snapshot/Cancel path restores it, and `write_settings` compares against what the file currently parses to, rewriting `[[highlights.group]]` as a whole only when highlights changed. A new `highlights` table is created implicit so no empty `[highlights]` header appears above the groups.
- **New groups open the words prompt first**, and leaving the editor removes a group that still has no words, so an empty group never lingers or gets saved.
- **Text fields reuse the bottom prompt** (`PromptKind::Hl*`) rather than inline editing inside the dialog: one text-entry path (with the 0.2.1 cursor) for everything.

### 2026-10-12 · 0.2.3

- **Document-specific highlights are keyed by canonical path** (resolves the plan's open question; content fingerprints break on every edit, and per-document sidecar files would litter the user's folders). Stored in `documents.toml` beside the config file (derived from the config path, so `--config` and the e2e harness's temp `XDG_CONFIG_HOME` are respected). *Known limitation:* moving or renaming a file outside Plica loses its rules; Save As copies them.
- **The file on disk is the source of truth for document rules.** A toggle re-reads `documents.toml`, edits only that document's `[[document]]` entry with `toml_edit` (other entries and comments are kept), writes it atomically via `Document::save`, then reloads the whole map. Two Plica instances editing different documents therefore don't overwrite each other's rules.
- **Highlight word defaults to Alt+H with F8 as fallback**: there is no GUI-editor convention for this, Alt+H is free in Kitty, but XFCE Terminal's menu bar (when shown) uses Alt+H for its Help menu.

### 2026-10-12 · 0.2.2

- **Highlights are matched per frame over the visible lines only**, never cached or run over the document. Each visible logical line is matched whole (so a phrase that soft-wraps still matches), except lines over 20 000 chars, where only the on-screen part ±512 chars is searched. Cost is (terms × visible text) per draw, which is small; it also means no invalidation logic for edits.
- **Overlaps are resolved by painting a per-char mark array** for each visible line, lowest priority first, so the earlier group wins wherever it covers; lookup per glyph is O(1). Draw precedence: base < highlight < control-char substitute < find match < selection (the caret is the hardware cursor, always on top).
- **Colours are resolved at draw time from the current `Theme`**, never stored as RGB, so palette-linked groups follow live palette changes with no extra plumbing. `Theme` now carries all 16 ANSI slots: a missing slot falls back to its normal/bright twin, then to Dracula's value (the user's ThemeWall file only defines color0, color1, color8, color15 in the sample; the live file defines all 16).
- **Contrast repair for highlights:** with a background, text is pushed to 4.5:1 against it; with only a text colour, to 3:1 against the editor background (word highlights are emphasis, not body text). In terminal-colour mode highlights become attributes (bold if the group has none).
- **Matching reuses `find::find_all_bytes`** (same whole-word and smart-lowercase rules as Find), so highlight and search agree on what a "word" is.
- **The config template's raw string became `r##"…"##`** because `"#rrggbb"` in a comment ended an `r#"` string.

### 2026-10-12 · 0.2.1

- **`lineedit.rs`: one single-line editor for every text field** (find, replace, prompts). Cursor is a byte index on a grapheme boundary; widths come from `CellWidth` like the editor layout, so clicks and the drawn caret agree. The horizontal scroll offset lives in a `Cell` so rendering (which only has shared access to the field) can keep it stable: the view only scrolls when the cursor would leave it, and scrolls back when deleted text frees space. Fields return `Outcome::Ignored` for keys they don't use, which is how the find bar still passes ↑/↓, Ctrl+S, etc. to the editor.

### 2026-10-12 · 0.2.0

- **Preferences write the config with `toml_edit`**, editing the user's document in place: values are replaced keeping each line's decor (trailing comments, spacing), keys are only added when they differ from the default (existing keys are always updated), and `[keys]`/unknown entries are untouched. A missing file starts from the commented `--print-config` template. *Safety:* a file with a syntax error is refused (the dialog stays open), the user's pre-Plica file is copied once to `config.toml.bak`, and the write goes through `Document::save` (atomic, follows symlinks such as dotfile links, keeps permissions).
- **Live preview = apply immediately; Cancel re-applies the snapshot taken when the dialog opened.** One code path (`apply_settings`) serves preview, Apply and Cancel, so they can't diverge.
- **Colour selection moved to `colours.rs`** so startup and Preferences use the same decision; a theme or palette change sets `palette_watch_dirty` and the main loop rebuilds the palette watcher for the new file.
- **CLI/env colour overrides are reflected into the Preferences model** at startup (`theme_choice`/`palette_setting`), so saving from Preferences records what is actually in use instead of silently reverting to the config's older value.
- **The kept-alive palette watcher is named `_palette_watcher`:** it is only held for its `Drop`; an underscore-prefixed binding keeps it alive without an unused-variable warning (a bare `_` would drop it at once).
- **Known limitation:** incremental find re-searches the whole document when the query changes (about 150 ms per keystroke on a 35 MB file, measured by `make perf`). Fine for normal files; a future optimisation could narrow the previous results or debounce.

### 2026-10-12 · 0.1.6

- **Primary selection is published when a selection settles:** mouse selections on button release, keyboard selections 400 ms after the last change (`primary_due`, checked in `tick`). *Why:* each publish may spawn xclip; publishing on every Shift+arrow would spawn one per keypress.
- **OSC 52 with target `p` is sent as well as the tool**, because Kitty sets the primary selection from it even when no X tool is installed (default `clipboard_control` allows write-primary).
- **Find-bar selections don't set primary**; only selections the user makes do, so searching doesn't overwrite what they last selected elsewhere.
- **`Clipboard::internal_only()` for unit tests.** *Problem found while adding this feature:* unit tests inherit the developer's `DISPLAY`, so any test that copied text would run xclip and overwrite the real desktop clipboard. Tests now use an internal-only clipboard (the e2e harness already clears `DISPLAY`/`WAYLAND_DISPLAY`).

### 2026-10-12 · 0.1.5

- **Config is parsed into a raw `toml::Table` and validated field by field** (`config.rs`), not deserialised with serde. *Why:* serde fails the whole struct on one bad value; walking the table lets each setting fall back on its own and lets every unknown key or bad value produce a precise warning (including the value written). A syntax error is the only whole-file fallback. Plica never writes the file (a Preferences UI will need a comment-preserving writer such as `toml_edit`).
- **Commands are a single table** (`commands!` macro in `keymap.rs`): config name, description and default chords live together, so the config parser, `--print-config` and the F1 help can't drift apart.
- **Chord normalisation:** an uppercase char means Shift (terminals send `S` with or without the SHIFT flag); BackTab means Shift+Tab. Legacy terminals can't send Ctrl+Shift+letter at all, so defaults always include a non-Shift alternative (F12 for Save As, Ctrl+Y for Redo).
- **Rebinding a chord already used by another command takes it over** (config wins over defaults), and that command's help entry shows "unbound" if it lost its only key.
- **Motion and editing keys are fixed** (arrows, Home/End, Backspace/Delete, Enter, Tab indent, typing, Alt+1..9): remapping them adds complexity without a user need yet.
- **The find bar consults the keymap first**, so a remapped Find/Replace/Find next still works while the bar has focus.
- **The e2e harness sets `XDG_CONFIG_HOME` to its temp dir**, so a real user config (or Kitty palette) can never change test results.

### 2026-10-12 · 0.1.4

- **Snapshots are written by one background thread** fed through a channel (`recovery.rs`). Ropey clones are O(1), so the UI thread hands over the rope without copying text; a 35 MB buffer never stalls typing. Writes and removals share the queue, so "write then remove" can't be reordered; `flush()` waits for the queue at exit/signal.
- **Snapshot timing:** a dirty tab is written when its content has been unchanged for 2 s, or at least every 30 s during continuous editing (`App::tick_recovery`, every main-loop pass). Clean tabs drop their snapshot.
- **Orphan detection = snapshot from a session whose Plica process is gone.** Liveness checks `/proc/<pid>/comm == "plica"` (guards against pid reuse) **and the process state is not Z/X**. *Bug found by e2e:* a killed Plica that its parent hadn't reaped yet is a zombie that still has `/proc/<pid>/comm`, so its snapshots were wrongly treated as belonging to a live instance.
- **Session ids are pid + nanoseconds + a per-process counter.** *Bug found by tests:* pid + milliseconds collided for two sessions created in the same millisecond, making each treat the other's snapshots as its own.
- **Restore applies the snapshot as one transaction on top of the file as it is on disk now**, so the user can Ctrl+Z to compare, and saving is an explicit choice. Restored snapshot files are deleted at once because this session snapshots the restored tabs itself.
- **Signals flush snapshots instead of writing `.txt` copies** (0.0.1 behaviour), so all unsaved work goes through one recovery path.
- **A normal quit removes this session's remaining snapshots:** by then any dirty tab was explicitly discarded via the quit dialog.

### 2026-10-12 · 0.1.3

- **Find has its own bar (`find.rs`), not the generic prompt:** it needs two fields, toggles, clickable controls and state that outlives a keypress. Its commands live in an `impl App` block in `find.rs` so `app.rs` stays manageable.
- **Matches are cached per `(buffer id, version, query, options, scope)`.** `Buffer` gained a process-unique id and a version bumped on every change (edit, undo, redo); highlights, the "n of m" count, stepping and Replace All all read the cache, so moving between matches never rescans. Capped at 200 000 matches (count shows "+").
- **Ctrl+R is a replace alias** because legacy (non-Kitty-protocol) terminals send Ctrl+H as Backspace.
- **Smart-case search was replaced by an explicit Match case toggle** to match GUI-editor expectations.
- **Regex search is deliberately not included yet** (plan: regex is an advanced, bounded option); literal + case + whole word cover the Phase 2 needs without a new dependency.
- **In-selection scope is adjusted by each replacement's length change**, so replacing one at a time keeps the scope covering the same text.

### 2026-10-12 · 0.1.2

- **Save chains via `AfterSave`.** Closing or quitting may need a save that is interrupted (Save As prompt, disk-conflict dialog, Replace dialog). The pending follow-up (close tab / continue Save-All-then-quit) is stored in `App::after_save` and resumed by `continue_after_save()` after any successful save; Esc, Cancel or a failed save clears it. Save All is re-entrant: it walks the tabs and stops at the first one needing input, and resuming simply walks again (saved tabs are no longer dirty).
- **External changes are detected by comparing disk stamps** (mtime + length + inode) recorded at load/save, never by trusting watcher events alone. Our own atomic save updates the stamp, so the watcher event it causes compares equal and is ignored. Watcher events are debounced 200 ms per path; focus-gained re-checks every tab as a backstop for filesystems without inotify.
- **Document watcher watches directories, not files** (same reason as the palette: atomic replace changes the inode). `DocWatcher::sync` adds/removes directories as tabs change, each loop iteration (cheap: a set compare).
- **Reload is an undoable transaction** (`Tab::reload_from_disk` replaces the whole text with `Buffer::transact`, then marks saved), so an unexpected external rewrite can be reverted with Ctrl+Z; the caret keeps its line/column.
- **Conflicts in background tabs are deferred:** the tab is flagged and the dialog appears when it becomes active, so a background change never interrupts typing in another tab.
- **Document paths are made absolute at load**, because watcher events carry absolute paths and tab identity must not depend on the working directory.

### 2026-10-12 · 0.1.1

- **Active tab is a plain field (`App::tab`); other tabs live in `bg_tabs`** in order around index `active`. *Why not `Vec<Tab>` with an index:* with an accessor, every `self.tab_mut().buf.insert(..)` borrows all of `self`, which clashes with layout helpers that also borrow `self`. A direct field lets Rust split borrows (`self.tab.buf` vs `self.regions`). Switching tabs is a `mem::replace` plus an insert/remove in `bg_tabs`.
- **An untouched Untitled tab is replaced** when a file is opened or dropped, so starting `plica` and then opening a file doesn't leave an empty tab behind.
- **Closing the last tab quits** (Mousepad/Gedit behaviour) instead of leaving an empty editor.
- **Unsaved-close confirmation is "press again"** for now (the `Pending` mechanism). Proper dialogs replace it in 0.1.2.
- **Tabs beat title-bar buttons for space:** the buttons are only drawn when all tabs fit beside them, since every button has a key.

### 2026-10-12 · 0.1.0

- **File drops are detected from paste content.** *Problem:* terminals have no drop event for the program inside them. Kitty pastes the dropped paths (one per line, unquoted, already decoded from `file://`); VTE terminals paste shell-quoted paths plus a trailing space; some paste raw `file://` URIs. *Decision:* a bracketed paste counts as a drop only if **every** token is an existing absolute path or local `file://` URI. *Trade-off:* a deliberately pasted bare path to a real file opens it; **Ctrl+V is exempt** so text insertion stays possible. With unsaved changes the drop is refused rather than discarding work (single buffer until tabs).
- **Indent style detected per file** (tabs unless space-indented lines dominate the first 2000 lines). Multi-line indent/outdent uses `Buffer::transact` so it is one undo step; positions are given against the original text and applied in descending order.
- **Title bar shortens the file name in the middle.** *Problem:* in narrow terminals the buttons were drawn over the name. Priority is file name > app name > full name; a middle ellipsis keeps the extension visible.

### 2026-10-12 · 0.0.2

- **User's Kitty remapped onto Super** (`kitty_mod ctrl+super`; backup `~/.config/kitty/kitty.conf.pre-keyreplan-20261012`). *Problem:* Kitty's defaults and the user's config took about 80 app chords (Shift+arrows, Ctrl+arrows, Ctrl+Shift+S/Z, Ctrl+Tab, Shift+click). *Decision:* conventional app keys win on the user's machine, **but Plica keeps fallbacks** (F12 Save As, Ctrl+Y redo, Alt+←/→ word motion, Alt+click extend) because stock terminals still take those chords.

### 2026-10-12 · 0.0.1 (Phase 0 spike)

- **Stack: Rust + crossterm + ratatui + ropey** (ADR 0001). ratatui is used only as a diffing cell buffer; the editor draws cells itself.
- **Caret is the terminal's hardware cursor** (steady bar), so Kitty draws it with the theme's cursor colours and cursor trail.
- **Wide glyphs reset their shadow cells** after writing the symbol; otherwise ratatui's diff can leave stale half-glyphs.
- **Control characters are never written raw** (`^X`, `^?`, U+FFFD); lone combining marks are drawn on U+25CC. *Problem:* raw C0/ESC in a file would corrupt the terminal.
- **Palette watcher watches the parent directory**, filters by file name, and also watches the symlink target's directory. *Problem:* theme tools replace the file by rename, which detaches an inode watch.
- **Watcher ignores open/read/close-nowrite events.** *Bug found by the e2e test:* Plica's own reload read the file, generated an access event, and triggered another reload: an endless loop every 150 ms (and status messages kept being overwritten). Only create/modify/remove/close-write count.
- **Invalid palette keeps the last-known-good colours.** A palette must define valid `foreground` and `background`, otherwise it is rejected whole; bad optional values are warnings.
- **Contrast repair:** roles are nudged toward white/black until they meet WCAG targets (4.5 for text, 2.5 for muted). The user's ThemeWall `color8` (`#505253`) is lifted to `#848686` for muted text.
- **Atomic save:** temp file in the same directory → fsync → rename → fsync dir; mode and owner copied; new files honour the umask (read from `/proc/self/status` so the process umask is never changed). *Workarounds:* symlinks are resolved so the link survives; files with **hard links are written in place**, because rename would split the link. ACLs/xattrs are not preserved (known limitation).
- **UTF-8 only:** other encodings are refused at load with the offset of the first bad byte, rather than risk lossy round-trips. *(Superseded in 0.3.1: other encodings are decoded with a verified round trip.)*
- **Line endings are never rewritten.** The rope holds the file's exact text; Enter and paste insert the document's dominant ending. *Problem:* terminals often deliver pasted newlines as CR, so pastes are normalised to the document's ending.
- **Undo grouping:** typing coalesces until a 1.5 s pause, a whitespace insert, caret movement or save. *Bug found by e2e:* typing over a selection put the first key in its own step; now the replacement and the following keys are one step.
- **Dirty tracking compares undo-stack identity**, not a revision counter, so undoing back to the saved state clears the ● marker.
- **Smart-case find lowercases only where UTF-8 length is unchanged**, so byte offsets in the lowered copy still map onto the original text.
- **Status messages take priority on narrow terminals:** the right-hand fields collapse to Ln/Col. *Problem:* at 60 columns messages like "Save failed" were cut to a few characters.
- **Clipboard copy goes to three places** (internal, OSC 52, wl-copy/xclip/xsel). Paste prefers the tool, with a 700 ms timeout so a dead clipboard owner can't hang the UI.
- **Recovery copy on SIGTERM/SIGHUP/SIGINT** to `$XDG_STATE_HOME/plica/recovery/`, never next to or over the original. *(Superseded in 0.1.4 by snapshots.)*
- **Kitty facts found during the spike:** Kitty eats Shift+click/drag in mouse-grabbing apps (its selection override), most Ctrl+Shift+letter chords, and Ctrl+Tab. With an app in front, a drop becomes a paste of plain paths.
- **e2e harness quirks (test-only):** pyte composes `e`+U+0301 into `é` and its display strings don't map to columns for wide glyphs, so checks NFC-normalise and compute columns from cells.
