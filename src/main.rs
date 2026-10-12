mod app;
mod buffer;
mod clipboard;
mod dialog;
mod document;
mod drop;
mod layout;
mod tab;
mod term;
mod theme;
mod ui;
mod watch;

use anyhow::{Context, Result, bail};
use app::{App, ThemeSource};
use buffer::Selection;
use tab::Tab;
use ratatui::Terminal;
use ratatui::crossterm::event;
use ratatui::prelude::CrosstermBackend;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const PALETTE_DEBOUNCE: Duration = Duration::from_millis(150);
const DOC_DEBOUNCE: Duration = Duration::from_millis(200);

const HELP: &str = "\
Plica — a mouse-friendly terminal text editor

Usage: plica [OPTIONS] [+LINE] [FILE...]

Arguments:
  FILE...              Files to open, one tab each (created on first save if missing)
  +LINE                Start with the caret on LINE of the first file

Options:
  --palette PATH       Follow a Kitty-format colour file, reloading live
  --builtin-theme      Use the built-in Dracula theme
  --check-palette PATH Parse a palette file, print the resolved colours and exit
  -h, --help           Print help
  -V, --version        Print version

Environment:
  PLICA_PALETTE        Palette file to follow (same as --palette)
  NO_COLOR             Use the terminal's default colours (unless a palette is given)

Without --palette/PLICA_PALETTE, Plica follows $XDG_CONFIG_HOME/kitty/current-theme.conf
if it exists, otherwise uses Dracula. Press F1 inside Plica for keys.
";

struct Args {
    files: Vec<PathBuf>,
    line: Option<usize>,
    palette: Option<PathBuf>,
    builtin: bool,
    check_palette: Option<PathBuf>,
}

fn parse_args() -> Result<Option<Args>> {
    let mut a = Args { files: Vec::new(), line: None, palette: None, builtin: false, check_palette: None };
    let mut it = std::env::args_os().skip(1);
    let mut only_files = false;
    while let Some(arg) = it.next() {
        let s = arg.to_string_lossy();
        if !only_files {
            match s.as_ref() {
                "-h" | "--help" => {
                    print!("{HELP}");
                    return Ok(None);
                }
                "-V" | "--version" => {
                    println!("plica {}", ui::VERSION);
                    return Ok(None);
                }
                "--palette" => {
                    a.palette = Some(it.next().context("--palette needs a path")?.into());
                    continue;
                }
                "--builtin-theme" => {
                    a.builtin = true;
                    continue;
                }
                "--check-palette" => {
                    a.check_palette = Some(it.next().context("--check-palette needs a path")?.into());
                    continue;
                }
                "--" => {
                    only_files = true;
                    continue;
                }
                _ if s.starts_with('+') && s.len() > 1 => {
                    a.line = Some(s[1..].parse().with_context(|| format!("bad line number `{s}`"))?);
                    continue;
                }
                _ if s.starts_with('-') && s.len() > 1 => bail!("unknown option `{s}` (see --help)"),
                _ => {}
            }
        }
        a.files.push(arg.into());
    }
    Ok(Some(a))
}

fn default_palette_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PLICA_PALETTE").filter(|p| !p.is_empty()) {
        return Some(app::expand_tilde(&p.to_string_lossy()));
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))?;
    let candidate = config.join("kitty").join("current-theme.conf");
    candidate.is_file().then_some(candidate)
}

