//! Clipboard: internal copy always, plus the system clipboard where possible.
//!
//! Copy writes to (1) the internal clipboard, (2) OSC 52 (works over SSH and
//! in Kitty without extra tools), and (3) a local tool (wl-copy / xclip /
//! xsel) when present. Paste prefers the local tool and falls back to the
//! internal clipboard. Terminal paste (Ctrl+Shift+V in Kitty) arrives as a
//! bracketed paste event instead and never goes through here.
//!
//! The X11/Wayland *primary selection* (select to copy, middle-click to
//! paste) uses the same three routes with the primary target.

use base64::Engine;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct Clipboard {
    internal: String,
    internal_primary: String,
    /// False = never touch the real clipboard (OSC 52 or tools); used by
    /// unit tests so they can't overwrite the developer's live clipboard.
    system: bool,
}

impl Clipboard {
    pub fn system() -> Self {
        Self { internal: String::new(), internal_primary: String::new(), system: true }
    }

    #[cfg(test)]
    pub fn internal_only() -> Self {
        Self { internal: String::new(), internal_primary: String::new(), system: false }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Clipboard,
    Primary,
}

struct Tool {
    copy: &'static [&'static str],
    paste: &'static [&'static str],
}

fn tool(target: Target) -> Option<Tool> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let x11 = std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty());
    let candidates: &[(bool, Tool)] = match target {
        Target::Clipboard => &[
            (wayland, Tool { copy: &["wl-copy"], paste: &["wl-paste", "--no-newline"] }),
            (x11, Tool { copy: &["xclip", "-selection", "clipboard", "-in"], paste: &["xclip", "-selection", "clipboard", "-out"] }),
            (x11, Tool { copy: &["xsel", "--clipboard", "--input"], paste: &["xsel", "--clipboard", "--output"] }),
        ],
        Target::Primary => &[
            (wayland, Tool { copy: &["wl-copy", "--primary"], paste: &["wl-paste", "--primary", "--no-newline"] }),
            (x11, Tool { copy: &["xclip", "-selection", "primary", "-in"], paste: &["xclip", "-selection", "primary", "-out"] }),
            (x11, Tool { copy: &["xsel", "--primary", "--input"], paste: &["xsel", "--primary", "--output"] }),
        ],
    };
    for (ok, t) in candidates {
        if *ok && which(t.copy[0]) {
            return Some(Tool { copy: t.copy, paste: t.paste });
        }
    }
    None
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|d| d.join(bin).is_file())
    })
}

impl Clipboard {
    /// Returns a short description of where the text went, for the status bar.
    pub fn copy(&mut self, text: &str) -> &'static str {
        self.internal = text.to_string();
        if !self.system {
            return "clipboard (internal)";
        }
        osc52('c', text);
        if let Some(t) = tool(Target::Clipboard)
            && run_with_input(t.copy, text).is_ok()
        {
            return "system clipboard";
        }
        "clipboard (OSC 52)"
    }

    /// Set the primary selection (after selecting text).
    pub fn set_primary(&mut self, text: &str) {
        if text.is_empty() || text == self.internal_primary {
            return;
        }
        self.internal_primary = text.to_string();
        if !self.system {
            return;
        }
        osc52('p', text);
        if let Some(t) = tool(Target::Primary) {
            let _ = run_with_input(t.copy, text);
        }
    }

    /// Text for a middle-click paste.
    pub fn paste_primary(&self) -> String {
        if self.system
            && let Some(t) = tool(Target::Primary)
            && let Ok(s) = run_capture(t.paste, Duration::from_millis(700))
            && !s.is_empty()
        {
            return s;
        }
        self.internal_primary.clone()
    }

    pub fn paste(&self) -> String {
        if self.system
            && let Some(t) = tool(Target::Clipboard)
            && let Ok(s) = run_capture(t.paste, Duration::from_millis(700))
            && !s.is_empty()
        {
            return s;
        }
        self.internal.clone()
    }
}

/// `target` is 'c' (clipboard) or 'p' (primary).
fn osc52(target: char, text: &str) {
    // Kitty and most modern terminals accept large payloads; cap to be safe.
    if text.len() > 1_000_000 {
        return;
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(text);
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;{target};{b64}\x07");
    let _ = out.flush();
}

fn run_with_input(argv: &[&str], text: &str) -> std::io::Result<()> {
    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    child.stdin.take().unwrap().write_all(text.as_bytes())?;
    // xclip/wl-copy fork into the background to serve the selection; the
    // foreground process exits promptly. Don't block the UI if it doesn't.
    wait_timeout(&mut child, Duration::from_millis(700));
    Ok(())
}

fn run_capture(argv: &[&str], timeout: Duration) -> std::io::Result<String> {
    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    if !wait_timeout(&mut child, timeout) {
        return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "clipboard tool timed out"));
    }
    let buf = reader.join().unwrap_or_default();
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Returns true if the child exited within the timeout (it is killed otherwise).
fn wait_timeout(child: &mut std::process::Child, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}
