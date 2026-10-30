//! The Open / Save As dialog: places (recent files, home, the user's
//! folders, bookmarks), a sortable folder listing with an optional preview
//! of the chosen file, a name field and, when saving, the encoding and
//! line endings to write.
//!
//! `FileDialog` is the model and does the file system work; `ui.rs` draws
//! it and records `FileHit` regions; the `impl App` part runs the outcomes.
//! Recent files and bookmarks are never stat'ed while listing them: they
//! may be on sleeping drives or automounts, and are touched only when
//! chosen.

use crate::app::{App, PickChoice, Prompt, PromptKind, SaveOpts};
use crate::document::LineEnding;
use crate::encoding::Encoding;
use crate::lineedit::{LineEdit, Outcome as EditOutcome};
use crate::picker::{self, Picker};
use crate::prefs::tilde;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// The preview reads at most this much of a file…
const PREVIEW_BYTES: u64 = 16 * 1024;
/// …and shows at most this many lines.
const PREVIEW_LINES: usize = 200;

/// Hidden by the "Text files" filter: extensions of files that are
/// almost never text. Folders are always shown.
const BINARY_EXTS: &[&str] = &[
    "7z", "a", "avi", "bin", "bmp", "bz2", "class", "db", "deb", "dll", "dmg", "doc", "docx", "exe", "flac", "gif", "gz", "ico", "img", "iso",
    "jar", "jpeg", "jpg", "m4a", "mkv", "mov", "mp3", "mp4", "o", "odt", "ogg", "otf", "pdf", "png", "ppt", "pptx", "psd", "pyc", "rar",
    "rpm", "so", "sqlite", "tar", "tgz", "tif", "tiff", "ttf", "wav", "webm", "webp", "woff", "woff2", "xcf", "xls", "xlsx", "xz", "zip",
    "zst",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Open,
    SaveAs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Name,
    Size,
    Modified,
}

/// Where keys go. Typing always goes to the name field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Name,
    List,
    Places,
    Encoding,
    LineEnd,
}

/// View choices kept between openings (this session only).
#[derive(Debug, Clone)]
pub struct Prefs {
    pub show_hidden: bool,
    pub preview: bool,
    pub text_only: bool,
    pub sort: Sort,
    pub descending: bool,
    /// The folder the dialog was last left in (used for Untitled tabs).
    pub last_dir: Option<PathBuf>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { show_hidden: false, preview: true, text_only: false, sort: Sort::Name, descending: false, last_dir: None }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub dir: bool,
    /// Not a regular file or folder (FIFO, socket, device): never read.
    pub special: bool,
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
    /// Recent files: the folder, shown in place of the size.
    pub folder: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceKind {
    Recent,
    Dir(PathBuf),
    Bookmark(PathBuf),
}

#[derive(Debug, Clone)]
pub struct Place {
    pub label: String,
    pub kind: PlaceKind,
}

impl Place {
    pub fn dir(&self) -> Option<&Path> {
        match &self.kind {
            PlaceKind::Dir(d) | PlaceKind::Bookmark(d) => Some(d),
            PlaceKind::Recent => None,
        }
    }
}

/// Clickable parts of the dialog, recorded at render time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileHit {
    Back,
    Forward,
    Up,
    /// A segment of the path bar (index into `FileDialog::crumbs`).
    Crumb(usize),
    NewFolder,
    Bookmark,
    Header(Sort),
    Hidden,
    Preview,
    TextOnly,
    Encoding,
    LineEnd,
    Ok,
    Cancel,
}

/// What the App should do after a key or click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Cancel,
    Open(PathBuf),
    /// A file from the Recent place (it may have gone since).
    OpenRecent(PathBuf),
    Save(PathBuf),
    NewFolder,
    PickEncoding,
    Info(String),
    Warn(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    Lines(Vec<String>),
    Note(String),
}

/// What the dialog starts with.
pub struct Init {
    pub mode: Mode,
    pub dir: PathBuf,
    pub name: String,
    /// The current document's folder (a place of its own).
    pub doc_dir: Option<PathBuf>,
    /// Recent files, newest first, with when each was used (unix seconds);
    /// None when the list is off.
    pub recent: Option<Vec<(PathBuf, u64)>>,
    pub enc: Encoding,
    /// None: the document's line endings are mixed (kept as they are).
    pub eol: Option<LineEnding>,
    pub bookmarks_file: Option<PathBuf>,
    pub prefs: Prefs,
}

pub struct FileDialog {
    pub mode: Mode,
    /// The folder shown (kept while the Recent place is shown, for Back).
    pub dir: PathBuf,
    /// Showing the Recent place instead of `dir`.
    pub recent_on: bool,
    entries: Vec<Entry>,
    recent: Option<Vec<Entry>>,
    /// Why `dir` couldn't be read.
    pub error: Option<String>,
    pub name: LineEdit,
    /// Index into `visible()`.
    pub sel: usize,
    pub scroll: Cell<usize>,
    pub places: Vec<Place>,
    pub place_sel: usize,
    /// First place shown, adjusted at draw time to keep `place_sel` in view.
    pub place_scroll: Cell<usize>,
    pub focus: Focus,
    pub prefs: Prefs,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    pub enc: Encoding,
    pub eol: Option<LineEnding>,
    /// The encoding and line endings the document had (Save As changes
    /// them only when they differ).
    orig: (Encoding, Option<LineEnding>),
    pub bookmarks: Vec<PathBuf>,
    bookmarks_file: Option<PathBuf>,
    doc_dir: Option<PathBuf>,
    last_click: Option<(Instant, usize)>,
    preview_cache: RefCell<Option<(PathBuf, Preview)>>,
}

