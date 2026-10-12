//! File identity, loading and safe saving.
//!
//! - Only valid UTF-8 is opened (a UTF-8 BOM is remembered and restored).
//!   Anything else is refused rather than risk silent corruption.
//! - Line endings are never rewritten: the buffer holds the file's exact
//!   bytes. `LineEnding` only decides what Enter inserts.
//! - Saves are atomic (temp file in the same directory, fsync, rename, fsync
//!   directory), follow symlinks to their target, and keep mode/owner.

use anyhow::{Context, Result, bail};
use std::fs::{self, File, Metadata};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const BOM: &[u8] = b"\xEF\xBB\xBF";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::CrLf => "CRLF",
        }
    }
}

/// What we knew about the file on disk at last load/save, to detect
/// external modification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskStamp {
    pub mtime: Option<SystemTime>,
    pub len: u64,
    pub ino: u64,
}

impl DiskStamp {
    fn of(m: &Metadata) -> Self {
        Self { mtime: m.modified().ok(), len: m.len(), ino: m.ino() }
    }
}

pub struct Document {
    pub path: Option<PathBuf>,
    pub line_ending: LineEnding,
    pub has_bom: bool,
    pub mixed_line_endings: bool,
    pub stamp: Option<DiskStamp>,
}

pub struct Loaded {
    pub doc: Document,
    pub text: String,
    pub is_new: bool,
}

pub fn detect_line_ending(text: &str) -> (LineEnding, bool) {
    let bytes = text.as_bytes();
    let (mut crlf, mut lf) = (0usize, 0usize);
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            if i > 0 && bytes[i - 1] == b'\r' {
                crlf += 1;
            } else {
                lf += 1;
            }
        }
    }
    let le = if crlf > lf { LineEnding::CrLf } else { LineEnding::Lf };
    (le, crlf > 0 && lf > 0)
}

pub fn load(path: &Path) -> Result<Loaded> {
    // Absolute paths keep tab identity and watcher events (always absolute)
    // comparable, whatever the working directory was.
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let path = abs.as_path();
    match fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Loaded {
                doc: Document {
                    path: Some(path.to_path_buf()),
                    line_ending: LineEnding::Lf,
                    has_bom: false,
                    mixed_line_endings: false,
                    stamp: None,
                },
                text: String::new(),
                is_new: true,
            });
        }
        Err(e) => return Err(e).with_context(|| format!("cannot open {}", path.display())),
        Ok(m) if m.is_dir() => bail!("{} is a directory", path.display()),
        Ok(m) if !m.is_file() => bail!("{} is not a regular file", path.display()),
        Ok(_) => {}
    }
    let mut bytes = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let meta = fs::metadata(path)?;
    let has_bom = bytes.starts_with(BOM);
    if has_bom {
        bytes.drain(..BOM.len());
    }
    let text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(e) => bail!(
            "{} is not valid UTF-8 (first bad byte at offset {}); refusing to open it to avoid corrupting it",
            path.display(),
            e.utf8_error().valid_up_to() + if has_bom { BOM.len() } else { 0 }
        ),
    };
    let (line_ending, mixed) = detect_line_ending(&text);
    Ok(Loaded {
        doc: Document {
            path: Some(path.to_path_buf()),
            line_ending,
            has_bom,
            mixed_line_endings: mixed,
            stamp: Some(DiskStamp::of(&meta)),
        },
        text,
        is_new: false,
    })
}

impl Document {
    pub fn untitled() -> Self {
        Self {
            path: None,
            line_ending: LineEnding::Lf,
            has_bom: false,
            mixed_line_endings: false,
            stamp: None,
        }
    }

    pub fn display_name(&self) -> String {
        match &self.path {
            Some(p) => p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned()),
            None => "Untitled".into(),
        }
    }

    /// True if the file on disk no longer matches what we loaded/saved.
    pub fn changed_on_disk(&self) -> bool {
        let Some(path) = &self.path else { return false };
        match (fs::metadata(path), &self.stamp) {
            (Ok(m), Some(s)) => DiskStamp::of(&m) != *s,
            (Ok(_), None) => true, // appeared since we opened a new file
            (Err(_), Some(_)) => true, // deleted/moved away
            (Err(_), None) => false,
        }
    }

    /// Atomically write `text` to the document path.
    pub fn save(&mut self, text: &str) -> Result<()> {
        let path = self.path.clone().context("no file name")?;
        let target = resolve_symlinks(&path)?;
        let existing = fs::metadata(&target).ok();
        if let Some(m) = &existing
            && !m.is_file()
        {
            bail!("{} is not a regular file; not overwriting", target.display());
        }

        let mut data = Vec::with_capacity(text.len() + 3);
        if self.has_bom {
            data.extend_from_slice(BOM);
        }
        data.extend_from_slice(text.as_bytes());

        // A rename would split hard links apart; write those in place instead.
        if let Some(m) = &existing
            && m.nlink() > 1
        {
            let mut f = fs::OpenOptions::new().write(true).truncate(true).open(&target)?;
            f.write_all(&data)?;
            f.sync_all()?;
        } else {
            atomic_write(&target, &data, existing.as_ref())?;
        }
        self.stamp = fs::metadata(&target).ok().map(|m| DiskStamp::of(&m));
        Ok(())
    }
}

