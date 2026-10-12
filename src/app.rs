//! Application state and command handling (single document for now).

use crate::buffer::{EditKind, Selection};
use crate::clipboard::Clipboard;
use crate::dialog::{Action, Button as DButton, Dialog};
use crate::drop;
use crate::recovery::{Recovery, Snapshot};
use crate::find::{FindBar, FindHit, SearchOpts};
use crate::config::{IndentMode, ThemeChoice};
use crate::prefs::{PrefsHit, PrefsState};
use crate::keymap::{Command, Keymap};
use crate::lineedit::LineEdit;
use crate::encoding::{self, Encoding};
use crate::picker::{self, Picker};
use crate::tab::{Tab, TabOpts};
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
/// Snapshot a dirty tab after this much idle time…
const SNAPSHOT_IDLE: Duration = Duration::from_secs(2);
/// …or at least this often while editing continues.
const SNAPSHOT_MAX_AGE: Duration = Duration::from_secs(30);
const SCROLL_STEP: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeSource {
    Builtin(theme::Builtin),
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
    GotoLine,
    PalettePath,
    SaveAs,
    Open,
    HlName,
    HlTerms,
    HlFileTypes,
    HlFg,
    HlBg,
}

pub struct Prompt {
    pub kind: PromptKind,
    pub input: LineEdit,
}

impl Prompt {
    pub fn new(kind: PromptKind, input: &str) -> Self {
        Self { kind, input: LineEdit::new(input) }
    }
}

impl PromptKind {
    pub fn label(self) -> &'static str {
        match self {
            PromptKind::GotoLine => "Go to line",
            PromptKind::PalettePath => "Palette file (empty = none)",
            PromptKind::SaveAs => "Save as",
            PromptKind::Open => "Open",
            PromptKind::HlName => "Group name",
            PromptKind::HlTerms => "Words and phrases (comma-separated)",
            PromptKind::HlFileTypes => "File types, e.g. md, txt (empty = all files)",
            PromptKind::HlFg => "Text colour: #rrggbb, color0-15, accent, warning... (empty = none)",
            PromptKind::HlBg => "Background: #rrggbb, color0-15, accent, warning... (empty = none)",
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
    Prefs,
    Save,
    Table,
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
    pub tab_bar: Rect,
    pub tab_hits: Vec<(Rect, TabHit)>,
    pub dialog_buttons: Vec<(Rect, usize)>,
    pub prefs_hits: Vec<(Rect, PrefsHit)>,
    pub find_hits: Vec<(Rect, FindHit)>,
    /// The prompt's text field (empty when no prompt is shown).
    pub prompt_field: Rect,
    /// The encoding name in the status bar (click: encoding menu).
    pub status_encoding: Rect,
    /// The picker popup, its filter field and its list rows.
    pub picker_box: Rect,
    pub picker_field: Rect,
    pub picker_list: Rect,
}

/// What a choice in the picker does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickChoice {
    Reopen(Encoding),
    SaveWith(Encoding),
    Table(TableAction),
}

/// Entries of the table menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableAction {
    Format,
    RowAbove,
    RowBelow,
    DeleteRow,
    RowUp,
    RowDown,
    ColLeft,
    ColRight,
    DeleteCol,
    MoveColLeft,
    MoveColRight,
    Align(crate::table::Align),
    SelectRow,
    SelectCol,
    SelectAll,
    Convert(crate::table::Style),
    New(crate::table::Style),
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabHit {
    Select(usize),
    Close(usize),
    Prev,
    Next,
}

/// What to do once the active tab has been saved successfully (a save can
/// be interrupted by the Save As prompt or a dialog, so this is remembered).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterSave {
    CloseTab,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveOutcome {
    Saved,
    /// Waiting on the Save As prompt or a dialog.
    Waiting,
    Failed,
}

pub struct App {
    /// The active tab. Others live in `bg_tabs`; the full order is
    /// `bg_tabs[..active] ++ [tab] ++ bg_tabs[active..]`. Keeping the active
    /// tab as a plain field lets the borrow checker split `self.tab.buf`
    /// from the rest of `self`.
    pub tab: Tab,
    pub bg_tabs: Vec<Tab>,
    pub active: usize,
    pub clipboard: Clipboard,
    pub theme: Theme,
    pub theme_source: ThemeSource,
    pub caps: Capabilities,
    pub wrap: bool,
    pub line_numbers: bool,
    pub tab_width: usize,
    pub indent_mode: IndentMode,
    pub keymap: Keymap,
    pub show_whitespace: bool,
    pub mouse_enabled: bool,
    /// X11/Wayland primary selection: select to copy, middle-click to paste.
    pub primary_selection: bool,
    pub highlights: crate::highlight::Highlights,
    /// High contrast and colour-role overrides, applied to every theme.
    pub adjust: theme::Adjust,
    pub doc_rules: crate::dochl::DocRules,
    primary_due: Option<Instant>,
    pub show_help: bool,
    pub prompt: Option<Prompt>,
    pub last_find: String,
    pub last_find_opts: SearchOpts,
    pub find: Option<FindBar>,
    pub status: Option<(String, Level, Instant)>,
    pub quit: bool,

    /// Rows exactly as last rendered (for mouse hit-testing), and how far
    /// each was scrolled sideways.
    pub rendered_rows: Vec<VisualRow>,
    pub rendered_scroll: Vec<usize>,
    pub regions: Regions,

    drag: Option<(DragUnit, Selection)>,
    dragging_scrollbar: bool,
    last_mouse: (u16, u16),
    last_click: Option<(Instant, u16, u16, u8)>,
    pub dialog: Option<Dialog>,
    pub picker: Option<Picker<PickChoice>>,
    /// Encoding for files that aren't UTF-8 or UTF-16 (None = guess).
    pub fallback_encoding: Option<&'static encoding_rs::Encoding>,
    /// Re-pad a table column as its cells grow or shrink.
    pub table_realign: bool,
    /// Table keys switched off (Esc) in the table starting at this line.
    pub table_keys_off: Option<usize>,
    /// The table the caret was in after the last event (for the hint).
    pub table_context: Option<usize>,
    /// Where the last table edit left the caret, in which buffer version.
    pub table_typed_at: Option<((u64, u64), usize)>,
    /// Selected table cells (a rectangle), when a selection spans cells.
    pub cell_sel: Option<crate::table_edit::CellSel>,
    pub recovery: Option<Recovery>,
    pub recovery_enabled: bool,
    pub prefs: Option<PrefsState>,
    /// Theme setting as configured (the resolved colours are `theme`).
    pub theme_choice: ThemeChoice,
    pub palette_setting: Option<PathBuf>,
    /// Set when the colour source changed and the palette watcher must be rebuilt.
    pub palette_watch_dirty: bool,
    /// Where the palette path resolved to when its watcher was made; a
    /// re-pointed symlink needs a new watcher for the new target's folder.
    pub palette_real: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    /// Snapshots from a crashed session, offered at startup.
    pub recovery_offer: Vec<Snapshot>,
    after_save: Option<AfterSave>,
}

impl App {
    pub fn new(tab: Tab, theme: Theme, theme_source: ThemeSource, caps: Capabilities) -> Self {
        Self {
            tab,
            bg_tabs: Vec::new(),
            active: 0,
            clipboard: Clipboard::system(),
            theme,
            theme_source,
            caps,
            wrap: true,
            line_numbers: true,
            tab_width: 4,
            indent_mode: IndentMode::Auto,
            keymap: Keymap::defaults(),
            show_whitespace: false,
            mouse_enabled: true,
            primary_selection: true,
            highlights: crate::highlight::Highlights::default(),
            adjust: theme::Adjust::default(),
            doc_rules: crate::dochl::DocRules::default(),
            primary_due: None,
            show_help: false,
            prompt: None,
            last_find: String::new(),
            last_find_opts: SearchOpts::default(),
            find: None,
            status: None,
            quit: false,
            rendered_rows: Vec::new(),
            rendered_scroll: Vec::new(),
            regions: Regions::default(),
            drag: None,
            dragging_scrollbar: false,
            last_mouse: (0, 0),
            last_click: None,
            dialog: None,
            picker: None,
            fallback_encoding: None,
            table_realign: true,
            table_keys_off: None,
            table_context: None,
            table_typed_at: None,
            cell_sel: None,
            recovery: None,
            recovery_enabled: true,
            prefs: None,
            theme_choice: ThemeChoice::Auto,
            palette_setting: None,
            palette_watch_dirty: false,
            palette_real: None,
            config_path: None,
            recovery_offer: Vec::new(),
            after_save: None,
        }
    }

    // ---- tabs ----------------------------------------------------------------------

    pub fn tab_opts(&self) -> TabOpts {
        TabOpts { tab_width: self.tab_width, indent: self.indent_mode, fallback_encoding: self.fallback_encoding }
    }

    pub fn tab_count(&self) -> usize {
        self.bg_tabs.len() + 1
    }

    pub fn tab_at(&self, i: usize) -> &Tab {
        match i.cmp(&self.active) {
            std::cmp::Ordering::Equal => &self.tab,
            std::cmp::Ordering::Less => &self.bg_tabs[i],
            std::cmp::Ordering::Greater => &self.bg_tabs[i - 1],
        }
    }

    pub fn switch_to(&mut self, i: usize) {
        if i == self.active || i >= self.tab_count() {
            return;
        }
        let placeholder = Tab::untitled(self.tab_opts());
        let cur = std::mem::replace(&mut self.tab, placeholder);
        self.bg_tabs.insert(self.active, cur);
        self.tab = self.bg_tabs.remove(i);
        self.active = i;
        self.after_tab_change();
    }

    fn cycle_tab(&mut self, delta: isize) {
        let n = self.tab_count() as isize;
        self.switch_to((self.active as isize + delta).rem_euclid(n) as usize);
    }

    /// Show `tab` in a new tab right of the current one, or in place of an
    /// untouched Untitled tab.
    fn add_tab(&mut self, tab: Tab) {
        if self.tab.is_pristine() {
            self.tab = tab;
        } else {
            let cur = std::mem::replace(&mut self.tab, tab);
            self.bg_tabs.insert(self.active, cur);
            self.active += 1;
        }
        self.after_tab_change();
    }

    /// Close the active tab without asking (its unsaved changes, if any, were
    /// deliberately discarded, so its recovery snapshot goes too). Closing
    /// the last tab quits.
    fn close_active_tab(&mut self) {
        if let Some(r) = &self.recovery
            && self.tab.snap_key.is_some()
        {
            r.remove(self.tab.uid);
        }
        if self.bg_tabs.is_empty() {
            self.quit = true;
            return;
        }
        self.tab = if self.active < self.bg_tabs.len() {
            self.bg_tabs.remove(self.active)
        } else {
            self.active -= 1;
            self.bg_tabs.remove(self.active)
        };
        self.after_tab_change();
    }

    /// Close the active tab, asking first if it has unsaved changes.
    fn request_close_tab(&mut self) {
        if !self.tab.buf.is_dirty() {
            self.close_active_tab();
            return;
        }
        let name = self.tab.doc.display_name();
        self.dialog = Some(Dialog::new(
            "Unsaved changes",
            vec![format!("Save changes to {name} before closing?")],
            vec![
                DButton::new("&Save", Action::SaveThenClose),
                DButton::new("Do&n't Save", Action::DiscardThenClose),
                DButton::new("&Cancel", Action::Cancel),
            ],
        ));
    }

    /// Quit, asking about unsaved tabs first.
    fn request_quit(&mut self) {
        let dirty: Vec<usize> = (0..self.tab_count()).filter(|&i| self.tab_at(i).buf.is_dirty()).collect();
        match dirty.len() {
            0 => self.quit = true,
            1 => {
                self.switch_to(dirty[0]);
                let name = self.tab.doc.display_name();
                self.dialog = Some(Dialog::new(
                    "Unsaved changes",
                    vec![format!("Save changes to {name} before quitting?")],
                    vec![
                        DButton::new("&Save", Action::SaveThenQuit),
                        DButton::new("Do&n't Save", Action::DiscardAndQuit),
                        DButton::new("&Cancel", Action::Cancel),
                    ],
                ));
            }
            n => {
                self.dialog = Some(Dialog::new(
                    "Unsaved changes",
                    vec![format!("{n} documents have unsaved changes.")],
                    vec![
                        DButton::new("Save &All", Action::SaveAllThenQuit),
                        DButton::new("&Quit Without Saving", Action::DiscardAndQuit),
                        DButton::new("&Cancel", Action::Cancel),
                    ],
                ));
            }
        }
    }

    fn on_tab_bar_click(&mut self, hit: TabHit) {
        match hit {
            TabHit::Select(i) => self.switch_to(i),
            TabHit::Close(i) => {
                self.switch_to(i);
                self.request_close_tab();
            }
            TabHit::Prev => self.cycle_tab(-1),
            TabHit::Next => self.cycle_tab(1),
        }
    }

    // ---- dialogs and save chains ---------------------------------------------------

