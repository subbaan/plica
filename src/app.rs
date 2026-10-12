//! Application state and command handling (single document for now).

use crate::buffer::{Buffer, EditKind, Selection};
use crate::clipboard::Clipboard;
use crate::document::{self, Document};
use crate::drop;
use crate::layout::{LayoutOpts, VisualRow, layout_line, row_index_for};
use crate::term::{self, Capabilities};
use crate::theme::{self, Theme};
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const SCROLL_STEP: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeSource {
    Builtin,
    /// Palette file; `last_error` is set while the file is unusable and the
    /// last-known-good palette remains active.
    File { path: PathBuf, last_error: Option<String> },
    /// NO_COLOR / terminal default colours.
    TerminalDefault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    Find,
    GotoLine,
    SaveAs,
    Open,
}

pub struct Prompt {
    pub kind: PromptKind,
    pub input: String,
}

impl PromptKind {
    pub fn label(self) -> &'static str {
        match self {
            PromptKind::Find => "Find",
            PromptKind::GotoLine => "Go to line",
            PromptKind::SaveAs => "Save as",
            PromptKind::Open => "Open",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragUnit {
    Char,
    Word,
    Line,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Save,
    Wrap,
    Mouse,
    Help,
}

/// Screen regions recorded at render time, used for mouse hit-testing.
#[derive(Default)]
pub struct Regions {
    pub text: Rect,
    pub gutter: Rect,
    pub scrollbar: Rect,
    pub buttons: Vec<(Rect, Button)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Quit,
    OverwriteExternal,
}

pub struct App {
    pub buf: Buffer,
    pub doc: Document,
    pub clipboard: Clipboard,
    pub theme: Theme,
    pub theme_source: ThemeSource,
    pub caps: Capabilities,
    pub wrap: bool,
    pub line_numbers: bool,
    pub tab_width: usize,
    /// What Tab inserts / indents with: "\t" or N spaces, detected per file.
    pub indent_unit: String,
    pub show_whitespace: bool,
    pub mouse_enabled: bool,
    pub show_help: bool,
    pub prompt: Option<Prompt>,
    pub last_find: String,
    pub status: Option<(String, Level, Instant)>,
    pub quit: bool,

    /// Viewport: first visible visual row is row `top_row` of line `top_line`.
    pub top_line: usize,
    pub top_row: usize,
    pub scroll_x: usize,
    /// Rows exactly as last rendered (for mouse hit-testing).
    pub rendered_rows: Vec<VisualRow>,
    pub regions: Regions,
    /// Set when the caret moved and the viewport must follow it.
    pub follow_caret: bool,

    drag: Option<(DragUnit, Selection)>,
    dragging_scrollbar: bool,
    last_mouse: (u16, u16),
    last_click: Option<(Instant, u16, u16, u8)>,
    pending: Option<Pending>,
}

impl App {
    pub fn new(buf: Buffer, doc: Document, theme: Theme, theme_source: ThemeSource, caps: Capabilities) -> Self {
        let mut app = Self {
            buf,
            doc,
            clipboard: Clipboard::default(),
            theme,
            theme_source,
            caps,
            wrap: true,
            line_numbers: true,
            tab_width: 4,
            indent_unit: String::new(),
            show_whitespace: false,
            mouse_enabled: true,
            show_help: false,
            prompt: None,
            last_find: String::new(),
            status: None,
            quit: false,
            top_line: 0,
            top_row: 0,
            scroll_x: 0,
            rendered_rows: Vec::new(),
            regions: Regions::default(),
            follow_caret: true,
            drag: None,
            dragging_scrollbar: false,
            last_mouse: (0, 0),
            last_click: None,
            pending: None,
        };
        app.indent_unit = detect_indent(&app.buf, app.tab_width);
        app
    }

    pub fn info(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Level::Info, Instant::now()));
    }