fn resolve_symlinks(path: &Path) -> Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(p) => Ok(p),
        // New file (or dangling symlink): canonicalize the parent.
        Err(_) => {
            let target = match fs::read_link(path) {
                Ok(t) if t.is_absolute() => t,
                Ok(t) => path.parent().unwrap_or(Path::new(".")).join(t),
                Err(_) => path.to_path_buf(),
            };
            let parent = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
            let parent = fs::canonicalize(parent)
                .with_context(|| format!("directory {} does not exist", parent.display()))?;
            Ok(parent.join(target.file_name().context("invalid file name")?))
        }
    }
}

fn atomic_write(target: &Path, data: &[u8], existing: Option<&Metadata>) -> Result<()> {
    let dir = target.parent().context("file has no parent directory")?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".plica-")
        .suffix(".tmp")
        .tempfile_in(dir)
        .with_context(|| format!("cannot create temporary file in {}", dir.display()))?;
    tmp.write_all(data)?;
    tmp.as_file().sync_all()?;
    if let Some(m) = existing {
        tmp.as_file().set_permissions(fs::Permissions::from_mode(m.mode() & 0o7777))?;
        // Keep owner/group where we are allowed to (e.g. editing as root).
        let _ = std::os::unix::fs::fchown(tmp.as_file(), Some(m.uid()), Some(m.gid()));
    } else {
        // New files: honour the umask like a normal create would.
        let mode = 0o666 & !current_umask();
        tmp.as_file().set_permissions(fs::Permissions::from_mode(mode))?;
    }
    tmp.persist(target).map_err(|e| e.error).with_context(|| format!("cannot replace {}", target.display()))?;
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

fn current_umask() -> u32 {
    // Read without changing it: /proc/self/status has a Umask line on Linux.
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Umask:"))
                .and_then(|v| u32::from_str_radix(v.trim(), 8).ok())
        })
        .unwrap_or(0o022)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn line_ending_detection() {
        assert_eq!(detect_line_ending("a\nb\n"), (LineEnding::Lf, false));
        assert_eq!(detect_line_ending("a\r\nb\r\n"), (LineEnding::CrLf, false));
        assert_eq!(detect_line_ending("a\r\nb\r\nc\n"), (LineEnding::CrLf, true));
        assert_eq!(detect_line_ending("no newline"), (LineEnding::Lf, false));
    }

    #[test]
    fn round_trip_preserves_bytes_bom_crlf_and_mode() {
        let d = tmpdir();
        let p = d.path().join("f.txt");
        let original = b"\xEF\xBB\xBFone\r\ntwo\r\nno final newline";
        fs::write(&p, original).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
        let mut l = load(&p).unwrap();
        assert!(l.doc.has_bom);
        assert_eq!(l.doc.line_ending, LineEnding::CrLf);
        l.doc.save(&l.text).unwrap();
        assert_eq!(fs::read(&p).unwrap(), original);
        assert_eq!(fs::metadata(&p).unwrap().mode() & 0o777, 0o640);
        assert!(!l.doc.changed_on_disk());
    }

    #[test]
    fn refuses_invalid_utf8() {
        let d = tmpdir();
        let p = d.path().join("latin1.txt");
        fs::write(&p, b"caf\xe9").unwrap();
        let err = load(&p).err().unwrap().to_string();
        assert!(err.contains("not valid UTF-8"), "{err}");
    }

    #[test]
    fn save_through_symlink_keeps_link() {
        let d = tmpdir();
        let real = d.path().join("real.txt");
        let link = d.path().join("link.txt");
        fs::write(&real, "old").unwrap();
        symlink(&real, &link).unwrap();
        let mut l = load(&link).unwrap();
        l.doc.save("new").unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(&real).unwrap(), "new");
    }

    #[test]
    fn hard_links_stay_linked() {
        let d = tmpdir();
        let a = d.path().join("a.txt");
        let b = d.path().join("b.txt");
        fs::write(&a, "old").unwrap();
        fs::hard_link(&a, &b).unwrap();
        let mut l = load(&a).unwrap();
        l.doc.save("new").unwrap();
        assert_eq!(fs::read_to_string(&b).unwrap(), "new");
    }

    #[test]
    fn detects_external_modification_and_new_files() {
        let d = tmpdir();
        let p = d.path().join("x.txt");
        let mut l = load(&p).unwrap();
        assert!(l.is_new);
        assert!(!l.doc.changed_on_disk());
        l.doc.save("hello").unwrap();
        assert!(!l.doc.changed_on_disk());
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&p, "someone else, longer").unwrap();
        assert!(l.doc.changed_on_disk());
    }

    #[test]
    fn no_temp_files_left_behind() {
        let d = tmpdir();
        let p = d.path().join("x.txt");
        let mut l = load(&p).unwrap();
        l.doc.save("a").unwrap();
        l.doc.save("b").unwrap();
        let names: Vec<_> = fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }
}