impl FileDialog {
    pub fn new(init: Init) -> Self {
        let recent = init.recent.map(|list| {
            list.into_iter()
                .map(|(path, time)| Entry {
                    name: path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned()),
                    folder: Some(path.parent().map(tilde).unwrap_or_default()),
                    dir: false,
                    special: false,
                    size: None,
                    modified: Some(UNIX_EPOCH + Duration::from_secs(time)),
                    path,
                })
                .collect()
        });
        let bookmarks = init.bookmarks_file.as_deref().map(bookmarks::load).unwrap_or_default();
        let mut d = Self {
            mode: init.mode,
            dir: init.dir,
            recent_on: false,
            entries: Vec::new(),
            recent,
            error: None,
            name: LineEdit::new(init.name),
            sel: 0,
            scroll: Cell::new(0),
            places: Vec::new(),
            place_sel: 0,
            place_scroll: Cell::new(0),
            focus: Focus::Name,
            prefs: init.prefs,
            back: Vec::new(),
            forward: Vec::new(),
            enc: init.enc,
            eol: init.eol,
            orig: (init.enc, init.eol),
            bookmarks,
            bookmarks_file: init.bookmarks_file,
            doc_dir: init.doc_dir,
            last_click: None,
            preview_cache: RefCell::new(None),
        };
        d.build_places();
        d.reload();
        d
    }

    pub fn title(&self) -> &'static str {
        match self.mode {
            Mode::Open => "Open",
            Mode::SaveAs => "Save as",
        }
    }

    /// The encoding and line endings to save with (None where unchanged).
    pub fn save_opts(&self) -> SaveOpts {
        let same_enc = self.enc.enc == self.orig.0.enc && self.enc.bom == self.orig.0.bom;
        SaveOpts { enc: (!same_enc).then_some(self.enc), eol: if self.eol == self.orig.1 { None } else { self.eol } }
    }

    // ---- places ----------------------------------------------------------------------

    fn build_places(&mut self) {
        let mut v = Vec::new();
        if self.mode == Mode::Open && self.recent.is_some() {
            v.push(Place { label: "Recent".into(), kind: PlaceKind::Recent });
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        if let Some(h) = &home {
            v.push(Place { label: "Home".into(), kind: PlaceKind::Dir(h.clone()) });
            for (key, fallback, label) in [("DESKTOP", "Desktop", "Desktop"), ("DOCUMENTS", "Documents", "Documents")] {
                let d = user_dir(h, key).unwrap_or_else(|| h.join(fallback));
                if &d != h && d.is_dir() {
                    v.push(Place { label: label.into(), kind: PlaceKind::Dir(d) });
                }
            }
        }
        if let Some(d) = &self.doc_dir
            && !v.iter().any(|p| p.dir() == Some(d))
        {
            v.push(Place { label: name_of(d), kind: PlaceKind::Dir(d.clone()) });
        }
        v.push(Place { label: "File system".into(), kind: PlaceKind::Dir(PathBuf::from("/")) });
        for b in &self.bookmarks {
            v.push(Place { label: name_of(b), kind: PlaceKind::Bookmark(b.clone()) });
        }
        self.places = v;
        self.place_sel = self.place_sel.min(self.places.len().saturating_sub(1));
    }

    /// The place showing now, if any (highlighted in the sidebar).
    pub fn current_place(&self) -> Option<usize> {
        if self.recent_on {
            return self.places.iter().position(|p| p.kind == PlaceKind::Recent);
        }
        self.places.iter().position(|p| p.dir() == Some(self.dir.as_path()))
    }

    pub fn is_bookmarked(&self) -> bool {
        self.bookmarks.contains(&self.dir)
    }

    fn go_place(&mut self, i: usize) -> Outcome {
        let Some(p) = self.places.get(i) else { return Outcome::Stay };
        self.place_sel = i;
        match p.kind.clone() {
            PlaceKind::Recent => {
                self.recent_on = true;
                self.after_move();
            }
            PlaceKind::Dir(d) | PlaceKind::Bookmark(d) => self.go(d),
        }
        Outcome::Stay
    }

    /// Bookmark the folder shown, or remove its bookmark. The file is
    /// re-read first, so another window's changes aren't lost.
    pub fn toggle_bookmark(&mut self) -> Outcome {
        if self.recent_on {
            return Outcome::Warn("Choose a folder to bookmark".into());
        }
        let dir = self.dir.clone();
        let on = !self.bookmarks.contains(&dir);
        self.set_bookmark(&dir, on)
    }

    fn set_bookmark(&mut self, dir: &Path, on: bool) -> Outcome {
        let Some(file) = self.bookmarks_file.clone() else {
            return Outcome::Warn("No config folder for bookmarks (HOME unset)".into());
        };
        if dir.to_str().is_none() {
            return Outcome::Warn("Can't bookmark a folder whose name isn't valid UTF-8".into());
        }
        let mut list = bookmarks::load(&file);
        list.retain(|p| p != dir);
        if on {
            list.push(dir.to_path_buf());
        }
        match bookmarks::save(&file, &list) {
            Ok(()) => {
                self.bookmarks = list;
                self.build_places();
                let name = name_of(dir);
                Outcome::Info(if on { format!("Bookmarked {name}") } else { format!("Removed the bookmark for {name}") })
            }
            Err(e) => Outcome::Warn(format!("Could not save bookmarks: {e:#}")),
        }
    }

    // ---- folders -------------------------------------------------------------------------

    /// Read the folder shown. Each entry is stat'ed once here (symlinks
    /// followed), never while drawing.
    pub fn reload(&mut self) {
        self.entries.clear();
        self.error = None;
        match std::fs::read_dir(&self.dir) {
            Ok(rd) => {
                for e in rd.flatten() {
                    let path = e.path();
                    let name = e.file_name().to_string_lossy().into_owned();
                    let meta = match e.file_type() {
                        Ok(t) if t.is_symlink() => std::fs::metadata(&path).ok(),
                        _ => e.metadata().ok(),
                    };
                    let (dir, special) = meta.as_ref().map_or((false, false), |m| (m.is_dir(), !m.is_dir() && !m.is_file()));
                    let size = meta.as_ref().filter(|m| m.is_file()).map(|m| m.len());
                    let modified = meta.as_ref().and_then(|m| m.modified().ok());
                    self.entries.push(Entry { name, path, dir, special, size, modified, folder: None });
                }
            }
            Err(e) => self.error = Some(e.to_string()),
        }
        self.sort();
    }

    fn sort(&mut self) {
        let (by, desc) = (self.prefs.sort, self.prefs.descending);
        self.entries.sort_by(|a, b| {
            let key = |e: &Entry| e.name.to_lowercase();
            let o = match by {
                Sort::Name => key(a).cmp(&key(b)),
                Sort::Size => a.size.cmp(&b.size).then_with(|| key(a).cmp(&key(b))),
                Sort::Modified => a.modified.cmp(&b.modified).then_with(|| key(a).cmp(&key(b))),
            }
            .then_with(|| a.name.cmp(&b.name));
            // Folders first, whichever way the files go.
            b.dir.cmp(&a.dir).then(if desc { o.reverse() } else { o })
        });
    }

    /// Sort by a column; the same column again reverses the order.
    pub fn sort_by(&mut self, s: Sort) {
        if self.recent_on {
            return;
        }
        let keep = self.selected().map(|e| e.path.clone());
        if self.prefs.sort == s {
            self.prefs.descending = !self.prefs.descending;
        } else {
            self.prefs.sort = s;
            self.prefs.descending = s != Sort::Name; // biggest / newest first
        }
        self.sort();
        self.select_path(keep.as_deref());
    }

    pub fn go(&mut self, dir: PathBuf) {
        if dir != self.dir {
            self.back.push(std::mem::replace(&mut self.dir, dir));
            self.forward.clear();
        }
        self.recent_on = false;
        self.reload();
        self.after_move();
    }

    /// Up to the parent folder, with the folder we came from selected.
    pub fn go_up(&mut self) {
        if self.recent_on {
            self.recent_on = false;
            self.reload();
            self.after_move();
            return;
        }
        let child = self.dir.clone();
        if let Some(parent) = child.parent() {
            self.go(parent.to_path_buf());
            self.select_path(Some(&child));
        }
    }

    pub fn go_back(&mut self) {
        if self.recent_on {
            self.go_up();
        } else if let Some(d) = self.back.pop() {
            self.forward.push(std::mem::replace(&mut self.dir, d));
            self.reload();
            self.after_move();
        }
    }

    pub fn go_forward(&mut self) {
        if let Some(d) = self.forward.pop() {
            self.back.push(std::mem::replace(&mut self.dir, d));
            self.recent_on = false;
            self.reload();
            self.after_move();
        }
    }

    pub fn can_back(&self) -> bool {
        self.recent_on || !self.back.is_empty()
    }

    pub fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    fn after_move(&mut self) {
        self.sel = 0;
        self.scroll.set(0);
        if self.mode == Mode::Open {
            self.name.set("");
        }
        if let Some(i) = self.current_place() {
            self.place_sel = i;
        }
    }

    /// The path bar: (label, folder) from `~` or `/` down to the folder shown.
    pub fn crumbs(&self) -> Vec<(String, PathBuf)> {
        let home = std::env::var_os("HOME").map(PathBuf::from).filter(|h| h.as_os_str().len() > 1);
        let (mut v, rest) = match home.as_ref().and_then(|h| self.dir.strip_prefix(h).ok().map(|r| (h, r))) {
            Some((h, r)) => (vec![("~".to_string(), h.clone())], r.to_path_buf()),
            None => (vec![("/".to_string(), PathBuf::from("/"))], self.dir.strip_prefix("/").map(Path::to_path_buf).unwrap_or_default()),
        };
        let mut at = v[0].1.clone();
        for c in rest.components() {
            at.push(c);
            v.push((c.as_os_str().to_string_lossy().into_owned(), at.clone()));
        }
        v
    }

    /// Make a folder in the one shown and select it.
    pub fn create_folder(&mut self, name: &str) -> Outcome {
        let name = name.trim();
        if name.is_empty() {
            return Outcome::Stay;
        }
        if name.contains('/') || name == "." || name == ".." {
            return Outcome::Warn(format!("\u{201c}{name}\u{201d} isn't a folder name"));
        }
        let path = self.dir.join(name);
        match std::fs::create_dir(&path) {
            Ok(()) => {
                self.reload();
                if name.starts_with('.') {
                    self.prefs.show_hidden = true;
                }
                self.select_path(Some(&path));
                self.focus = Focus::List;
                Outcome::Info(format!("Made folder {name}"))
            }
            Err(e) => Outcome::Warn(format!("Could not make {name}: {e}")),
        }
    }

    // ---- the list ------------------------------------------------------------------------

    fn list(&self) -> &[Entry] {
        match (&self.recent, self.recent_on) {
            (Some(r), true) => r,
            _ => &self.entries,
        }
    }

    pub fn entry(&self, i: usize) -> &Entry {
        &self.list()[i]
    }

    /// The name field filters the list when opening, unless it holds a path.
    fn filter(&self) -> Option<String> {
        let t = self.name.text().trim();
        (self.mode == Mode::Open && !t.is_empty() && !is_path(t)).then(|| t.to_lowercase())
    }

    /// Indices into the list of the entries shown, in order.
    pub fn visible(&self) -> Vec<usize> {
        let filter = self.filter();
        let list = self.list();
        (0..list.len())
            .filter(|&i| {
                let e = &list[i];
                if self.recent_on {
                    return filter.as_ref().is_none_or(|f| format!("{} {}", e.name, e.folder.as_deref().unwrap_or("")).to_lowercase().contains(f));
                }
                (self.prefs.show_hidden || !e.name.starts_with('.'))
                    && (!self.prefs.text_only || e.dir || !is_binary_name(&e.name))
                    && filter.as_ref().is_none_or(|f| e.name.to_lowercase().contains(f))
            })
            .collect()
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.visible().get(self.sel).map(|&i| self.entry(i))
    }

    pub fn select_path(&mut self, path: Option<&Path>) {
        if let Some(p) = path
            && let Some(row) = self.visible().iter().position(|&i| self.entry(i).path == p)
        {
            self.sel = row;
        }
    }

    /// After typing: select the first name starting with the text (else the
    /// first that contains it), and go into a folder typed with a final `/`.
    fn name_changed(&mut self) {
        let t = self.name.text().to_string();
        if t.ends_with('/') && t.trim() == t {
            let target = self.resolve(&t);
            if target.is_dir() {
                self.name.set("");
                let target = std::path::absolute(&target).unwrap_or(target);
                self.go(normalise(&target));
                return;
            }
        }
        if let Some(f) = self.filter() {
            let vis = self.visible();
            self.sel = vis.iter().position(|&i| self.entry(i).name.to_lowercase().starts_with(&f)).unwrap_or(0);
        }
    }

    /// The field's text as a path: `~` expanded, relative to the folder shown.
    fn resolve(&self, t: &str) -> PathBuf {
        let p = if t == "~" { std::env::var_os("HOME").map_or_else(|| PathBuf::from(t), PathBuf::from) } else { crate::app::expand_tilde(t) };
        if p.is_absolute() { p } else { self.dir.join(p) }
    }

    pub fn move_sel(&mut self, delta: isize) {
        let n = self.visible().len();
        self.sel = (self.sel as isize + delta).clamp(0, n.saturating_sub(1) as isize) as usize;
        self.focus = Focus::List;
        self.copy_selected_name();
    }

    /// Saving: a chosen file's name goes into the name field.
    fn copy_selected_name(&mut self) {
        if self.mode == Mode::SaveAs
            && let Some(e) = self.selected().filter(|e| !e.dir)
        {
            let n = e.name.clone();
            self.name.set(&n);
        }
    }

    /// Keep `sel` within `rows` visible rows; returns the first shown row.
    pub fn scroll_for(&self, rows: usize) -> usize {
        let mut top = self.scroll.get();
        if self.sel < top {
            top = self.sel;
        } else if rows > 0 && self.sel >= top + rows {
            top = self.sel + 1 - rows;
        }
        top = top.min(self.visible().len().saturating_sub(rows));
        self.scroll.set(top);
        top
    }

    /// Enter on the selected entry: into a folder, or open / save a file.
    fn activate_selected(&mut self) -> Outcome {
        let Some(e) = self.selected() else { return Outcome::Stay };
        let (path, dir, name) = (e.path.clone(), e.dir, e.name.clone());
        if dir {
            self.go(path);
            return Outcome::Stay;
        }
        match self.mode {
            Mode::Open if self.recent_on => Outcome::OpenRecent(path),
            Mode::Open => Outcome::Open(path),
            Mode::SaveAs => {
                self.name.set(&name);
                Outcome::Save(path)
            }
        }
    }

    /// The Open / Save button, or Enter in the name field.
    pub fn accept(&mut self) -> Outcome {
        let t = self.name.text().trim().to_string();
        if self.focus == Focus::List || t.is_empty() {
            if self.mode == Mode::SaveAs && self.selected().is_none_or(|e| !e.dir) && t.is_empty() {
                return Outcome::Warn("Type a name to save as".into());
            }
            return self.activate_selected();
        }
        if self.recent_on && !is_path(&t) {
            return self.activate_selected();
        }
        let path = self.resolve(&t);
        if path.is_dir() {
            self.name.set("");
            let path = std::path::absolute(&path).unwrap_or(path);
            self.go(normalise(&path));
            return Outcome::Stay;
        }
        match self.mode {
            // A name that isn't there picks the selected match (type-ahead),
            // or else makes a new file, as typing a path always did.
            Mode::Open if !path.exists() && !is_path(&t) && self.selected().is_some() => self.activate_selected(),
            Mode::Open => Outcome::Open(path),
            Mode::SaveAs => Outcome::Save(path),
        }
    }

    pub fn click_row(&mut self, row: usize) -> Outcome {
        let now = Instant::now();
        let double = matches!(self.last_click, Some((t, r)) if r == row && now.duration_since(t) < DOUBLE_CLICK);
        self.last_click = if double { None } else { Some((now, row)) };
        if row >= self.visible().len() {
            return Outcome::Stay;
        }
        self.sel = row;
        self.focus = Focus::List;
        self.copy_selected_name();
        if double { self.activate_selected() } else { Outcome::Stay }
    }

    /// Keep `place_sel` within `rows` rows; returns the first place shown.
    pub fn place_scroll_for(&self, rows: usize) -> usize {
        let mut top = self.place_scroll.get();
        if self.place_sel < top {
            top = self.place_sel;
        } else if rows > 0 && self.place_sel >= top + rows {
            top = self.place_sel + 1 - rows;
        }
        top = top.min(self.places.len().saturating_sub(rows));
        self.place_scroll.set(top);
        top
    }

    pub fn click_place(&mut self, i: usize) -> Outcome {
        self.focus = Focus::Places;
        self.go_place(i)
    }

    pub fn wheel(&mut self, down: bool) {
        let focus = self.focus;
        self.move_sel(if down { 3 } else { -3 });
        self.focus = focus;
    }

    // ---- options -------------------------------------------------------------------------

    pub fn toggle_hidden(&mut self) {
        let keep = self.selected().map(|e| e.path.clone());
        self.prefs.show_hidden = !self.prefs.show_hidden;
        self.sel = 0;
        self.select_path(keep.as_deref());
    }

    pub fn toggle_text_only(&mut self) {
        let keep = self.selected().map(|e| e.path.clone());
        self.prefs.text_only = !self.prefs.text_only;
        self.sel = 0;
        self.select_path(keep.as_deref());
    }

    pub fn cycle_eol(&mut self) {
        self.eol = match (self.eol, self.orig.1) {
            (Some(LineEnding::Lf), _) => Some(LineEnding::CrLf),
            // Mixed endings can be kept as they are.
            (Some(LineEnding::CrLf), None) => None,
            (Some(LineEnding::CrLf), Some(_)) | (None, _) => Some(LineEnding::Lf),
        };
    }

    pub fn eol_label(&self) -> &'static str {
        match self.eol {
            Some(LineEnding::Lf) => "LF (Linux, macOS)",
            Some(LineEnding::CrLf) => "CRLF (Windows)",
            None => "Mixed (unchanged)",
        }
    }

    // ---- preview -------------------------------------------------------------------------

    /// What the preview pane shows for the selected entry (read once per
    /// file and cached).
    pub fn preview(&self) -> Preview {
        let Some(e) = self.selected() else { return Preview::Note(String::new()) };
        if e.dir {
            return Preview::Note("Folder".into());
        }
        if e.special {
            return Preview::Note("Not a regular file".into());
        }
        if let Some((p, v)) = &*self.preview_cache.borrow()
            && p == &e.path
        {
            return v.clone();
        }
        let v = read_preview(&e.path);
        *self.preview_cache.borrow_mut() = Some((e.path.clone(), v.clone()));
        v
    }

    // ---- input ---------------------------------------------------------------------------

    pub fn on_key(&mut self, k: &KeyEvent, page: usize) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let page = page.max(1) as isize;
        match k.code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Tab => self.focus = self.next_focus(1),
            KeyCode::BackTab => self.focus = self.next_focus(-1),
            KeyCode::Char('d') if ctrl => return self.toggle_bookmark(),
            KeyCode::Char('n' | 'N') if ctrl => return Outcome::NewFolder,
            KeyCode::Char('h') if ctrl => self.toggle_hidden(),
            KeyCode::Char('.') if alt => self.toggle_hidden(),
            KeyCode::Char('p') if alt => self.prefs.preview = !self.prefs.preview,
            KeyCode::Up if alt => self.go_up(),
            KeyCode::Left if alt => self.go_back(),
            KeyCode::Right if alt => self.go_forward(),
            KeyCode::Home if alt => {
                if let Some(h) = std::env::var_os("HOME") {
                    self.go(PathBuf::from(h));
                }
            }
            _ => return self.focused_key(k, page),
        }
        Outcome::Stay
    }

    fn next_focus(&self, dir: isize) -> Focus {
        let mut order = vec![Focus::Name, Focus::Places];
        if self.mode == Mode::SaveAs {
            order.extend([Focus::Encoding, Focus::LineEnd]);
        }
        let cur = if self.focus == Focus::List { Focus::Name } else { self.focus };
        let i = order.iter().position(|f| *f == cur).unwrap_or(0) as isize;
        order[(i + dir).rem_euclid(order.len() as isize) as usize]
    }

    fn focused_key(&mut self, k: &KeyEvent, page: isize) -> Outcome {
        let activate = matches!(k.code, KeyCode::Enter | KeyCode::Char(' '));
        match self.focus {
            Focus::Places => match k.code {
                KeyCode::Up => self.place_sel = self.place_sel.saturating_sub(1),
                KeyCode::Down => self.place_sel = (self.place_sel + 1).min(self.places.len().saturating_sub(1)),
                KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') => {
                    let o = self.go_place(self.place_sel);
                    self.focus = Focus::Name;
                    return o;
                }
                KeyCode::Delete => {
                    if let Some(PlaceKind::Bookmark(d)) = self.places.get(self.place_sel).map(|p| p.kind.clone()) {
                        return self.set_bookmark(&d, false);
                    }
                }
                _ => return self.name_key(k, page),
            },
            Focus::Encoding if activate || matches!(k.code, KeyCode::Left | KeyCode::Right) => return Outcome::PickEncoding,
            Focus::LineEnd if activate || matches!(k.code, KeyCode::Left | KeyCode::Right) => self.cycle_eol(),
            _ => return self.name_key(k, page),
        }
        Outcome::Stay
    }

    /// Keys for the list and the name field (where typing always goes).
    fn name_key(&mut self, k: &KeyEvent, page: isize) -> Outcome {
        match k.code {
            KeyCode::Enter => return self.accept(),
            KeyCode::Up => self.move_sel(-1),
            KeyCode::Down => self.move_sel(1),
            KeyCode::PageUp => self.move_sel(-page),
            KeyCode::PageDown => self.move_sel(page),
            KeyCode::Home | KeyCode::End if self.focus == Focus::List => {
                self.move_sel(if k.code == KeyCode::Home { isize::MIN / 2 } else { isize::MAX / 2 })
            }
            KeyCode::Backspace if self.name.is_empty() => self.go_up(),
            _ => {
                let before = self.focus;
                self.focus = Focus::Name;
                match self.name.key(k) {
                    EditOutcome::Changed => self.name_changed(),
                    EditOutcome::Moved => {}
                    EditOutcome::Ignored => self.focus = before,
                }
            }
        }
        Outcome::Stay
    }

    pub fn on_hit(&mut self, hit: FileHit) -> Outcome {
        match hit {
            FileHit::Back => self.go_back(),
            FileHit::Forward => self.go_forward(),
            FileHit::Up => self.go_up(),
            FileHit::Crumb(i) => {
                if let Some((_, d)) = self.crumbs().into_iter().nth(i) {
                    let child = self.dir.clone();
                    self.go(d);
                    self.select_path(Some(&child));
                }
            }
            FileHit::NewFolder => return Outcome::NewFolder,
            FileHit::Bookmark => return self.toggle_bookmark(),
            FileHit::Header(s) => self.sort_by(s),
            FileHit::Hidden => self.toggle_hidden(),
            FileHit::Preview => self.prefs.preview = !self.prefs.preview,
            FileHit::TextOnly => self.toggle_text_only(),
            FileHit::Encoding => {
                self.focus = Focus::Encoding;
                return Outcome::PickEncoding;
            }
            FileHit::LineEnd => {
                self.focus = Focus::LineEnd;
                self.cycle_eol();
            }
            FileHit::Ok => {
                if self.focus != Focus::List {
                    self.focus = Focus::Name;
                }
                return self.accept();
            }
            FileHit::Cancel => return Outcome::Cancel,
        }
        Outcome::Stay
    }

    /// A file or folder dropped on the window while the dialog is open.
    pub fn dropped(&mut self, path: &Path) -> Outcome {
        if path.is_dir() {
            self.go(path.to_path_buf());
            return Outcome::Stay;
        }
        match self.mode {
            Mode::Open => Outcome::Open(path.to_path_buf()),
            Mode::SaveAs => {
                if let Some(parent) = path.parent() {
                    self.go(parent.to_path_buf());
                }
                self.name.set(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                self.focus = Focus::Name;
                Outcome::Stay
            }
        }
    }

    pub fn paste(&mut self, text: &str) {
        self.focus = Focus::Name;
        self.name.insert(text);
        self.name_changed();
    }
}