    pub fn warn(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Level::Warn, Instant::now()));
    }

    pub fn layout_opts(&self) -> LayoutOpts {
        let w = self.regions.text.width.max(1) as usize;
        LayoutOpts { tab_width: self.tab_width, wrap_width: self.wrap.then_some(w) }
    }

    pub fn rows_of(&self, line: usize) -> Vec<VisualRow> {
        layout_line(self.buf.rope(), line, self.layout_opts())
    }

    // ---- palette ---------------------------------------------------------------

    /// Re-read the palette file. On failure keep the current (last-known-good)
    /// theme and record why.
    pub fn reload_palette(&mut self) {
        let ThemeSource::File { path, .. } = &self.theme_source else { return };
        let path = path.clone();
        let result = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| theme::parse_kitty(&t).map_err(|e| e.to_string()));
        match result {
            Ok(report) => {
                self.theme = theme::resolve(&report.palette);
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if report.warnings.is_empty() {
                    self.info(format!("Palette reloaded from {name}"));
                } else {
                    self.warn(format!("Palette reloaded with {} warning(s): {}", report.warnings.len(), report.warnings[0]));
                }
                self.theme_source = ThemeSource::File { path, last_error: None };
            }
            Err(e) => {
                self.warn(format!("Palette unusable ({e}); keeping previous colours"));
                self.theme_source = ThemeSource::File { path, last_error: Some(e) };
            }
        }
    }

    // ---- events ------------------------------------------------------------------

    pub fn handle_event(&mut self, ev: Event) {
        match ev {
            Event::Key(k) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => self.on_key(k),
            Event::Mouse(m) => self.on_mouse(m),
            Event::Paste(text) => {
                // Terminals deliver a file drag-and-drop as a paste of paths.
                let dropped = drop::parse_dropped_paths(&text);
                if let Some(p) = &mut self.prompt {
                    match (dropped, p.kind) {
                        (Some(paths), PromptKind::Open | PromptKind::SaveAs) => {
                            p.input = paths[0].display().to_string();
                        }
                        _ => p.input.push_str(&text.replace(['\r', '\n'], " ")),
                    }
                    if p.kind == PromptKind::Find {
                        self.find_incremental();
                    }
                } else if let Some(paths) = dropped {
                    self.open_dropped(&paths);
                } else {
                    self.paste_text(&text);
                }
            }
            Event::FocusGained if self.doc.changed_on_disk() => {
                self.warn("File changed on disk since it was opened/saved");
            }
            _ => {}
        }
    }

    /// Called regularly from the main loop (drag autoscroll).
    pub fn tick(&mut self) -> bool {
        if self.drag.is_some() {
            let (x, y) = self.last_mouse;
            let t = self.regions.text;
            if y < t.y || y >= t.y + t.height {
                self.on_drag(x, y);
                return true;
            }
        }
        false
    }

    fn on_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);

        // Any key other than the confirming one cancels a pending confirmation.
        let pending = self.pending.take();

        if self.show_help {
            self.show_help = false;
            return;
        }
        if self.prompt.is_some() {
            self.on_prompt_key(k);
            return;
        }

        let lower = match k.code {
            KeyCode::Char(c) => Some(c.to_ascii_lowercase()),
            _ => None,
        };
        match (k.code, lower) {
            // -- file / app --
            (_, Some('s')) if ctrl && shift => self.open_prompt(PromptKind::SaveAs),
            (KeyCode::F(12), _) => self.open_prompt(PromptKind::SaveAs),
            (_, Some('s')) if ctrl => self.save(pending == Some(Pending::OverwriteExternal)),
            (_, Some('o')) if ctrl => {
                if self.buf.is_dirty() {
                    self.warn("Save or undo changes before opening another file (tabs arrive in 0.2)");
                } else {
                    self.open_prompt(PromptKind::Open);
                }
            }
            (_, Some('q')) | (_, Some('w')) if ctrl => {
                if self.buf.is_dirty() && pending != Some(Pending::Quit) {
                    self.pending = Some(Pending::Quit);
                    self.warn("Unsaved changes! Press again to quit without saving, or Ctrl+S to save");
                } else {
                    self.quit = true;
                }
            }
            (_, Some('n')) if ctrl => {
                if self.buf.is_dirty() {
                    self.warn("Save or undo changes before starting a new file (tabs arrive in 0.2)");
                } else {
                    self.buf = Buffer::new();
                    self.doc = Document::untitled();
                    self.indent_unit = detect_indent(&self.buf, self.tab_width);
                    self.info("New file");
                }
            }
            (KeyCode::F(1), _) => self.show_help = true,
            (_, Some('w')) if alt => {
                self.show_whitespace = !self.show_whitespace;
                self.info(if self.show_whitespace { "Whitespace shown" } else { "Whitespace hidden" });
            }
            (_, Some('m')) if alt => self.toggle_mouse(),
            (_, Some('z')) if alt => self.toggle_wrap(),
            (_, Some('l')) if alt => self.line_numbers = !self.line_numbers,

            // -- edit --
            (_, Some('z')) if ctrl && shift => self.redo(),
            (_, Some('z')) if ctrl => self.undo(),
            (_, Some('y')) if ctrl => self.redo(),
            (_, Some('c')) if ctrl => self.copy(),
            (_, Some('x')) if ctrl => self.cut(),
            (_, Some('v')) if ctrl => {
                let t = self.clipboard.paste();
                self.paste_text(&t);
            }
            (_, Some('a')) if ctrl => {
                self.buf.sel = Selection { anchor: 0, head: self.buf.len_chars() };
                self.buf.break_undo_group();
            }
            (_, Some('f')) if ctrl => self.open_prompt(PromptKind::Find),
            (_, Some('g')) if ctrl => self.open_prompt(PromptKind::GotoLine),
            (KeyCode::F(3), _) => self.find_next(!shift),

            (KeyCode::Esc, _) => {
                self.buf.sel = Selection::caret(self.buf.sel.head);
            }
            (KeyCode::Enter, _) => self.newline(),
            (KeyCode::Tab, _) if shift => self.outdent(),
            (KeyCode::Tab, _) => self.tab(),
            (KeyCode::BackTab, _) => self.outdent(),
            (KeyCode::Backspace, _) => {
                let h = self.buf.sel.head;
                let from = if ctrl || alt { self.buf.word_left(h) } else { self.buf.prev_grapheme(h) };
                self.buf.delete((from, h), EditKind::Deleting);
            }
            (KeyCode::Delete, _) => {
                let h = self.buf.sel.head;
                let to = if ctrl || alt { self.buf.word_right(h) } else { self.buf.next_grapheme(h) };
                self.buf.delete((h, to), EditKind::Deleting);
            }
            (KeyCode::Char(c), _) if !ctrl && !alt => {
                let mut s = [0u8; 4];
                self.buf.insert(c.encode_utf8(&mut s), EditKind::Typing);
            }

            // -- motion --
            (KeyCode::Left, _) => self.move_horiz(false, ctrl || alt, shift),
            (KeyCode::Right, _) => self.move_horiz(true, ctrl || alt, shift),
            // Ctrl+Up/Down scroll the view without moving the caret.
            (KeyCode::Up, _) if ctrl && !shift => return self.scroll_rows(-1),
            (KeyCode::Down, _) if ctrl && !shift => return self.scroll_rows(1),
            (KeyCode::Up, _) => self.move_vert(-1, shift),
            (KeyCode::Down, _) => self.move_vert(1, shift),
            (KeyCode::PageUp, _) => self.move_vert(-(self.page_rows() as isize), shift),
            (KeyCode::PageDown, _) => self.move_vert(self.page_rows() as isize, shift),
            (KeyCode::Home, _) => {
                let to = if ctrl { 0 } else { self.smart_home() };
                self.move_to(to, shift);
            }
            (KeyCode::End, _) => {
                let to = if ctrl { self.buf.len_chars() } else { self.visual_row_end() };
                self.move_to(to, shift);
            }
            _ => return,
        }
        self.follow_caret = true;
    }

    fn page_rows(&self) -> usize {
        (self.regions.text.height as usize).saturating_sub(1).max(1)
    }

    fn move_to(&mut self, pos: usize, extend: bool) {
        let pos = self.buf.snap(pos);
        if extend {
            self.buf.sel.head = pos;
        } else {
            self.buf.sel = Selection::caret(pos);
        }
        self.buf.preferred_col = None;
        self.buf.break_undo_group();
    }

    fn move_horiz(&mut self, right: bool, word: bool, extend: bool) {
        let sel = self.buf.sel;
        // Collapsing a selection with plain Left/Right goes to its edge.
        if !extend && !sel.is_empty() && !word {
            self.move_to(if right { sel.end() } else { sel.start() }, false);
            return;
        }
        let h = sel.head;
        let to = match (right, word) {
            (true, false) => self.buf.next_grapheme(h),
            (false, false) => self.buf.prev_grapheme(h),
            (true, true) => self.buf.word_right(h),
            (false, true) => self.buf.word_left(h),
        };
        self.move_to(to, extend);
    }

    fn move_vert(&mut self, delta: isize, extend: bool) {
        let head = self.buf.sel.head;
        let line = self.buf.line_of(head);
        let rows = self.rows_of(line);
        let idx = row_index_for(&rows, head);
        let col = self.buf.preferred_col.unwrap_or_else(|| rows[idx].col_of(head));

        // Walk `delta` visual rows across line boundaries.
        let (mut line, mut rows, mut idx) = (line, rows, idx as isize);
        let mut remaining = delta;
        while remaining != 0 {
            let step = remaining.signum();
            let next = idx + step;
            if next >= 0 && (next as usize) < rows.len() {
                idx = next;
            } else if step < 0 && line > 0 {
                line -= 1;
                rows = self.rows_of(line);
                idx = rows.len() as isize - 1;
            } else if step > 0 && line + 1 < self.buf.len_lines() {
                line += 1;
                rows = self.rows_of(line);
                idx = 0;
            } else {
                // Top/bottom of document: go to start/end like GUI editors.
                let to = if step < 0 { 0 } else { self.buf.len_chars() };
                self.move_to(to, extend);
                return;
            }
            remaining -= step;
        }
        let to = rows[idx as usize].pos_at_col(col);
        self.move_to(to, extend);
        self.buf.preferred_col = Some(col);
    }

    /// Home toggles between first non-blank char and the start of the row.
    fn smart_home(&self) -> usize {
        let head = self.buf.sel.head;
        let line = self.buf.line_of(head);
        let rows = self.rows_of(line);
        let row = &rows[row_index_for(&rows, head)];
        if row.row_in_line > 0 {
            return row.start;
        }
        let start = self.buf.line_start(line);
        let end = self.buf.line_end(line);
        let rope = self.buf.rope();
        let mut first = start;
        while first < end && matches!(rope.char(first), ' ' | '\t') {
            first += 1;
        }
        if head == first { start } else { first }
    }

    fn visual_row_end(&self) -> usize {
        let head = self.buf.sel.head;
        let rows = self.rows_of(self.buf.line_of(head));
        let row = &rows[row_index_for(&rows, head)];
        if row.is_last { row.end } else { row.glyphs.last().map_or(row.start, |g| g.start) }
    }

    /// Lines touched by the selection (a selection ending at column 0 does
    /// not include that last line).
    fn selected_lines(&self) -> std::ops::RangeInclusive<usize> {
        let sel = self.buf.sel;
        let first = self.buf.line_of(sel.start());
        let mut last = self.buf.line_of(sel.end());
        if last > first && self.buf.line_start(last) == sel.end() {
            last -= 1;
        }
        first..=last
    }

    fn tab(&mut self) {
        let lines = self.selected_lines();
        if lines.start() != lines.end() {
            // Multi-line selection: indent every non-blank line.
            let unit = self.indent_unit.clone();
            let ops = lines
                .filter(|&l| self.buf.line_end(l) > self.buf.line_start(l))
                .map(|l| (self.buf.line_start(l), 0, unit.clone()))
                .collect();
            self.buf.transact(ops);
            return;
        }
        if self.indent_unit == "\t" {
            self.buf.insert("\t", EditKind::Typing);
        } else {
            // Spaces up to the next tab stop.
            let start = self.buf.sel.start();
            let rows = self.rows_of(self.buf.line_of(start));
            let col = rows[row_index_for(&rows, start)].col_of(start);
            let n = self.tab_width - col % self.tab_width;
            self.buf.insert(&" ".repeat(n), EditKind::Typing);
        }
    }

    fn outdent(&mut self) {
        let rope = self.buf.rope();
        let mut ops = Vec::new();
        for l in self.selected_lines() {
            let start = self.buf.line_start(l);
            let end = self.buf.line_end(l);
            let n = if start < end && rope.char(start) == '\t' {
                1
            } else {
                (start..end.min(start + self.tab_width)).take_while(|&p| rope.char(p) == ' ').count()
            };
            if n > 0 {
                ops.push((start, n, String::new()));
            }
        }
        if !ops.is_empty() {
            self.buf.transact(ops);
        }
    }

    fn newline(&mut self) {
        // Auto-indent: copy the current line's leading whitespace.
        let head = self.buf.sel.start();
        let line = self.buf.line_of(head);
        let start = self.buf.line_start(line);
        let rope = self.buf.rope();
        let mut indent = String::new();
        let mut p = start;
        while p < head && matches!(rope.char(p), ' ' | '\t') {
            indent.push(rope.char(p));
            p += 1;
        }
        let text = format!("{}{}", self.doc.line_ending.as_str(), indent);
        self.buf.insert(&text, EditKind::Other);
    }

    fn undo(&mut self) {
        if !self.buf.undo() {
            self.info("Nothing to undo");
        }
    }

    fn redo(&mut self) {
        if !self.buf.redo() {
            self.info("Nothing to redo");
        }
    }

    fn copy(&mut self) {
        if self.buf.sel.is_empty() {
            return;
        }
        let text = self.buf.selected_text();
        let dest = self.clipboard.copy(&text);
        self.info(format!("Copied {} chars to {dest}", text.chars().count()));
    }

    fn cut(&mut self) {
        if self.buf.sel.is_empty() {
            return;
        }
        let text = self.buf.selected_text();
        self.clipboard.copy(&text);
        self.buf.delete((0, 0), EditKind::Other);
    }

    /// Insert pasted text literally as one undo step, adopting the document's
    /// line ending (terminals often deliver newlines as CR).
    fn paste_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let normalized = if self.doc.line_ending.as_str() == "\n" {
            normalized
        } else {
            normalized.replace('\n', self.doc.line_ending.as_str())
        };
        self.buf.insert(&normalized, EditKind::Other);
        self.buf.break_undo_group();
    }

    fn toggle_wrap(&mut self) {
        self.wrap = !self.wrap;
        self.top_row = 0;
        self.scroll_x = 0;
        self.info(if self.wrap { "Soft wrap on" } else { "Soft wrap off" });
    }

    fn toggle_mouse(&mut self) {
        self.mouse_enabled = !self.mouse_enabled;
        if term::set_mouse(self.mouse_enabled).is_err() {
            self.warn("Could not change mouse mode");
            return;
        }
        if self.mouse_enabled {
            self.info("Mouse: Plica (Alt+M to give the mouse to the terminal)");
        } else {
            self.warn("Mouse: terminal selection mode. Alt+M to return mouse to Plica");
        }
    }

    // ---- saving --------------------------------------------------------------------

    fn save(&mut self, force: bool) {
        if self.doc.path.is_none() {
            self.open_prompt(PromptKind::SaveAs);
            return;
        }
        if !force && self.doc.changed_on_disk() {
            self.pending = Some(Pending::OverwriteExternal);
            self.warn("File changed on disk! Ctrl+S again to overwrite it, F12 to save a copy");
            return;
        }
        let text = self.buf.rope().to_string();
        match self.doc.save(&text) {
            Ok(()) => {
                self.buf.mark_saved();
                self.info(format!("Saved {}", self.doc.display_name()));
            }
            Err(e) => self.warn(format!("Save failed: {e:#}")),
        }
    }

    fn save_as(&mut self, path: &str) {
        let path = expand_tilde(path);
        if path.exists() {
            // Keep it simple and safe for now: never silently clobber.
            self.warn(format!("{} exists; choose another name (overwrite prompt arrives in 0.2)", path.display()));
            return;
        }
        let old = self.doc.path.replace(path);
        let old_stamp = self.doc.stamp.take();
        let text = self.buf.rope().to_string();
        match self.doc.save(&text) {
            Ok(()) => {
                self.buf.mark_saved();
                self.info(format!("Saved as {}", self.doc.display_name()));
            }
            Err(e) => {
                self.doc.path = old;
                self.doc.stamp = old_stamp;
                self.warn(format!("Save failed: {e:#}"));
            }
        }
    }

    fn open_path(&mut self, path: &str) {
        self.open_file(&expand_tilde(path));
    }

    /// Replace the (clean) current buffer with a file. Returns true on success.
    fn open_file(&mut self, path: &Path) -> bool {
        match document::load(path) {
            Ok(l) => {
                self.buf = Buffer::from_str(&l.text);
                self.doc = l.doc;
                self.indent_unit = detect_indent(&self.buf, self.tab_width);
                self.top_line = 0;
                self.top_row = 0;
                self.scroll_x = 0;
                self.follow_caret = true;
                let name = self.doc.display_name();
                self.info(if l.is_new { format!("New file {name}") } else { format!("Opened {name}") });
                if self.doc.mixed_line_endings {
                    self.warn("File has mixed line endings; they are preserved as-is");
                }
                true
            }
            Err(e) => {
                self.warn(format!("{e:#}"));
                false
            }
        }
    }

    /// Files dropped onto the terminal window.
    fn open_dropped(&mut self, paths: &[PathBuf]) {
        let first = &paths[0];
        if first.is_dir() {
            // Let the user pick a file inside the dropped folder.
            let mut input = first.display().to_string();
            if !input.ends_with('/') {
                input.push('/');
            }
            self.prompt = Some(Prompt { kind: PromptKind::Open, input });
            return;
        }
        if self.buf.is_dirty() {
            self.warn(format!(
                "Unsaved changes: save (Ctrl+S) then drop {} again. Tabs in 0.2 will open it alongside",
                first.file_name().map_or_else(|| first.display().to_string(), |n| n.to_string_lossy().into_owned())
            ));
            return;
        }
        if self.open_file(first) && paths.len() > 1 {
            self.info(format!(
                "Opened {} ({} more dropped; multiple files open in tabs from 0.2)",
                self.doc.display_name(),
                paths.len() - 1
            ));
        }
    }

    // ---- prompt / find ---------------------------------------------------------------

    fn open_prompt(&mut self, kind: PromptKind) {
        let input = match kind {
            PromptKind::Find => {
                let sel = self.buf.selected_text();
                if !sel.is_empty() && !sel.contains('\n') { sel } else { self.last_find.clone() }
            }
            PromptKind::SaveAs => self.doc.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
            _ => String::new(),
        };
        self.prompt = Some(Prompt { kind, input });
    }

    fn on_prompt_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let Some(p) = &mut self.prompt else { return };
        let kind = p.kind;
        match k.code {
            KeyCode::Esc => {
                self.prompt = None;
            }
            KeyCode::Enter => {
                let input = p.input.clone();
                match kind {
                    PromptKind::Find => {
                        self.last_find = input;
                        self.find_next(!shift);
                        return; // keep the bar open for repeated Enter
                    }
                    PromptKind::GotoLine => self.goto_line(&input),
                    PromptKind::SaveAs if !input.is_empty() => self.save_as(&input),
                    PromptKind::Open if !input.is_empty() => self.open_path(&input),
                    _ => {}
                }
                self.prompt = None;
                self.follow_caret = true;
            }
            KeyCode::Backspace => {
                p.input.pop();
                if kind == PromptKind::Find {
                    self.find_incremental();
                }
            }
            KeyCode::F(3) => {
                self.last_find = p.input.clone();
                self.find_next(!shift);
            }
            KeyCode::Char('v') if ctrl => {
                let t = self.clipboard.paste();
                if let Some(p) = &mut self.prompt {
                    p.input.push_str(&t.replace(['\r', '\n'], " "));
                }
            }
            KeyCode::Char(c) if !ctrl => {
                p.input.push(c);
                if kind == PromptKind::Find {
                    self.find_incremental();
                }
            }
            _ => {}
        }
    }

    fn goto_line(&mut self, input: &str) {
        match input.trim().parse::<usize>() {
            Ok(n) if n >= 1 => {
                let line = (n - 1).min(self.buf.len_lines() - 1);
                let pos = self.buf.line_start(line);
                self.move_to(pos, false);
            }
            _ => self.warn("Not a line number"),
        }
    }

    fn find_incremental(&mut self) {
        let Some(p) = &self.prompt else { return };
        self.last_find = p.input.clone();
        // Search from the start of the current selection so typing extends the match.
        let from = self.buf.sel.start();
        if let Some((s, e)) = find_in(&self.buf, &self.last_find, from, true) {
            self.buf.sel = Selection { anchor: s, head: e };
            self.follow_caret = true;
        }
    }

    pub fn find_next(&mut self, forward: bool) {
        if self.last_find.is_empty() {
            self.open_prompt(PromptKind::Find);
            return;
        }
        let from = if forward { self.buf.sel.end() } else { self.buf.sel.start() };
        match find_in(&self.buf, &self.last_find, from, forward) {
            Some((s, e)) => {
                if (forward && s < from) || (!forward && s >= from) {
                    self.info("Search wrapped");
                }
                self.buf.sel = Selection { anchor: s, head: e };
                self.follow_caret = true;
            }
            None => self.warn(format!("Not found: {}", self.last_find)),
        }
    }

    // ---- mouse -------------------------------------------------------------------------

    fn on_mouse(&mut self, m: MouseEvent) {
        self.last_mouse = (m.column, m.row);
        let alt = m.modifiers.contains(KeyModifiers::ALT);
        let shift = m.modifiers.contains(KeyModifiers::SHIFT);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.pending = None;
                if self.show_help {
                    self.show_help = false;
                    return;
                }
                if let Some(b) = self.button_at(m.column, m.row) {
                    self.press_button(b);
                    return;
                }
                if contains(self.regions.scrollbar, m.column, m.row) {
                    self.dragging_scrollbar = true;
                    self.scrollbar_jump(m.row);
                    return;
                }
                let Some(pos) = self.pos_at_screen(m.column, m.row) else { return };
                let count = self.click_count(m.column, m.row);
                if contains(self.regions.gutter, m.column, m.row) {
                    // Gutter: select whole lines; drag extends by lines.
                    let range = self.line_range(pos);
                    if shift || alt {
                        let anchor = self.buf.sel.start().min(range.0);
                        self.buf.sel = Selection { anchor, head: range.1.max(self.buf.sel.end()) };
                    } else {
                        self.buf.sel = Selection { anchor: range.0, head: range.1 };
                    }
                    self.drag = Some((DragUnit::Line, Selection { anchor: range.0, head: range.1 }));
                    self.buf.preferred_col = None;
                    self.buf.break_undo_group();
                    return;
                }
                if (shift || alt) && count == 1 {
                    // Extend selection. Alt+click is the fallback for terminals
                    // that reserve Shift+click (stock Kitty, many others).
                    self.buf.sel.head = pos;
                    self.drag = Some((DragUnit::Char, Selection::caret(self.buf.sel.anchor)));
                } else {
                    let (unit, range) = match count {
                        1 => (DragUnit::Char, (pos, pos)),
                        2 => (DragUnit::Word, self.buf.word_at(pos)),
                        _ => (DragUnit::Line, self.line_range(pos)),
                    };
                    self.buf.sel = Selection { anchor: range.0, head: range.1 };
                    self.drag = Some((unit, Selection { anchor: range.0, head: range.1 }));
                }
                self.buf.preferred_col = None;
                self.buf.break_undo_group();
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.dragging_scrollbar {
                    self.scrollbar_jump(m.row);
                } else {
                    self.on_drag(m.column, m.row);
                }
            }
            MouseEventKind::Up(_) => {
                self.drag = None;
                self.dragging_scrollbar = false;
            }
            MouseEventKind::ScrollUp if shift => self.scroll_x = self.scroll_x.saturating_sub(SCROLL_STEP * 2),
            MouseEventKind::ScrollDown if shift => self.scroll_horiz(SCROLL_STEP * 2),
            MouseEventKind::ScrollLeft => self.scroll_x = self.scroll_x.saturating_sub(SCROLL_STEP * 2),
            MouseEventKind::ScrollRight => self.scroll_horiz(SCROLL_STEP * 2),
            MouseEventKind::ScrollUp => self.scroll_rows(-(SCROLL_STEP as isize)),
            MouseEventKind::ScrollDown => self.scroll_rows(SCROLL_STEP as isize),
            _ => {}
        }
    }

    fn scroll_horiz(&mut self, n: usize) {
        if !self.wrap {
            self.scroll_x += n;
        }
    }

    fn on_drag(&mut self, x: u16, y: u16) {
        let Some((unit, origin)) = self.drag else { return };
        let t = self.regions.text;
        // Autoscroll when dragging above/below the text area.
        if y < t.y {
            self.scroll_rows(-1);
        } else if y >= t.y + t.height {
            self.scroll_rows(1);
        }
        if !self.wrap {
            if x < t.x && self.scroll_x > 0 {
                self.scroll_x -= 1;
            } else if x >= t.x + t.width {
                self.scroll_x += 1;
            }
        }
        let cy = y.clamp(t.y, t.y + t.height.saturating_sub(1));
        let cx = x.clamp(t.x, t.x + t.width);
        let Some(pos) = self.pos_at_screen(cx, cy) else { return };
        let (s, e) = match unit {
            DragUnit::Char => {
                self.buf.sel = Selection { anchor: origin.anchor, head: pos };
                return;
            }
            DragUnit::Word => self.buf.word_at(pos),
            DragUnit::Line => self.line_range(pos),
        };
        // Keep the originally clicked word/line selected, extend toward the pointer.
        self.buf.sel = if s < origin.start() {
            Selection { anchor: origin.end(), head: s }
        } else {
            Selection { anchor: origin.start(), head: e.max(origin.end()) }
        };
    }

    fn line_range(&self, pos: usize) -> (usize, usize) {
        let line = self.buf.line_of(pos);
        let start = self.buf.line_start(line);
        let end = if line + 1 < self.buf.len_lines() { self.buf.line_start(line + 1) } else { self.buf.len_chars() };
        (start, end)
    }

    fn click_count(&mut self, x: u16, y: u16) -> u8 {
        let now = Instant::now();
        let count = match self.last_click {
            Some((t, lx, ly, c)) if now.duration_since(t) < DOUBLE_CLICK && lx == x && ly == y => c % 3 + 1,
            _ => 1,
        };
        self.last_click = Some((now, x, y, count));
        count
    }

    /// Map a screen cell in the text area (or gutter) to a buffer position.
    fn pos_at_screen(&self, x: u16, y: u16) -> Option<usize> {
        let t = self.regions.text;
        if t.width == 0 || t.height == 0 || self.rendered_rows.is_empty() {
            return None;
        }
        if x < self.regions.gutter.x || y < t.y || y >= t.y + t.height {
            return None;
        }
        let i = (y - t.y) as usize;
        let Some(row) = self.rendered_rows.get(i) else {
            return Some(self.buf.len_chars());
        };
        let col = (x.saturating_sub(t.x)) as usize + if self.wrap { 0 } else { self.scroll_x };
        Some(row.pos_at_col(col))
    }

    fn button_at(&self, x: u16, y: u16) -> Option<Button> {
        self.regions.buttons.iter().find(|(r, _)| contains(*r, x, y)).map(|(_, b)| *b)
    }

    fn press_button(&mut self, b: Button) {
        match b {
            Button::Save => self.save(false),
            Button::Wrap => self.toggle_wrap(),
            Button::Mouse => self.toggle_mouse(),
            Button::Help => self.show_help = true,
        }
    }

    fn scrollbar_jump(&mut self, y: u16) {
        let r = self.regions.scrollbar;
        if r.height == 0 {
            return;
        }
        let frac = (y.saturating_sub(r.y)) as f64 / r.height.saturating_sub(1).max(1) as f64;
        let lines = self.buf.len_lines();
        self.top_line = ((frac.clamp(0.0, 1.0) * lines.saturating_sub(1) as f64).round() as usize).min(lines - 1);
        self.top_row = 0;
        self.follow_caret = false;
    }

    // ---- viewport ------------------------------------------------------------------------

    /// Scroll the viewport by visual rows without moving the caret.
    pub fn scroll_rows(&mut self, delta: isize) {
        self.follow_caret = false;
        if delta > 0 {
            for _ in 0..delta {
                let n = self.rows_of(self.top_line).len();
                if self.top_row + 1 < n {
                    self.top_row += 1;
                } else if self.top_line + 1 < self.buf.len_lines() {
                    self.top_line += 1;
                    self.top_row = 0;
                } else {
                    break;
                }
            }
        } else {
            for _ in 0..(-delta) {
                if self.top_row > 0 {
                    self.top_row -= 1;
                } else if self.top_line > 0 {
                    self.top_line -= 1;
                    self.top_row = self.rows_of(self.top_line).len() - 1;
                } else {
                    break;
                }
            }
        }
    }

    /// Adjust the viewport so the caret is visible. Requires `regions.text`.
    pub fn ensure_caret_visible(&mut self) {
        let height = self.regions.text.height as usize;
        if height == 0 {
            return;
        }
        self.top_line = self.top_line.min(self.buf.len_lines() - 1);
        self.top_row = self.top_row.min(self.rows_of(self.top_line).len() - 1);
        if !self.follow_caret {
            return;
        }
        let head = self.buf.sel.head;
        let cline = self.buf.line_of(head);
        let crows = self.rows_of(cline);
        let crow = row_index_for(&crows, head);

        if (cline, crow) < (self.top_line, self.top_row) {
            self.top_line = cline;
            self.top_row = crow;
        } else {
            // Count rows from top to caret; stop once past the viewport.
            let mut count = 0usize;
            let mut line = self.top_line;
            let mut first = self.top_row;
            let mut visible = false;
            while line <= cline && count < height {
                let n = if line == cline { crow + 1 } else { self.rows_of(line).len() };
                count += n - first;
                first = 0;
                if line == cline && count <= height {
                    visible = true;
                }
                line += 1;
            }
            if !visible {
                // Place the caret row at the bottom of the viewport.
                let (mut l, mut r) = (cline, crow);
                for _ in 0..height - 1 {
                    if r > 0 {
                        r -= 1;
                    } else if l > 0 {
                        l -= 1;
                        r = self.rows_of(l).len() - 1;
                    } else {
                        break;
                    }
                }
                self.top_line = l;
                self.top_row = r;
            }
        }

        if !self.wrap {
            let w = self.regions.text.width as usize;
            let col = crows[crow].col_of(head);
            let margin = 4.min(w / 4);
            if col < self.scroll_x + margin {
                self.scroll_x = col.saturating_sub(margin);
            } else if col + margin >= self.scroll_x + w {
                self.scroll_x = col + margin + 1 - w;
            }
        } else {
            self.scroll_x = 0;
        }
        self.follow_caret = false;
    }

    /// Collect the visual rows for the viewport starting at the top position.
    pub fn visible_rows(&self, height: usize) -> Vec<VisualRow> {
        let mut out = Vec::with_capacity(height);
        let mut line = self.top_line;
        let mut skip = self.top_row;
        while out.len() < height && line < self.buf.len_lines() {
            for r in self.rows_of(line).into_iter().skip(skip) {
                if out.len() == height {
                    break;
                }
                out.push(r);
            }
            skip = 0;
            line += 1;
        }
        out
    }
}

