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
        ├─ syntax.rs ── syntect grammars → token kinds; incremental per-line parse cache (per tab)
        ├─ lineedit.rs ─ single-line text field (cursor, word motion, scroll, click)
        ├─ picker.rs ── filterable list popup (encoding and table menus)
        ├─ table.rs ─── Markdown table recognition, TableModel with source spans, format/edit ops
        ├─ table_border.rs ─ bordered ASCII/Unicode tables: recognition, rendering, minimal replace
        ├─ table_edit.rs ─ App: typing in cells, Tab/Enter navigation, menu actions, insert/convert
        ├─ recovery.rs ─ snapshot writer thread, orphan scan, restore data
        ├─ recent.rs ── recent files and where each was left (recent.toml in the state dir)
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
- **Nothing walks the whole document per keystroke** except find when the query or document changes (O(n), cached otherwise). Syntax parsing is bounded by a time budget per frame and resumes between events.
- **Every UI region asks the Theme for a semantic role**, so a palette swap recolours everything.

### Event loop

`main.rs` polls crossterm (250 ms idle, 50 ms while a status message or palette reload is pending), drains all queued events before one redraw, checks the signal flag, and applies a palette reload 150 ms after the last watcher event. Redraws go through ratatui's diff, so only changed cells are sent to the terminal.

### Viewport

The top of the view is `(top_line, top_row)`: logical line plus visual row within it. That makes wrapped scrolling independent of document size. `ensure_caret_visible` counts rows from the top only as far as the viewport height.

## Decision and workaround log

### 2026-10-26 · 0.5.18