/// Text that names a path rather than filters the list.
fn is_path(t: &str) -> bool {
    t.contains('/') || t == "~" || t == ".."
}

/// `a/./b/../c` → `a/c`, without touching the disk (symlinks are kept as
/// typed, like a shell's `cd`).
fn normalise(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    if out.as_os_str().is_empty() { PathBuf::from("/") } else { out }
}

fn name_of(p: &Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

pub fn is_binary_name(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && BINARY_EXTS.contains(&ext.to_lowercase().as_str()))
}

/// A folder from `user-dirs.dirs` (`XDG_DESKTOP_DIR="$HOME/Desktop"`).
fn user_dir(home: &Path, key: &str) -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME").filter(|p| !p.is_empty()).map_or_else(|| home.join(".config"), PathBuf::from);
    let text = std::fs::read_to_string(config.join("user-dirs.dirs")).ok()?;
    parse_user_dir(&text, home, key)
}

fn parse_user_dir(text: &str, home: &Path, key: &str) -> Option<PathBuf> {
    let want = format!("XDG_{key}_DIR=");
    let line = text.lines().map(str::trim).find(|l| l.starts_with(&want))?;
    let v = line[want.len()..].trim().trim_matches('"');
    match v.strip_prefix("$HOME") {
        Some(rest) => Some(home.join(rest.trim_start_matches('/'))),
        None if v.starts_with('/') => Some(PathBuf::from(v)),
        None => None,
    }
}

