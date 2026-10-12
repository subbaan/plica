# Plica: Implementation Plan

**Planning document version:** 0.1.0  
**Project working name:** Plica  
**Status:** Proposed, ready for technical discovery and staged implementation  
**Target:** A mouse-friendly terminal text editor for everyday text, notes, lists and structured information.

## 1. Product definition

Plica is a conventional, approachable text editor running entirely inside a terminal. Its interaction model should feel closer to Mousepad or Gedit than Vim or Emacs. Typing edits immediately, conventional keyboard shortcuts work, and mouse selection, scrolling, tab switching and clickable controls are first-class. It should be comfortable inside Kitty on Linux while remaining portable to other capable terminal emulators.

Plica is deliberately smaller and easier to learn than an IDE or full word processor. Its distinguishing features are **reliably editable plain-text/Markdown tables**, **user-defined word/phrase highlighting**, and **live integration with a shared desktop colour file**. Syntax highlighting is optional and not part of the first release's core identity.

### 1.1 Product principles

1. **Editing first:** The editor opens quickly into a usable text buffer, with no mode-learning prerequisite.
2. **Mouse and keyboard parity:** Core actions must be reachable by keyboard; mouse interaction should be natural wherever terminal protocols allow it.
3. **Plain text stays plain:** File contents must never acquire hidden formatting or Plica-only metadata as a side effect of ordinary editing.
4. **Explicit, reversible structure:** Recognised tables have enhanced editing operations, but the on-disk representation is ordinary text.
5. **Predictable data safety:** Save, undo, recovery and external-change handling must be robust.
6. **Palette coordination:** External desktop themes are a first-class feature, never a hard dependency.
7. **Restraint:** Avoid an IDE platform, plugin framework, Git panel, LSP, integrated terminal, and rich-text document format in the initial scope.

### 1.2 Non-goals for v1

- Rich-text/WYSIWYG documents, fonts, embedded images, page layout or word-processing document formats.
- Spreadsheet formulas, merged cells, databases or arbitrary nesting inside tables.
- Full syntax-aware IDE features, language servers or project indexing.
- Reimplementing the user's external desktop theme switcher.

## 2. Agent instructions and discovery gate

**Before writing production code:**

1. Inspect the available repository and existing code, if any. Honour existing language, conventions, tooling and tests. If starting fresh, document the chosen language and TUI stack with pros/cons.
2. Investigate **current** terminal capabilities and TUI library behaviour for Kitty, xterm-compatible emulators, XFCE Terminal, SSH and tmux. Specifically verify mouse drag reporting, text selection conflicts, clipboard protocol support, bracketed paste, truecolour and terminal resize behaviour. Do not assume a framework supports all these correctly.
3. Produce a short architecture decision record (ADR) for language/framework. Rust or Go are plausible; choose on input handling, Unicode/grapheme layout, text-buffer editing, ecosystem maturity, ease of release and maintainability rather than trendiness.
4. Inspect the **actual sample** desktop colour file before finalising the parser, if it is available. Treat `~/.config/kitty/current-theme.conf` as the proposed initial path, **not as a confirmed, guaranteed path**. Make it configurable and validate supported syntax.
5. Research name conflicts separately before a public release. “Plica” is a working name, not a trademark clearance.
6. Document unresolved constraints and break phases into reviewable PRs. Do not attempt to implement the entire plan in one patch.

## 3. Architecture

Suggested modules (names illustrative, not prescribed):

- `app`: application state, commands, startup/shutdown lifecycle.
- `buffer`: Unicode text model, positions, selections, transactions, undo/redo, dirty state.
- `document`: file identity, encoding, line endings, file watcher, reload and save.
- `layout`: screen cells, grapheme widths, wrap, viewport mapping and hit-testing.
- `input`: keyboard shortcuts, terminal mouse events, wheel, paste, resize, focus.
- `ui`: editor, tab bar, menu, find bar, dialogs, status bar, optional panels.
- `tables`: recognisers, logical cell/row model, edit transactions and serialisers.
- `highlight`: exact-word/phrase matcher, groups and rendering overlays.
- `theme`: palette parsers, semantic role mapping, contrast checks, file watching.
- `config`: schema, validation, migration and user preferences.
- `recovery`: autosave snapshots and crash/session recovery.

