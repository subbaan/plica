//! Crash-recovery snapshots of unsaved work.
//!
//! Snapshots live in `$XDG_STATE_HOME/plica/recovery/` (never next to or in
//! place of the original file). A background thread does the writing, so
//! saving a large buffer never stalls typing; the rope is cloned in O(1).
//!
//! File format (UTF-8): a header of `key: value` lines, a blank line, then
//! the document text exactly as in the buffer.
//!
//! ```text
//! plica-recovery 1
//! pid: 4242
//! path: /home/me/notes.txt      (empty for Untitled; %-escaped)
//! name: notes.txt
//! time: 1760270000              (unix seconds)
//! encoding: windows-1252        (the document's, for saving after restore)
//!
//! ...text...
//! ```

use crate::encoding::Encoding;
use ropey::Rope;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};

const MAGIC: &str = "plica-recovery 1";
const EXT: &str = "plica-snap";

enum Job {
    Write { file: PathBuf, header: String, text: Rope },
    Remove(PathBuf),
    Flush(Sender<()>),
}

pub struct Recovery {
    dir: PathBuf,
    session: String,
    tx: Sender<Job>,
}

/// A snapshot left behind by a Plica process that is no longer running.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub file: PathBuf,
    pub doc_path: Option<PathBuf>,
    pub name: String,
    pub time: u64,
    pub text: String,
    /// Missing in snapshots from Plica before 0.3.1.
    pub encoding: Option<Encoding>,
    /// The text wasn't valid UTF-8 (a damaged file): the bad bytes became
    /// U+FFFD so the rest can still be recovered.
    pub damaged: bool,
}

pub fn default_dir() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/state")))?;
    Some(state.join("plica").join("recovery"))
}

impl Recovery {
    pub fn new(dir: PathBuf) -> std::io::Result<Self> {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new().name("plica-recovery".into()).spawn(move || {
            for job in rx {
                match job {
                    Job::Write { file, header, text } => {
                        let _ = write_atomic(&file, &header, &text);
                    }
                    Job::Remove(file) => {
                        let _ = fs::remove_file(&file);
                    }
                    Job::Flush(done) => {
                        let _ = done.send(());
                    }
                }
            }
        })?;
        // pid + nanoseconds + a per-process counter: unique even for two
        // sessions created in the same instant (or by the same process).
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let session = format!("{}.{}.{n}", std::process::id(), now.as_nanos());
        Ok(Self { dir, session, tx })
    }

    fn file_for(&self, tab_uid: u64) -> PathBuf {
        self.dir.join(format!("{}-{tab_uid}.{EXT}", self.session))
    }

    pub fn write(&self, tab_uid: u64, doc_path: Option<&Path>, name: &str, encoding: Encoding, text: &Rope) {
        let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let path = doc_path.map(|p| escape(&p.to_string_lossy())).unwrap_or_default();
        let header = format!(
            "{MAGIC}\npid: {}\npath: {path}\nname: {}\ntime: {time}\nencoding: {}\n\n",
            std::process::id(),
            escape(name),
            encoding.config_name()
        );
        let _ = self.tx.send(Job::Write { file: self.file_for(tab_uid), header, text: text.clone() });
    }

    pub fn remove(&self, tab_uid: u64) {
        let _ = self.tx.send(Job::Remove(self.file_for(tab_uid)));
    }

    /// Block until every queued write/remove has finished.
    pub fn flush(&self) {
        let (tx, rx) = mpsc::channel();
        if self.tx.send(Job::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(std::time::Duration::from_secs(10));
        }
    }

    /// Snapshots from Plica processes that are no longer running. Also
    /// removes half-written temp files those processes left behind (a
    /// snapshot is renamed into place only once complete, so the previous
    /// complete one is still there).
    pub fn orphans(&self) -> Vec<Snapshot> {
        let Ok(entries) = fs::read_dir(&self.dir) else { return Vec::new() };
        let files: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        for tmp in files.iter().filter(|p| p.extension().is_some_and(|x| x == "tmp")) {
            let pid = tmp.file_name().and_then(|n| n.to_str()).and_then(|n| n.split('.').next()).and_then(|p| p.parse::<u32>().ok());
            if pid.is_some_and(|pid| !plica_running(pid)) {
                let _ = fs::remove_file(tmp);
            }
        }
        let mut out: Vec<Snapshot> = files
            .into_iter()
            .filter(|p| p.extension().is_some_and(|x| x == EXT))
            .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&format!("{}-", self.session))))
            .filter_map(|p| parse(&p))
            .filter(|(pid, _)| !plica_running(*pid))
            .map(|(_, s)| s)
            .collect();
        out.sort_by_key(|s| s.time);
        out
    }

    /// Delete snapshot files (after restoring or discarding them).
    pub fn delete(&self, snaps: &[Snapshot]) {
        for s in snaps {
            let _ = self.tx.send(Job::Remove(s.file.clone()));
        }
    }
}

fn write_atomic(file: &Path, header: &str, text: &Rope) -> std::io::Result<()> {
    let tmp = file.with_extension("tmp");
    {
        let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(header.as_bytes())?;
        for chunk in text.chunks() {
            f.write_all(chunk.as_bytes())?;
        }
        f.sync_all()?;
    }
    fs::rename(&tmp, file)
}

