# Phase 0 report: spike results

**App version:** 0.0.2 (report written at 0.0.1; Kitty column updated after the key re-plan) · **Date:** 2026-10-12 · Stack: see [ADR 0001](adr/0001-language-and-tui-stack.md)

## What was built

A working single-buffer editor. It covers most of the Phase 1 feature list so the risky parts are proven in real code rather than a throwaway demo:
rope buffer with grapheme navigation and undo/redo · soft word wrap with wrap-aware caret movement · mouse click/drag/double/triple-click, drag autoscroll, wheel, clickable title-bar buttons and scrollbar · find, go-to-line · atomic save · Kitty palette parser with live reload · terminal teardown on exit, panic and signals.

## Gate status (plan §9, Phase 0)

| Gate item | Status | Evidence |
|---|---|---|
| Mouse drag selection | ✅ automated | `tests/e2e/pty_harness.py`: SGR drag across lines, selection drawn in palette colour |
| Unicode cursor hit-testing | ✅ automated | click lands on grapheme boundaries with CJK + combining marks; unit tests for wide-glyph halves, tabs, wrap boundaries |
| Editing | ✅ automated | typing, replace selection, undo/redo grouping, bracketed paste (one undo step), CRLF preserved |
| Terminal teardown | ✅ automated (normal quit, SIGTERM) | alt screen, mouse, bracketed paste and keyboard flags restored. Panic path uses the same idempotent `restore()` but is untested |
| Live palette change | ✅ automated | atomic replace → whole UI recoloured; garbage file → last-known-good kept with ⚠; repaired file → applied |
| Real Kitty, interactive | ⏳ **needs you** | checklist below |

All automated checks: `make check` (28 unit tests + clippy) and `make e2e` (40 end-to-end checks).

## Capability matrix

✅ verified · 🔍 found by reading config/docs, not exercised · ⏳ not yet tested

| Capability | Kitty (your config since 2026-10-12) | xterm-compatible | XFCE Terminal | tmux | SSH |
|---|---|---|---|---|---|
| SGR mouse, drag, wheel | 🔍 supported | ✅ (pyte emulation) | ⏳ | ⏳ needs `set -g mouse on` | ⏳ passes through |
| Shift+click/drag reaches app | ✅ parser-verified: yes (stock Kitty: no, it uses Shift for its own selection) | usually yes | ⏳ | ⏳ | — |
| Alt+click reaches app | 🔍 yes | yes | ⏳ | ⏳ | — |
| Kitty keyboard protocol | 🔍 yes (detected at startup) | no → legacy keys | no | no (tmux strips it) | depends on terminal |
| Ctrl+←/→ reaches app | ✅ parser-verified: yes (window focus moved to Super+arrows) | yes | ⏳ | ⏳ | — |
| Ctrl+Shift+S / Ctrl+Shift+Z | ✅ parser-verified: yes (stock Kitty: no, `paste_from_selection` / `scroll_to_prompt`) | legacy can't tell them from Ctrl+S/Ctrl+Z | ⏳ | ⏳ | — |
| Ctrl+Tab | ✅ parser-verified: yes (stock Kitty: no, `next_tab`) | often not distinguishable | ⏳ | ⏳ | — |
| Bracketed paste | 🔍 yes | ✅ | ⏳ | ⏳ | ⏳ |
| OSC 52 clipboard write | 🔍 yes (default `clipboard_control` allows write) | varies | ⏳ | needs `set-clipboard on` | ✅ by design |
| System clipboard tool | X11: xclip, xsel present; no wl-clipboard | | | | n/a |
| Truecolour | 🔍 yes (`COLORTERM=truecolor`) | ✅ 256-colour fallback | ⏳ | ⏳ | ⏳ |
| Resize | ⏳ | ⏳ | ⏳ | ⏳ | ⏳ |

### Your Kitty re-plan (2026-10-12)

`kitty_mod` is now Ctrl+Super; everyday Kitty actions are on plain Super, and only Ctrl+Shift+C/V and Shift+Insert remain outside Super. Inside mouse apps, Ctrl+Shift+drag forces Kitty's selection. Backup: `~/.config/kitty/kitty.conf.pre-keyreplan-20261012`. "Parser-verified" means Kitty's own config loader reports these bindings; it has not been tested interactively yet.