**Architectural rule:** The canonical document model is text. Tables and highlights are *derived views/operations* over the underlying text. Keep rendering, disk representation and input handling cleanly separated. Never make the text buffer dependent on one table format or terminal framework.

### 3.1 Text engine

- Use a suitable rope/piece-table/gap-buffer implementation after benchmarking realistic files. Do not default to a naive whole-file string for every edit.
- Define offset conventions precisely: byte offsets for IO, Unicode grapheme clusters for cursor motion, terminal display-cell widths for layout. Never equate these three.
- Handle combining marks, emoji, double-width characters, tabs, CRLF/LF, Unicode normalization without silently rewriting content, and selection across wrapped lines.
- Support one logical undo transaction per user action. Typing may coalesce sensibly; table changes must undo atomically.
- Render incrementally on input; don't scan or repaint the whole document for ordinary edits.

### 3.2 Input and clipboard policy

- Conventional defaults: Ctrl+S save, Ctrl+Shift+S Save As, Ctrl+O open, Ctrl+N new, Ctrl+F find, Ctrl+H replace, Ctrl+Z undo, Ctrl+Y or Ctrl+Shift+Z redo, Ctrl+C/X/V clipboard, Ctrl+A select all, Ctrl+W close tab, Ctrl+Tab cycle tabs.
- Resolve conflicts with terminal-reserved chords empirically. Offer configurable bindings and a discoverable menu/shortcut list; allow alternatives.
- Support click-to-place-caret, mouse drag selection, Shift-click extend selection, wheel scrolling, clickable tabs, menus and scrollbars where appropriate.
- **Terminal limitation:** applications with mouse reporting enabled typically take mouse selection away from the terminal itself. Provide an intuitive temporary bypass / setting and document terminal-specific Shift-selection behaviour.
- Clipboard: integrate OSC 52 when appropriate and system clipboard integration where available. Support primary selection as an optional Linux feature. Always provide an internal clipboard fallback; avoid silently losing copied text across applications.
- Bracketed paste must insert literal text (no accidental command/shortcut execution); large paste remains performant and one undo step.
- On unsupported terminal capabilities, fall back safely and state limitations clearly.

## 4. Editing behaviour and basic UI

### 4.1 Main screen

- Single or multiple document tabs, editor viewport, lightweight status bar, optional top menu/toolbar or command palette.
- Status: file name, modified marker, line/column, selection count, encoding, line ending, current mode (normal/table), save or conflict warnings.
- Useful editor affordances: line numbers toggle, soft wrap toggle, whitespace visibility toggle, indentation controls, go-to-line, find/replace, search next/previous.
- No modal editing mode for normal text. Context-specific table navigation should be clear and escapable.
- Support opening files from arguments, including `plica file.txt`, multiple files, and ideally `plica +42 file.txt` if specified later.

### 4.2 File safety

- Never overwrite an externally modified file without warning and explicit resolution (compare/reload/overwrite/save copy).
- Atomic saves (temporary file in same directory + fsync/rename as appropriate); preserve permissions where feasible and avoid symlink surprises. Verify ownership/ACL and special-file behaviour.
- Detect text encoding; guarantee UTF-8; define how non-UTF-8 inputs are handled, with no silent corruption. Preserve existing LF/CRLF unless changed explicitly.
- Prompt on dirty tab close, quitting with unsaved buffers, and replace-all operations when appropriate.
- Backup/recovery snapshots are stored in application state, **not written into the source document**. Support recovery after crash and clearing stale snapshots.
- Do not use unsafe silent autosave to overwrite original files. Optional timed autosave should mean recovery snapshots by default.
- File watcher change notifications should be debounced and distinguish own saves from external changes.

## 5. Tables: core differentiator

### 5.1 Supported table formats