    fn run_action(&mut self, action: Action) {
        self.dialog = None;
        match action {
            Action::Cancel => self.after_save = None,
            Action::SaveThenClose => {
                self.after_save = Some(AfterSave::CloseTab);
                if self.save_active(false) == SaveOutcome::Saved {
                    self.continue_after_save();
                }
            }
            Action::DiscardThenClose => self.close_active_tab(),
            Action::SaveThenQuit | Action::SaveAllThenQuit => self.save_all_then_quit(),
            Action::DiscardAndQuit => {
                self.discard_all_snapshots();
                self.quit = true;
            }
            Action::Overwrite => {
                if self.save_active(true) == SaveOutcome::Saved {
                    self.continue_after_save();
                }
            }
            Action::ReloadFromDisk => {
                self.after_save = None;
                self.reload_active();
            }
            Action::KeepMine => {
                self.after_save = None;
                self.tab.disk_conflict = false;
                self.info("Keeping your version; saving will ask before overwriting");
            }
            Action::SaveAs => self.open_prompt(PromptKind::SaveAs),
            Action::Replace(path) => {
                if self.save_as_path(path) {
                    self.continue_after_save();
                }
            }
            Action::RestoreRecovery => self.restore_recovery(),
            Action::DiscardRecovery => {
                if let Some(r) = &self.recovery {
                    r.delete(&self.recovery_offer);
                }
                let n = self.recovery_offer.len();
                self.recovery_offer.clear();
                self.info(format!("Discarded {n} recovered document(s)"));
            }
            Action::RecoveryLater => {
                self.recovery_offer.clear();
                self.info("Recovered work kept; it will be offered again next time");
            }
            Action::SaveInexact => {
                self.tab.doc.inexact = false;
                if self.save_active(false) == SaveOutcome::Saved {
                    self.continue_after_save();
                }
            }
            Action::SaveUtf8 => {
                self.use_utf8();
                if self.save_active(false) == SaveOutcome::Saved {
                    self.continue_after_save();
                }
            }
            Action::SaveAsUtf8(path) => {
                self.use_utf8();
                if self.save_as_path(path) {
                    self.continue_after_save();
                }
            }
            Action::EncReopen => self.open_encoding_picker(false),
            Action::EncSave => self.open_encoding_picker(true),
        }
    }

    // ---- crash recovery ------------------------------------------------------------

    /// Offer snapshots found at startup (from a crashed or killed session).
    pub fn offer_recovery(&mut self, snaps: Vec<Snapshot>) {
        if snaps.is_empty() {
            return;
        }
        let mut lines = vec![format!(
            "Plica found unsaved work from a session that ended unexpectedly ({} document{}):",
            snaps.len(),
            if snaps.len() == 1 { "" } else { "s" }
        )];
        for s in snaps.iter().take(6) {
            lines.push(format!("  • {}", s.doc_path.as_ref().map_or_else(|| s.name.clone(), |p| p.display().to_string())));
        }
        if snaps.len() > 6 {
            lines.push(format!("  … and {} more", snaps.len() - 6));
        }
        self.recovery_offer = snaps;
        self.dialog = Some(Dialog::new(
            "Recover unsaved work",
            lines,
            vec![
                DButton::new("&Restore", Action::RestoreRecovery),
                DButton::new("&Discard", Action::DiscardRecovery),
                DButton::new("&Later", Action::RecoveryLater),
            ],
        ));
    }

    /// Open each recovered document with its snapshot text applied as one
    /// undoable edit (Ctrl+Z shows the version on disk).
    fn restore_recovery(&mut self) {
        let snaps = std::mem::take(&mut self.recovery_offer);
        let mut restored = 0;
        for s in &snaps {
            let opened = match &s.doc_path {
                Some(p) => self.open_file(p),
                None => {
                    self.add_tab(Tab::untitled(self.tab_opts()));
                    true
                }
            };
            if !opened {
                continue;
            }
            if self.tab.buf.rope() != &s.text[..] {
                let len = self.tab.buf.len_chars();
                self.tab.buf.transact(vec![(0, len, s.text.clone())]);
                self.tab.buf.sel = Selection::caret(0);
            }
            if let Some(e) = s.encoding {
                self.tab.doc.encoding = e;
            }
            restored += 1;
        }
        if let Some(r) = &self.recovery {
            // The restored text now lives in this session's tabs (and will be
            // snapshotted again by it).
            r.delete(&snaps);
        }
        self.info(format!("Restored {restored} document(s); review and save them"));
    }

    /// Called from the main loop: snapshot dirty tabs once they have been
    /// idle for a moment (or periodically while typing continues), and drop
    /// snapshots of tabs that became clean.
    pub fn tick_recovery(&mut self, force: bool) {
        let Some(r) = self.recovery.take() else { return };
        let now = Instant::now();
        for i in 0..self.tab_count() {
            let tab = self.tab_at_mut(i);
            let key = tab.buf.cache_key();
            if key != tab.seen_key {
                tab.seen_key = key;
                tab.changed_at = now;
            }
            if tab.buf.is_dirty() {
                let stale = tab.snap_key != Some(key);
                let idle = now.duration_since(tab.changed_at) >= SNAPSHOT_IDLE;
                let overdue = now.duration_since(tab.snap_at) >= SNAPSHOT_MAX_AGE;
                if stale && (force || idle || overdue) {
                    r.write(tab.uid, tab.doc.path.as_deref(), &tab.doc.display_name(), tab.doc.encoding, tab.buf.rope());
                    tab.snap_key = Some(key);
                    tab.snap_at = now;
                }
            } else if tab.snap_key.take().is_some() {
                r.remove(tab.uid);
            }
        }
        self.recovery = Some(r);
    }

    /// Turn crash recovery on or off at runtime (Preferences).
    pub fn set_recovery_enabled(&mut self, on: bool) {
        self.recovery_enabled = on;
        if on && self.recovery.is_none() {
            self.recovery = crate::recovery::default_dir().and_then(|d| Recovery::new(d).ok());
            if self.recovery.is_none() {
                self.warn("Cannot create the recovery directory");
            }
        } else if !on {
            self.discard_all_snapshots();
            if let Some(r) = self.recovery.take() {
                r.flush();
            }
            for i in 0..self.tab_count() {
                self.tab_at_mut(i).snap_key = None;
            }
        }
    }

    /// Unsaved changes the user chose to throw away (quit without saving):
    /// remove their snapshots.
    pub fn discard_all_snapshots(&mut self) {
        let Some(r) = &self.recovery else { return };
        for t in self.all_tabs().filter(|t| t.snap_key.is_some()) {
            r.remove(t.uid);
        }
    }

    fn continue_after_save(&mut self) {
        match self.after_save.take() {
            Some(AfterSave::CloseTab) => self.close_active_tab(),
            Some(AfterSave::Quit) => self.save_all_then_quit(),
            None => {}
        }
    }

    /// Save every unsaved tab in order, stopping to ask for a name or about
    /// a disk conflict; the chain resumes after each successful save.
    fn save_all_then_quit(&mut self) {
        for i in 0..self.tab_count() {
            if self.tab_at(i).buf.is_dirty() {
                self.switch_to(i);
                self.after_save = Some(AfterSave::Quit);
                if self.save_active(false) != SaveOutcome::Saved {
                    return;
                }
            }
        }
        self.after_save = None;
        self.quit = true;
    }

    // ---- external changes ----------------------------------------------------------

    pub fn tab_at_mut(&mut self, i: usize) -> &mut Tab {
        match i.cmp(&self.active) {
            std::cmp::Ordering::Equal => &mut self.tab,
            std::cmp::Ordering::Less => &mut self.bg_tabs[i],
            std::cmp::Ordering::Greater => &mut self.bg_tabs[i - 1],
        }
    }

    /// Directories holding open files (for the document watcher).
    pub fn watched_dirs(&self) -> std::collections::BTreeSet<PathBuf> {
        self.all_tabs()
            .filter_map(|t| t.doc.path.as_ref()?.parent().map(Path::to_path_buf))
            .collect()
    }

    /// A watched path changed on disk (already debounced).
    pub fn on_disk_change(&mut self, path: &Path) {
        if let Some(i) = (0..self.tab_count()).find(|&i| self.tab_at(i).doc.path.as_deref() == Some(path)) {
            self.check_disk(i);
        }
    }

    pub fn check_all_disk(&mut self) {
        for i in 0..self.tab_count() {
            self.check_disk(i);
        }
    }

    fn check_disk(&mut self, i: usize) {
        let is_active = i == self.active;
        let tab = self.tab_at_mut(i);
        if !tab.doc.changed_on_disk() {
            return;
        }
        let name = tab.doc.display_name();
        let exists = tab.doc.path.as_ref().is_some_and(|p| p.exists());
        if !exists {
            // Forget the stamp so we warn once; saving recreates the file.
            tab.doc.stamp = None;
            self.warn(format!("{name} was deleted or moved on disk; save to recreate it"));
            return;
        }
        if !tab.buf.is_dirty() {
            match tab.reload_from_disk(None) {
                Ok(()) => self.info(format!("Reloaded {name} (changed on disk; Ctrl+Z restores)")),
                Err(e) => self.warn(format!("Could not reload {name}: {e:#}")),
            }
        } else if is_active {
            self.show_conflict_dialog();
        } else {
            tab.disk_conflict = true;
            self.warn(format!("{name} changed on disk while it has unsaved changes"));
        }
    }

    fn show_conflict_dialog(&mut self) {
        self.tab.disk_conflict = false;
        let name = self.tab.doc.display_name();
        self.dialog = Some(Dialog::new(
            "File changed on disk",
            vec![format!("{name} changed on disk, and you have unsaved changes.")],
            vec![
                DButton::new("&Keep Mine", Action::KeepMine),
                DButton::new("&Reload (lose my changes)", Action::ReloadFromDisk),
            ],
        ));
    }

    fn reload_active(&mut self) {
        let name = self.tab.doc.display_name();
        match self.tab.reload_from_disk(None) {
            Ok(()) => self.info(format!("Reloaded {name} (Ctrl+Z restores your version)")),
            Err(e) => self.warn(format!("Could not reload {name}: {e:#}")),
        }
    }

    fn after_tab_change(&mut self) {
        self.drag = None;
        self.rendered_rows.clear();
        self.tab.follow_caret = true;
        if self.tab.disk_conflict && self.dialog.is_none() {
            self.show_conflict_dialog();
        }
    }

