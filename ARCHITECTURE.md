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
        ├─ document.rs ─ load (UTF-8 only), atomic save, line-ending detection, disk stamps
        ├─ clipboard.rs ─ internal + OSC 52 + wl-copy/xclip/xsel
        ├─ drop.rs ──── recognise file drops delivered as pastes
        ├─ dialog.rs ── modal dialogs (buttons, mnemonics, focus) → Action
        ├─ find.rs ──── search engine, match cache, find/replace bar commands
        ├─ recovery.rs ─ snapshot writer thread, orphan scan, restore data
        └─ theme.rs ─── parse_kitty → Palette → resolve → Theme (semantic roles, contrast repair)
ui.rs ── draws App into ratatui's cell buffer; Painter maps roles → truecolour / 256 / plain
```

### Core invariants

- **Text is the canonical model.** Highlights and (future) tables are views computed from the rope; on-disk bytes are exactly the rope (plus a remembered BOM).
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
- **UTF-8 only:** other encodings are refused at load with the offset of the first bad byte, rather than risk lossy round-trips.
- **Line endings are never rewritten.** The rope holds the file's exact text; Enter and paste insert the document's dominant ending. *Problem:* terminals often deliver pasted newlines as CR, so pastes are normalised to the document's ending.
- **Undo grouping:** typing coalesces until a 1.5 s pause, a whitespace insert, caret movement or save. *Bug found by e2e:* typing over a selection put the first key in its own step; now the replacement and the following keys are one step.
- **Dirty tracking compares undo-stack identity**, not a revision counter, so undoing back to the saved state clears the ● marker.
- **Smart-case find lowercases only where UTF-8 length is unchanged**, so byte offsets in the lowered copy still map onto the original text.
- **Status messages take priority on narrow terminals:** the right-hand fields collapse to Ln/Col. *Problem:* at 60 columns messages like "Save failed" were cut to a few characters.
- **Clipboard copy goes to three places** (internal, OSC 52, wl-copy/xclip/xsel). Paste prefers the tool, with a 700 ms timeout so a dead clipboard owner can't hang the UI.
- **Recovery copy on SIGTERM/SIGHUP/SIGINT** to `$XDG_STATE_HOME/plica/recovery/`, never next to or over the original. *(Superseded in 0.1.4 by snapshots.)*
- **Kitty facts found during the spike:** Kitty eats Shift+click/drag in mouse-grabbing apps (its selection override), most Ctrl+Shift+letter chords, and Ctrl+Tab. With an app in front, a drop becomes a paste of plain paths.
- **e2e harness quirks (test-only):** pyte composes `e`+U+0301 into `é` and its display strings don't map to columns for wide glyphs, so checks NFC-normalise and compute columns from cells.