fn check_palette(path: &Path) -> Result<()> {
    let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let report = theme::parse_kitty(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    for w in &report.warnings {
        println!("warning: {w}");
    }
    let t = theme::resolve(&report.palette);
    println!("{t:#?}");
    let pairs = [
        ("text", t.editor_fg, t.editor_bg),
        ("selection", t.selection_fg, t.selection_bg),
        ("search", t.search_fg, t.search_bg),
        ("status", t.surface_fg, t.surface_bg),
        ("accent", t.accent_fg, t.accent),
        ("muted", t.muted, t.editor_bg),
    ];
    for (name, fg, bg) in pairs {
        println!("contrast {name:<10} {fg} on {bg}: {:.2}", theme::contrast(fg, bg));
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        term::restore();
        eprintln!("plica: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let Some(args) = parse_args()? else { return Ok(()) };
    if let Some(p) = &args.check_palette {
        return check_palette(p);
    }

    // Load documents before touching the terminal so errors print cleanly.
    const TAB_WIDTH: usize = 4;
    let mut tabs: Vec<Tab> = Vec::new();
    for path in &args.files {
        if tabs.iter().any(|t| t.shows(path)) {
            continue;
        }
        tabs.push(Tab::open(path, TAB_WIDTH)?.0);
    }
    if tabs.is_empty() {
        tabs.push(Tab::untitled(TAB_WIDTH));
    }

    // Choose the colour source.
    let palette_path = if args.builtin { None } else { args.palette.clone().or_else(default_palette_path) };
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let (theme, source, startup_warning) = match palette_path {
        _ if no_color && args.palette.is_none() && !args.builtin => {
            (theme::resolve(&theme::dracula()), ThemeSource::TerminalDefault, None)
        }
        Some(path) => {
            let path = std::path::absolute(&path).unwrap_or(path);
            let parsed = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| theme::parse_kitty(&t).map_err(|e| e.to_string()));
            match parsed {
                Ok(r) => (theme::resolve(&r.palette), ThemeSource::File { path, last_error: None }, None),
                Err(e) => (
                    theme::resolve(&theme::dracula()),
                    ThemeSource::File { path: path.clone(), last_error: Some(e.clone()) },
                    Some(format!("Palette {} unusable ({e}); using Dracula until it is fixed", path.display())),
                ),
            }
        }
        None => (theme::resolve(&theme::dracula()), ThemeSource::Builtin, None),
    };

    let (guard, caps) = term::TermGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    terminal.clear()?;

    let mut tabs = tabs.into_iter();
    let mut app = App::new(tabs.next().expect("at least one tab"), theme, source, caps);
    app.bg_tabs = tabs.collect();
    if let Some(line) = args.line {
        let l = line.saturating_sub(1).min(app.tab.buf.len_lines() - 1);
        app.tab.buf.sel = Selection::caret(app.tab.buf.line_start(l));
    }
    if let Some(w) = startup_warning {
        app.warn(w);
    } else if app.tab.doc.mixed_line_endings {
        app.warn("File has mixed line endings; they are preserved as-is");
    }

    // Palette watcher (parent directory, survives atomic replacement).
    let (tx, rx) = mpsc::channel();
    let _watcher = match &app.theme_source {
        ThemeSource::File { path, .. } => match watch::PaletteWatcher::new(path, tx) {
            Ok(w) => Some(w),
            Err(e) => {
                app.warn(format!("Cannot watch palette for changes: {e}"));
                None
            }
        },
        _ => None,
    };

    // Document watcher: reload clean tabs / flag conflicts on external change.
    let (doc_tx, doc_rx) = mpsc::channel::<PathBuf>();
    let mut doc_watcher = watch::DocWatcher::new(doc_tx).ok();
    let mut doc_due: std::collections::HashMap<PathBuf, Instant> = Default::default();

    let mut palette_due: Option<Instant> = None;
    let mut redraw = true;
    let mut killed = false;
    while !app.quit {
        if guard.quit_signal.load(Ordering::SeqCst) {
            killed = true;
            break;
        }
        if redraw {
            terminal.draw(|f| ui::draw(f, &mut app))?;
            redraw = false;
        }
        while rx.try_recv().is_ok() {
            palette_due = Some(Instant::now() + PALETTE_DEBOUNCE);
        }
        if let Some(w) = &mut doc_watcher {
            w.sync(&app.watched_dirs());
        }
        while let Ok(p) = doc_rx.try_recv() {
            doc_due.insert(p, Instant::now() + DOC_DEBOUNCE);
        }
        let now = Instant::now();
        let ready: Vec<PathBuf> = doc_due.iter().filter(|(_, t)| **t <= now).map(|(p, _)| p.clone()).collect();
        for p in ready {
            doc_due.remove(&p);
            app.on_disk_change(&p);
            redraw = true;
        }
        if palette_due.is_some_and(|t| Instant::now() >= t) {
            palette_due = None;
            app.reload_palette();
            redraw = true;
            continue;
        }
        let timeout = if palette_due.is_some() || !doc_due.is_empty() || app.status.is_some() { Duration::from_millis(50) } else { Duration::from_millis(250) };
        if event::poll(timeout)? {
            // Drain everything queued before redrawing (fast typing, paste, drags).
            loop {
                app.handle_event(event::read()?);
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
            redraw = true;
        } else {
            redraw |= app.tick();
            // Let expiring status messages disappear.
            if let Some((_, _, at)) = &app.status
                && at.elapsed() > Duration::from_secs(11)
            {
                app.status = None;
                redraw = true;
            } else if app.status.is_some() {
                redraw = true;
            }
        }
    }

    drop(terminal);
    drop(guard);
    if killed {
        for tab in app.all_tabs().filter(|t| t.buf.is_dirty()) {
            match write_recovery(tab) {
                Ok(p) => eprintln!("plica: terminated with unsaved changes; recovery copy written to {}", p.display()),
                Err(e) => eprintln!("plica: terminated with unsaved changes and could not write recovery copy: {e:#}"),
            }
        }
    }
    Ok(())
}

/// Emergency snapshot under $XDG_STATE_HOME/plica/recovery (never the original file).
fn write_recovery(tab: &Tab) -> Result<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/state")))
        .context("no HOME")?;
    let dir = state.join("plica").join("recovery");
    std::fs::create_dir_all(&dir)?;
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let path = dir.join(format!("{}.{ts}.{nanos:09}.txt", tab.doc.display_name()));
    std::fs::write(&path, tab.buf.rope().to_string())?;
    Ok(path)
}
