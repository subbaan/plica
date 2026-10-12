# ADR 0001: Language and TUI stack

**Status:** Accepted (approved by the user 2026-10-12) · **Date:** 2026-10-12 · **App version:** 0.0.1

## Decision

**Rust** with **crossterm** (input and terminal control) and **ratatui** (only as a cell buffer that sends just the changed cells), **ropey** for text, **unicode-segmentation** for graphemes, **notify** for file watching. Distributed as one static binary.

## Options considered

| | Rust + crossterm/ratatui | Go + tcell / bubbletea |
|---|---|---|
| Mouse (SGR, drag, any-motion) | crossterm: full SGR 1006, drag, modifiers | tcell: full; bubbletea: good |
| Kitty keyboard protocol (Ctrl+Shift+letter, Ctrl+I vs Tab) | crossterm: progressive enhancement flags, with a query to detect support | tcell: partial; bubbletea v2: yes |
| Text buffer | ropey: mature rope with char/byte/line index conversion, chunk iteration for grapheme cursors | no equally mature rope; would hand-roll a piece table |
| Unicode grapheme/width | unicode-segmentation `GraphemeCursor` works over rope chunks; ratatui's `CellWidth` | uniseg / go-runewidth: good |
| Rendering | ratatui `Buffer` gives a sent-only-changed-cells redraw for free; we own all widgets | tcell screen: also only sends changed cells |
| Release | `cargo build --release`, single binary, no runtime | single binary, very easy cross-compile |
| Maintainability | strong types for the three offset spaces; exhaustive matches for commands | simpler language, weaker typing for offsets |

Both would work. Rust wins on the text model (ropey plus grapheme cursors over chunks) and on precise keyboard handling. Those are the two areas the plan calls highest-risk (tables, Unicode hit-testing, shortcut conflicts). Go's advantage (simpler cross-compiling) matters less for a Linux-first tool.

## Consequences and conventions

- **Offsets** (plan §3.1): inside Plica a position is a *char index into the rope*, always snapped to a grapheme boundary (`buffer.rs`). Byte offsets appear only in `document.rs` (IO). Display cells appear only in `layout.rs`. Layout uses ratatui's own `CellWidth`, so hit-testing and rendering use the same width table.
- **Rendering:** ratatui widgets are not used for the editor; `ui.rs` writes cells directly. Wide glyphs reset their shadow cells so the redraw logic skips them. Control characters are never written raw (shown as `^X`, `^?` or U+FFFD); lone combining marks are shown on a dotted circle.
- **Caret** is the terminal's hardware cursor (steady bar), so Kitty draws it with the theme's own cursor colours.
- **Layout** is computed per logical line, on demand (visible lines plus lines the caret passes through). Nothing is computed over the whole document per keystroke. Known limit: one multi-MB *single line* is laid out whole when visible.
- **Undo:** one transaction per user action; typing coalesces until a pause (1.5 s), a space, or caret movement. Typing over a selection plus the keys that follow it is one step; paste is one step.
- **Dependencies** were picked for maturity: ratatui 0.30, crossterm 0.29 (through ratatui's re-export, so versions can't drift apart), ropey 1.6, notify 8, tempfile 3, signal-hook 0.3, base64 0.22, anyhow 1.

## Revisit if

- ratatui's cell-width rules diverge from Kitty's for emoji ZWJ sequences in ways users notice (we could switch to Kitty's text-sizing / width query).
- Very long single lines become a real use case (would need cached per-line layout chunks).
