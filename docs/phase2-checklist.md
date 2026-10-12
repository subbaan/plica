# Phase 2 gate: manual checklist

**Gate (plan §9):** workflows feel like a lightweight graphical editor rather than a terminal demo; data safety and clipboard behaviour validated on Kitty **and at least one other terminal** (e.g. XFCE Terminal).

Automated coverage: `make check` (unit tests), `make e2e` (pty end-to-end: tabs, dialogs, external changes, find/replace, recovery after SIGTERM/SIGKILL, config, preferences), `make perf` (35 MB file). What remains needs a real terminal and a person.

Run each list in **Kitty** and in **XFCE Terminal**:

1. **Tabs:** `./plica a.txt b.txt`. Click tabs, Ctrl+PgUp/PgDn, Ctrl+Tab (Kitty only after the key re-plan), Alt+1/Alt+9, middle-click a tab, wheel over the tab bar.
2. **Drop:** drag two files from Thunar onto the window: each opens in a tab. Drag a folder: the Open prompt appears inside it.
3. **Clipboard:** select text, Ctrl+C, paste into a GUI app. Copy in a GUI app, Ctrl+V into Plica. Select text, then middle-click in another app (primary). Select in another app, middle-click in Plica.
4. **Dialogs:** edit, Ctrl+W → Save / Don't Save / Cancel (keys and mouse). Edit two tabs, Ctrl+Q → Save All with an Untitled tab (asks for a name).
5. **External change:** open a file, change it from another terminal (`echo x >> file`): clean tab reloads, Ctrl+Z restores. Edit first, then change it externally: Keep Mine / Reload dialog.
6. **Recovery:** edit without saving, `kill -9` the Plica process from another terminal, start Plica: Restore brings the text back as unsaved.
7. **Find/replace:** Ctrl+F, Alt+C / Alt+W, Ctrl+H (Ctrl+R in XFCE Terminal), Replace one, Alt+A; Ctrl+Z undoes Replace All.
8. **Preferences:** F9, toggle wrap/line numbers/colours (preview), Cancel reverts; change something and Save: check `~/.config/plica/config.toml` kept any comments you had.
9. **Teardown:** after each quit, the terminal behaves normally (typing, mouse, colours).

Note any differences between the two terminals in `ARCHITECTURE.md` (log) and the conflict table in `docs/phase0-report.md`.