fn read_preview(path: &Path) -> Preview {
    use std::io::Read;
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return Preview::Note("Not a regular file".into());
    }
    let mut bytes = Vec::new();
    if let Err(e) = std::fs::File::open(path).and_then(|f| f.take(PREVIEW_BYTES).read_to_end(&mut bytes)) {
        return Preview::Note(format!("Can't read it: {e}"));
    }
    if bytes.is_empty() {
        return Preview::Note("Empty file".into());
    }
    let d = match crate::encoding::detect(bytes, &crate::encoding::Options::default()) {
        Ok(d) => d,
        Err(e) if e.contains("binary") => return Preview::Note("Binary file".into()),
        Err(_) => return Preview::Note("No preview".into()),
    };
    let controls = d.text.chars().filter(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')).count();
    if d.text.contains('\0') || controls * 20 > d.text.chars().count() {
        return Preview::Note("Binary file".into());
    }
    let lines = d
        .text
        .lines()
        .take(PREVIEW_LINES)
        .map(|l| l.replace('\t', "    ").chars().filter(|c| !c.is_control()).collect())
        .collect();
    Preview::Lines(lines)
}

/// "512 B", "6.1 kB", "18 kB", "2.3 MB" (decimal units, like file managers).
pub fn human_size(n: u64) -> String {
    if n < 1000 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1000.0;
    for unit in ["kB", "MB", "GB", "TB"] {
        if v < 999.5 || unit == "TB" {
            return if v < 9.95 { format!("{v:.1} {unit}") } else { format!("{v:.0} {unit}") };
        }
        v /= 1000.0;
    }
    unreachable!()
}