**MVP:** Pipe-delimited Markdown tables, e.g.:

```text
| Device     | Type      | Status |
|------------|-----------|--------|
| MPC One    | Sampler   | Active |
| Electribe 2| Groovebox | Active |
```

**Second step:** Plain-text aligned tables and optionally bordered Unicode/ASCII tables. Establish the exact grammar before coding. Avoid automatically interpreting arbitrary prose aligned with spaces as a table.

Both formats should use one logical `TableModel` (rows, columns, cell spans in source, formatting preferences) with separate parser/serializer adapters. The underlying file remains ordinary text.

### 5.2 Interactions

- Click within a recognised cell to edit its contents. Preserve normal caret and selection semantics inside the cell.
- Tab/Shift+Tab navigates cells when explicitly in table context; provide a reliable way to insert a literal tab or exit the table. Decide whether Tab on the last cell appends a row, and make this discoverable.
- Enter behaviour must be deliberate: prefer *new row* on Enter when navigating cells, with an explicit way to insert multiline content if the format supports it; for Markdown pipes, embedded newlines are not valid ordinary cells, so avoid pretending otherwise.
- Context actions: add/remove rows or columns, move row up/down, format/re-align, convert between supported table formats, select row/column/cell range.
- Typing longer values must reflow relevant column widths **without disturbing cell text**, selection or surrounding document.
- Undo/redo of row insertion, deletion, cell paste, conversion and realignment must be atomic.
- Copy selected cells as tab-separated data where appropriate; plain-text copy of selected source must remain available.
- Pasting TSV grid data into a table can expand rows/columns only with predictable rules. Treat pasted Markdown and multiline content carefully and never discard text.
- Preserve alignment style and delimiters on round-trip. Avoid gratuitously reformatting every table in a file on each edit.
- Provide an explicit command to format a table; auto-alignment can be enabled with a clear preference.

### 5.3 Edge cases and boundaries

- Escaped pipe characters and inline-code spans in Markdown, delimiter/alignment row syntax, empty cells, ragged rows, leading/trailing pipes, separator-row validity.
- Unicode display widths, emojis, tabs, combining characters and resize to a viewport narrower than a table.
- Horizontal scrolling for wide tables; soft wrap policy for cells must be documented and consistent.
- A table edit cannot accidentally alter adjacent prose or another table.
- Invalid/ambiguous structures degrade to ordinary text editing without data loss.
- Large tables should not trigger an O(document size) parse or entire-screen redraw for each keystroke.

### 5.4 Crucial design caveat

**Do not equate the on-screen cell layout with stored whitespace.** Maintain a logical table model, map logical cell boundaries to exact source spans, then serialize the smallest changed region. Selection and hit testing use rendered cell geometry; saving uses explicit source spans. This separation avoids a fragile terminal-only pseudo-spreadsheet design.

## 6. User-defined word and phrase highlighting

### 6.1 Behaviour

- Define named rule groups, each with matching terms and a chosen foreground/background and optional bold/underline.
- Support whole-word, case-insensitive and case-sensitive matching. Literal phrases should work. Regex is an advanced optional setting, safely bounded and disabled by default.
- Rules can be **global**, **file-type scoped**, and **document-specific**. Document-local rule data lives in separate config/state, never inside `.txt`/`.md` content unless users deliberately export it.
- Provide a preferences UI to manage groups, priority, enabled state and colours, with preview examples.
- Render matches immediately as a visual overlay. Define deterministic precedence for overlap with selection, search results, table headers, caret, diagnostics and active find match.
- Colours may be hard-coded RGB or linked to semantic palette slots, so they can follow the shared desktop theme.
- Keep matching incremental or viewport-focused. Do not perform unbounded regex evaluation on each typed character.

### 6.2 Initial visual rules

- Default sample groups could demonstrate `TODO`, `FIXME`, `IMPORTANT`, `DONE`, and `IDEA` but should be easy to disable/remove.
- Respect contrast and accessibility. If an accent is too faint on the editor background, apply a readable fallback or warning.
- Highlighting changes visual style only and never modifies file text.

