//! Terminal mode setup and guaranteed teardown.
//!
//! `restore()` is idempotent and is called from the guard's Drop, from the
//! panic hook, and on SIGTERM/SIGHUP/SIGINT (via a flag polled by the main
//! loop), so the terminal is not left in raw/mouse mode after a crash.

use ratatui::crossterm::{
    cursor::{SetCursorStyle, Show},
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        supports_keyboard_enhancement,
    },
};
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static KEYBOARD_ENHANCED: AtomicBool = AtomicBool::new(false);
static MOUSE_ON: AtomicBool = AtomicBool::new(false);
/// stdin was the terminal at startup (a hung-up tty no longer passes isatty).
static STDIN_TTY: AtomicBool = AtomicBool::new(false);

pub struct TermGuard {
    pub quit_signal: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy)]
pub struct Capabilities {
    /// Kitty keyboard protocol: distinguishes Ctrl+Shift+letter, Ctrl+I/Tab, etc.
    pub keyboard_enhanced: bool,
    pub truecolor: bool,
}

impl TermGuard {
    pub fn enter() -> io::Result<(Self, Capabilities)> {
        let quit_signal = Arc::new(AtomicBool::new(false));
        for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGHUP, signal_hook::consts::SIGINT] {
            signal_hook::flag::register(sig, Arc::clone(&quit_signal))?;
        }

        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            prev(info);
        }));

        STDIN_TTY.store(rustix::termios::isatty(rustix::stdio::stdin()), Ordering::SeqCst);
        enable_raw_mode()?;
        ACTIVE.store(true, Ordering::SeqCst);
        let mut out = io::stdout();
        execute!(out, EnterAlternateScreen, EnableBracketedPaste, EnableFocusChange)?;
        set_mouse(true)?;
        let keyboard_enhanced = supports_keyboard_enhancement().unwrap_or(false);
        if keyboard_enhanced {
            execute!(
                out,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
            KEYBOARD_ENHANCED.store(true, Ordering::SeqCst);
        }
        execute!(out, SetCursorStyle::SteadyBar)?;
        let truecolor = matches!(
            std::env::var("COLORTERM").as_deref(),
            Ok("truecolor") | Ok("24bit")
        ) || std::env::var("TERM").is_ok_and(|t| t.contains("kitty") || t.contains("direct"));
        Ok((Self { quit_signal }, Capabilities { keyboard_enhanced, truecolor }))
    }
}

/// Toggle mouse reporting. With it off, the terminal's own selection works
/// (the "mouse bypass" from the plan, §3.2).
pub fn set_mouse(on: bool) -> io::Result<()> {
    let mut out = io::stdout();
    if on {
        execute!(out, EnableMouseCapture)?;
    } else {
        execute!(out, DisableMouseCapture)?;
    }
    MOUSE_ON.store(on, Ordering::SeqCst);
    Ok(())
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        restore();
    }
}

pub fn restore() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut out = io::stdout();
    if KEYBOARD_ENHANCED.swap(false, Ordering::SeqCst) {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    if MOUSE_ON.swap(false, Ordering::SeqCst) {
        let _ = execute!(out, DisableMouseCapture);
    }
    let _ = execute!(
        out,
        DisableFocusChange,
        DisableBracketedPaste,
        SetCursorStyle::DefaultUserShape,
        LeaveAlternateScreen,
        Show
    );
    let _ = disable_raw_mode();
    let _ = out.flush();
}

/// True once the terminal has gone away (its window was closed): the tty
/// reports a hangup. Checked every loop pass, because SIGHUP may not arrive
/// (nohup, or Plica not in the foreground process group).
pub fn hung_up() -> bool {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    if !STDIN_TTY.load(Ordering::SeqCst) {
        return false;
    }
    let stdin = rustix::stdio::stdin();
    let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
    let zero = Timespec { tv_sec: 0, tv_nsec: 0 };
    match poll(&mut fds, Some(&zero)) {
        Ok(_) => fds[0].revents().intersects(PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL),
        Err(_) => false,
    }
}