/// Local calendar time: (year, month 1–12, day, hour, minute).
type Local = (i32, u32, u32, u32, u32);

fn local(t: SystemTime) -> Option<Local> {
    // `time_t` is inferred, not named: musl's libc marks the alias deprecated.
    let secs = t.duration_since(UNIX_EPOCH).ok()?.as_secs().try_into().ok()?;
    // SAFETY: localtime_r only writes the `tm` it is given; zeroed is a
    // valid `tm` (it holds integers and, on glibc, a nullable pointer).
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    ok.then(|| (tm.tm_year + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32, tm.tm_hour as u32, tm.tm_min as u32))
}

/// "Today 11:40", "Yesterday", "12 Oct", "3 Sep 2024".
pub fn when(t: SystemTime) -> String {
    let now = SystemTime::now();
    match (local(t), local(now), local(now - Duration::from_secs(86_400))) {
        (Some(t), Some(now), Some(yday)) => when_local(t, now, yday),
        _ => String::new(),
    }
}

fn when_local(t: Local, now: Local, yday: Local) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let day = |x: Local| (x.0, x.1, x.2);
    let month = MONTHS[(t.1 as usize).clamp(1, 12) - 1];
    if day(t) == day(now) {
        format!("Today {:02}:{:02}", t.3, t.4)
    } else if day(t) == day(yday) {
        "Yesterday".into()
    } else if t.0 == now.0 && day(t) < day(now) {
        format!("{} {month}", t.2)
    } else {
        format!("{} {month} {}", t.2, t.0)
    }
}