## 7. Themes and external shared colour file

### 7.1 User requirement

Provide a Preferences option to select an **external colours file** so Plica follows the same desktop palette as Kitty and LatticeFM. The proposed initial example is:

```text
~/.config/kitty/current-theme.conf
```

This is a candidate example based on prior project discussions. The agent **must confirm the actual file format and path on the target system** and must never bake this path into the parser or assume all palettes follow one convention.

### 7.2 Theme source modes

1. **External colour file:** configurable path, live reload, format detection/selection, preview and diagnostics.
2. **Built-in theme:** start with a known palette such as Dracula; add Gruvbox, Solarized and Nord if licensing/colour values are confirmed.
3. **Custom Plica theme:** user-defined overrides for semantic roles.
4. Optional **Terminal default colours** mode for users who want the terminal to own foreground/background and ANSI colours.

Configuration should include: external path, enable live reload, theme source, optional colour-role overrides, high-contrast adjustment mode, and preferred table/highlight styles.

### 7.3 Parser and semantic colour roles

- Initial parser: Kitty-style `foreground`, `background`, `selection_foreground`, `selection_background`, `cursor`, and `color0`..`color15` where supplied; support comments and normal accepted whitespace.
- Missing optional fields should use sensible fallbacks. Invalid files must never crash the editor or replace the active palette with random colours.
- Treat ANSI slots as **colours**, not guaranteed meanings such as danger or success. Map to internal semantic roles (`editor_fg`, `editor_bg`, `muted`, `accent`, `selection`, `search_match`, `menu`, `status`, `table_border`, `highlight_group_N`) using documented rules and contrast checks.
- Keep parse, resolve, contrast, and terminal-render layers separate. Later Base16/Tinted support can be an adapter, not a new theme engine.
- Truecolour (`24-bit`) where supported; use terminal-appropriate fallbacks where unavailable.

### 7.4 Live update lifecycle

- Watch for write/rename/atomic replacement, including the **parent directory** so replacing a watched inode does not lose future updates.
- Debounce reloads. Validate new contents completely, then swap the active palette as one transaction and repaint only what is necessary.
- If temporarily missing or invalid, continue using last-known-good palette and show a non-disruptive warning in Preferences/status.
- When the file reappears or is repaired, update automatically.
- Re-resolve explicit semantic roles, tables and palette-linked word highlights on every valid palette change.
- Preview changes before applying Preferences. Remember that live changes to the selected *source file* are expected to apply automatically if live reload is enabled.

### 7.5 Terminal constraints

- Respect `NO_COLOR` if applicable to chosen mode, the user's terminal truecolour support and accessibility preferences.
- Track foreground/background restoration and mouse-mode teardown on abnormal exit as far as possible; the editor must never leave a terminal visibly broken after a routine shutdown.
- Keep fixed user colours fixed across theme changes, and resolve palette-linked colours by role rather than memorising old RGB values.

## 8. Preferences and configuration

- Provide a keyboard-accessible, mouse-friendly in-app Preferences dialog and a documented editable config file.
- Group settings into **Editor**, **Interaction**, **Tables**, **Highlights**, **Appearance**, **Files & Recovery**, and **Keybindings**. Avoid long explanatory paragraphs in UI; use concise hint text/help.
- Validate settings with helpful, non-destructive errors. Version the config schema and support migration.
- Use standard XDG paths for Linux: `$XDG_CONFIG_HOME/plica`, `$XDG_STATE_HOME/plica`, `$XDG_CACHE_HOME/plica` with documented fallbacks.
- **No mandatory Python runtime or environment management for end users.** Release should be straightforward to install/use.

## 9. Implementation phases and release gates

### Phase 0: Technical spike and ADR (no production implementation)

Deliver: capability matrix, input/clipboard proof of concept, language/framework decision, file palette parser sample, text-buffer strategy, initial architecture diagrams, and risks.

**Gate:** Demonstrate mouse drag selection, Unicode cursor hit testing, editing, terminal teardown and a live palette change in the chosen stack. Confirm any compromise to the UX before coding the full application.