/// Guess the file's indentation style from its first lines: tabs unless
/// space-indented lines clearly dominate. Empty files default to tabs.
pub fn detect_indent(buf: &Buffer, tab_width: usize) -> String {
    let rope = buf.rope();
    let (mut tabs, mut spaces) = (0usize, 0usize);
    for line in rope.lines().take(2000) {
        match (line.get_char(0), line.get_char(1)) {
            (Some('\t'), _) => tabs += 1,
            (Some(' '), Some(' ')) => spaces += 1,
            _ => {}
        }
    }
    if spaces > tabs { " ".repeat(tab_width) } else { "\t".to_string() }
}

fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return Path::new(&home).join(rest);
    }
    PathBuf::from(p)
}

fn smart_case(text: &str, query: &str) -> (String, String) {
    if query.chars().any(char::is_uppercase) {
        (text.to_string(), query.to_string())
    } else {
        (lower_same_len(text), lower_same_len(query))
    }
}

/// All non-overlapping smart-case matches in `text`, as byte ranges.
pub fn find_all(text: &str, query: &str) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }
    let (hay, needle) = smart_case(text, query);
    hay.match_indices(&needle).map(|(i, m)| (i, i + m.len())).collect()
}

/// Smart-case literal search: case-insensitive unless the query has capitals.
/// Returns a char range. Wraps around the document.
pub fn find_in(buf: &Buffer, query: &str, from: usize, forward: bool) -> Option<(usize, usize)> {
    if query.is_empty() {
        return None;
    }
    let (hay, needle) = smart_case(&buf.rope().to_string(), query);
    let from_b = buf.rope().char_to_byte(from.min(buf.len_chars()));
    let found = if forward {
        hay[from_b..].find(&needle).map(|i| i + from_b).or_else(|| hay.find(&needle))
    } else {
        hay[..from_b].rfind(&needle).or_else(|| hay.rfind(&needle))
    }?;
    let s = buf.rope().byte_to_char(found);
    let e = buf.rope().byte_to_char(found + needle.len());
    Some((s, e))
}

