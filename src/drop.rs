//! Recognise a file drag-and-drop that the terminal delivered as a paste.
//!
//! Terminals have no separate drop event for the program inside them; they
//! paste the dropped files' paths. Formats seen in practice:
//! - Kitty: absolute paths (decoded from file:// URIs), one per line, unquoted.
//! - XFCE Terminal / GNOME Terminal (VTE): shell-quoted paths separated by
//!   spaces, with a trailing space: `'/a/b c.txt' '/d.txt' `.
//! - Some terminals: raw `file:///...` URIs, percent-encoded.
//!
//! A paste counts as a drop only if **every** token is an absolute path (or
//! file:// URI) to something that exists. Anything else stays ordinary text.

use std::path::{Path, PathBuf};

pub fn parse_dropped_paths(text: &str) -> Option<Vec<PathBuf>> {
    let text = text.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    if text.is_empty() || text.len() > 64 * 1024 {
        return None;
    }
    let mut paths = Vec::new();
    for line in text.split(['\n', '\r']).map(str::trim).filter(|l| !l.is_empty()) {
        // An unquoted Kitty line may contain spaces; try it whole first.
        if let Some(p) = to_existing_path(line) {
            paths.push(p);
            continue;
        }
        for tok in shell_split(line)? {
            paths.push(to_existing_path(&tok)?);
        }
    }
    (!paths.is_empty()).then_some(paths)
}

fn to_existing_path(tok: &str) -> Option<PathBuf> {
    let p = if let Some(rest) = tok.strip_prefix("file://") {
        // file:///path or file://localhost/path; other hosts are not local.
        let path = if rest.starts_with('/') { rest } else { rest.strip_prefix("localhost")? };
        PathBuf::from(percent_decode(path)?)
    } else {
        PathBuf::from(tok)
    };
    (p.is_absolute() && Path::new(&p).exists()).then_some(p)
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Minimal POSIX-shell word splitting: whitespace, '...', "...", backslash.
/// Returns None on unbalanced quotes.
fn shell_split(s: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        ch => cur.push(ch),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => cur.push(chars.next()?),
                        ch => cur.push(ch),
                    }
                }
            }
            '\\' => {
                in_word = true;
                cur.push(chars.next()?);
            }
            c if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("plain.txt");
        let b = d.path().join("with space's.md");
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        (d, a, b)
    }

    #[test]
    fn kitty_style_lines() {
        let (_d, a, b) = setup();
        let text = format!("{}\n{}", a.display(), b.display());
        assert_eq!(parse_dropped_paths(&text), Some(vec![a, b]));
    }

    #[test]
    fn vte_style_quoted_with_trailing_space() {
        let (_d, a, b) = setup();
        let q = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
        let text = format!("{} {} ", q(&a), q(&b));
        assert_eq!(parse_dropped_paths(&text), Some(vec![a, b]));
    }

    #[test]
    fn file_uris_percent_encoded() {
        let (_d, _a, b) = setup();
        let enc = b.display().to_string().replace(' ', "%20").replace('\'', "%27");
        assert_eq!(parse_dropped_paths(&format!("file://{enc}\r\n")), Some(vec![b.clone()]));
        assert_eq!(parse_dropped_paths(&format!("file://localhost{enc}")), Some(vec![b]));
        assert_eq!(parse_dropped_paths("file://otherhost/etc/hostname"), None);
    }

    #[test]
    fn ordinary_text_is_not_a_drop() {
        let (_d, a, _b) = setup();
        assert_eq!(parse_dropped_paths("hello world"), None);
        assert_eq!(parse_dropped_paths(""), None);
        assert_eq!(parse_dropped_paths("relative/path.txt"), None);
        assert_eq!(parse_dropped_paths("/no/such/file.txt"), None);
        // One real path mixed with prose is text, not a drop.
        assert_eq!(parse_dropped_paths(&format!("see {} for details", a.display())), None);
        assert_eq!(parse_dropped_paths(&format!("{}\nmore text", a.display())), None);
        assert_eq!(parse_dropped_paths("'unbalanced"), None);
    }
}
