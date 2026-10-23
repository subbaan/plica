# Phase 6 gate: manual checklist (v1.0)

**Gate (plan §9):** complete manual regression suite, automated tests and crash recovery drills. No known data-loss issues. Versioned release notes.

**Automated coverage:**

- `make check`: clippy, about 170 unit tests (editing, layout, encodings, tables, highlights, themes, syntax colours, recovery, config) and the release build.
- `make e2e`: 20 pty sessions with real keys and mouse. They cover typing, mouse selection, palettes changing live, files changed outside Plica, crash recovery after SIGTERM, SIGKILL and a closed window, tabs, find/replace, config, Preferences, highlights, encodings, tables, overwrite mode and syntax colours.
- `make perf`: a 35 MB text file and a 2 MB Python file (startup, jumps, typing, idle CPU, memory).
- Randomised invariant tests: tables (`table_invariants_under_random_operations`), the buffer change log (`changes_since_covers_every_edit`) and incremental syntax parsing (`incremental_matches_fresh_parse`).
- Release notes: `CHANGELOG.md`, one entry per version.

What remains needs your eyes. Use the release binary from `make dist` (static musl) where it says so.

## Everyday use (plan §11)

1. **First minute:** `plica notes.txt` in Kitty. Type at once, mouse-select, copy into another app, paste back, undo, save, quit.
2. **Tabs:** open several files; switch with the mouse, Ctrl+PageUp/Down and Alt+1…9; close a modified tab and get a safe prompt.
3. **Find and replace** a phrase, save, reopen: the content is as expected.
4. **Unicode:** a file with emoji, CJK and combining accents. Check the cursor, selection, clicks and table alignment.

## Colours

5. **Live theme:** switch the Kitty theme (`kitty +kitten themes`) with Plica open. Every region updates: text, selection, menus, table borders, status bar, highlights and **syntax colours**.
6. **Broken palette:** write an invalid palette file. Plica keeps its colours and recovers when the file is fixed.
7. **Syntax colours:** open a shell script without an extension, a Python file, Markdown, TOML and a diff. The language in the status bar is right, the colours are readable and the main kinds look different in your palette (`plica --check-palette PATH` lists them). F4 and a click on the language switch it; Preferences → Appearance turns colours off.
8. **Light theme:** repeat 7 with `--theme solarized-light`.

## Data safety drills

9. **Kill:** edit, then `kill -9` Plica, restart. Recovery offers the text, and the original file is untouched until you save.
10. **Window closed:** edit, close the terminal window, restart. Same as 9.
11. **External change:** modify an open file from another program, with and without unsaved edits. A clean tab reloads (undoably); a modified one asks before any overwrite.
12. **Disk trouble:** save into a read-only folder, and onto a full disk (a small tmpfs: `sudo mount -t tmpfs -o size=1M tmpfs /mnt/x`). The save fails with a message, the original is intact and the tab stays modified.
13. **Odd files:** a symlink, a hard link, a CRLF file, a file without a final newline, an empty file, a windows-1252 file. Save each unchanged and compare: `cmp` shows no difference.

## Terminals and systems

14. **Terminals:** repeat 1, 2 and 5 in XFCE Terminal (menu bar on and off: F6/F7/F8 fallbacks) and one more (GNOME Terminal, Konsole or Alacritty). Mouse clicks, drags, wheel and Alt+click work; Ctrl+Shift chords have their fallbacks.
15. **tmux and SSH:** inside tmux (`set -g mouse on`) and over SSH: mouse, colours, copy (OSC 52) and exit restoring the terminal.
16. **Other distributions:** run the `make dist` binary on an older system (for example a Debian 12 or Ubuntu 22.04 container or VM): `plica -V`, open, edit, save.
17. **Accessibility:** with `--theme terminal` (or `NO_COLOR=1`) everything is still visible without colour: selection, matches, highlights, comments, the current table cell. High contrast (Preferences → Appearance) reads well. Everything you can click is also reachable with keys (F1 lists them).

## Release

18. **Docs:** README, `man -l docs/plica.1`, `docs/keys.md` and F1 help agree with what you see.
19. **Package:** `make dist`; unpack the tarball somewhere new and check the sha256, the binary and the bundled docs and licences.

Record anything odd in `ARCHITECTURE.md` (log); a data-loss issue blocks the release.