/// Lowercase only where it keeps the UTF-8 length identical, so byte offsets
/// in the lowered string still map onto the original text.
fn lower_same_len(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut l = c.to_lowercase();
            match (l.next(), l.next()) {
                (Some(lc), None) if lc.len_utf8() == c.len_utf8() => lc,
                _ => c,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyEventState, MouseEvent};

    fn app_with(text: &str, w: u16, h: u16) -> App {
        let caps = Capabilities { keyboard_enhanced: true, truecolor: true };
        let theme = theme::resolve(&theme::dracula());
        let mut a = App::new(Buffer::from_str(text), Document::untitled(), theme, ThemeSource::Builtin, caps);
        a.regions.text = Rect { x: 5, y: 1, width: w, height: h };
        a.regions.gutter = Rect { x: 0, y: 1, width: 5, height: h };
        a
    }

    fn key(a: &mut App, code: KeyCode, mods: KeyModifiers) {
        a.handle_event(Event::Key(KeyEvent { code, modifiers: mods, kind: KeyEventKind::Press, state: KeyEventState::NONE }));
    }

    fn text(a: &App) -> String {
        a.buf.rope().to_string()
    }

    #[test]
    fn viewport_follows_caret_down_and_up() {
        let body: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let mut a = app_with(&body, 40, 10);
        a.buf.sel = Selection::caret(a.buf.line_start(50));
        a.follow_caret = true;
        a.ensure_caret_visible();
        assert_eq!(a.top_line, 41, "caret line should be the last visible row");
        a.buf.sel = Selection::caret(a.buf.line_start(3));
        a.follow_caret = true;
        a.ensure_caret_visible();
        assert_eq!(a.top_line, 3);
    }

    #[test]
    fn vertical_motion_walks_wrapped_rows_and_keeps_column() {
        // Width 10: first line wraps into 3 rows.
        let mut a = app_with("aaaa bbbb cccc dddd eeee\nxy\n", 10, 10);
        a.buf.sel = Selection::caret(2);
        key(&mut a, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(a.buf.sel.head, 12, "same column on the next visual row");
        key(&mut a, KeyCode::Down, KeyModifiers::NONE);
        key(&mut a, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(a.buf.sel.head, a.buf.line_start(1) + 2, "short line clamps to its end");
        key(&mut a, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(a.buf.sel.head, 22, "preferred column is remembered");
    }

    #[test]
    fn indent_detection_and_tab_behaviour() {
        let mut a = app_with("def f():\n    return 1\n    pass\n", 40, 10);
        assert_eq!(a.indent_unit, "    ");
        a.buf.sel = Selection::caret(a.buf.line_start(1) + 2);
        key(&mut a, KeyCode::Tab, KeyModifiers::NONE);
        assert!(text(&a).contains("\n      return"), "Tab pads to the next tab stop");
        let a2 = app_with("x\n\ty\n", 40, 10);
        assert_eq!(a2.indent_unit, "\t");
    }

    #[test]
    fn tab_and_shift_tab_on_selection_indent_lines_atomically() {
        let mut a = app_with("one\n\ntwo\nthree\n", 40, 10);
        a.buf.sel = Selection { anchor: 1, head: a.buf.line_start(3) }; // ends at col 0 of line 3
        key(&mut a, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(text(&a), "\tone\n\n\ttwo\nthree\n", "blank line and line 3 untouched");
        key(&mut a, KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(text(&a), "one\n\ntwo\nthree\n");
        key(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(text(&a), "\tone\n\n\ttwo\nthree\n");
    }

    #[test]
    fn ctrl_up_down_scrolls_without_moving_caret() {
        let body: String = (0..50).map(|i| format!("{i}\n")).collect();
        let mut a = app_with(&body, 40, 10);
        key(&mut a, KeyCode::Down, KeyModifiers::CONTROL);
        key(&mut a, KeyCode::Down, KeyModifiers::CONTROL);
        a.ensure_caret_visible();
        assert_eq!(a.top_line, 2);
        assert_eq!(a.buf.sel.head, 0);
    }

    #[test]
    fn dropped_file_opens_instead_of_inserting_path() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("dropped file.txt");
        std::fs::write(&f, "dropped contents").unwrap();
        let mut a = app_with("", 40, 10);
        a.handle_event(Event::Paste(format!("{}\n", f.display())));
        assert_eq!(text(&a), "dropped contents");
        assert_eq!(a.doc.path.as_deref(), Some(f.as_path()));
        // With unsaved changes the drop is refused, nothing is inserted or lost.
        a.handle_event(Event::Paste("x".into()));
        let before = text(&a);
        a.handle_event(Event::Paste(format!("'{}' ", f.display())));
        assert_eq!(text(&a), before);
        assert!(a.status.as_ref().unwrap().0.contains("Unsaved"));
        // Ordinary paste still inserts text.
        a.handle_event(Event::Paste("/not/a/real/path".into()));
        assert!(text(&a).contains("/not/a/real/path"));
    }

    #[test]
    fn dropped_folder_opens_prompt_and_drop_into_open_prompt_fills_it() {
        let d = tempfile::tempdir().unwrap();
        let mut a = app_with("", 40, 10);
        a.handle_event(Event::Paste(d.path().display().to_string()));
        let p = a.prompt.as_ref().unwrap();
        assert_eq!(p.kind, PromptKind::Open);
        assert!(p.input.ends_with('/'));
        let f = d.path().join("a.txt");
        std::fs::write(&f, "").unwrap();
        a.handle_event(Event::Paste(f.display().to_string()));
        assert_eq!(a.prompt.as_ref().unwrap().input, f.display().to_string());
    }

    #[test]
    fn gutter_click_selects_line() {
        let mut a = app_with("alpha\nbeta\ngamma\n", 40, 10);
        a.rendered_rows = a.visible_rows(10);
        let ev = |kind| Event::Mouse(MouseEvent { kind, column: 1, row: 2, modifiers: KeyModifiers::NONE });
        a.handle_event(ev(MouseEventKind::Down(MouseButton::Left)));
        a.handle_event(ev(MouseEventKind::Up(MouseButton::Left)));
        assert_eq!(a.buf.selected_text(), "beta\n");
    }

    #[test]
    fn find_smart_case_and_wrap() {
        let b = Buffer::from_str("Alpha beta ALPHA alpha");
        assert_eq!(find_in(&b, "alpha", 1, true), Some((11, 16)));
        assert_eq!(find_in(&b, "Alpha", 1, true), Some((0, 5))); // wraps
        assert_eq!(find_in(&b, "alpha", 11, false), Some((0, 5)));
        assert_eq!(find_in(&b, "zzz", 0, true), None);
    }

    #[test]
    fn find_with_multibyte_text() {
        let b = Buffer::from_str("café Ünïcode ünïcode");
        // Lowercase query is case-insensitive, so "Ünïcode" matches first.
        assert_eq!(find_in(&b, "ünïcode", 0, true), Some((5, 12)));
        assert_eq!(find_in(&b, "ünï", 6, true), Some((13, 16)));
        // Capitals make it case-sensitive.
        assert_eq!(find_in(&b, "ÜNÏ", 0, true), None);
    }
}