### Phase 1: Reliable basic editor (target Plica 0.1.0)

Implement single-buffer editing, file open/save, undo/redo, keyboard movement and selection, mouse click/drag/wheel, word wrap, line numbers, find, conventional shortcuts, basic status bar, UTF-8, LF/CRLF, resize handling and Dracula built-in theme.

**Gate:** Regular `.txt` files edit safely with keyboard and mouse and survive common failure scenarios. Real-world documents of at least several MB remain responsive. No table or highlight feature is needed for this gate.

### Phase 2: Application polish and data safety (target 0.2.0)

Implement tabs, Save As, find/replace, clipboard integrations and fallbacks, Preferences scaffolding, external change warnings, snapshot recovery, larger-file tests, resilient config, and terminal fallback handling.

**Gate:** Workflows feel like a lightweight graphical editor rather than a terminal demo; data safety and clipboard behavior validated on Kitty and at least one other terminal.

### Phase 3: Shared colours and highlighting (target 0.3.0)

Implement external Kitty-colour file parser, live watcher including atomic replacements, semantic roles, contrast checks, theme preview, global highlight groups, file/document scopes and palette-linked colour rules.

**Gate:** Switching the common desktop colour file updates *every* Plica interface region and linked highlight while open; corrupted or removed files do not break the session.

### Phase 4: Markdown tables (target 0.4.0)

Build logical parser/source mappings, format command, cell hit testing, editing, keyboard navigation, row/column actions, paste rules and atomic undo. Ensure normal text editing remains unchanged outside recognised tables.

**Gate:** All table invariants and round-trip preservation tests pass; Markdown tables remain readable in ordinary text editors.

### Phase 5: Plain-text tables and UX polish (target 0.5.0)

Add explicitly formatted plain-text/ASCII or Unicode-border tables using the same logical engine, conversion commands, cell selection enhancements, useful menus, tutorials and profiling.

**Gate:** Plain-text and Markdown tables share editing behaviour and are portable. No false-positive conversion of arbitrary aligned prose.

### Phase 6: Public beta / v1.0 readiness

Package binaries and installation instructions, write README/man page/keymap cheat sheet, validate accessibility and mouse interactions, investigate licensing/name conflicts, regression-test across supported terminals and distros, and document supported/unsupported file sizes/formats.

**Gate:** Complete manual regression suite, automated tests and crash recovery drills. No known data-loss issues. Versioned release notes.

**Version convention:** Version numbers for application and plan are independent. Every code/document revision must update its own version or explicitly record a revision/changelog entry. The release labels above are proposals, not promises.

## 10. Automated tests

### Text editing

- UTF-8 graphemes, combining characters, CJK wide glyphs, emoji sequences, tabs, wrapped-line movement and hit-testing.
- Undo coalescing, selection replacement, multi-line paste, large paste as one transaction, Ctrl+C/X/V behavior, and out-of-range cursor prevention.
- Files with LF, CRLF, missing final newline, empty files, long lines, different permissions, symlink paths, and external modifications.

### Mouse and terminal

- Click-to-caret, drag over wrap boundaries, drag outside viewport (autoscroll), wheel behavior, resize, selection outside window, text-versus-terminal selection conflict.
- Alt/Shift modifiers, terminal mode activation/deactivation, tmux/SSH compatibility claims and clipboard fallbacks.

### Tables

- Parse/serialize round trip; text before/after untouched; escaped Markdown pipes; empty/ragged rows; Unicode widths; huge or narrow tables.
- Edit cell, paste grid, insert/delete row/column, format, convert, undo/redo, selection restoration after reflow, Save and reopen.
- Fuzz table parsing/formatting with malformed input. Formatting never loses cell contents.

### Highlight/theme

- Whole word/case/phrase boundaries, overlap precedence, highlight matches near edits, palette role reload, fixed vs linked colours.
- Atomic palette replacement, permission error, truncated file, invalid hex, deleted file, restored file, rapid theme switching, symlinked theme path.
- Sufficient contrast for caret/selection/search and highlighted tokens across representative dark/light palettes.