/// Read a snapshot. A damaged one still yields what can be read: an
/// unreadable pid counts as a finished process, and invalid UTF-8 in the
/// text is replaced (and flagged) rather than losing the whole document.
fn parse(file: &Path) -> Option<(u32, Snapshot)> {
    let data = fs::read(file).ok()?;
    let split = data.windows(2).position(|w| w == b"\n\n")?;
    let head = String::from_utf8_lossy(&data[..split]);
    let body = &data[split + 2..];
    let (text, damaged) = match std::str::from_utf8(body) {
        Ok(t) => (t.to_string(), false),
        Err(_) => (String::from_utf8_lossy(body).into_owned(), true),
    };
    let mut lines = head.lines();
    if lines.next()? != MAGIC {
        return None;
    }
    let (mut pid, mut path, mut name, mut time, mut encoding) = (0u32, String::new(), String::new(), 0u64, None);
    for l in lines {
        let (k, v) = l.split_once(": ").unwrap_or((l.trim_end_matches(':'), ""));
        match k {
            "pid" => pid = v.parse().unwrap_or(0),
            "path" => path = unescape(v),
            "name" => name = unescape(v),
            "time" => time = v.parse().unwrap_or(0),
            "encoding" => encoding = Encoding::parse(v),
            _ => {}
        }
    }
    let doc_path = (!path.is_empty()).then(|| PathBuf::from(path));
    Some((pid, Snapshot { file: file.to_path_buf(), doc_path, name, time, text, encoding, damaged }))
}

/// Is a process with this pid alive *and* a Plica (guards against pid reuse)?
/// A zombie (exited but not yet reaped by its parent) counts as gone: it
/// still has a /proc entry and its comm, but it will never clean up.
fn plica_running(pid: u32) -> bool {
    let is_plica = fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == "plica");
    is_plica && !process_state(pid).is_some_and(|st| st == 'Z' || st == 'X')
}

/// Process state letter from /proc/<pid>/stat (the field after the
/// parenthesised command name, which may itself contain spaces/parens).
fn process_state(pid: u32) -> Option<char> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat[stat.rfind(')')? + 1..].trim_start().chars().next()
}

fn escape(s: &str) -> String {
    s.replace('%', "%25").replace('\n', "%0A").replace('\r', "%0D")
}

fn unescape(s: &str) -> String {
    s.replace("%0A", "\n").replace("%0D", "\r").replace("%25", "%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_parse_round_trip_and_orphan_detection() {
        let d = tempfile::tempdir().unwrap();
        let r = Recovery::new(d.path().join("rec")).unwrap();
        let text = Rope::from_str("line one\n\nline three, after a blank line\r\n");
        let enc = Encoding::parse("shift_jis").unwrap();
        r.write(7, Some(Path::new("/tmp/odd%name\nx.txt")), "x.txt", enc, &text);
        r.flush();
        let files: Vec<_> = fs::read_dir(d.path().join("rec")).unwrap().map(|e| e.unwrap().path()).collect();
        assert_eq!(files.len(), 1, "no temp file left: {files:?}");
        let mode = std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&files[0]).unwrap().permissions());
        assert_eq!(mode & 0o777, 0o600);
        let (pid, s) = parse(&files[0]).unwrap();
        assert_eq!(pid, std::process::id());
        assert_eq!(s.text, text.to_string(), "text after the header is exact");
        assert_eq!(s.doc_path.as_deref(), Some(Path::new("/tmp/odd%name\nx.txt")));
        assert_eq!(s.encoding, Some(enc));
        // Our own snapshots are never orphans; a fresh session sees them
        // only once the writing process is gone (here it is the test binary,
        // whose comm is not "plica", so it counts as gone).
        assert!(r.orphans().is_empty());
        let other = Recovery::new(d.path().join("rec")).unwrap();
        assert_eq!(other.orphans().len(), 1);
        other.delete(&other.orphans());
        other.flush();
        assert!(other.orphans().is_empty());
    }

    #[test]
    fn damaged_snapshots_still_offer_their_text_and_stale_temps_go() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("rec");
        let r = Recovery::new(dir.clone()).unwrap();
        // A dead session (pid 0 never runs Plica): a snapshot with a bad
        // byte in its text and a mangled pid, and a half-written temp file.
        let mut bytes = format!("{MAGIC}\npid: 12x\npath: /tmp/a.txt\nname: a.txt\ntime: 5\n\n").into_bytes();
        bytes.extend_from_slice(b"caf\xe9 and the rest\n");
        fs::write(dir.join(format!("0.1.0-1.{EXT}")), &bytes).unwrap();
        fs::write(dir.join("0.1.0-2.tmp"), b"plica-recovery 1\npid: 0\n").unwrap();
        // Garbage that isn't a snapshot at all is left alone.
        fs::write(dir.join(format!("0.1.0-3.{EXT}")), b"\xff\xfe junk").unwrap();
        let o = r.orphans();
        assert_eq!(o.len(), 1, "{o:?}");
        assert!(o[0].damaged);
        assert_eq!(o[0].text, "caf\u{fffd} and the rest\n");
        assert_eq!(o[0].doc_path.as_deref(), Some(Path::new("/tmp/a.txt")));
        assert!(!dir.join("0.1.0-2.tmp").exists(), "stale temp file removed");
        assert!(dir.join(format!("0.1.0-3.{EXT}")).exists());
    }

    #[test]
    fn own_process_state_is_running() {
        let st = process_state(std::process::id()).unwrap();
        assert!(!matches!(st, 'Z' | 'X'), "{st}");
    }

    #[test]
    fn remove_after_write_is_ordered() {
        let d = tempfile::tempdir().unwrap();
        let r = Recovery::new(d.path().to_path_buf()).unwrap();
        r.write(1, None, "Untitled", Encoding::UTF8, &Rope::from_str("x"));
        r.remove(1);
        r.flush();
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 0);
    }
}