- **Status bar path: `prefs::tilde`, then `ui::fit_start`** (drop the start, keep the file name), measured with `layout::text_width` so wide characters count as two cells. `set_stringn` alone cut the end, leaving only the directory.
- **README screenshots are scripted** (`tools/readme-shots.sh`), so they can be retaken after UI changes. Each shot gets a fresh Kitty (`--config` pointing at a temporary `kitty.conf` that only includes a copied Kitty theme file, so the user's Kitty config, fonts and key maps are never read) with its own `--listen-on` socket. The scene is set with `kitty @ send-key/send-text`, and the window is captured by its X11 id (`kitty @ ls` → `platform_window_id`) with ImageMagick `import`, so nothing else on screen is captured. *Workarounds:* Kitty resets `HOME` for its child, so Plica's `HOME`/XDG variables are set with `env` inside the Kitty command line. A temporary `HOME` holds the demo files, config, state and the theme copy, so every path on screen reads `~/…`. The four theme-grid tiles are written to the work folder, not `docs/screenshots/`, so the script never deletes anything. There's no mouse injection, so scenes use only keys.

### 2026-10-26 · 0.5.17

- **Atomic saves bypassed the read-only bit.** *Problem:* found by the Phase 6 drills: saving a 0444 file in a writable folder succeeded and left a 0444 file with the new text. The kernel checks a file's own write bit only when the file is opened for writing; replacing it by rename (`atomic_write`) only needs write permission on the folder, the same reason `rm` can delete a read-only file. *Decision:* `Document::save` checks `access(W_OK)` on the resolved target before writing and fails with the typed `document::ReadOnly` (EACCES or EROFS; other errors fall through to the write, which reports them). `access`, not a trial `open(O_WRONLY)`: opening for write fires inotify `IN_CLOSE_WRITE` on close, which other programs watching the file would take as a change. The app turns `ReadOnly` into a dialog (Save As / Cancel) and keeps `after_save`, so Save As continues a quit or close chain. No "Save anyway", like gedit and nano; `chmod` is the deliberate way. Root passes `access` and saves, as in other editors. It covers every `Document::save` caller, including Preferences (`config.toml`), `documents.toml` and `recent.toml`.
- **e2e harness: `gc.collect()` before each new `Term`.** *Problem:* adding one session made an unrelated later check ("enc: guess announced") fail every run, with half a frame on screen. Each `Term` is a reference cycle (pyte's `write_process_input` lambda holds `self.p`), so old sessions are freed only by Python's cyclic GC, at an arbitrary allocation, and pexpect's close sleeps 0.1 s or more per pty. When that landed inside the new session's fixed 1 s startup pump, the frame hadn't fully arrived when the check ran. Collecting before spawning keeps that cost outside the timed window.
- **Phase 6 drills run outside `make e2e`** (2026-10-26), against the `make dist` binary: odd files round-trip byte for byte (CRLF, no final newline, empty, windows-1252, BOM, UTF-16, mixed endings, stray bytes, symlink, hard link, mode 0640); a full disk (64 kB tmpfs mounted in an `unshare -rm` namespace, no sudo) fails cleanly with the original intact and no temp file; the static binary opens, edits and saves in an empty chroot; legacy-encoded fallbacks (F6–F9, F12, Ctrl+Y, Ctrl+PgUp/PgDn, Alt+arrows, Alt+1…9, Alt+click) and the same keys in a stock Kitty (`--config NONE`, driven with `kitty @ send-key`) work. No-colour mode: selection and the current match are reversed, other matches underlined, highlights bold, comments dim. *Not covered:* XFCE Terminal/Konsole windows themselves, tmux and SSH (not installed/running here), pasting into another app. *Test limits:* pyte (the e2e terminal) normalises combining marks and drops emoji modifiers and ZWJ parts, so grapheme checks compare saved bytes and caret columns, not cell text.

### 2026-10-26 · 0.5.16

- **Untitled tabs hold Alt+H groups in memory** (`Tab::doc_groups`), since `documents.toml` is keyed by path and an Untitled tab has none. `active_highlights` reads them in place of `doc_rules` while there's no path. Every way an Untitled tab gets a path (Save, Save As, Save All, close/quit dialogs) goes through `App::save_as_path`, which calls `DocRules::adopt` after a successful save and clears the tab's copy only if that write worked. The toggle itself is the shared `dochl::toggle_marked`, so both paths behave the same.
- **`adopt` merges, `carry_over` doesn't.** Save As of a named file leaves the target's existing rules alone (they were set for that path on purpose). An Untitled document's marks were made just now, so they're merged into a same-named group on the target (case-insensitive term dedupe) instead of being dropped. Not in recovery snapshots: losing marks after a crash costs a keystroke per word, and keeping them would mean a new snapshot format.

### 2026-10-26 · 0.5.15

- **An Untitled document matches `file_types` as `txt`** (`Group::applies_to` with no path). It used to match only groups without file types, so a user's txt-only group vanished in a new tab. Plain text is what an Untitled tab is (no syntax colours either), so md-only or py-only groups still wait for a name. Extensionless files (`README`, `Makefile`) are unchanged: they match only groups without file types.

### 2026-10-25 · 0.5.14

- **Places live in `$XDG_STATE_HOME/plica/recent.toml`, not the config dir:** they're Plica's own state, written all the time, and safe to delete. (Per-document highlights stay in `documents.toml` next to `config.toml` because the user writes those.) Keyed by canonical path like `documents.toml`. Lines and columns are 1-based on disk so the file reads naturally; the column counts chars, and restoring snaps it to a grapheme boundary and clamps it to the current text.
- **Two windows don't lose each other's entries:** changes collect in `Recent::pending`, and `flush` re-reads the file, merges (the newer `time` per path wins, a tie goes to this window), trims to 500 and writes atomically through `Document::save`. Flushes happen on tab close, on quit (also after SIGHUP or a terminal error), and from `App::tick_recent` every 30 s, which only records tabs whose `Tab::spot()` differs from what was last recorded. Nothing happens per keystroke.
- **The recent list never stats its entries.** The user has old mechanical drives and network automounts, and a stat per entry for a 500-entry list could wake them all. A missing file is noticed when it's chosen, and then dropped from the list. Opening it through `open_file` would otherwise make a "New file" tab.
- **Only hand-made choices are remembered.** Language: only when `syntax_manual` is set (stored as the grammar's name, because grammar indices aren't stable between versions; "" means plain text). Encoding: only after Reopen with / Save with (`Tab::encoding_manual`). It comes back as the decoder's `prefer`, not `force`, so a BOM or a file that has since become UTF-8 still decides; the choice lapses when the bytes override it. `--encoding` (force) is never remembered. Wrap and the other view toggles are global settings, not per-file, so they aren't stored.
- **`+LINE` beats the remembered line** and resets the top line to 0, so the view is placed as before (the caret at the bottom of the screen). Recovery restores still put the caret at the start, because the text was replaced.
- **Reach:** Alt+R (free in XFCE Terminal's menu bar, unlike Alt+E/H/T) and ↓/↑ in the Ctrl+O prompt (its hint says so). Ctrl+O itself stays a typed path, so muscle memory and creating new files are unchanged.
- **e2e:** the harness's sessions share one state dir, so a file reopened by a later session now comes back at its old place. The table session's reopen check presses Ctrl+Home first. Session 21 covers the feature.

### 2026-10-23 · 0.5.13

- **Muted takes `color8` only if it recedes** (less contrast with the background than the text has), else a text/background mix. Solarized's canonical slot layout puts base03, its darkest colour, in `color8`, so on Solarized Light comments and line numbers were darker than body text. Found while previewing screenshots; `muted_recedes_in_every_builtin` covers it.
- **Screenshot kit:** `tools/screenshot.sh` copies `docs/screenshots/demo/` (fictional files plus a demo config with one highlight group) into a fresh `plica-demo.*` folder under `$TMPDIR` or `/tmp`, and points `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `XDG_CACHE_HOME` there. It unsets `PLICA_PALETTE` and `NO_COLOR` and starts `kitty --config NONE` with the theme's background, so none of the user's palette, highlight groups, `documents.toml`, recovery snapshots or Kitty settings can appear in a screenshot. The folder is left in place (path printed), never deleted.

### 2026-10-23 · 0.5.12

Phase 6 test audit against the plan's automated-test list (section 10). The gaps it found:

- **Damaged recovery snapshots are recovered lossily** instead of being skipped: the file is read as bytes, the header up to the first blank line must start with the magic line, an unreadable pid counts as a dead process, and invalid UTF-8 in the text becomes U+FFFD with `Snapshot::damaged` set (shown in the dialog). Losing a whole document over one bad byte was the worse failure. A file without the magic header is left alone, never deleted.
- **Stale `.tmp` snapshot files** (a crash during `write_atomic`) are removed by `Recovery::orphans` when the pid in their name isn't a running Plica. The previous complete snapshot is still in place, because a write only replaces it by rename.
- **Drag autoscroll hit-tested against the rows drawn before the scroll**, so the selection's head trailed one row behind the edge (and stopped there on release). `on_drag` now re-lays out `rendered_rows` and `rendered_scroll` after it scrolls. Found by the new unit test `drag_past_the_edge_autoscrolls_and_extends_selection`.
- New tests: syntax colours meet their contrast targets and keep comment, string, keyword, function and type distinct in every built-in theme (normal and high contrast); empty and newline-only files round-trip byte for byte.

### 2026-10-23 · 0.5.10

- **Syntax colours use syntect with two-face's grammars and the pure-Rust `fancy-regex` engine.** Measured before choosing: Oniguruma parses about 3.5× faster and makes a smaller binary, but it is C, and the static musl release build would then need a musl C compiler (`musl-gcc`), which isn't installed. That's the same trade-off as mimalloc (0.5.5). The cost of fancy-regex is about 0.1 ms per line of Rust, 0.2 ms for Python and 0.75 ms for Markdown, so the design below never parses more than it must. The binary grows from 2.8 to 6.1 MB (grammars and regex engine). two-face (bat's set, 213 grammars) costs only about 0.6 MB more than syntect's 75 and adds TOML, INI, TypeScript, Dockerfile, Git files and more. Grammar licences go into THIRD_PARTY_LICENSES.md through `examples/grammar_licences.rs`.
- **Tokens map to kinds, not colours.** A syntect `Theme` is built whose "colours" are kind numbers (comment, string, keyword…), so syntect's own selector scoring decides each token's kind (e.g. `storage.type.function` (`fn`, `def`) is a keyword, `storage.type` (`i32`) a type). Kinds resolve to palette slots and roles at draw time, contrast-repaired against the background (4.5:1, comments 3:1; 7:1 / 4.5:1 in high contrast), so live palette changes recolour code. The defaults follow the usual terminal conventions (strings `color2`, keywords `color5`…). In plain-terminal mode comments are dim and keywords and headings bold. Draw precedence: base < syntax < table borders < word highlights < control chars < find match < selection.
- **Incremental parse (`syntax::Colouring`):** the parser state at the start of every 32nd line is kept, and `exact` is the line up to which start states are known exactly. `Buffer` now keeps a log of what each version replaced (`changes_since`, composing several edits into one covering change), so an edit moves `exact` back to its first line, drops kept data inside the changed lines and shifts what follows. Re-parsing stops as soon as it reaches a kept state that comes out equal: the rest is still right. A kept state past `exact` is trusted only up to the next *boundary* (a changed line, or where an earlier exact parse stopped). *Bug found by the randomized test:* without the second kind of boundary, an edit far above made a later convergence trust runs from an older parse. `incremental_matches_fresh_parse` compares against a fresh parse after random edits, budgets and unfinished idle work.
- **Lazy, budgeted parsing:** a frame parses towards the bottom of the view for at most 12 ms. Lines it can't reach are guessed, from a kept state at most 64 lines up, or from scratch 64 lines up. The main loop then polls every 1 ms and `App::tick` continues in 8 ms slices until the view is exact, then redraws. After a jump more than 1 MB of text past `exact`, the view keeps its guesses (like Vim's syntax sync) instead of parsing everything above it: about 4 s of CPU per MB. Files over 10 MB, and lines over 8 KB, are not parsed. Runs are kept for at most 4000 lines near the view.
- **Detection** (`syntax::detect`): file name, then extension (also lowercased), then the first line (`#!…`). It reads the buffer's first line, never the file (syntect's `find_syntax_for_file` would). It re-runs whenever the path or first line changes (Save As, typing a shebang into a new file), unless the user chose a language from the menu (`Tab::syntax_manual`). Plain text shows no label in the status bar (it stays short at 60 columns); the language menu is also on F4.
- `[profile.dev.package.*] opt-level = 3` for syntect and the regex crates: unoptimised, the grammar tests took minutes.
- **Fixed:** `--theme NAME` was missing its `continue`, so `--theme` was also opened as a file (found while testing syntax colours with the e2e harness).

### 2026-10-18 · 0.5.9

- **Tables switch:** `Tab::table_at` returns None when `Tab::tables_enabled` is off. Every table feature goes through it, so one check turns them all off (cell keys, re-padding, sideways scrolling, the column click, the status field). The flag is per tab, copied from `TabOpts` and updated in every tab by `apply_settings`, because `Tab` can't see `App`. The table menu and Format table say tables are off instead of offering to insert a table that wouldn't be recognised.
- **Edit config.toml from Preferences** opens the file as an ordinary tab instead of a special editor. A missing file opens as an unsaved tab holding `config::default_text()`, so nothing is written until the user saves. If the dialog has unsaved changes they are saved to the file first, otherwise saving the tab would silently drop them. If that save fails (a broken file), the tab still opens, because that's how to fix it.
- **Saving config.toml applies only what changed in the file:** `save_active` loads the file's config before writing, and `config_saved` merges the settings that differ between before and after into the session's settings (`Settings::merge_changed`). Reapplying the whole file would undo session-only state that has no place in the file: a `--palette`/`--builtin-theme` override, or the Apply button. The keymap is always taken from the file, because nothing changes keys for a session only.

### 2026-10-17 · 0.5.8

- **Overwrite mode follows Gedit/Mousepad:** Insert toggles it (the remappable `toggle_overwrite` command), and a click on the status bar's INS/OVR field toggles it too. Only typed characters overtype (`Buffer::overtype`, one grapheme at a time). Paste, Tab, Enter, Backspace and Delete behave as in insert mode, and a selection is replaced as usual. The line ending is never overwritten, so overtyping at the end of a line extends it instead of joining lines. The overtype is a Delete + Insert pair in one `Typing` transaction, so it coalesces and undoes exactly like normal typing.
- **Tables:** in a cell, overtype replaces only the cell's text (up to `content.end`), never padding or a pipe; past the text it inserts and the column re-pads as usual. A paste into a cell always inserts.
- **Block cursor:** `ui::draw` sets `App::caret_block` only when the terminal cursor is the document caret (not a prompt, find bar or picker field, which always insert), and the main loop sends `SetCursorStyle` only when that changes. `term::restore` already resets the user's cursor shape on exit.
- The compact status bar (shown while a message is too long) still shows `OVR`, because the mode changes what typing does; `INS` is dropped there.
- e2e harness: session 17 leaves a recovery snapshot behind, so later sessions start with the recovery dialog (session 18 discards it).

### 2026-10-13 · 0.5.6

- **Closing the terminal window spun Plica at 100% CPU** (user report). A hung-up tty polls readable and `read()` returns 0 (or EIO). crossterm 0.29's default mio event source loops on `read()` until it gets an event or WouldBlock, so it never returned and the SIGHUP flag was never checked. Fixed in three layers:
  - crossterm's **`use-dev-tty`** event source (enabled through a direct `crossterm` dependency; Cargo unifies it with ratatui's) breaks out on a 0-byte read and returns an EIO, so `event::poll` comes back within its timeout.
  - **`term::hung_up()`** polls stdin for POLLHUP every loop pass, because SIGHUP doesn't always arrive (Plica in another session or process group). It uses an isatty result recorded at startup: on a hung-up tty, isatty itself fails with EIO.
  - Terminal I/O errors in the main loop now end it like a signal, so unsaved tabs are snapshotted, instead of `?` returning straight out.

  Exit messages use `writeln!` to stderr, because `eprintln!` panics when stderr is the dead terminal. e2e session 17 covers both cases (`setsid -w` gives the no-SIGHUP one). Harness note: a pty test must drain the master, answer the cursor-position and DA queries (or startup stalls for about 2 s per query), and not hold the master open in a blocked `read()`.

### 2026-10-13 · 0.5.5

- **Licence: MIT** (the user's choice; copyright "subbaan", their public GitHub name).
- **Release binary: static musl.** It is statically linked (`ldd`: not dynamic), so the glibc 2.39 requirement is gone. Measured: the 35 MB perf test is identical. Typing that widens a column of a 50 000-row table is about 30% slower (Markdown 190 → 250 ms, ASCII 130 → 180 ms), because musl's malloc is slower than glibc's. That's acceptable for that extreme case; a faster allocator (mimalloc) would add a C build dependency, so it isn't used. Third-party notices list the musl target's dependency tree.

### 2026-10-13 · 0.5.4

- **Release packaging without publishing:** `make dist` only builds files. Tagging and `gh release create` are documented in `docs/release.md` and stay manual, because publishing is the user's decision. The target never deletes anything (the user's rule): it copies over a versioned staging folder.
- **Third-party notices** come from `cargo tree -e normal --target x86_64-unknown-linux-gnu` (only crates actually compiled in, not dev or other-platform dependencies) plus each crate's LICENSE/NOTICE/COPYING files from the registry. Identical texts are printed once (Apache-2.0 repeated ~100 times took the file from 900 KB to 260 KB).
- **Portability:** the binary needs glibc ≥ 2.39 (built on Manjaro). A static musl build would fix that but needs the musl target installed (`rust-musl` package or rustup), which is left to the user.

### 2026-10-13 · 0.5.3

- **Docs that can't drift silently:** `docs/keys.md` is generated from the `Command` table plus a list of fixed keys (`keymap::cheat_sheet`, also `--print-keys`), and a unit test fails when the file differs from the generator. Another test fails when the man page misses an option listed in `--help` or names a different version. The README is hand-written; its claims were checked against the code when written (key handling, paths, limits).
- **Licence not chosen** (the user's call before any public release). All dependencies are permissive (mostly MIT/Apache-2.0; also ISC, Zlib, BSD-3-Clause and Unicode data licences), and the built-in themes adapt MIT-licensed palettes.

### 2026-10-13 · 0.5.2

- **A text copy stays the source text; spreadsheet data is an explicit choice** (user report: pasting tables into Calc only worked for Markdown). Offering both on the clipboard at once (text/plain plus TSV or text/html) isn't possible through wl-copy/xclip/xsel, which serve one type per call. Silently copying TSV for whole-table selections would break pasting a table into another document. So: a one-step "Copy for a spreadsheet" menu action, and a status hint when a text copy contains a whole table.

### 2026-10-12 · 0.5.1

- **The table's outer corners are outside it for Enter** (user report: with a table at line 1, column 1, there was no way to push it down). The 0.3.2 fix that maps an edge caret to the edge cell (so typed text can't land outside a row) made column 1 of the first line "in the first cell", so Enter added a row. Now Enter at the start of the first line, or at the end of the last line past its closing pipe or border, falls through to plain editing (`at_table_edge`). Typing at those spots still goes into the cell, and Enter anywhere else still adds a row: a plain line break mid-table would split it.

### 2026-10-12 · 0.5.0

- **Bordered tables ignore fences and indentation; Markdown tables don't.** GFM defines pipe tables, so they follow its rules (no code blocks, indent < 4). Bordered tables are a plain-text convention, and their complete-border grammar can't match prose. In Markdown files they typically sit in code blocks *so that* they render monospaced, which is exactly where editing them as tables helps. An ASCII-art box in a code block reads as a one-column table, but editing it only keeps the box closed.
- **Gate evidence:** `no_false_positives_on_aligned_prose_and_code` (corpus) plus the cross-format invariant fuzz. Six seeds × 2500 rounds passed before release.

### 2026-10-12 · 0.4.4

- **Failed table recognition returns the range it rules out.** `Table::scan` gives `Err((a, b))`: the lines around the asked one that can't hold a table in either format. Markdown now scans the whole block for its first header + delimiter pair (not just up to the asked line), so its answer holds for every line of the block. The cache keeps single-line misses in a HashSet and longer ranges in a short list. *Found by profiling:* a 50 000-row ASCII table broken by an edit made every visible line re-scan the whole block twice (bordered, then Markdown), about 1 s per frame.
- **Bordered rule lines refuse edits** (they are drawn by Plica). Typing there would break the table, and with it the cheap path above.
- **Width fast paths:** `layout::text_width` returns the byte length for printable ASCII. Bordered parsing reads each line once and maps joint columns directly for ASCII lines. Markdown realign computes only the edited column's width (`inner_width`) and reads widths from rope chunks without copying. Profiled before and after; 50k-row typing that widens a column: Markdown 390 → 190 ms, ASCII 450 → 130 ms.

### 2026-10-12 · 0.4.3

- **Plain-text table grammar (plan §14 open question): bordered only.** ASCII `+---+` / `|` and Unicode box tables ship. Border-less, space-aligned tables don't, because they can't be told apart from prose laid out in columns, which the Phase 5 gate forbids misreading. A bordered table must be complete: top and bottom rules, and every bar and joint at the top rule's joint columns (by display width). A broken one is plain text.
- **Column boundaries come from positions, not separators.** A `|` inside an ASCII cell is just text, so bordered tables need no escaping. Cell text copies and pastes as is, and conversion to or from Markdown adds or removes `\|`.
- **One model, two writers.** A bordered table presents the same shape as a Markdown one: `[header, separator, body…]`. The separator is the header rule, or a *virtual* empty row at the header's line when there is none. The outer rules and any rules between rows belong to `Table::extent` but aren't rows, so `first_line`/`last_line` come from the extent. All App code (navigation, cell selection, paste, the fuzz test) works unchanged. Markdown keeps its minimal per-cell edits. A bordered table is re-rendered from its cell grid on every edit (it must stay lined up), and `minimal_op` writes back only the changed middle of the text.
- **Alignment is read from padding** in bordered tables, which have no syntax for it. A column is right- or centre-aligned when every padded cell agrees and at least one shows it, otherwise left. Rendering pads accordingly, so it survives edits and conversion.
- **New tables** are rendered by the same writers (`render_markdown`, `table_border::render`). On a non-blank line the new table goes after that line, with a blank line between, because a Markdown table directly under a paragraph line isn't one.

### 2026-10-12 · 0.4.2

- **Enter in a table starts the next row at its first cell** (user feedback: the same column felt wrong after Enter at the end of a row). This matches Tab in the last cell and row-by-row data entry. The menu's "Insert row below" keeps the column, for filling in one column.

### 2026-10-12 · 0.4.1

- **Find keeps its haystack.** `FindBar` holds the searched text and its lowercased copy (`Hay`), keyed by buffer version and scope. Typing a query then only searches; before, every keystroke copied the rope to a `String` and lowercased all of it. `lower_same_len` has an ASCII fast path (a whole-ASCII string uses `to_ascii_lowercase`; otherwise ASCII chars skip the Unicode tables). Measured with `perf`: the smart-case lowercasing was 85% of find's time. 35 MB "find a late line" (7 keystrokes) went from 1380 ms (1000 ms at 0.3.1; the hot code hadn't changed, the difference was codegen) to 268 ms. The haystack is dropped when the find bar closes. This supersedes the earlier ~140 ms-per-keystroke find limitation.

### 2026-10-12 · 0.4.0

- **Cell escaping follows GFM's scanner exactly.** cmark-gfm reads a backslash and the char after it as one unit, so `\\|` is an escaped backslash followed by a *real* pipe. *Bug found by the invariant test:* pasting the data `a\|b`, or typing a backslash before an existing `\|`, produced `a\\|b` and split the cell, so the header and delimiter counts no longer matched and the table vanished. Three functions now handle it:
  - `table::sanitize` runs on every edited cell and escapes any bare pipe;
  - `escape` (data → cell) doubles runs of backslashes before a pipe or at the end;
  - `unescape` (cell → data) halves them again, so copy and paste round-trip exactly.
- **The invariant test drives the App, not just the model.** It uses keys, menu actions and paste events, so it covers the real code paths: caret placement, undo grouping, cell-selection clearing. `PLICA_FUZZ_ROUNDS` and `PLICA_FUZZ_SEED` scale it up. 150 rounds run in `make check`; 2000-round runs with several seeds passed before release.

### 2026-10-12 · 0.3.6

- **Soft-wrap policy for tables: never wrap a table row.** A wrapped Markdown row is unreadable and its cells don't line up. `App::rows_of` lays out table lines with `wrap_width: None`, and because every layout path (viewport, caret motion, hit-testing) goes through `rows_of`, they all agree.
- **Per-row horizontal scroll.** Without soft wrap, `Tab::scroll_x` scrolls the view as before. With it, only the caret's table scrolls, by `Tab::table_scroll_x`. It follows the caret in `ensure_caret_visible` and resets to 0 when the caret leaves the table. `App::row_scroll_x(line)` gives each line's offset. The draw loop records them per rendered row (`App::rendered_scroll`), and `pos_at_screen` uses those recorded values, so a click maps through exactly what was drawn even if the caret moves on the same click.
- **The table cache's "not a table" set is a HashSet.** `rows_of` now asks `table_at` for every line it lays out, and a Vec `contains` would make scrolling through a long file quadratic.

### 2026-10-12 · 0.3.5

- **A cell selection is its own state** (`App::cell_sel`: table, anchor and head cell, buffer version), not a text selection. A rectangle of cells can't be expressed as one char range. While it is active, the text caret sits in the head cell with no text selection, and the draw loop paints the selected cells' spans. It is dropped as soon as the buffer changes in another way (version check) or by any key that doesn't act on cells: copy, cut, paste, Shift+arrows, Delete, Backspace, typing and Esc keep it, everything else clears it first.
- **How a cell selection starts:** dragging from one cell into another, Shift+↑/↓, or Shift+←/→ only when the caret is at that edge of the cell's text. Inside a cell, Shift+arrows still select text, so ordinary selection keeps working in tables.
- **Copy is data, not source:** selected cells copy as TSV with `\|` unescaped; a paste escapes `|` again, so the round trip is exact. A plain copy of the source is still available with table keys off.
- **Pastes go through one grid rewrite** (`Table::rewrite`). The table's contents become a grid (header + body), the pasted block is written at the top-left cell, and the grid grows as needed (rows appended, columns added to every row). A table that was lined up (`format()` has nothing to do) is re-padded as a whole to stay lined up. In one that wasn't, only rows that received data are rewritten, without padding, so a sloppy table isn't silently reformatted. New rows inserted at the same position are merged into one insert, so applying them can't reverse their order.
- **Paste format detection:** Markdown rows if every non-empty line has an unescaped pipe (`---` rows dropped), otherwise tab-separated lines. A trailing line break doesn't make an empty row.

### 2026-10-12 · 0.3.4

- **`table::Ops`** names the `(at, delete_len, insert)` replacement list that all table edits return (clippy's type-complexity lint). Process note: 0.3.3 was committed after a failing `make check`, because the commands were chained with `;`. The release step now chains `make check && make e2e && git commit`.

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