### Reliability/performance

- Repeated reopen/close, big undo stacks, long sessions, memory ceiling and incremental rendering behavior.
- Save interruption and crash recovery simulation, damaged recovery snapshot handling, ambiguous external changes, temporary directory cleanup.

## 11. Manual acceptance scenarios

1. From Kitty, run `plica notes.txt`, type immediately, mouse-select text, copy it into another application, paste back, undo, save and quit.
2. Open multiple files; switch with mouse and Ctrl+Tab, close a dirty tab and get a safe prompt.
3. Search for a phrase, replace matches, and preserve content across save/reload.
4. Select external palette source in Preferences. Change the shared theme using the existing theme workflow while Plica stays open. Plica visually updates, including selected text, menus, table borders, status bar and linked highlights.
5. Temporarily write an invalid palette file. Plica retains last-known-good colours and recovers once valid file content returns.
6. Configure an `IMPORTANT` group and a multiword phrase group. Test whole-word and case sensitivity; confirm no text content is modified.
7. Edit a Markdown table cell to a much longer string. Its column realigns, neighbouring data stays intact, and one Undo restores the previous structure.
8. Paste a rectangular TSV selection into a table, then copy a cell range out. Verify exact values, file contents and reopen behaviour.
9. Reduce terminal width below the table width. Horizontal navigation remains usable and cell content is not corrupted.
10. Kill Plica after making edits and restart. A recovery option restores the buffer without overwriting the original silently.
11. Modify an open file externally. The application prevents a silent destructive overwrite.
12. Open a file containing emoji/CJK/combining accents and verify cursor, selection, mouse clicks and table alignment.

## 12. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Terminal mouse protocol cannot emulate GUI selection perfectly | Early capability spike, graceful fallback, documented selection escape hatch |
| Table geometry and underlying text diverge | Central source-span mapping, derived logical table model, serializer tests |
| Reformatting changes unrelated text | Localized text transactions; explicit format command; round-trip tests |
| Unicode breaks columns/caret hit testing | Grapheme-aware buffer plus independent display-width mapping |
| Palette syntax changes or theme file replaced atomically | Pluggable parser, directory watcher, last-known-good palette |
| Accidental file loss on save/recovery | Atomic saves, external-change checks, recovery snapshots and fault injection |
| Scope expands into an IDE or spreadsheet | Enforce non-goals and per-phase release gates |
| CLI shortcuts conflict with terminal | Shortcut capability testing and user-remappable alternatives |

## 13. Deliverables expected from the coding agent

For each phase, provide:

1. Brief architectural notes and any changed design decisions.
2. A small, reviewable implementation with a clear version bump.
3. Automated tests and exact commands to run them.
4. Manual acceptance walkthrough for mouse/keyboard and regression-sensitive behaviour.
5. Changelog and known limitations.
6. A short list of decisions requiring user judgement, only when genuinely necessary.

**Initial agent assignment:** Start with **Phase 0 only**, inspect the repository and actual user colour-file sample, verify the terminal input/rendering approach, then present the ADR and implementation slices for review. Do not prematurely lock in a language, TUI library, table parser or clipboard mechanism before the spike.

## 14. Open design decisions to resolve in Phase 0/early implementation

- Language/framework choice and whether the UI toolkit can implement the required mouse-selection model.
- Exact location and grammar of the user's shared palette file and whether Base16 imports belong in the first or a later phase.
- Clipboard and primary-selection policy on Wayland versus X11, remote SSH and tmux.
- Default table Enter/Tab behaviour and how to exit table navigation or insert literal separators.
- Plain-text table grammar and whether ASCII/Unicode-bordered forms both ship.
- Whether document-specific highlighting is mapped by canonical path, content fingerprint or explicit per-document sidecar configuration.
- File encoding breadth for the first release, with a firm promise of lossless handling or read-only warnings where needed.

---

*Plan version 0.1.0. Prepared as an implementation brief for a coding agent.*
