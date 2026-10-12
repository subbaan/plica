mod app;
mod buffer;
mod clipboard;
mod colours;
mod config;
mod dialog;
mod document;
mod drop;
mod find;
mod highlight;
mod prefs;
mod keymap;
mod lineedit;
mod layout;
mod recovery;
mod tab;
mod term;
mod theme;
mod ui;
mod watch;

use anyhow::{Context, Result, bail};
use app::{App, ThemeSource};
use buffer::Selection;
use tab::{Tab, TabOpts};
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
  --config PATH        Use this config file instead of the default
  --print-config       Print a commented default config and exit
  --check-config       Check the config file, list any problems and exit
  --check-palette PATH Parse a palette file, print the resolved colours and exit
  -h, --help           Print help
  -V, --version        Print version

Environment:
  PLICA_PALETTE        Palette file to follow (same as --palette)
  NO_COLOR             Use the terminal's default colours (when theme is \"auto\" and
                       no palette is configured)

Config: $XDG_CONFIG_HOME/plica/config.toml (see --print-config). Colours come from
--palette / PLICA_PALETTE, else the config's [appearance], else
$XDG_CONFIG_HOME/kitty/current-theme.conf if it exists, else Dracula.
Press F1 inside Plica for keys.
";

struct Args {
    files: Vec<PathBuf>,
    line: Option<usize>,
    palette: Option<PathBuf>,
    builtin: bool,
    check_palette: Option<PathBuf>,
    config: Option<PathBuf>,
    check_config: bool,
}

