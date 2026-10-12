# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project status

Plica is a mouse-friendly terminal text editor (Mousepad/Gedit feel, not Vim/Emacs). `PLICA_IMPLEMENTATION_PLAN_v0.1.0.md` is the source of truth for scope, phases and release gates. Phases 0–3 are done (0.3.0; manual gate checklists in `docs/phase2-checklist.md` and `docs/phase3-checklist.md`), plus text-encoding support in 0.3.1 (`encoding.rs`); next is Phase 4 (0.4.0: Markdown tables). The stack (Rust, crossterm/ratatui, ropey) in `docs/adr/0001-language-and-tui-stack.md` is approved. `docs/phase0-report.md` has the capability matrix, terminal shortcut conflicts, known limitations and the proposed next slices. Break work into small, reviewable slices.

## Commands

- `make build`: release build; recreates the `./plica` → `target/release/plica` symlink. No install target, by design.
- `make check`: clippy with `-D warnings`, unit tests, release build. Run after every change.
- `cargo test <name>` runs a single unit test (e.g. `cargo test layout::tests::word_wrap`).
- `make perf`: 35 MB large-file responsiveness test.
- `make e2e`: pty end-to-end test (`tests/e2e/pty_harness.py`; needs python-pexpect + python-pyte). Sends real key and SGR mouse sequences, checks rendered cells and colours, saved bytes, live palette reload and terminal teardown. Uses a temp dir, never the user's real palette or state dir.
- `./plica --check-palette PATH`: print the parsed palette, resolved roles and contrast ratios.
- Version lives only in `Cargo.toml` (`ui::VERSION` reads it).

## Workflow for every update

1. Bump the version in `Cargo.toml` (patch by default).
2. Add a `CHANGELOG.md` entry (user-facing changes).
3. Add an `ARCHITECTURE.md` log entry for any design decision, workaround, or bug whose cause isn't obvious from the code.
4. Run `make check` and `make e2e`.
5. Commit and push to the private repo `github.com/subbaan/plica` (branch `main`).

## Code map and history

`ARCHITECTURE.md` has the module overview, the core invariants and a dated **decision and workaround log**. Read it before changing a module, so you don't undo a workaround (e.g. the palette watcher's event filter, writing hard-linked files in place).

Before choosing any shortcut, check the terminal-conflict table in `docs/phase0-report.md`: stock Kitty and other terminals take most Ctrl+Shift+letter chords, Ctrl+Tab and Shift+click. The user's own Kitty has been remapped onto Super so apps get conventional keys, but Plica must still offer a working fallback for every action, for other people's terminals.

Mouse hit-testing uses `App::rendered_rows`, the rows exactly as last drawn, so clicks always match the screen. Width always comes from ratatui's `CellWidth`, never `unicode-width` directly, so layout and rendering agree.

## Architectural invariants (from the plan)

These span every module and are the things most easily broken:

- **Text is the canonical model.** Tables and highlights are derived views/operations over the buffer. The buffer must not depend on any table format or terminal framework. Keep rendering, disk representation and input handling separate.
- **Three distinct offset spaces:** byte offsets (IO), grapheme clusters (cursor motion), terminal display cells (layout/hit-testing). Never conflate them.
- **Tables:** logical `TableModel` mapped to exact source spans; screen cell geometry is for hit-testing only. Serialize only the smallest changed region; never reformat unrelated text or other tables; table edits undo atomically. Ambiguous structures degrade to plain text editing with no data loss.
- **Plain text stays plain:** no Plica metadata in user files. Document-specific highlight rules, recovery snapshots and state live under XDG dirs (`$XDG_CONFIG_HOME/plica`, `$XDG_STATE_HOME/plica`, `$XDG_CACHE_HOME/plica`).
- **File safety:** atomic saves (temp file in same dir + fsync/rename), preserve LF/CRLF and permissions, never silently overwrite externally modified files, autosave means recovery snapshots, not writes to the original.
- **Themes:** parse → resolve to semantic roles → contrast check → render are separate layers. The external palette path (`~/.config/kitty/current-theme.conf` is only a candidate) must be configurable, not hardcoded; inspect the real file before finalising the parser. Watch the parent directory to survive atomic replacement; on invalid/missing files keep the last-known-good palette. ANSI slots are colours, not meanings.
- Incremental rendering/matching: no whole-document scan or full repaint per keystroke.
- Restore terminal state (mouse mode, colours) on exit, including abnormal exit where possible.

## Scope guardrails

Non-goals for v1: rich text/WYSIWYG, spreadsheet features (formulas, merged cells), LSP/IDE features, plugin framework, Git panel, integrated terminal. End users must not need a Python runtime.

## Versioning

App and plan versions are independent. Phase 0 = 0.0.x; Phase 1 → 0.1.0, Phase 2 → 0.2.0, … Phase 4 (Markdown tables) → 0.4.0. The binary must answer `-h/--help` and `-V/--version`.