### How Plica handles the conflicts (still needed for stock terminals)

| Plan default | Problem | Plica 0.0.1 |
|---|---|---|
| Ctrl+Shift+S Save As | Kitty takes it | **F12** (Office convention) also works |
| Ctrl+Shift+Z redo | Kitty takes it | **Ctrl+Y** |
| Ctrl+←/→ word motion | your kitty.conf takes it | **Alt+←/→** also works |
| Shift+click extend | stock Kitty takes it | **Alt+click** also works |
| Mouse-mode bypass | | Kitty's override (Shift+drag stock, Ctrl+Shift+drag in your config); **Alt+M** / "Mouse" button hands the mouse to the terminal fully |
| Ctrl+Tab tabs | Kitty takes it | to decide in 0.2 (Ctrl+PageUp/PageDown is the usual alternative) |

## Palette file findings

- `~/.config/kitty/current-theme.conf` exists. It is written by **ThemeWall**, which also keeps a `.themewall-backup`, and is pulled into Kitty via `include current-theme.conf`.
- Format: plain Kitty `key  #rrggbb`, with comments. It holds foreground, background, cursor, cursor_text_color, selection_*, url_color, tab and border colours, and color0..color15. The parser also accepts `#rgb`, `rgb:r/g/b` and `none`, and warns on `include`.
- `plica --check-palette ~/.config/kitty/current-theme.conf` parses it with no warnings. All roles meet the contrast targets; muted text was lifted from `#505253` to `#848686`.
- Plica watches the **directory**, so it doesn't matter whether ThemeWall rewrites the file in place or replaces it. Plica's own reads are ignored (that was a real reload-loop bug, caught by the e2e test).
- The path is the default only when the file exists; `--palette` / `PLICA_PALETTE` override it. It is not hardcoded in the parser.

## Performance (35 MB, 400k lines, mixed Unicode)

Startup to first render 0.16 s · Ctrl+End, typing, PageDown: each under the harness's ~60 ms polling floor · idle CPU 0 · RSS 44 MB.

## Manual checklist for you (real Kitty)

1. `./plica /tmp/t.txt`: type, then drag-select with the mouse. Double-click a word, triple-click a line.
2. Shift+click and Alt+click both extend a selection. Ctrl+Shift+drag should do *Kitty's* selection. Ctrl+←/→ moves by word.
3. Select text, Ctrl+C, paste into another app. Copy elsewhere, then Ctrl+V into Plica. Try Ctrl+Shift+V (Kitty's paste).
4. Change theme with ThemeWall while Plica is open: everything should recolour within about 0.2 s.
5. Resize the window. Toggle wrap (Alt+Z) on a long line and scroll sideways with Shift+wheel.
6. Quit (Ctrl+Q): the terminal should be completely normal afterwards.

## Known limitations (0.0.1)

Single buffer (tabs in 0.2) · Save As won't overwrite existing files (prompt in 0.2) · UTF-8 only; other encodings are refused rather than risk corruption · no config file yet (CLI flags/env) · ACLs/xattrs aren't preserved on save · a recovery snapshot is written only on SIGTERM/SIGHUP/SIGINT, not periodically · a multi-MB single line is laid out whole.

## Proposed implementation slices

1. **0.1.0, Phase 1 completion:** resize/scroll polish, whitespace-visibility toggle, indent/outdent of a selection, case-sensitivity toggle in find, Rust-side integration tests for viewport math.
2. **0.2.0-a:** config file (XDG, versioned schema) and keybinding remapping.
3. **0.2.0-b:** tabs, multiple files, Ctrl+PageUp/PageDown.
4. **0.2.0-c:** dialogs (dirty close, overwrite, external change: reload/compare/overwrite/save copy).
5. **0.2.0-d:** periodic recovery snapshots and a restore flow; find and replace.
6. **0.3.0:** semantic-role preview UI, highlight groups.

## Decisions for you

1. **Approve the stack** (Rust/crossterm/ratatui/ropey) before Phase 1 builds further on it.
2. **Tab switching (0.2):** Ctrl+Tab now reaches apps in your Kitty; Ctrl+PageUp/PageDown is still needed as the fallback for other terminals.