/// Bookmarked folders: `$XDG_CONFIG_HOME/plica/bookmarks.toml`, next to
/// config.toml because the user chose them (unlike recent.toml).
pub mod bookmarks {
    use anyhow::{Context, Result};
    use std::path::{Path, PathBuf};

    pub fn default_file() -> Option<PathBuf> {
        Some(crate::config::default_path()?.with_file_name("bookmarks.toml"))
    }

    /// A missing or unreadable file is an empty list.
    pub fn load(file: &Path) -> Vec<PathBuf> {
        std::fs::read_to_string(file).map(|t| parse(&t)).unwrap_or_default()
    }

    pub fn parse(text: &str) -> Vec<PathBuf> {
        let Ok(t) = text.parse::<toml::Table>() else { return Vec::new() };
        let Some(arr) = t.get("folders").and_then(|v| v.as_array()) else { return Vec::new() };
        arr.iter().filter_map(|v| v.as_str()).filter(|s| s.starts_with('/')).map(PathBuf::from).collect()
    }

    pub fn render(list: &[PathBuf]) -> String {
        let mut arr = toml_edit::Array::new();
        for p in list {
            if let Some(s) = p.to_str() {
                arr.push(s);
            }
        }
        arr.set_trailing_comma(true);
        for v in arr.iter_mut() {
            v.decor_mut().set_prefix("\n  ");
        }
        arr.set_trailing("\n");
        let mut d = toml_edit::DocumentMut::new();
        d.insert("folders", toml_edit::value(arr));
        format!("# Plica's bookmarked folders (Ctrl+D in the Open / Save as dialog).\n{d}")
    }

    pub fn save(file: &Path, list: &[PathBuf]) -> Result<()> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        }
        let mut out = crate::document::Document::untitled();
        out.path = Some(file.to_path_buf());
        out.save(&render(list))
    }
}

// ---- App ---------------------------------------------------------------------------------

impl App {
    /// The Open or Save As dialog, in the active document's folder (or
    /// `at`, or where the dialog was last used).
    pub fn open_file_dialog(&mut self, mode: Mode, at: Option<PathBuf>) {
        let doc_dir = self.tab.doc.path.as_deref().and_then(Path::parent).map(Path::to_path_buf);
        let dir = at
            .or_else(|| doc_dir.clone())
            .or_else(|| self.file_dlg_prefs.last_dir.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        let name = match mode {
            Mode::SaveAs => self.tab.doc.path.as_deref().map(name_of).unwrap_or_default(),
            Mode::Open => String::new(),
        };
        let recent = match (mode, &self.recent) {
            (Mode::Open, Some(r)) => Some(r.list().into_iter().map(|e| (e.path, e.time)).collect()),
            _ => None,
        };
        let d = &self.tab.doc;
        let current = d.path.clone();
        let mut dlg = FileDialog::new(Init {
            mode,
            dir,
            name,
            doc_dir,
            recent,
            enc: d.encoding,
            eol: (!d.mixed_line_endings).then_some(d.line_ending),
            bookmarks_file: bookmarks::default_file(),
            prefs: self.file_dlg_prefs.clone(),
        });
        dlg.select_path(current.as_deref());
        self.file_dlg = Some(dlg);
    }

    fn close_file_dialog(&mut self) -> Option<FileDialog> {
        let d = self.file_dlg.take()?;
        let last_dir = if d.error.is_none() { Some(d.dir.clone()) } else { self.file_dlg_prefs.last_dir.clone() };
        self.file_dlg_prefs = Prefs { last_dir, ..d.prefs.clone() };
        Some(d)
    }

    pub fn on_file_dialog_key(&mut self, k: KeyEvent) {
        if k.code == KeyCode::Char('v') && k.modifiers.contains(KeyModifiers::CONTROL) {
            let t = self.clipboard.paste();
            if let Some(d) = &mut self.file_dlg {
                d.paste(&t);
            }
            return;
        }
        let page = self.regions.file_list.height as usize;
        let Some(d) = &mut self.file_dlg else { return };
        let o = d.on_key(&k, page);
        self.file_dialog_outcome(o);
    }

    pub fn on_file_dialog_mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        let r = &self.regions;
        let inside = |a: Rect| x >= a.x && x < a.x + a.width && y >= a.y && y < a.y + a.height;
        let hit = r.file_hits.iter().find(|(a, _)| inside(*a)).map(|(_, h)| *h);
        let (field, list, places) = (r.file_field, r.file_list, r.file_places);
        let Some(d) = &mut self.file_dlg else { return };
        let o = match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(h) = hit {
                    d.on_hit(h)
                } else if inside(field) {
                    d.focus = Focus::Name;
                    d.name.click((x - field.x) as usize);
                    Outcome::Stay
                } else if inside(list) {
                    d.click_row(d.scroll.get() + (y - list.y) as usize)
                } else if inside(places) {
                    d.click_place(d.place_scroll.get() + (y - places.y) as usize)
                } else {
                    Outcome::Stay
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if !inside(places) => {
                d.wheel(m.kind == MouseEventKind::ScrollDown);
                Outcome::Stay
            }
            _ => Outcome::Stay,
        };
        self.file_dialog_outcome(o);
    }