    /// All tabs, in order (for recovery snapshots).
    pub fn all_tabs(&self) -> impl Iterator<Item = &Tab> {
        (0..self.tab_count()).map(|i| self.tab_at(i))
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

    /// Visual rows of `line`. Table rows never soft-wrap (a wrapped table
    /// is unreadable); a wide table scrolls sideways instead.
    pub fn rows_of(&self, line: usize) -> Vec<VisualRow> {
        let mut opts = self.layout_opts();
        if opts.wrap_width.is_some() && self.tab.table_at(line).is_some() {
            opts.wrap_width = None;
        }
        layout_line(self.tab.buf.rope(), line, opts)
    }

    /// How far `line` is scrolled sideways: the whole view without soft
    /// wrap; with it, only the caret's table.
    pub fn row_scroll_x(&self, line: usize) -> usize {
        if !self.wrap {
            return self.tab.scroll_x;
        }
        match self.caret_table() {
            Some(t) if (t.first_line()..=t.last_line()).contains(&line) => self.tab.table_scroll_x,
            _ => 0,
        }
    }

    // ---- palette ---------------------------------------------------------------

    /// Re-read the palette file. On failure keep the current (last-known-good)
    /// theme and record why.
    pub fn reload_palette(&mut self) {
        let ThemeSource::File { path, .. } = &self.theme_source else { return };
        let path = path.clone();
        let real = std::fs::canonicalize(&path).ok();
        if real != self.palette_real {
            self.palette_watch_dirty = true;
        }
        let result = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| theme::parse_kitty(&t).map_err(|e| e.to_string()));
        match result {
            Ok(report) => {
                self.theme = theme::resolve_with(&report.palette, &self.adjust);
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
                            p.input.set(&paths[0].display().to_string());
                        }
                        _ => p.input.insert(&text),
                    }
                } else if let Some(paths) = dropped {
                    self.open_dropped(&paths);
                } else if self.find.is_some() {
                    self.find_insert_text(&text);
                } else {
                    self.paste_text(&text);
                }
            }
            Event::FocusGained => self.check_all_disk(),
            _ => {}
        }
        self.note_table_context();
    }

    /// Called regularly from the main loop (drag autoscroll).
    pub fn tick(&mut self) -> bool {
        if self.primary_due.is_some_and(|t| Instant::now() >= t) {
            self.primary_due = None;
            self.publish_primary();
        }
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

        if let Some(d) = &mut self.dialog {
            if let Some(action) = d.on_key(k) {
                self.run_action(action);
            }
            return;
        }
        if let Some(p) = &mut self.picker {
            let page = (self.regions.picker_list.height as usize).max(1);
            match p.on_key(&k, page) {
                picker::Outcome::Open => {}
                picker::Outcome::Cancel => self.picker = None,
                picker::Outcome::Chosen(c) => self.pick_done(c),
            }
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        if self.prompt.is_some() {
            self.on_prompt_key(k);
            return;
        }
        if self.prefs.is_some() {
            self.on_prefs_key(k);
            return;
        }
        if self.find.is_some() && self.on_find_key(k) {
            return;
        }

        if self.cell_sel.is_some() {
            // Keys that act on selected cells keep them; anything else
            // drops the cell selection first.
            let cmd = self.keymap.lookup(&k);
            let arrow = matches!(k.code, KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right);
            let keeps = matches!(cmd, Some(Command::Copy | Command::Cut | Command::Paste | Command::TableMenu))
                || (shift && arrow && !ctrl && !alt)
                || (cmd.is_none() && matches!(k.code, KeyCode::Delete | KeyCode::Backspace | KeyCode::Char(_) | KeyCode::Esc));
            if !keeps {
                self.cell_sel = None;
            }
        }
        if let Some(cmd) = self.keymap.lookup(&k) {
            self.run_command(cmd);
            return;
        }
        if self.table_key(&k) {
            return;
        }

        match (k.code, ()) {
            (KeyCode::Char(c @ '1'..='9'), _) if alt => {
                // Alt+9 is always the last tab, like browsers.
                let n = c as usize - '1' as usize;
                self.switch_to(if c == '9' { self.tab_count() - 1 } else { n });
            }
            (KeyCode::Esc, _) => {
                self.tab.buf.sel = Selection::caret(self.tab.buf.sel.head);
            }
            (KeyCode::Enter, _) => self.newline(),
            (KeyCode::Tab, _) if shift => self.outdent(),
            (KeyCode::Tab, _) => self.tab(),
            (KeyCode::BackTab, _) => self.outdent(),
            (KeyCode::Backspace, _) => {
                let h = self.tab.buf.sel.head;
                let from = if ctrl || alt { self.tab.buf.word_left(h) } else { self.tab.buf.prev_grapheme(h) };
                self.tab.buf.delete((from, h), EditKind::Deleting);
            }
            (KeyCode::Delete, _) => {
                let h = self.tab.buf.sel.head;
                let to = if ctrl || alt { self.tab.buf.word_right(h) } else { self.tab.buf.next_grapheme(h) };
                self.tab.buf.delete((h, to), EditKind::Deleting);
            }
            (KeyCode::Char(c), _) if !ctrl && !alt => {
                let mut s = [0u8; 4];
                self.tab.buf.insert(c.encode_utf8(&mut s), EditKind::Typing);
            }

            // -- motion --
            (KeyCode::Left, _) => self.move_horiz(false, ctrl || alt, shift),
            (KeyCode::Right, _) => self.move_horiz(true, ctrl || alt, shift),
            (KeyCode::Up, _) => self.move_vert(-1, shift),
            (KeyCode::Down, _) => self.move_vert(1, shift),
            (KeyCode::PageUp, _) => self.move_vert(-(self.page_rows() as isize), shift),
            (KeyCode::PageDown, _) => self.move_vert(self.page_rows() as isize, shift),
            (KeyCode::Home, _) => {
                let to = if ctrl { 0 } else { self.smart_home() };
                self.move_to(to, shift);
            }
            (KeyCode::End, _) => {
                let to = if ctrl { self.tab.buf.len_chars() } else { self.visual_row_end() };
                self.move_to(to, shift);
            }
            _ => return,
        }
        self.tab.follow_caret = true;
    }

    /// Run a (remappable) command.
    pub fn run_command(&mut self, cmd: Command) {
        match cmd {
            Command::Save => {
                self.after_save = None;
                self.save_active(false);
            }
            Command::SaveAs => self.open_prompt(PromptKind::SaveAs),
            Command::Open => self.open_prompt(PromptKind::Open),
            Command::NewTab => {
                self.add_tab(Tab::untitled(self.tab_opts()));
                self.info("New file");
            }
            Command::CloseTab => self.request_close_tab(),
            Command::Quit => self.request_quit(),
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            Command::Copy => self.copy(),
            Command::Cut => self.cut(),
            Command::Paste => {
                let t = self.clipboard.paste();
                self.paste_text(&t);
            }
            Command::SelectAll => {
                self.tab.buf.sel = Selection { anchor: 0, head: self.tab.buf.len_chars() };
                self.tab.buf.break_undo_group();
                self.schedule_primary();
            }
            Command::Find => self.open_find(false),
            Command::Replace => self.open_find(true),
            Command::FindNext => self.find_step(true),
            Command::FindPrev => self.find_step(false),
            Command::GotoLine => self.open_prompt(PromptKind::GotoLine),
            Command::NextTab => self.cycle_tab(1),
            Command::PrevTab => self.cycle_tab(-1),
            Command::ToggleWrap => self.toggle_wrap(),
            Command::ToggleLineNumbers => self.line_numbers = !self.line_numbers,
            Command::ToggleWhitespace => {
                self.show_whitespace = !self.show_whitespace;
                self.info(if self.show_whitespace { "Whitespace shown" } else { "Whitespace hidden" });
            }
            Command::ToggleMouse => self.toggle_mouse(),
            // Scrolling leaves the caret where it is (and off-screen if need be).
            Command::ScrollUp => return self.scroll_rows(-1),
            Command::ScrollDown => return self.scroll_rows(1),
            Command::Help => self.show_help = true,
            Command::Preferences => self.open_prefs(),
            Command::HighlightWord => self.highlight_word(),
            Command::Encoding => self.encoding_menu(),
            Command::TableMenu => self.table_menu(),
            Command::FormatTable => self.format_table(),
        }
        self.tab.follow_caret = true;
    }

    /// Keyboard selections publish the primary selection once they settle,
    /// so holding Shift+arrow doesn't spawn a clipboard tool per keypress.
    fn schedule_primary(&mut self) {
        if self.primary_selection {
            self.primary_due = Some(Instant::now() + Duration::from_millis(400));
        }
    }

    fn publish_primary(&mut self) {
        if self.primary_selection && !self.tab.buf.sel.is_empty() {
            let text = self.tab.buf.selected_text();
            self.clipboard.set_primary(&text);
        }
    }

    fn page_rows(&self) -> usize {
        (self.regions.text.height as usize).saturating_sub(1).max(1)
    }

    fn move_to(&mut self, pos: usize, extend: bool) {
        let pos = self.tab.buf.snap(pos);
        if extend {
            self.tab.buf.sel.head = pos;
            self.schedule_primary();
        } else {
            self.tab.buf.sel = Selection::caret(pos);
        }
        self.tab.buf.preferred_col = None;
        self.tab.buf.break_undo_group();
    }

    fn move_horiz(&mut self, right: bool, word: bool, extend: bool) {
        let sel = self.tab.buf.sel;
        // Collapsing a selection with plain Left/Right goes to its edge.
        if !extend && !sel.is_empty() && !word {
            self.move_to(if right { sel.end() } else { sel.start() }, false);
            return;
        }
        let h = sel.head;
        let to = match (right, word) {
            (true, false) => self.tab.buf.next_grapheme(h),
            (false, false) => self.tab.buf.prev_grapheme(h),
            (true, true) => self.tab.buf.word_right(h),
            (false, true) => self.tab.buf.word_left(h),
        };
        self.move_to(to, extend);
    }

    fn move_vert(&mut self, delta: isize, extend: bool) {
        let head = self.tab.buf.sel.head;
        let line = self.tab.buf.line_of(head);
        let rows = self.rows_of(line);
        let idx = row_index_for(&rows, head);
        let col = self.tab.buf.preferred_col.unwrap_or_else(|| rows[idx].col_of(head));

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
            } else if step > 0 && line + 1 < self.tab.buf.len_lines() {
                line += 1;
                rows = self.rows_of(line);
                idx = 0;
            } else {
                // Top/bottom of document: go to start/end like GUI editors.
                let to = if step < 0 { 0 } else { self.tab.buf.len_chars() };
                self.move_to(to, extend);
                return;
            }
            remaining -= step;
        }
        let to = rows[idx as usize].pos_at_col(col);
        self.move_to(to, extend);
        self.tab.buf.preferred_col = Some(col);
    }

    /// Home toggles between first non-blank char and the start of the row.
    fn smart_home(&self) -> usize {
        let head = self.tab.buf.sel.head;
        let line = self.tab.buf.line_of(head);
        let rows = self.rows_of(line);
        let row = &rows[row_index_for(&rows, head)];
        if row.row_in_line > 0 {
            return row.start;
        }
        let start = self.tab.buf.line_start(line);
        let end = self.tab.buf.line_end(line);
        let rope = self.tab.buf.rope();
        let mut first = start;
        while first < end && matches!(rope.char(first), ' ' | '\t') {
            first += 1;
        }
        if head == first { start } else { first }
    }

    fn visual_row_end(&self) -> usize {
        let head = self.tab.buf.sel.head;
        let rows = self.rows_of(self.tab.buf.line_of(head));
        let row = &rows[row_index_for(&rows, head)];
        if row.is_last { row.end } else { row.glyphs.last().map_or(row.start, |g| g.start) }
    }

    /// Lines touched by the selection (a selection ending at column 0 does
    /// not include that last line).
    fn selected_lines(&self) -> std::ops::RangeInclusive<usize> {
        let sel = self.tab.buf.sel;
        let first = self.tab.buf.line_of(sel.start());
        let mut last = self.tab.buf.line_of(sel.end());
        if last > first && self.tab.buf.line_start(last) == sel.end() {
            last -= 1;
        }
        first..=last
    }

    fn tab(&mut self) {
        let lines = self.selected_lines();
        if lines.start() != lines.end() {
            // Multi-line selection: indent every non-blank line.
            let unit = self.tab.indent_unit.clone();
            let ops = lines
                .filter(|&l| self.tab.buf.line_end(l) > self.tab.buf.line_start(l))
                .map(|l| (self.tab.buf.line_start(l), 0, unit.clone()))
                .collect();
            self.tab.buf.transact(ops);
            return;
        }
        if self.tab.indent_unit == "\t" {
            self.tab.buf.insert("\t", EditKind::Typing);
        } else {
            // Spaces up to the next tab stop.
            let start = self.tab.buf.sel.start();
            let rows = self.rows_of(self.tab.buf.line_of(start));
            let col = rows[row_index_for(&rows, start)].col_of(start);
            let n = self.tab_width - col % self.tab_width;
            self.tab.buf.insert(&" ".repeat(n), EditKind::Typing);
        }
    }

    fn outdent(&mut self) {
        let rope = self.tab.buf.rope();
        let mut ops = Vec::new();
        for l in self.selected_lines() {
            let start = self.tab.buf.line_start(l);
            let end = self.tab.buf.line_end(l);
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
            self.tab.buf.transact(ops);
        }
    }

    fn newline(&mut self) {
        // Auto-indent: copy the current line's leading whitespace.
        let head = self.tab.buf.sel.start();
        let line = self.tab.buf.line_of(head);
        let start = self.tab.buf.line_start(line);
        let rope = self.tab.buf.rope();
        let mut indent = String::new();
        let mut p = start;
        while p < head && matches!(rope.char(p), ' ' | '\t') {
            indent.push(rope.char(p));
            p += 1;
        }
        let text = format!("{}{}", self.tab.doc.line_ending.as_str(), indent);
        self.tab.buf.insert(&text, EditKind::Other);
    }

    fn undo(&mut self) {
        if !self.tab.buf.undo() {
            self.info("Nothing to undo");
        }
    }

    fn redo(&mut self) {
        if !self.tab.buf.redo() {
            self.info("Nothing to redo");
        }
    }

    fn copy(&mut self) {
        if let Some((tsv, n)) = self.cell_sel_text() {
            let dest = self.clipboard.copy(&tsv);
            self.info(format!("Copied {n} cells to {dest} as tab-separated text"));
            return;
        }
        if self.tab.buf.sel.is_empty() {
            return;
        }
        let text = self.tab.buf.selected_text();
        let dest = self.clipboard.copy(&text);
        self.info(format!("Copied {} chars to {dest}", text.chars().count()));
    }

    fn cut(&mut self) {
        if let Some((tsv, n)) = self.cell_sel_text() {
            self.clipboard.copy(&tsv);
            self.clear_cell_sel();
            self.info(format!("Cut {n} cells (tab-separated)"));
            return;
        }
        if self.tab.buf.sel.is_empty() {
            return;
        }
        let text = self.tab.buf.selected_text();
        self.clipboard.copy(&text);
        self.tab.buf.delete((0, 0), EditKind::Other);
    }

    /// Insert pasted text literally as one undo step, adopting the document's
    /// line ending (terminals often deliver newlines as CR).
    fn paste_text(&mut self, text: &str) {
        if text.is_empty() || self.table_paste(text) {
            return;
        }
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let normalized = if self.tab.doc.line_ending.as_str() == "\n" {
            normalized
        } else {
            normalized.replace('\n', self.tab.doc.line_ending.as_str())
        };
        self.tab.buf.insert(&normalized, EditKind::Other);
        self.tab.buf.break_undo_group();
    }

    fn toggle_wrap(&mut self) {
        self.wrap = !self.wrap;
        self.tab.top_row = 0;
        self.tab.scroll_x = 0;
        self.tab.table_scroll_x = 0;
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

    /// Save the active tab. Without `force`, a file that changed on disk
    /// raises a dialog instead of being overwritten.
    fn save_active(&mut self, force: bool) -> SaveOutcome {
        if self.tab.doc.path.is_none() {
            self.open_prompt(PromptKind::SaveAs);
            return SaveOutcome::Waiting;
        }
        if self.tab.doc.inexact {
            let name = self.tab.doc.display_name();
            let enc = self.tab.doc.encoding.label();
            self.dialog = Some(Dialog::new(
                "Not decoded exactly",
                vec![
                    format!("{name} couldn't be read exactly as {enc}."),
                    "Saving over it would change bytes you haven't edited.".into(),
                    format!("Save As writes a copy; {} reopens it in another encoding.", self.keymap.label(Command::Encoding)),
                ],
                vec![
                    DButton::new("Save &Anyway", Action::SaveInexact),
                    DButton::new("Save &As…", Action::SaveAs),
                    DButton::new("&Cancel", Action::Cancel),
                ],
            ));
            return SaveOutcome::Waiting;
        }
        if !force && self.tab.doc.changed_on_disk() {
            let name = self.tab.doc.display_name();
            let gone = !self.tab.doc.path.as_ref().is_some_and(|p| p.exists());
            let line = if gone {
                format!("{name} was deleted or moved on disk since it was opened.")
            } else {
                format!("{name} has changed on disk since it was opened.")
            };
            self.dialog = Some(Dialog::new(
                "File changed on disk",
                vec![line],
                vec![
                    DButton::new("&Overwrite", Action::Overwrite),
                    DButton::new("&Reload (lose my changes)", Action::ReloadFromDisk),
                    DButton::new("Save &As…", Action::SaveAs),
                    DButton::new("&Cancel", Action::Cancel),
                ],
            ));
            return SaveOutcome::Waiting;
        }
        let text = self.tab.buf.rope().to_string();
        match self.tab.doc.save(&text) {
            Ok(()) => {
                self.tab.buf.mark_saved();
                self.tab.disk_conflict = false;
                self.info(format!("Saved {}", self.tab.doc.display_name()));
                SaveOutcome::Saved
            }
            Err(e) => {
                if let Some(u) = e.downcast_ref::<encoding::Unmappable>() {
                    // Keep `after_save`: converting continues the chain.
                    self.unmappable_dialog(&u.clone(), Action::SaveUtf8);
                    return SaveOutcome::Waiting;
                }
                self.after_save = None;
                self.warn(format!("Save failed: {e:#}"));
                SaveOutcome::Failed
            }
        }
    }

    /// A char the document's encoding can't store: select it and offer to
    /// save as UTF-8 instead (`then`).
    fn unmappable_dialog(&mut self, u: &encoding::Unmappable, then: Action) {
        let rope = self.tab.buf.rope();
        let c = rope.byte_to_char(u.offset.min(rope.len_bytes()));
        let line = rope.char_to_line(c);
        let col = c - rope.line_to_char(line) + 1;
        self.tab.buf.sel = Selection { anchor: c, head: self.tab.buf.next_grapheme(c) };
        self.tab.follow_caret = true;
        self.dialog = Some(Dialog::new(
            format!("Can't save in {}", self.tab.doc.encoding.label()),
            vec![format!("{u} (line {}, column {col}; selected).", line + 1), "Save as UTF-8 instead, or change the text?".into()],
            vec![DButton::new("Save as &UTF-8", then), DButton::new("&Cancel", Action::Cancel)],
        ));
    }

    /// Switch the active document to UTF-8 (keeping any raw-byte placeholders).
    fn use_utf8(&mut self) {
        let raw = self.tab.doc.encoding.raw;
        self.tab.doc.encoding = Encoding { raw, ..Encoding::UTF8 };
    }

    /// Save As from the prompt: confirm before replacing another file.
    fn save_as(&mut self, input: &str) {
        let path = std::path::absolute(expand_tilde(input)).unwrap_or_else(|_| expand_tilde(input));
        if self.tab.doc.inexact && self.tab.shows(&path) {
            // Saving over the original: ask as a plain save would.
            if self.save_active(false) == SaveOutcome::Saved {
                self.continue_after_save();
            }
            return;
        }
        if path.exists() && !self.tab.shows(&path) {
            let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
            self.dialog = Some(Dialog::new(
                "Replace file?",
                vec![format!("{name} already exists. Replace it?")],
                vec![DButton::new("&Replace", Action::Replace(path)), DButton::new("&Cancel", Action::Cancel)],
            ));
            return;
        }
        if self.save_as_path(path) {
            self.continue_after_save();
        }
    }

    fn save_as_path(&mut self, path: PathBuf) -> bool {
        let old = self.tab.doc.path.replace(path);
        let old_stamp = self.tab.doc.stamp.take();
        let text = self.tab.buf.rope().to_string();
        match self.tab.doc.save(&text) {
            Ok(()) => {
                self.tab.buf.mark_saved();
                self.tab.disk_conflict = false;
                self.info(format!("Saved as {}", self.tab.doc.display_name()));
                if let (Some(from), Some(to)) = (old.as_deref(), self.tab.doc.path.clone())
                    && let Err(e) = self.doc_rules.carry_over(from, &to)
                {
                    self.warn(format!("Document highlights not copied: {e:#}"));
                }
                true
            }
            Err(e) => {
                let path = std::mem::replace(&mut self.tab.doc.path, old);
                self.tab.doc.stamp = old_stamp;
                if let (Some(u), Some(path)) = (e.downcast_ref::<encoding::Unmappable>(), path) {
                    self.unmappable_dialog(&u.clone(), Action::SaveAsUtf8(path));
                    return false;
                }
                self.after_save = None;
                self.warn(format!("Save failed: {e:#}"));
                false
            }
        }
    }

    /// Groups for the active tab, in priority order: this document's own,
    /// then the config's, then the samples.
    pub fn active_highlights(&self) -> Vec<crate::highlight::Group> {
        if !self.highlights.enabled {
            return Vec::new();
        }
        let path = self.tab.doc.path.as_deref();
        let mut v: Vec<_> = path
            .map(|p| self.doc_rules.groups(p).iter().filter(|g| g.enabled && !g.terms.is_empty()).cloned().collect())
            .unwrap_or_default();
        v.extend(self.highlights.active(path));
        v
    }

    /// Toggle the selected text (or the word at the caret) in this
    /// document's "Marked" highlight group.
    fn highlight_word(&mut self) {
        let Some(path) = self.tab.doc.path.clone() else {
            self.warn("Save the file first: document highlights are kept by file path");
            return;
        };
        let sel = self.tab.buf.sel;
        let (s, e) = if sel.is_empty() { self.tab.buf.word_at(sel.head) } else { (sel.start(), sel.end()) };
        let term: String = self.tab.buf.rope().slice(s..e).into();
        let term = term.trim();
        if term.is_empty() || term.contains('\n') || !term.chars().any(crate::buffer::is_word_char) {
            self.warn("Select a word or phrase on one line to highlight");
            return;
        }
        let label = self.keymap.label(Command::HighlightWord);
        match self.doc_rules.toggle_marked(&path, term) {
            Ok(true) => self.info(format!("Highlighting \u{201c}{term}\u{201d} in this document ({label} again to remove)")),
            Ok(false) => self.info(format!("Removed highlight \u{201c}{term}\u{201d}")),
            Err(e) => self.warn(format!("Could not save document highlights: {e:#}")),
        }
    }

    /// Tell the user anything unusual about how the active tab was read:
    /// its encoding was guessed, bytes were kept raw, decoding wasn't exact,
    /// or line endings are mixed.
    pub fn announce_open(&mut self) {
        use crate::encoding::How;
        let d = &self.tab.doc;
        let name = d.display_name();
        let enc = d.encoding.label();
        let key = self.keymap.label(Command::Encoding);
        if d.inexact {
            self.warn(format!("{name} isn't exact as {enc}: saving over it will ask first ({key}: try another encoding)"));
        } else if d.raw_bytes > 0 {
            let n = d.raw_bytes;
            self.warn(format!("{name}: {n} byte{} not valid {enc}, shown as \\xNN and saved unchanged", if n == 1 { " is" } else { "s are" }));
        } else if d.mixed_line_endings {
            self.warn("File has mixed line endings; they are preserved as-is");
        } else {
            match d.how {
                How::Guess => self.info(format!("Opened as {enc} (a guess; {key} to change)")),
                How::Fallback => self.info(format!("Opened as {enc} (fallback encoding; {key} to change)")),
                How::Utf16Pattern => self.info(format!("Opened as {enc} (no byte-order mark)")),
                How::Bom | How::Utf8 | How::Chosen | How::Kept => {}
            }
        }
    }

    /// The encoding menu: reopen the file as another encoding, or save it in one.
    fn encoding_menu(&mut self) {
        use crate::encoding::How;
        let d = &self.tab.doc;
        let mut what = d.encoding.label();
        match d.how {
            How::Guess => what.push_str(" (guessed)"),
            How::Fallback => what.push_str(" (fallback)"),
            How::Utf16Pattern => what.push_str(" (no byte-order mark)"),
            _ => {}
        }
        if d.inexact {
            what.push_str(", not decoded exactly");
        } else if d.raw_bytes > 0 {
            what.push_str(&format!(", {} raw byte(s)", d.raw_bytes));
        }
        let mut lines = vec![format!("{} is {what}.", d.display_name())];
        let mut buttons = Vec::new();
        if d.path.as_ref().is_some_and(|p| p.exists()) {
            lines.push("Reopen with: read the file again in another encoding.".into());
            buttons.push(DButton::new("&Reopen with…", Action::EncReopen));
        }
        lines.push("Save with: write it in another encoding.".into());
        buttons.push(DButton::new("&Save with…", Action::EncSave));
        buttons.push(DButton::new("&Cancel", Action::Cancel));
        self.dialog = Some(Dialog::new("Encoding", lines, buttons));
    }

    fn open_encoding_picker(&mut self, saving: bool) {
        let cur = self.tab.doc.encoding;
        let items = encoding::menu(saving)
            .into_iter()
            .map(|(e, detail)| picker::Item {
                label: e.label(),
                detail,
                current: e.enc == cur.enc && (!saving || e.bom == cur.bom),
                value: if saving { PickChoice::SaveWith(e) } else { PickChoice::Reopen(e) },
            })
            .collect();
        let title = if saving { "Save with encoding" } else { "Reopen with encoding" };
        self.picker = Some(Picker::new(title, items));
    }

    fn pick_done(&mut self, c: PickChoice) {
        self.picker = None;
        match c {
            PickChoice::Reopen(e) => self.reopen_as(e),
            PickChoice::SaveWith(e) => self.save_with(e),
            PickChoice::Table(a) => self.table_action(a),
        }
        self.tab.follow_caret = true;
    }

    /// The table menu (its actions also have their own commands).
    pub fn table_menu(&mut self) {
        use crate::table::Style;
        let Some(cur) = self.caret_table() else {
            // Not in a table: offer to insert one.
            let item = |label: &str, detail: &str, style| picker::Item { label: label.to_string(), detail: detail.to_string(), current: false, value: PickChoice::Table(TableAction::New(style)) };
            let items = vec![
                item("New Markdown table", "| a | b |", Style::Markdown),
                item("New Unicode box table", "┌───┬───┐", Style::Unicode),
                item("New ASCII table", "+---+---+", Style::Ascii),
            ];
            let mut items = items;
            items.push(picker::Item { label: "How tables work".into(), detail: String::new(), current: false, value: PickChoice::Table(TableAction::Help) });
            self.picker = Some(Picker::new("Insert table", items));
            return;
        };
        let item = |label: &str, cmd: Option<Command>, value| picker::Item {
            label: label.to_string(),
            detail: cmd.map(|c| self.keymap.label(c)).filter(|l| l != "unbound").unwrap_or_default(),
            current: false,
            value: PickChoice::Table(value),
        };
        use crate::table::Align;
        let items = vec![
            item("Format table", Some(Command::FormatTable), TableAction::Format),
            item("Insert row above", None, TableAction::RowAbove),
            item("Insert row below", None, TableAction::RowBelow),
            item("Delete row", None, TableAction::DeleteRow),
            item("Move row up (Alt+↑)", None, TableAction::RowUp),
            item("Move row down (Alt+↓)", None, TableAction::RowDown),
            item("Insert column left", None, TableAction::ColLeft),
            item("Insert column right", None, TableAction::ColRight),
            item("Delete column", None, TableAction::DeleteCol),
            item("Move column left", None, TableAction::MoveColLeft),
            item("Move column right", None, TableAction::MoveColRight),
            item("Align column left", None, TableAction::Align(Align::Left)),
            item("Align column centre", None, TableAction::Align(Align::Center)),
            item("Align column right", None, TableAction::Align(Align::Right)),
            item("Align column: none", None, TableAction::Align(Align::None)),
            item("Select row", None, TableAction::SelectRow),
            item("Select column", None, TableAction::SelectCol),
            item("Select table (Ctrl+C copies as tab-separated)", None, TableAction::SelectAll),
        ];
        let mut items = items;
        items.push(item("How tables work", None, TableAction::Help));
        for style in [Style::Markdown, Style::Unicode, Style::Ascii] {
            if style != cur.style {
                items.push(picker::Item {
                    label: format!("Convert to {} table", style.label()),
                    detail: String::new(),
                    current: false,
                    value: PickChoice::Table(TableAction::Convert(style)),
                });
            }
        }
        self.picker = Some(Picker::new(format!("{} table", cur.style.label()), items));
    }

    /// Read the active file again, decoded as `e` (one undoable step).
    fn reopen_as(&mut self, e: Encoding) {
        let name = self.tab.doc.display_name();
        let dirty = self.tab.buf.is_dirty();
        match self.tab.reload_from_disk(Some(e)) {
            Ok(()) => {
                let undo = if dirty { " (Ctrl+Z brings back your edits)" } else { "" };
                self.info(format!("Reopened {name} as {}{undo}", self.tab.doc.encoding.label()));
                if self.tab.doc.inexact || self.tab.doc.raw_bytes > 0 {
                    self.announce_open();
                }
            }
            Err(err) => self.warn(format!("Could not reopen {name}: {err:#}")),
        }
    }

    /// Save the active document in encoding `e` from now on.
    fn save_with(&mut self, e: Encoding) {
        let text = self.tab.buf.rope().to_string();
        let raw = e.enc == self.tab.doc.encoding.enc && self.tab.doc.encoding.raw;
        let e = Encoding { raw, ..e };
        if let Err(u) = e.encode(&text) {
            self.unmappable_dialog(&u, Action::SaveUtf8);
            // The dialog's offer is about the target encoding.
            if let Some(d) = &mut self.dialog {
                d.title = format!("Can't save in {}", e.label());
                d.lines[1] = "Change the text, or choose another encoding.".into();
                d.buttons.retain(|b| b.action == Action::Cancel);
            }
            return;
        }
        let old = std::mem::replace(&mut self.tab.doc.encoding, e);
        self.after_save = None;
        match self.save_active(false) {
            SaveOutcome::Saved => self.info(format!("Saved {} as {}", self.tab.doc.display_name(), e.label())),
            SaveOutcome::Failed => self.tab.doc.encoding = old,
            SaveOutcome::Waiting => {}
        }
    }

    fn open_path(&mut self, path: &str) {
        self.open_file(&expand_tilde(path));
    }

    /// Open a file in a tab (switching to it if it is already open).
    /// Returns true on success.
    fn open_file(&mut self, path: &Path) -> bool {
        if let Some(i) = (0..self.tab_count()).find(|&i| self.tab_at(i).shows(path)) {
            self.switch_to(i);
            self.info(format!("{} is already open", self.tab.doc.display_name()));
            return true;
        }
        match Tab::open(path, self.tab_opts(), None) {
            Ok((tab, is_new)) => {
                self.add_tab(tab);
                let name = self.tab.doc.display_name();
                self.info(if is_new { format!("New file {name}") } else { format!("Opened {name}") });
                self.announce_open();
                true
            }
            Err(e) => {
                self.warn(format!("{e:#}"));
                false
            }
        }
    }

    /// Files dropped onto the terminal window: each opens in its own tab.
    fn open_dropped(&mut self, paths: &[PathBuf]) {
        if paths.len() == 1 && paths[0].is_dir() {
            // Let the user pick a file inside the dropped folder.
            let mut input = paths[0].display().to_string();
            if !input.ends_with('/') {
                input.push('/');
            }
            self.prompt = Some(Prompt::new(PromptKind::Open, &input));
            return;
        }
        let files: Vec<&PathBuf> = paths.iter().filter(|p| !p.is_dir()).collect();
        let mut opened = 0;
        for p in &files {
            if self.open_file(p) {
                opened += 1;
            }
        }
        if opened > 1 {
            self.info(format!("Opened {opened} dropped files"));
        }
    }

    // ---- prompt / find ---------------------------------------------------------------

    fn open_prompt(&mut self, kind: PromptKind) {
        let input = match kind {
            PromptKind::SaveAs => self.tab.doc.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
            _ => String::new(),
        };
        self.prompt = Some(Prompt::new(kind, &input));
    }

    fn on_prompt_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(p) = &mut self.prompt else { return };
        let kind = p.kind;
        match k.code {
            KeyCode::Esc => {
                self.prompt = None;
                self.after_save = None;
            }
            KeyCode::Enter => {
                let input = p.input.text().to_string();
                match kind {
                    PromptKind::GotoLine => self.goto_line(&input),
                    PromptKind::PalettePath => self.set_palette_path(&input),
                    PromptKind::SaveAs if !input.is_empty() => self.save_as(&input),
                    PromptKind::Open if !input.is_empty() => self.open_path(&input),
                    PromptKind::HlName | PromptKind::HlTerms | PromptKind::HlFileTypes | PromptKind::HlFg | PromptKind::HlBg => {
                        self.hl_prompt_done(kind, &input)
                    }
                    _ => {}
                }
                self.prompt = None;
                self.tab.follow_caret = true;
            }
            KeyCode::Char('v') if ctrl => {
                let t = self.clipboard.paste();
                if let Some(p) = &mut self.prompt {
                    p.input.insert(&t);
                }
            }
            _ => {
                p.input.key(&k);
            }
        }
    }

    fn goto_line(&mut self, input: &str) {
        match input.trim().parse::<usize>() {
            Ok(n) if n >= 1 => {
                let line = (n - 1).min(self.tab.buf.len_lines() - 1);
                let pos = self.tab.buf.line_start(line);
                self.move_to(pos, false);
            }
            _ => self.warn("Not a line number"),
        }
    }

    // ---- mouse -------------------------------------------------------------------------

    fn on_mouse(&mut self, m: MouseEvent) {
        self.last_mouse = (m.column, m.row);
        let alt = m.modifiers.contains(KeyModifiers::ALT);
        let shift = m.modifiers.contains(KeyModifiers::SHIFT);
        if let (MouseEventKind::Down(MouseButton::Left), Some(p)) = (m.kind, &mut self.prompt)
            && self.dialog.is_none()
            && contains(self.regions.prompt_field, m.column, m.row)
        {
            p.input.click((m.column - self.regions.prompt_field.x) as usize);
            return;
        }
        if self.picker.is_some() && self.dialog.is_none() {
            self.on_picker_mouse(m);
            return;
        }
        match m.kind {
            _ if self.dialog.is_some() => {
                if let MouseEventKind::Down(MouseButton::Left) = m.kind
                    && let Some(&(_, i)) = self.regions.dialog_buttons.iter().find(|(r, _)| contains(*r, m.column, m.row))
                {
                    let action = self.dialog.as_ref().map(|d| d.buttons[i].action.clone());
                    if let Some(a) = action {
                        self.run_action(a);
                    }
                }
            }
            _ if self.prefs.is_some() => {
                if let MouseEventKind::Down(MouseButton::Left) = m.kind
                    && self.prompt.is_none()
                    && let Some(&(_, hit)) = self.regions.prefs_hits.iter().find(|(r, _)| contains(*r, m.column, m.row))
                {
                    self.on_prefs_click(hit);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.cell_sel = None;
                if self.show_help {
                    self.show_help = false;
                    return;
                }
                if let Some(hit) = self.tab_hit_at(m.column, m.row) {
                    self.on_tab_bar_click(hit);
                    return;
                }
                if let Some(&(r, hit)) = self.regions.find_hits.iter().find(|(r, _)| contains(*r, m.column, m.row)) {
                    self.on_find_click(hit, (m.column - r.x) as usize);
                    return;
                }
                if let Some(b) = self.button_at(m.column, m.row) {
                    self.press_button(b);
                    return;
                }
                if contains(self.regions.status_encoding, m.column, m.row) {
                    self.encoding_menu();
                    return;
                }
                if contains(self.regions.scrollbar, m.column, m.row) {
                    self.dragging_scrollbar = true;
                    self.scrollbar_jump(m.row);
                    return;
                }
                let Some(pos) = self.pos_at_screen(m.column, m.row) else { return };
                let count = self.click_count(m.column, m.row);
                if count == 1 && !shift && !alt && !contains(self.regions.gutter, m.column, m.row) && self.click_selects_column(pos) {
                    return;
                }
                if contains(self.regions.gutter, m.column, m.row) {
                    // Gutter: select whole lines; drag extends by lines.
                    let range = self.line_range(pos);
                    if shift || alt {
                        let anchor = self.tab.buf.sel.start().min(range.0);
                        self.tab.buf.sel = Selection { anchor, head: range.1.max(self.tab.buf.sel.end()) };
                    } else {
                        self.tab.buf.sel = Selection { anchor: range.0, head: range.1 };
                    }
                    self.drag = Some((DragUnit::Line, Selection { anchor: range.0, head: range.1 }));
                    self.tab.buf.preferred_col = None;
                    self.tab.buf.break_undo_group();
                    return;
                }
                if (shift || alt) && count == 1 {
                    // Extend selection. Alt+click is the fallback for terminals
                    // that reserve Shift+click (stock Kitty, many others).
                    self.tab.buf.sel.head = pos;
                    self.drag = Some((DragUnit::Char, Selection::caret(self.tab.buf.sel.anchor)));
                } else {
                    let (unit, range) = match count {
                        1 => (DragUnit::Char, (pos, pos)),
                        2 => (DragUnit::Word, self.tab.buf.word_at(pos)),
                        _ => (DragUnit::Line, self.line_range(pos)),
                    };
                    self.tab.buf.sel = Selection { anchor: range.0, head: range.1 };
                    self.drag = Some((unit, Selection { anchor: range.0, head: range.1 }));
                }
                self.tab.buf.preferred_col = None;
                self.tab.buf.break_undo_group();
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.dragging_scrollbar {
                    self.scrollbar_jump(m.row);
                } else {
                    self.on_drag(m.column, m.row);
                }
            }
            MouseEventKind::Up(_) => {
                if self.drag.take().is_some() {
                    self.publish_primary();
                }
                self.dragging_scrollbar = false;
            }
            MouseEventKind::Down(MouseButton::Right) if contains(self.regions.text, m.column, m.row) => {
                // Right-click in a table: caret there (unless inside the
                // selection) and the table menu.
                if let Some(pos) = self.pos_at_screen(m.column, m.row)
                    && self.tab.table_at(self.tab.buf.line_of(pos)).is_some()
                {
                    let sel = self.tab.buf.sel;
                    if sel.is_empty() || pos < sel.start() || pos > sel.end() {
                        self.tab.buf.sel = Selection::caret(pos);
                    }
                    self.table_menu();
                }
            }
            MouseEventKind::Down(MouseButton::Middle) if self.primary_selection && contains(self.regions.text, m.column, m.row) => {
                // Linux convention: paste the primary selection at the pointer.
                if let Some(pos) = self.pos_at_screen(m.column, m.row) {
                    let text = self.clipboard.paste_primary();
                    if !text.is_empty() {
                        self.tab.buf.sel = Selection::caret(pos);
                        self.tab.buf.break_undo_group();
                        self.paste_text(&text);
                        self.tab.follow_caret = true;
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Middle) => {
                if let Some(TabHit::Select(i) | TabHit::Close(i)) = self.tab_hit_at(m.column, m.row) {
                    self.on_tab_bar_click(TabHit::Close(i));
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if contains(self.regions.tab_bar, m.column, m.row) =>
            {
                self.cycle_tab(if m.kind == MouseEventKind::ScrollUp { -1 } else { 1 });
            }
            MouseEventKind::ScrollUp if shift => self.scroll_horiz_back(SCROLL_STEP * 2),
            MouseEventKind::ScrollDown if shift => self.scroll_horiz(SCROLL_STEP * 2),
            MouseEventKind::ScrollLeft => self.scroll_horiz_back(SCROLL_STEP * 2),
            MouseEventKind::ScrollRight => self.scroll_horiz(SCROLL_STEP * 2),
            MouseEventKind::ScrollUp => self.scroll_rows(-(SCROLL_STEP as isize)),
            MouseEventKind::ScrollDown => self.scroll_rows(SCROLL_STEP as isize),
            _ => {}
        }
    }

    /// Sideways scroll: the view without soft wrap, else the caret's table.
    fn scroll_horiz(&mut self, n: usize) {
        if !self.wrap {
            self.tab.scroll_x += n;
        } else if self.caret_table().is_some() {
            self.tab.table_scroll_x += n;
        }
    }

    fn scroll_horiz_back(&mut self, n: usize) {
        if !self.wrap {
            self.tab.scroll_x = self.tab.scroll_x.saturating_sub(n);
        } else {
            self.tab.table_scroll_x = self.tab.table_scroll_x.saturating_sub(n);
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
            if x < t.x && self.tab.scroll_x > 0 {
                self.tab.scroll_x -= 1;
            } else if x >= t.x + t.width {
                self.tab.scroll_x += 1;
            }
        }
        let cy = y.clamp(t.y, t.y + t.height.saturating_sub(1));
        let cx = x.clamp(t.x, t.x + t.width);
        let Some(pos) = self.pos_at_screen(cx, cy) else { return };
        let (s, e) = match unit {
            DragUnit::Char => {
                // Dragging from one table cell into another selects cells.
                let line = self.tab.buf.line_of(origin.anchor);
                if let Some(t) = self.tab.table_at(line)
                    && self.tab.buf.line_of(pos) >= t.first_line()
                    && self.tab.buf.line_of(pos) <= t.last_line()
                    && let (Some(a), Some(h)) = (t.cell_at(self.tab.buf.rope(), origin.anchor), t.cell_at(self.tab.buf.rope(), pos))
                    && a != h
                {
                    self.set_cell_sel(&t, a, h);
                    return;
                }
                self.cell_sel = None;
                self.tab.buf.sel = Selection { anchor: origin.anchor, head: pos };
                return;
            }
            DragUnit::Word => self.tab.buf.word_at(pos),
            DragUnit::Line => self.line_range(pos),
        };
        // Keep the originally clicked word/line selected, extend toward the pointer.
        self.tab.buf.sel = if s < origin.start() {
            Selection { anchor: origin.end(), head: s }
        } else {
            Selection { anchor: origin.start(), head: e.max(origin.end()) }
        };
    }

    fn line_range(&self, pos: usize) -> (usize, usize) {
        let line = self.tab.buf.line_of(pos);
        let start = self.tab.buf.line_start(line);
        let end = if line + 1 < self.tab.buf.len_lines() { self.tab.buf.line_start(line + 1) } else { self.tab.buf.len_chars() };
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
            return Some(self.tab.buf.len_chars());
        };
        let sx = self.rendered_scroll.get(i).copied().unwrap_or_else(|| self.row_scroll_x(row.line));
        let col = (x.saturating_sub(t.x)) as usize + sx;
        Some(row.pos_at_col(col))
    }

    #[cfg(test)]
    pub fn pos_at_screen_pub(&self, x: u16, y: u16) -> Option<usize> {
        self.pos_at_screen(x, y)
    }

    fn tab_hit_at(&self, x: u16, y: u16) -> Option<TabHit> {
        // Close markers are listed before their tab, so they win.
        self.regions.tab_hits.iter().find(|(r, _)| contains(*r, x, y)).map(|(_, h)| *h)
    }

    fn on_picker_mouse(&mut self, m: MouseEvent) {
        let Some(p) = &mut self.picker else { return };
        let (x, y) = (m.column, m.row);
        let r = &self.regions;
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if contains(r.picker_field, x, y) {
                    p.filter.click((x - r.picker_field.x) as usize);
                } else if contains(r.picker_list, x, y) {
                    if let picker::Outcome::Chosen(c) = p.click((y - r.picker_list.y) as usize) {
                        self.pick_done(c);
                    }
                } else if !contains(r.picker_box, x, y) {
                    self.picker = None;
                }
            }
            MouseEventKind::ScrollUp => p.wheel(false),
            MouseEventKind::ScrollDown => p.wheel(true),
            _ => {}
        }
    }

    fn button_at(&self, x: u16, y: u16) -> Option<Button> {
        self.regions.buttons.iter().find(|(r, _)| contains(*r, x, y)).map(|(_, b)| *b)
    }

    fn press_button(&mut self, b: Button) {
        match b {
            Button::Prefs => self.open_prefs(),
            Button::Save => {
                self.after_save = None;
                self.save_active(false);
            }
            Button::Table => self.table_menu(),
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
        let lines = self.tab.buf.len_lines();
        self.tab.top_line = ((frac.clamp(0.0, 1.0) * lines.saturating_sub(1) as f64).round() as usize).min(lines - 1);
        self.tab.top_row = 0;
        self.tab.follow_caret = false;
    }

    // ---- viewport ------------------------------------------------------------------------

    /// Scroll the viewport by visual rows without moving the caret.
    pub fn scroll_rows(&mut self, delta: isize) {
        self.tab.follow_caret = false;
        if delta > 0 {
            for _ in 0..delta {
                let n = self.rows_of(self.tab.top_line).len();
                if self.tab.top_row + 1 < n {
                    self.tab.top_row += 1;
                } else if self.tab.top_line + 1 < self.tab.buf.len_lines() {
                    self.tab.top_line += 1;
                    self.tab.top_row = 0;
                } else {
                    break;
                }
            }
        } else {
            for _ in 0..(-delta) {
                if self.tab.top_row > 0 {
                    self.tab.top_row -= 1;
                } else if self.tab.top_line > 0 {
                    self.tab.top_line -= 1;
                    self.tab.top_row = self.rows_of(self.tab.top_line).len() - 1;
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
        self.tab.top_line = self.tab.top_line.min(self.tab.buf.len_lines() - 1);
        self.tab.top_row = self.tab.top_row.min(self.rows_of(self.tab.top_line).len() - 1);
        if !self.tab.follow_caret {
            return;
        }
        let head = self.tab.buf.sel.head;
        let cline = self.tab.buf.line_of(head);
        let crows = self.rows_of(cline);
        let crow = row_index_for(&crows, head);

        if (cline, crow) < (self.tab.top_line, self.tab.top_row) {
            self.tab.top_line = cline;
            self.tab.top_row = crow;
        } else {
            // Count rows from top to caret; stop once past the viewport.
            let mut count = 0usize;
            let mut line = self.tab.top_line;
            let mut first = self.tab.top_row;
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
                self.tab.top_line = l;
                self.tab.top_row = r;
            }
        }

        let w = self.regions.text.width as usize;
        let col = crows[crow].col_of(head);
        let margin = 4.min(w / 4);
        let follow = |sx: &mut usize| {
            if col < *sx + margin {
                *sx = col.saturating_sub(margin);
            } else if col + margin >= *sx + w {
                *sx = col + margin + 1 - w;
            }
        };
        if !self.wrap {
            follow(&mut self.tab.scroll_x);
        } else {
            self.tab.scroll_x = 0;
            if self.caret_table().is_some() {
                follow(&mut self.tab.table_scroll_x);
            } else {
                self.tab.table_scroll_x = 0;
            }
        }
        self.tab.follow_caret = false;
    }

    /// Collect the visual rows for the viewport starting at the top position.
    pub fn visible_rows(&self, height: usize) -> Vec<VisualRow> {
        let mut out = Vec::with_capacity(height);
        let mut line = self.tab.top_line;
        let mut skip = self.tab.top_row;
        while out.len() < height && line < self.tab.buf.len_lines() {
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::buffer::Buffer;
    use ratatui::crossterm::event::{KeyEventState, MouseEvent};

    pub(crate) fn app_with(text: &str, w: u16, h: u16) -> App {
        let caps = Capabilities { keyboard_enhanced: true, truecolor: true };
        let theme = theme::resolve(&theme::dracula());
        let tab = Tab::new(Buffer::from_str(text), crate::document::Document::untitled(), TabOpts { tab_width: 4, indent: IndentMode::Auto, fallback_encoding: None });
        let mut a = App::new(tab, theme, ThemeSource::Builtin(theme::Builtin::Dracula), caps);
        a.clipboard = Clipboard::internal_only();
        a.regions.text = Rect { x: 5, y: 1, width: w, height: h };
        a.regions.gutter = Rect { x: 0, y: 1, width: 5, height: h };
        a
    }

    pub(crate) fn key(a: &mut App, code: KeyCode, mods: KeyModifiers) {
        a.handle_event(Event::Key(KeyEvent { code, modifiers: mods, kind: KeyEventKind::Press, state: KeyEventState::NONE }));
    }

    pub(crate) fn text(a: &App) -> String {
        a.tab.buf.rope().to_string()
    }

    #[test]
    fn viewport_follows_caret_down_and_up() {
        let body: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let mut a = app_with(&body, 40, 10);
        a.tab.buf.sel = Selection::caret(a.tab.buf.line_start(50));
        a.tab.follow_caret = true;
        a.ensure_caret_visible();
        assert_eq!(a.tab.top_line, 41, "caret line should be the last visible row");
        a.tab.buf.sel = Selection::caret(a.tab.buf.line_start(3));
        a.tab.follow_caret = true;
        a.ensure_caret_visible();
        assert_eq!(a.tab.top_line, 3);
    }

    #[test]
    fn vertical_motion_walks_wrapped_rows_and_keeps_column() {
        // Width 10: first line wraps into 3 rows.
        let mut a = app_with("aaaa bbbb cccc dddd eeee\nxy\n", 10, 10);
        a.tab.buf.sel = Selection::caret(2);
        key(&mut a, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(a.tab.buf.sel.head, 12, "same column on the next visual row");
        key(&mut a, KeyCode::Down, KeyModifiers::NONE);
        key(&mut a, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(a.tab.buf.sel.head, a.tab.buf.line_start(1) + 2, "short line clamps to its end");
        key(&mut a, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(a.tab.buf.sel.head, 22, "preferred column is remembered");
    }

    #[test]
    fn indent_detection_and_tab_behaviour() {
        let mut a = app_with("def f():\n    return 1\n    pass\n", 40, 10);
        assert_eq!(a.tab.indent_unit, "    ");
        a.tab.buf.sel = Selection::caret(a.tab.buf.line_start(1) + 2);
        key(&mut a, KeyCode::Tab, KeyModifiers::NONE);
        assert!(text(&a).contains("\n      return"), "Tab pads to the next tab stop");
        let a2 = app_with("x\n\ty\n", 40, 10);
        assert_eq!(a2.tab.indent_unit, "\t");
    }

    #[test]
    fn tab_and_shift_tab_on_selection_indent_lines_atomically() {
        let mut a = app_with("one\n\ntwo\nthree\n", 40, 10);
        a.tab.buf.sel = Selection { anchor: 1, head: a.tab.buf.line_start(3) }; // ends at col 0 of line 3
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
        assert_eq!(a.tab.top_line, 2);
        assert_eq!(a.tab.buf.sel.head, 0);
    }

    #[test]
    fn dropped_file_opens_instead_of_inserting_path() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("dropped file.txt");
        std::fs::write(&f, "dropped contents").unwrap();
        let mut a = app_with("", 40, 10);
        a.handle_event(Event::Paste(format!("{}\n", f.display())));
        assert_eq!(text(&a), "dropped contents");
        assert_eq!(a.tab_count(), 1, "pristine Untitled tab is replaced");
        assert_eq!(a.tab.doc.path.as_deref(), Some(f.as_path()));
        // Dropping the same file again just switches to it.
        a.handle_event(Event::Paste("x".into()));
        a.handle_event(Event::Paste(format!("'{}' ", f.display())));
        assert_eq!(a.tab_count(), 1);
        assert!(text(&a).starts_with('x'), "unsaved edit kept");
        // Ordinary paste still inserts text.
        a.handle_event(Event::Paste("/not/a/real/path".into()));
        assert!(text(&a).contains("/not/a/real/path"));
    }

    #[test]
    fn several_dropped_files_open_in_tabs_and_unsaved_work_is_kept() {
        let d = tempfile::tempdir().unwrap();
        let f1 = d.path().join("one.txt");
        let f2 = d.path().join("two.txt");
        std::fs::write(&f1, "1").unwrap();
        std::fs::write(&f2, "2").unwrap();
        let mut a = app_with("draft", 40, 10);
        a.tab.buf.sel = Selection::caret(5);
        a.handle_event(Event::Paste("!".into()));
        a.handle_event(Event::Paste(format!("{}\n{}", f1.display(), f2.display())));
        assert_eq!(a.tab_count(), 3);
        assert_eq!(text(&a), "2");
        assert_eq!(a.tab_at(0).buf.rope().to_string(), "draft!");
        assert!(a.tab_at(0).buf.is_dirty());
    }

    #[test]
    fn tab_switching_order_and_closing() {
        let mut a = app_with("A", 40, 10); // has content, so not pristine
        for c in ['B', 'C'] {
            key(&mut a, KeyCode::Char('n'), KeyModifiers::CONTROL);
            key(&mut a, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!((a.tab_count(), a.active, text(&a).as_str()), (3, 2, "C"));
        key(&mut a, KeyCode::PageUp, KeyModifiers::CONTROL);
        assert_eq!(text(&a), "B");
        key(&mut a, KeyCode::Char('1'), KeyModifiers::ALT);
        assert_eq!(text(&a), "A");
        key(&mut a, KeyCode::Char('9'), KeyModifiers::ALT);
        assert_eq!(text(&a), "C");
        key(&mut a, KeyCode::Tab, KeyModifiers::CONTROL);
        assert_eq!(text(&a), "A", "Ctrl+Tab wraps around");
        key(&mut a, KeyCode::BackTab, KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(text(&a), "C");
        // Order is preserved across switches.
        let order: Vec<String> = a.all_tabs().map(|t| t.buf.rope().to_string()).collect();
        assert_eq!(order, ["A", "B", "C"]);
        // Closing a dirty tab asks; "Don't Save" closes it.
        key(&mut a, KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert_eq!(a.tab_count(), 3);
        assert!(a.dialog.is_some());
        key(&mut a, KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!((a.tab_count(), text(&a).as_str()), (2, "B"), "last tab closed -> left neighbour");
        // Quit with one unsaved tab (B): Cancel keeps running, "Don't Save" quits.
        key(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
        key(&mut a, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!a.quit && a.dialog.is_none());
        key(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
        key(&mut a, KeyCode::Char('n'), KeyModifiers::NONE);
        assert!(a.quit);
    }

    pub(crate) fn type_str(a: &mut App, s: &str) {
        for c in s.chars() {
            key(a, KeyCode::Char(c), KeyModifiers::NONE);
        }
    }

    #[test]
    fn save_then_close_untitled_goes_through_save_as() {
        let d = tempfile::tempdir().unwrap();
        let mut a = app_with("", 40, 10);
        type_str(&mut a, "hello");
        key(&mut a, KeyCode::Char('n'), KeyModifiers::CONTROL); // second tab so closing doesn't quit
        key(&mut a, KeyCode::Char('1'), KeyModifiers::ALT);
        key(&mut a, KeyCode::Char('w'), KeyModifiers::CONTROL);
        key(&mut a, KeyCode::Char('s'), KeyModifiers::NONE); // Save
        assert_eq!(a.prompt.as_ref().map(|p| p.kind), Some(PromptKind::SaveAs));
        let target = d.path().join("named.txt");
        type_str(&mut a, &target.display().to_string());
        key(&mut a, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
        assert_eq!(a.tab_count(), 1, "tab closed after the save");
    }

    #[test]
    fn save_all_then_quit_saves_files_and_asks_names() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f.txt");
        std::fs::write(&f, "file").unwrap();
        let mut a = app_with("", 40, 10);
        type_str(&mut a, "untitled text");
        a.open_file(&f);
        key(&mut a, KeyCode::End, KeyModifiers::NONE);
        type_str(&mut a, "!");
        key(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
        key(&mut a, KeyCode::Char('a'), KeyModifiers::NONE); // Save All
        assert!(!a.quit);
        assert_eq!(a.prompt.as_ref().map(|p| p.kind), Some(PromptKind::SaveAs), "asks a name for Untitled");
        let target = d.path().join("u.txt");
        type_str(&mut a, &target.display().to_string());
        key(&mut a, KeyCode::Enter, KeyModifiers::NONE);
        assert!(a.quit);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "file!");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "untitled text");
    }

    #[test]
    fn external_change_reloads_clean_tab_undoably() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("w.txt");
        std::fs::write(&f, "line one\nline two\n").unwrap();
        let mut a = app_with("", 40, 10);
        a.open_file(&f);
        a.tab.buf.sel = Selection::caret(a.tab.buf.line_start(1) + 4);
        std::fs::write(&f, "changed one\nline two\nline three\n").unwrap();
        a.on_disk_change(&f);
        assert_eq!(text(&a), "changed one\nline two\nline three\n");
        assert!(!a.tab.buf.is_dirty());
        assert_eq!(a.tab.buf.sel.head, a.tab.buf.line_start(1) + 4, "caret keeps line/column");
        key(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(text(&a), "line one\nline two\n", "Ctrl+Z restores the previous text");
        // Our own save must not count as an external change.
        key(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
        a.on_disk_change(&f);
        assert!(a.dialog.is_none());
        assert_eq!(text(&a), "line one\nline two\n");
    }

    #[test]
    fn external_change_with_unsaved_edits_asks_and_save_asks_before_overwrite() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("c.txt");
        std::fs::write(&f, "base").unwrap();
        let mut a = app_with("", 40, 10);
        a.open_file(&f);
        type_str(&mut a, "mine ");
        std::fs::write(&f, "theirs, longer").unwrap();
        a.on_disk_change(&f);
        assert!(a.dialog.is_some(), "conflict dialog");
        key(&mut a, KeyCode::Char('k'), KeyModifiers::NONE); // Keep Mine
        assert_eq!(text(&a), "mine base");
        key(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(a.dialog.is_some(), "save asks before overwriting");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "theirs, longer");
        key(&mut a, KeyCode::Char('o'), KeyModifiers::NONE); // Overwrite
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "mine base");
    }

    #[test]
    fn background_conflict_shows_dialog_when_tab_activated() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("bg.txt");
        std::fs::write(&f, "x").unwrap();
        let mut a = app_with("", 40, 10);
        a.open_file(&f);
        type_str(&mut a, "edit");
        key(&mut a, KeyCode::Char('n'), KeyModifiers::CONTROL);
        std::fs::write(&f, "external!").unwrap();
        a.on_disk_change(&f);
        assert!(a.dialog.is_none(), "no dialog while the tab is in the background");
        key(&mut a, KeyCode::Char('1'), KeyModifiers::ALT);
        assert!(a.dialog.is_some());
        key(&mut a, KeyCode::Char('r'), KeyModifiers::NONE); // Reload
        assert_eq!(text(&a), "external!");
    }

    #[test]
    fn save_as_onto_existing_file_asks_to_replace() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("exists.txt");
        std::fs::write(&f, "old").unwrap();
        let mut a = app_with("", 40, 10);
        type_str(&mut a, "new");
        key(&mut a, KeyCode::F(12), KeyModifiers::NONE);
        type_str(&mut a, &f.display().to_string());
        key(&mut a, KeyCode::Enter, KeyModifiers::NONE);
        assert!(a.dialog.is_some());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "old");
        key(&mut a, KeyCode::Char('r'), KeyModifiers::NONE); // Replace
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "new");
        assert!(!a.tab.buf.is_dirty());
    }

    #[test]
    fn closing_the_last_clean_tab_quits() {
        let mut a = app_with("", 40, 10);
        key(&mut a, KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert!(a.quit);
    }

    #[test]
    fn dropped_folder_opens_prompt_and_drop_into_open_prompt_fills_it() {
        let d = tempfile::tempdir().unwrap();
        let mut a = app_with("", 40, 10);
        a.handle_event(Event::Paste(d.path().display().to_string()));
        let p = a.prompt.as_ref().unwrap();
        assert_eq!(p.kind, PromptKind::Open);
        assert!(p.input.text().ends_with('/'));
        let f = d.path().join("a.txt");
        std::fs::write(&f, "").unwrap();
        a.handle_event(Event::Paste(f.display().to_string()));
        assert_eq!(a.prompt.as_ref().unwrap().input.text(), f.display().to_string());
    }

    #[test]
    fn gutter_click_selects_line() {
        let mut a = app_with("alpha\nbeta\ngamma\n", 40, 10);
        a.rendered_rows = a.visible_rows(10);
        let ev = |kind| Event::Mouse(MouseEvent { kind, column: 1, row: 2, modifiers: KeyModifiers::NONE });
        a.handle_event(ev(MouseEventKind::Down(MouseButton::Left)));
        a.handle_event(ev(MouseEventKind::Up(MouseButton::Left)));
        assert_eq!(a.tab.buf.selected_text(), "beta\n");
    }

    #[test]
    fn find_bar_counts_steps_and_wraps() {
        let mut a = app_with("cat Cat cat dog", 40, 10);
        key(&mut a, KeyCode::Char('f'), KeyModifiers::CONTROL);
        type_str(&mut a, "cat");
        assert_eq!(a.tab.buf.sel, Selection { anchor: 0, head: 3 });
        assert_eq!(a.find_status(), "1 of 3");
        key(&mut a, KeyCode::Enter, KeyModifiers::NONE);
        key(&mut a, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(a.find_status(), "3 of 3");
        key(&mut a, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(a.find_status(), "1 of 3", "wraps");
        key(&mut a, KeyCode::Enter, KeyModifiers::SHIFT);
        assert_eq!(a.find_status(), "3 of 3", "Shift+Enter goes back");
        key(&mut a, KeyCode::Char('c'), KeyModifiers::ALT); // Match case
        assert_eq!(a.find_status(), "2 of 2", "stays on the current match if it still matches");
        key(&mut a, KeyCode::Esc, KeyModifiers::NONE);
        assert!(a.find.is_none());
        key(&mut a, KeyCode::F(3), KeyModifiers::NONE);
        assert!(a.find.is_some(), "F3 reopens with the last search and options");
        assert!(a.find.as_ref().unwrap().opts.match_case);
    }

    #[test]
    fn highlight_word_toggles_document_rule() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("n.txt");
        std::fs::write(&f, "ask Alice now\n").unwrap();
        let mut a = app_with("", 40, 10);
        a.doc_rules = crate::dochl::DocRules::load(Some(d.path().join("documents.toml"))).0;
        a.highlight_word();
        assert!(a.status.as_ref().unwrap().0.contains("Save the file first"));
        assert!(a.open_file(&f));
        a.tab.buf.sel = Selection::caret(6); // inside "Alice"
        a.highlight_word();
        let names: Vec<_> = a.active_highlights().iter().map(|g| g.name.clone()).collect();
        assert_eq!(names[0], crate::dochl::MARKED, "document group first: {names:?}");
        assert_eq!(a.active_highlights()[0].terms, ["Alice"]);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "ask Alice now\n", "file untouched");
        a.highlight_word();
        assert_ne!(a.active_highlights()[0].name, crate::dochl::MARKED, "second press removes it");
    }

    #[test]
    fn preferences_highlights_page() {
        use crate::prefs::Section;
        let d = tempfile::tempdir().unwrap();
        let cfg = d.path().join("config.toml");
        std::fs::write(&cfg, "# mine\n[editor]\nwrap = true\n").unwrap();
        let mut a = app_with("ask Alice", 40, 10);
        a.config_path = Some(cfg.clone());
        a.open_prefs();
        a.prefs.as_mut().unwrap().section = Section::ALL.iter().position(|s| *s == Section::Highlights).unwrap();
        // Rows: Highlighting, Samples, then the samples. Switch the first sample off.
        key(&mut a, KeyCode::Down, KeyModifiers::NONE);
        key(&mut a, KeyCode::Down, KeyModifiers::NONE);
        key(&mut a, KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(a.highlights.groups.len(), 1, "the sample became a user group");
        assert!(!a.highlights.groups[0].enabled);
        // New group: the words prompt opens first.
        key(&mut a, KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(a.prompt.as_ref().unwrap().kind, PromptKind::HlTerms);
        type_str(&mut a, "Alice, release day");
        key(&mut a, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(a.highlights.groups[1].terms, ["Alice", "release day"]);
        assert!(a.active_highlights().iter().any(|g| g.terms == ["Alice", "release day"]), "previews live");
        // Editor: Text colour row, step right twice from accent -> search.
        key(&mut a, KeyCode::Down, KeyModifiers::NONE);
        key(&mut a, KeyCode::Right, KeyModifiers::NONE);
        key(&mut a, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(a.highlights.groups[1].fg, Some(crate::highlight::Colour::Role(crate::highlight::Role::Search)));
        key(&mut a, KeyCode::Esc, KeyModifiers::NONE);
        assert!(a.prefs.as_ref().unwrap().hl_edit.is_none(), "Esc leaves the editor, not Preferences");
        // Own groups come before the sample slots (the edited sample kept its place).
        let order = a.highlights.ordered();
        assert_eq!(order[0].0.terms, ["Alice", "release day"]);
        assert_eq!(order[1].1, crate::highlight::Origin::Override(0));
        // A second group, moved above the first with Alt+Up.
        key(&mut a, KeyCode::Char('n'), KeyModifiers::NONE);
        type_str(&mut a, "Bob");
        key(&mut a, KeyCode::Enter, KeyModifiers::NONE);
        key(&mut a, KeyCode::Esc, KeyModifiers::NONE);
        key(&mut a, KeyCode::Up, KeyModifiers::ALT);
        let order = a.highlights.ordered();
        assert_eq!(order[0].0.terms, ["Bob"]);
        assert_eq!(order[1].0.terms, ["Alice", "release day"]);
        let expected = a.highlights.clone();
        key(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(a.prefs.is_none(), "saved");
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(text.contains("# mine") && !text.contains("[highlights]\n"), "{text}");
        let (parsed, w) = crate::config::parse(&text);
        assert!(w.is_empty(), "{w:?}\n{text}");
        assert_eq!(parsed.highlights, expected, "{text}");
        // Cancel restores.
        a.open_prefs();
        a.highlights.groups.clear();
        key(&mut a, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(a.highlights, expected);
    }

    #[test]
    fn text_fields_have_a_cursor() {
        let mut a = app_with("cat cot cut", 40, 10);
        key(&mut a, KeyCode::Char('f'), KeyModifiers::CONTROL);
        type_str(&mut a, "ct");
        assert_eq!(a.find_status(), "No results");
        key(&mut a, KeyCode::Left, KeyModifiers::NONE);
        type_str(&mut a, "o");
        assert_eq!(a.find.as_ref().unwrap().query.text(), "cot", "typed at the cursor");
        assert_eq!(a.find_status(), "1 of 1", "editing mid-field searches again");
        let caret = a.tab.buf.sel;
        key(&mut a, KeyCode::Home, KeyModifiers::NONE);
        assert_eq!(a.tab.buf.sel, caret, "Home stays in the field");
        key(&mut a, KeyCode::Esc, KeyModifiers::NONE);

        key(&mut a, KeyCode::Char('g'), KeyModifiers::CONTROL);
        type_str(&mut a, "23");
        key(&mut a, KeyCode::Home, KeyModifiers::NONE);
        key(&mut a, KeyCode::Delete, KeyModifiers::NONE);
        type_str(&mut a, "1");
        key(&mut a, KeyCode::End, KeyModifiers::NONE);
        key(&mut a, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(a.prompt.as_ref().unwrap().input.text(), "1");
    }

    #[test]
    fn replace_one_and_all_with_undo() {
        let mut a = app_with("a-b a-b a-b", 40, 10);
        key(&mut a, KeyCode::Char('h'), KeyModifiers::CONTROL);
        type_str(&mut a, "a-b");
        key(&mut a, KeyCode::Tab, KeyModifiers::NONE);
        type_str(&mut a, "X");
        key(&mut a, KeyCode::Enter, KeyModifiers::NONE); // replace current, go next
        assert_eq!(text(&a), "X a-b a-b");
        assert_eq!(a.find_status(), "1 of 2");
        key(&mut a, KeyCode::Char('a'), KeyModifiers::ALT); // replace all
        assert_eq!(text(&a), "X X X");
        key(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(text(&a), "X a-b a-b", "replace all is one undo step");
    }

    #[test]
    fn replace_all_in_selection_only() {
        let mut a = app_with("x\nx\nx\nx\n", 40, 10);
        a.tab.buf.sel = Selection { anchor: 2, head: 6 }; // lines 2-3
        key(&mut a, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert!(a.find.as_ref().unwrap().scope.is_some(), "multi-line selection enables In selection");
        assert_eq!(a.find.as_ref().unwrap().focus, crate::find::Field::Find, "empty query: Find field first");
        type_str(&mut a, "x");
        key(&mut a, KeyCode::Tab, KeyModifiers::NONE);
        type_str(&mut a, "yy");
        key(&mut a, KeyCode::Enter, KeyModifiers::ALT);
        assert_eq!(text(&a), "x\nyy\nyy\nx\n");
    }

    #[test]
    fn whole_word_option() {
        let mut a = app_with("cat concat cat", 40, 10);
        key(&mut a, KeyCode::Char('f'), KeyModifiers::CONTROL);
        type_str(&mut a, "cat");
        assert_eq!(a.find_status(), "1 of 3");
        key(&mut a, KeyCode::Char('w'), KeyModifiers::ALT);
        assert_eq!(a.find_status(), "1 of 2");
    }

    #[test]
    fn snapshots_follow_dirty_state_and_restore_after_crash() {
        let d = tempfile::tempdir().unwrap();
        let rec_dir = d.path().join("rec");
        let f = d.path().join("doc.txt");
        std::fs::write(&f, "on disk\n").unwrap();
        let snaps = |dir: &Path| std::fs::read_dir(dir).map(|r| r.count()).unwrap_or(0);

        // Session 1: edit, snapshot, then "crash" (drop without cleanup).
        let mut a = app_with("", 40, 10);
        a.recovery = Some(Recovery::new(rec_dir.clone()).unwrap());
        a.open_file(&f);
        type_str(&mut a, "X");
        key(&mut a, KeyCode::Char('n'), KeyModifiers::CONTROL);
        type_str(&mut a, "untitled work");
        a.tick_recovery(false);
        a.recovery.as_ref().unwrap().flush();
        assert_eq!(snaps(&rec_dir), 0, "not idle yet: no snapshot");
        a.tick_recovery(true);
        a.recovery.as_ref().unwrap().flush();
        assert_eq!(snaps(&rec_dir), 2);
        // Saving/undoing to clean removes a snapshot.
        key(&mut a, KeyCode::Char('1'), KeyModifiers::ALT);
        key(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert!(!a.tab.buf.is_dirty());
        a.tick_recovery(false);
        a.recovery.as_ref().unwrap().flush();
        assert_eq!(snaps(&rec_dir), 1);
        type_str(&mut a, "edited again ");
        a.tick_recovery(true);
        a.recovery.as_ref().unwrap().flush();
        assert_eq!(snaps(&rec_dir), 2);
        drop(a);

        // Session 2: finds both (the test process isn't named "plica", so
        // session 1 counts as gone) and restores them.
        let mut b = app_with("", 40, 10);
        let r = Recovery::new(rec_dir.clone()).unwrap();
        let orphans = r.orphans();
        b.recovery = Some(r);
        b.offer_recovery(orphans);
        assert!(b.dialog.is_some());
        key(&mut b, KeyCode::Char('r'), KeyModifiers::NONE);
        b.recovery.as_ref().unwrap().flush();
        let texts: Vec<String> = b.all_tabs().map(|t| t.buf.rope().to_string()).collect();
        assert!(texts.contains(&"edited again on disk\n".to_string()), "{texts:?}");
        assert!(texts.contains(&"untitled work".to_string()), "{texts:?}");
        assert!(b.all_tabs().all(|t| t.buf.is_dirty()), "restored work is unsaved");
        assert_eq!(snaps(&rec_dir), 0, "old snapshots deleted after restore");
        // Ctrl+Z on the restored file shows the disk version.
        let i = (0..b.tab_count()).find(|&i| b.tab_at(i).doc.path.is_some()).unwrap();
        b.switch_to(i);
        key(&mut b, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(text(&b), "on disk\n");
    }

    #[test]
    fn discarding_on_close_or_quit_removes_snapshots() {
        let d = tempfile::tempdir().unwrap();
        let rec_dir = d.path().join("rec");
        let mut a = app_with("", 40, 10);
        a.recovery = Some(Recovery::new(rec_dir.clone()).unwrap());
        type_str(&mut a, "x");
        key(&mut a, KeyCode::Char('n'), KeyModifiers::CONTROL);
        type_str(&mut a, "y");
        a.tick_recovery(true);
        key(&mut a, KeyCode::Char('w'), KeyModifiers::CONTROL);
        key(&mut a, KeyCode::Char('n'), KeyModifiers::NONE); // Don't Save
        key(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
        key(&mut a, KeyCode::Char('n'), KeyModifiers::NONE); // Don't Save -> quit
        assert!(a.quit);
        a.recovery.as_ref().unwrap().flush();
        assert_eq!(std::fs::read_dir(&rec_dir).unwrap().count(), 0);
    }

    #[test]
    fn selections_set_primary_and_middle_click_pastes_it() {
        let mut a = app_with("hello world\nsecond line\n", 40, 10);
        a.rendered_rows = a.visible_rows(10);
        let mouse = |kind, column, row| Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE });
        // Drag-select "hello" (text starts at x=5).
        a.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), 5, 1));
        a.handle_event(mouse(MouseEventKind::Drag(MouseButton::Left), 10, 1));
        a.handle_event(mouse(MouseEventKind::Up(MouseButton::Left), 10, 1));
        assert_eq!(a.clipboard.paste_primary(), "hello");
        // Middle-click at the start of line 2 inserts it there.
        a.handle_event(mouse(MouseEventKind::Down(MouseButton::Middle), 5, 2));
        assert_eq!(text(&a), "hello world\nhellosecond line\n");
        // Keyboard selection publishes after it settles.
        a.tab.buf.sel = Selection::caret(6);
        key(&mut a, KeyCode::End, KeyModifiers::SHIFT);
        a.primary_due = Some(Instant::now());
        a.tick();
        assert_eq!(a.clipboard.paste_primary(), "world");
    }

    #[test]
    fn preferences_preview_cancel_and_save() {
        let d = tempfile::tempdir().unwrap();
        let cfg = d.path().join("plica").join("config.toml");
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(&cfg, "# mine\n[editor]\nwrap = true # keep\n").unwrap();
        let mut a = app_with("x", 40, 10);
        a.config_path = Some(cfg.clone());
        assert!(a.wrap);
        key(&mut a, KeyCode::F(9), KeyModifiers::NONE);
        assert!(a.prefs.is_some());
        key(&mut a, KeyCode::Down, KeyModifiers::NONE); // Soft wrap row
        key(&mut a, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(!a.wrap, "change previews live");
        key(&mut a, KeyCode::Esc, KeyModifiers::NONE);
        assert!(a.wrap, "Cancel reverts");
        // Change again and save.
        key(&mut a, KeyCode::F(9), KeyModifiers::NONE);
        key(&mut a, KeyCode::Down, KeyModifiers::NONE);
        key(&mut a, KeyCode::Char(' '), KeyModifiers::NONE);
        key(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(a.prefs.is_none());
        let saved = std::fs::read_to_string(&cfg).unwrap();
        assert!(saved.contains("# mine") && saved.contains("wrap = false # keep"), "{saved}");
        assert_eq!(std::fs::read_to_string(cfg.with_extension("toml.bak")).unwrap(), "# mine\n[editor]\nwrap = true # keep\n");
        let (c, w) = crate::config::parse(&saved);
        assert!(w.is_empty() && !c.wrap);
    }

    #[test]
    fn preferences_refuse_to_overwrite_broken_config() {
        let d = tempfile::tempdir().unwrap();
        let cfg = d.path().join("config.toml");
        std::fs::write(&cfg, "[editor\nwrap = ").unwrap();
        let mut a = app_with("", 40, 10);
        a.config_path = Some(cfg.clone());
        key(&mut a, KeyCode::F(9), KeyModifiers::NONE);
        key(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(a.prefs.is_some(), "dialog stays open");
        assert!(a.status.as_ref().unwrap().0.contains("syntax error"));
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), "[editor\nwrap = ", "file untouched");
    }

    #[test]
    fn preferences_create_config_from_template_when_missing() {
        let d = tempfile::tempdir().unwrap();
        let cfg = d.path().join("new").join("config.toml");
        let mut a = app_with("", 40, 10);
        a.config_path = Some(cfg.clone());
        key(&mut a, KeyCode::F(9), KeyModifiers::NONE);
        key(&mut a, KeyCode::Right, KeyModifiers::NONE); // tab width 4 -> 5
        key(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
        let saved = std::fs::read_to_string(&cfg).unwrap();
        assert!(saved.contains("tab_width = 5") && saved.contains("# Plica configuration"), "{saved}");
        assert!(!cfg.with_extension("toml.bak").exists(), "no backup of a file that didn't exist");
    }

    #[test]
    fn legacy_file_saves_back_and_unmappable_offers_utf8() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("latin1.txt");
        let original = b"Caf\xe9 cr\xe8me br\xfbl\xe9e et d\xe9j\xe0 vu, na\xefve.\n";
        std::fs::write(&f, original).unwrap();
        let mut a = app_with("", 40, 10);
        assert!(a.open_file(&f));
        assert_eq!(a.tab.doc.encoding.label(), "windows-1252");
        assert!(a.status.as_ref().unwrap().0.contains("a guess"), "{:?}", a.status);
        // Typing a char windows-1252 has (é) saves fine, in windows-1252.
        key(&mut a, KeyCode::End, KeyModifiers::CONTROL);
        type_str(&mut a, "é");
        key(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert_eq!(std::fs::read(&f).unwrap(), [&original[..], b"\xe9"].concat());
        // ✓ isn't in windows-1252: nothing is written, the char is
        // selected, and the dialog offers UTF-8.
        type_str(&mut a, "✓");
        key(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
        let dlg = a.dialog.as_ref().expect("unmappable dialog");
        assert!(dlg.title.contains("windows-1252") && dlg.lines[0].contains("line 2"), "{dlg:?}");
        assert_eq!(a.tab.buf.selected_text(), "✓");
        assert_eq!(std::fs::read(&f).unwrap(), [&original[..], b"\xe9"].concat());
        key(&mut a, KeyCode::Char('u'), KeyModifiers::NONE); // Save as UTF-8
        assert!(a.dialog.is_none());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "Café crème brûlée et déjà vu, naïve.\né✓");
        assert!(a.tab.doc.encoding.is_utf8() && !a.tab.buf.is_dirty());
    }

    #[test]
    fn reopen_and_save_with_another_encoding_via_picker() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("ru.txt");
        let (koi, _, _) = encoding_rs::KOI8_R.encode("Привет, мир");
        std::fs::write(&f, &koi).unwrap();
        let mut a = app_with("", 60, 12);
        assert!(a.open_file(&f));
        // Whatever the guess was, reopen explicitly as KOI8-R from the menu.
        key(&mut a, KeyCode::Char('e'), KeyModifiers::ALT);
        key(&mut a, KeyCode::Char('r'), KeyModifiers::NONE);
        assert!(a.picker.is_some());
        type_str(&mut a, "koi8-r");
        key(&mut a, KeyCode::Enter, KeyModifiers::NONE);
        assert!(a.picker.is_none());
        assert_eq!(text(&a), "Привет, мир");
        assert_eq!(a.tab.doc.encoding.label(), "KOI8-R");
        // Save with UTF-16 LE (BOM): the file is rewritten.
        key(&mut a, KeyCode::F(7), KeyModifiers::NONE);
        key(&mut a, KeyCode::Char('s'), KeyModifiers::NONE);
        type_str(&mut a, "little");
        key(&mut a, KeyCode::Enter, KeyModifiers::NONE);
        let bytes = std::fs::read(&f).unwrap();
        assert!(bytes.starts_with(b"\xFF\xFE"), "{bytes:?}");
        assert_eq!(crate::document::load(&f).unwrap().text, "Привет, мир");
        assert_eq!(a.tab.doc.encoding.label(), "UTF-16 LE BOM");
        // Save with an encoding that lacks the text: refused, file unchanged.
        a.save_with(Encoding::parse("windows-1252").unwrap());
        assert!(a.dialog.as_ref().unwrap().title.contains("windows-1252"));
        assert_eq!(std::fs::read(&f).unwrap(), bytes);
        assert_eq!(a.tab.doc.encoding.label(), "UTF-16 LE BOM");
    }

    #[test]
    fn inexact_file_asks_before_saving_over_it() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("bad16.txt");
        // UTF-16 LE BOM with an unpaired surrogate, then an odd trailing byte.
        let original = b"\xFF\xFEa\x00\x00\xD8b\x00z";
        std::fs::write(&f, original).unwrap();
        let mut a = app_with("", 60, 12);
        assert!(a.open_file(&f));
        // Raw placeholders keep it exact, so no warning dialog is needed.
        assert!(!a.tab.doc.inexact, "{:?}", a.tab.doc.encoding);
        a.tab.doc.inexact = true; // as if decoding had been lossy
        type_str(&mut a, "x");
        key(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert_eq!(a.dialog.as_ref().unwrap().title, "Not decoded exactly");
        key(&mut a, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(std::fs::read(&f).unwrap(), original);
        key(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
        key(&mut a, KeyCode::Char('a'), KeyModifiers::NONE); // Save Anyway
        assert!(a.dialog.is_none() && !a.tab.buf.is_dirty());
        assert_ne!(std::fs::read(&f).unwrap(), original);
    }
}