fn parse_args() -> Result<Option<Args>> {
    let mut a = Args { files: Vec::new(), line: None, palette: None, builtin: false, check_palette: None, config: None, check_config: false };
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
                "--config" => {
                    a.config = Some(it.next().context("--config needs a path")?.into());
                    continue;
                }
                "--print-config" => {
                    print!("{}", config::default_text());
                    return Ok(None);
                }
                "--check-config" => {
                    a.check_config = true;
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

fn env_palette() -> Option<PathBuf> {
    std::env::var_os("PLICA_PALETTE").filter(|p| !p.is_empty()).map(|p| app::expand_tilde(&p.to_string_lossy()))
}

fn check_palette(path: &Path, highlights: &highlight::Highlights) -> Result<()> {
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
    // Highlight groups (all file types) as they would be drawn.
    let all = highlight::Highlights { groups: highlights.groups.iter().map(|g| highlight::Group { file_types: Vec::new(), ..g.clone() }).collect(), ..highlights.clone() };
    for g in all.active(None) {
        let r = highlight::resolve(&g, &t);
        let bg = r.bg.unwrap_or(t.editor_bg);
        let fg = r.fg.unwrap_or(t.editor_fg);
        let spec = |c: Option<highlight::Colour>| c.map_or("-".to_string(), |c| format!("{} = {}", c.name(), c.resolve(&t)));
        let lifted = match g.fg {
            Some(c) if c.resolve(&t) != fg => " (adjusted for contrast)",
            _ => "",
        };
        println!("highlight {:<12} fg {} bg {}: drawn {fg} on {bg}{lifted}, contrast {:.2}", g.name, spec(g.fg), spec(g.bg), theme::contrast(fg, bg));
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
    let config_path = args.config.clone().or_else(config::default_path);
    let (cfg, cfg_warnings) = match &config_path {
        Some(p) => config::load(p),
        None => (config::Config::default(), Vec::new()),
    };
    if let Some(p) = &args.check_palette {
        return check_palette(p, &cfg.highlights);
    }
    if args.check_config {
        let shown = config_path.as_ref().map_or_else(|| "(no config path)".into(), |p| p.display().to_string());
        if cfg_warnings.is_empty() {
            println!("{shown}: OK");
            return Ok(());
        }
        for w in &cfg_warnings {
            println!("{shown}: {w}");
        }
        std::process::exit(1);
    }

    // Load documents before touching the terminal so errors print cleanly.
    let tab_opts = TabOpts { tab_width: cfg.tab_width, indent: cfg.indent };
    let mut tabs: Vec<Tab> = Vec::new();
    for path in &args.files {
        if tabs.iter().any(|t| t.shows(path)) {
            continue;
        }
        tabs.push(Tab::open(path, tab_opts)?.0);
    }
    if tabs.is_empty() {
        tabs.push(Tab::untitled(tab_opts));
    }

    // Choose the colour source (CLI > environment > config > auto).
    let palette_overridden = args.builtin || args.palette.is_some() || env_palette().is_some();
    let (colour_choice, theme_note) = if args.builtin {
        (colours::Colours::Builtin, None)
    } else if let Some(p) = args.palette.clone().or_else(env_palette) {
        (colours::Colours::Palette(p), None)
    } else {
        colours::decide(cfg.theme, cfg.palette.as_deref())
    };
    let (theme, source, startup_warning) = colours::load(colour_choice, theme_note);

    let (guard, caps) = term::TermGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    terminal.clear()?;

    let recovery = if cfg.recovery { recovery::default_dir().and_then(|d| recovery::Recovery::new(d).ok()) } else { None };
    let orphans = recovery.as_ref().map(|r| r.orphans()).unwrap_or_default();

    let mut tabs = tabs.into_iter();
    let mut app = App::new(tabs.next().expect("at least one tab"), theme, source, caps);
    app.bg_tabs = tabs.collect();
    app.tab_width = cfg.tab_width;
    app.indent_mode = cfg.indent;
    app.wrap = cfg.wrap;
    app.line_numbers = cfg.line_numbers;
    app.show_whitespace = cfg.show_whitespace;
    app.keymap = cfg.keymap();
    app.primary_selection = cfg.primary_selection;
    app.highlights = cfg.highlights.clone();
    app.theme_choice = cfg.theme;
    app.palette_setting = cfg.palette.clone();
    app.recovery_enabled = cfg.recovery;
    app.config_path = config_path.clone();
    if palette_overridden {
        // Colours came from --palette/--builtin-theme/PLICA_PALETTE; record
        // what that means so Preferences shows (and saves) the truth.
        match &app.theme_source {
            ThemeSource::File { path, .. } => {
                app.theme_choice = config::ThemeChoice::Palette;
                app.palette_setting = Some(path.clone());
            }
            _ => app.theme_choice = config::ThemeChoice::Dracula,
        }
    }
    if !cfg.mouse {
        app.mouse_enabled = false;
        let _ = term::set_mouse(false);
    }
    if let Some(line) = args.line {
        let l = line.saturating_sub(1).min(app.tab.buf.len_lines() - 1);
        app.tab.buf.sel = Selection::caret(app.tab.buf.line_start(l));
    }
    app.recovery = recovery;
    if !cfg_warnings.is_empty() {
        let more = if cfg_warnings.len() > 1 { format!(" (+{} more; run plica --check-config)", cfg_warnings.len() - 1) } else { String::new() };
        app.warn(format!("Config: {}{more}", cfg_warnings[0]));
    }
    if cfg.recovery && recovery_unavailable(&app) {
        app.warn("Recovery snapshots unavailable (cannot create $XDG_STATE_HOME/plica/recovery)");
    }
    app.offer_recovery(orphans);
    if let Some(w) = startup_warning {
        app.warn(w);
    } else if app.tab.doc.mixed_line_endings {
        app.warn("File has mixed line endings; they are preserved as-is");
    }

    // Palette watcher (parent directory, survives atomic replacement).
    // Rebuilt when Preferences change the colour source.
    let (tx, rx) = mpsc::channel();
    let make_palette_watcher = |app: &mut App, tx: mpsc::Sender<watch::PaletteTouched>| match &app.theme_source {
        ThemeSource::File { path, .. } => match watch::PaletteWatcher::new(path, tx) {
            Ok(w) => Some(w),
            Err(e) => {
                app.warn(format!("Cannot watch palette for changes: {e}"));
                None
            }
        },
        _ => None,
    };
    let mut _palette_watcher = make_palette_watcher(&mut app, tx.clone());

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
        app.tick_recovery(false);
        if app.palette_watch_dirty {
            app.palette_watch_dirty = false;
            _palette_watcher = make_palette_watcher(&mut app, tx.clone());
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

    if killed {
        // Snapshot every unsaved tab now; the next start offers them.
        app.tick_recovery(true);
    } else {
        // Normal quit: any remaining unsaved changes were explicitly discarded.
        app.tick_recovery(false);
        app.discard_all_snapshots();
    }
    if let Some(r) = &app.recovery {
        r.flush();
    }
    drop(terminal);
    drop(guard);
    let dirty = app.all_tabs().filter(|t| t.buf.is_dirty()).count();
    if killed && dirty > 0 {
        if app.recovery.is_some() {
            eprintln!("plica: terminated with {dirty} unsaved document(s); start plica again to recover them");
        } else {
            eprintln!("plica: terminated with {dirty} unsaved document(s) and no recovery directory; changes lost");
        }
    }
    Ok(())
}

fn recovery_unavailable(app: &App) -> bool {
    app.recovery.is_none()
}