    pub fn file_dialog_paste(&mut self, text: &str) {
        let dropped = crate::drop::parse_dropped_paths(text);
        let Some(d) = &mut self.file_dlg else { return };
        let o = match dropped {
            Some(paths) => d.dropped(&paths[0]),
            None => {
                d.paste(text);
                Outcome::Stay
            }
        };
        self.file_dialog_outcome(o);
    }

    fn file_dialog_outcome(&mut self, o: Outcome) {
        match o {
            Outcome::Stay => {}
            Outcome::Cancel => {
                self.close_file_dialog();
                self.cancel_save_chain();
            }
            Outcome::Open(p) => {
                self.close_file_dialog();
                self.open_file(&p);
            }
            Outcome::OpenRecent(p) => {
                self.close_file_dialog();
                self.open_recent(&p);
            }
            Outcome::Save(p) => {
                if let Some(d) = self.close_file_dialog() {
                    self.save_as_with(p, d.save_opts());
                }
            }
            Outcome::NewFolder => {
                if self.file_dlg.as_ref().is_some_and(|d| d.recent_on) {
                    self.warn("Choose a folder to make the new folder in");
                } else {
                    self.prompt = Some(Prompt::new(PromptKind::NewFolder, ""));
                }
            }
            Outcome::PickEncoding => {
                let cur = self.file_dlg.as_ref().map_or(self.tab.doc.encoding, |d| d.enc);
                let items = crate::encoding::menu(true)
                    .into_iter()
                    .map(|(e, detail)| picker::Item { label: e.label(), detail, current: e.enc == cur.enc && e.bom == cur.bom, value: PickChoice::DialogEncoding(e) })
                    .collect();
                self.picker = Some(Picker::new("Save with encoding", items));
            }
            Outcome::Info(s) => self.info(s),
            Outcome::Warn(s) => self.warn(s),
        }
    }

    /// The New folder prompt's answer.
    pub fn new_folder_done(&mut self, name: &str) {
        if let Some(d) = &mut self.file_dlg {
            let o = d.create_folder(name);
            self.file_dialog_outcome(o);
        }
    }

    pub fn dialog_encoding_chosen(&mut self, e: Encoding) {
        if let Some(d) = &mut self.file_dlg {
            d.enc = Encoding { raw: d.enc.raw, ..e };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn typed(d: &mut FileDialog, s: &str) {
        for c in s.chars() {
            d.on_key(&key(KeyCode::Char(c)), 5);
        }
    }

    fn dialog(mode: Mode, dir: &Path) -> FileDialog {
        FileDialog::new(Init {
            mode,
            dir: dir.to_path_buf(),
            name: String::new(),
            doc_dir: None,
            recent: None,
            enc: Encoding::UTF8,
            eol: Some(LineEnding::Lf),
            bookmarks_file: Some(dir.join("config").join("bookmarks.toml")),
            prefs: Prefs::default(),
        })
    }

    fn names(d: &FileDialog) -> Vec<String> {
        d.visible().iter().map(|&i| d.entry(i).name.clone()).collect()
    }

    fn tree() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("docs")).unwrap();
        std::fs::create_dir(t.path().join(".git")).unwrap();
        std::fs::write(t.path().join("b.txt"), "bee\n").unwrap();
        std::fs::write(t.path().join("A.md"), "# Title\n\tindented\n").unwrap();
        std::fs::write(t.path().join("big.log"), "x".repeat(5000)).unwrap();
        std::fs::write(t.path().join(".hidden"), "").unwrap();
        std::fs::write(t.path().join("pic.png"), b"\x89PNG\r\n\x1a\n\0\0\0").unwrap();
        std::fs::write(t.path().join("docs").join("inner.txt"), "in\n").unwrap();
        t
    }

    #[test]
    fn folders_first_hidden_off_and_sorting() {
        let t = tree();
        let mut d = dialog(Mode::Open, t.path());
        assert_eq!(names(&d), ["docs", "A.md", "b.txt", "big.log", "pic.png"]);
        d.toggle_hidden();
        assert_eq!(names(&d), [".git", "docs", ".hidden", "A.md", "b.txt", "big.log", "pic.png"]);
        d.toggle_hidden();
        d.sort_by(Sort::Size);
        assert_eq!(names(&d)[..2], ["docs", "big.log"], "biggest first, folders still first");
        d.sort_by(Sort::Size);
        assert_eq!(names(&d)[1..], ["b.txt", "pic.png", "A.md", "big.log"], "smallest first");
        d.toggle_text_only();
        assert!(!names(&d).contains(&"pic.png".to_string()));
    }

    #[test]
    fn typing_filters_and_enter_opens_the_match() {
        let t = tree();
        let mut d = dialog(Mode::Open, t.path());
        typed(&mut d, "b");
        assert_eq!(names(&d), ["b.txt", "big.log"]);
        typed(&mut d, "ig");
        assert_eq!(d.on_key(&key(KeyCode::Enter), 5), Outcome::Open(t.path().join("big.log")));
        // A name that matches nothing is a new file, as the old prompt did.
        d.name.set("");
        typed(&mut d, "new.txt");
        assert_eq!(d.on_key(&key(KeyCode::Enter), 5), Outcome::Open(t.path().join("new.txt")));
    }

    #[test]
    fn slash_goes_into_folders_and_backspace_comes_back() {
        let t = tree();
        let mut d = dialog(Mode::Open, t.path());
        typed(&mut d, "docs/");
        assert_eq!(d.dir, t.path().join("docs"));
        assert!(d.name.is_empty());
        assert_eq!(names(&d), ["inner.txt"]);
        d.on_key(&key(KeyCode::Backspace), 5);
        assert_eq!(d.dir, t.path());
        assert_eq!(d.selected().map(|e| e.name.as_str()), Some("docs"), "the folder we came from");
        d.on_key(&KeyEvent::new(KeyCode::Left, KeyModifiers::ALT), 5);
        assert_eq!(d.dir, t.path().join("docs"), "back");
        d.on_key(&KeyEvent::new(KeyCode::Right, KeyModifiers::ALT), 5);
        assert_eq!(d.dir, t.path(), "forward");
        // A typed absolute path works from anywhere.
        let inner = t.path().join("docs").join("inner.txt");
        d.paste(&inner.display().to_string());
        assert_eq!(d.accept(), Outcome::Open(inner));
    }

