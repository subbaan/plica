# Phase 3 gate: manual checklist

**Gate (plan §9):** switching the common desktop colour file updates *every* Plica interface region and linked highlight while open; corrupted or removed files do not break the session.

Automated coverage: `make e2e` session 13 checks every region (editor, title and status bars, find bar, accent, selection, highlights, an open dialog) across a dark → light switch, plus an unreadable, truncated, invalid-hex and deleted palette, restore, rapid switching, and a re-pointed symlink whose new target is then edited. Sessions 11 and 12 cover highlight groups, document highlights and the Preferences page. What remains needs your real theme workflow and eyes.

Run in **Kitty** (and spot-check **XFCE Terminal**):

1. **Live theme switch:** `./plica notes.txt` with no `--palette` (it finds `~/.config/kitty/current-theme.conf`). Open the find bar (Ctrl+F) and Preferences (F9) in turn, and switch themes with ThemeWall each time. Everything recolours: text, selection, find matches, the tab bar, status bar, find bar, Preferences, dialogs and TODO/FIXME highlights. Nothing keeps the old colours.
2. **Bad file:** with Plica open, `cp ~/.config/kitty/current-theme.conf /tmp/` then `echo garbage > ~/.config/kitty/current-theme.conf`: colours stay, ⚠ in the status bar. Copy the file back: colours return. (Keep your backup safe!)
3. **Highlights:** add a group in Preferences → Highlights (e.g. your name plus a two-word phrase, colour `color4`), Save. Check whole-word and match-case behave as you expect, and the file is unchanged (`git diff` or compare). Switch theme: the group's colour follows the palette.
4. **Document highlight:** in a saved file, put the caret on a word and press Alt+H (XFCE Terminal with menu bar: F8). Every occurrence in that file is highlighted; other files are not. Quit and reopen: still there. Alt+H again removes it. `~/.config/plica/documents.toml` holds the rule.
5. **Themes and contrast:** Preferences → Appearance: step through the built-in themes and look at the preview strip; turn on High contrast. Try `./plica --theme solarized-light` in a light terminal.
6. **Text fields:** in Save As and Find, use ←/→, Home/End, Ctrl+Backspace and click to place the cursor.
7. **Teardown:** after each quit, the terminal behaves normally.
8. **Encodings (0.3.1):** make a test copy, e.g. `printf 'caf\xe9 cr\xe8me\n' > /tmp/l1.txt`, and open it. It shows "café crème", the status bar says `windows-1252?`. Edit, save, and `xxd /tmp/l1.txt` still shows single-byte `e9`. Type ✓ and save: Plica offers UTF-8 instead. Click the encoding in the status bar (or Alt+E / F7): Reopen with… and Save with… lists filter as you type. Try a real old file of yours, if you have one, on a copy.

Record anything odd in `ARCHITECTURE.md` (log).
