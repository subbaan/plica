# Changelog

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