    #[test]
    fn save_as_keeps_the_name_and_clicks_fill_it() {
        let t = tree();
        let mut d = dialog(Mode::SaveAs, t.path());
        typed(&mut d, "notes");
        assert_eq!(names(&d).len(), 5, "saving doesn't filter");
        d.on_key(&key(KeyCode::Down), 5); // onto A.md (docs is row 0)
        assert_eq!(d.name.text(), "A.md");
        d.click_row(0);
        assert_eq!(d.click_row(0), Outcome::Stay, "double-click goes into docs");
        assert_eq!(d.dir, t.path().join("docs"));
        assert_eq!(d.name.text(), "A.md", "the name stays when moving");
        d.name.set("out.txt");
        d.focus = Focus::Name;
        assert_eq!(d.accept(), Outcome::Save(t.path().join("docs").join("out.txt")));
        d.name.set("");
        assert!(matches!(d.accept(), Outcome::Warn(_)));
    }

    #[test]
    fn options_only_when_changed() {
        let t = tree();
        let mut d = dialog(Mode::SaveAs, t.path());
        assert_eq!(d.save_opts(), SaveOpts::default());
        d.cycle_eol();
        assert_eq!(d.save_opts().eol, Some(LineEnding::CrLf));
        d.cycle_eol();
        assert_eq!(d.save_opts(), SaveOpts::default());
        // Mixed endings: "unchanged" is a choice.
        d.orig.1 = None;
        d.eol = None;
        d.cycle_eol();
        d.cycle_eol();
        d.cycle_eol();
        assert_eq!(d.eol, None);
    }

    #[test]
    fn bookmarks_round_trip_and_show_as_places() {
        let t = tree();
        let mut d = dialog(Mode::Open, t.path());
        assert!(matches!(d.toggle_bookmark(), Outcome::Info(_)));
        assert!(d.is_bookmarked());
        let file = t.path().join("config").join("bookmarks.toml");
        assert_eq!(bookmarks::load(&file), [t.path().to_path_buf()]);
        assert!(d.places.iter().any(|p| p.kind == PlaceKind::Bookmark(t.path().to_path_buf())));
        // Another window's bookmark survives this one's change.
        let other = PathBuf::from("/elsewhere");
        bookmarks::save(&file, &[t.path().to_path_buf(), other.clone()]).unwrap();
        d.toggle_bookmark();
        assert_eq!(bookmarks::load(&file), [other]);
        assert_eq!(bookmarks::parse("folders = [\"relative\", 3, \"/ok\"]"), [PathBuf::from("/ok")]);
    }

    #[test]
    fn recent_place_filters_and_opens() {
        let t = tree();
        let mut d = FileDialog::new(Init {
            mode: Mode::Open,
            dir: t.path().to_path_buf(),
            name: String::new(),
            doc_dir: None,
            recent: Some(vec![(PathBuf::from("/x/one.txt"), 10), (PathBuf::from("/y/two.md"), 5)]),
            enc: Encoding::UTF8,
            eol: None,
            bookmarks_file: None,
            prefs: Prefs::default(),
        });
        assert_eq!(d.places[0].kind, PlaceKind::Recent);
        d.click_place(0);
        assert!(d.recent_on);
        typed(&mut d, "two");
        assert_eq!(d.accept(), Outcome::OpenRecent(PathBuf::from("/y/two.md")));
        d.go_back();
        assert!(!d.recent_on);
        assert_eq!(d.dir, t.path());
    }

    #[test]
    fn new_folder_is_made_and_selected() {
        let t = tree();
        let mut d = dialog(Mode::SaveAs, t.path());
        assert!(matches!(d.create_folder("made"), Outcome::Info(_)));
        assert!(t.path().join("made").is_dir());
        assert_eq!(d.selected().map(|e| e.name.as_str()), Some("made"));
        assert!(matches!(d.create_folder("a/b"), Outcome::Warn(_)));
        assert!(matches!(d.create_folder("made"), Outcome::Warn(_)), "already there");
    }

    #[test]
    fn preview_reads_text_and_spots_binary() {
        let t = tree();
        let mut d = dialog(Mode::Open, t.path());
        d.select_path(Some(&t.path().join("A.md")));
        assert_eq!(d.preview(), Preview::Lines(vec!["# Title".into(), "    indented".into()]));
        d.select_path(Some(&t.path().join("pic.png")));
        assert_eq!(d.preview(), Preview::Note("Binary file".into()));
        d.select_path(Some(&t.path().join("docs")));
        assert_eq!(d.preview(), Preview::Note("Folder".into()));
    }

    #[test]
    fn crumbs_and_normalising() {
        let t = tree();
        let d = dialog(Mode::Open, &t.path().join("docs"));
        let c = d.crumbs();
        assert_eq!(c.last().unwrap(), &("docs".to_string(), t.path().join("docs")));
        assert_eq!(normalise(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
        assert_eq!(normalise(Path::new("/..")), PathBuf::from("/"));
    }

    #[test]
    fn formats() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(6_100), "6.1 kB");
        assert_eq!(human_size(18_400), "18 kB");
        assert_eq!(human_size(2_300_000), "2.3 MB");
        let now = (2026, 10, 30, 14, 2);
        let yday = (2026, 10, 29, 14, 2);
        assert_eq!(when_local((2026, 10, 30, 9, 5), now, yday), "Today 09:05");
        assert_eq!(when_local((2026, 10, 29, 23, 0), now, yday), "Yesterday");
        assert_eq!(when_local((2026, 9, 3, 1, 0), now, yday), "3 Sep");
        assert_eq!(when_local((2024, 9, 3, 1, 0), now, yday), "3 Sep 2024");
        assert!(is_binary_name("x.PNG") && !is_binary_name(".png") && !is_binary_name("notes.txt"));
        let home = Path::new("/home/u");
        assert_eq!(parse_user_dir("XDG_DESKTOP_DIR=\"$HOME/Desk\"\n", home, "DESKTOP"), Some(PathBuf::from("/home/u/Desk")));
        assert_eq!(parse_user_dir("XDG_DESKTOP_DIR=\"$HOME/\"\n", home, "DOCUMENTS"), None);
    }
}
