# Changelog

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
