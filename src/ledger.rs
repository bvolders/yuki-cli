//! A small local JSON record the CLI keeps next to its work: the sync state
//! of `upload dir` and the ledger of invoice numbers.
//!
//! [`Ledger::open`] takes an OS advisory lock on `.<name>.lock` next to the
//! file, which the kernel releases when the process exits or dies, so a crash
//! never leaves a stale lock. The file carries a `"version"`; one that does
//! not parse, or is newer than this build, is refused rather than replaced,
//! since it may be the only record of what was sent. [`Ledger::save`] writes
//! atomically ([`atomic_write`]). Fields a format does not know are kept by
//! the format itself, with a `#[serde(flatten)]` map.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::YukiError;

/// What a ledger file holds.
pub trait LedgerFormat: Serialize + DeserializeOwned + Default {
    /// The format version this build reads and writes.
    const VERSION: u32;
    /// What the file is, for messages: "sync state".
    const WHAT: &'static str;
    /// What moving a corrupt file aside costs, appended to the refusal.
    const START_OVER: &'static str;
    /// Read a file without `"version"` as [`VERSION`](Self::VERSION): for
    /// files written before the format was versioned.
    const UNVERSIONED: bool = false;

    /// Checks beyond parsing; the error says what is wrong.
    fn check(&self) -> Result<(), String> {
        Ok(())
    }
}

/// A ledger file, loaded, and locked unless [`peek`](Self::peek)ed.
#[derive(Debug)]
pub struct Ledger<S> {
    path: PathBuf,
    state: S,
    lock: Option<fs::File>,
}

/// The state before a [`Ledger::write_ahead`], to [`undo`](Ledger::undo) it.
#[must_use = "commit or undo the write-ahead"]
pub struct WriteAhead<S>(S);

impl<S: LedgerFormat> Ledger<S> {
    /// Lock and load the ledger at `path`; a missing file is an empty state.
    /// Refused while another process holds the lock.
    pub fn open(path: &Path) -> Result<Self, YukiError> {
        let lock = lock(path)?;
        Ok(Self {
            lock: Some(lock),
            ..Self::peek(path)?
        })
    }

    /// Load the ledger at `path` without locking it, to read only.
    pub fn peek(path: &Path) -> Result<Self, YukiError> {
        Ok(Self {
            path: path.to_path_buf(),
            state: load(path)?,
            lock: None,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn state(&self) -> &S {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut S {
        &mut self.state
    }

    /// Write the state atomically. Only an [`open`](Self::open)ed ledger saves.
    pub fn save(&self) -> Result<(), YukiError> {
        if self.lock.is_none() {
            return Err(YukiError::Config(format!(
                "{} was read without its lock and cannot be saved",
                self.path.display()
            )));
        }
        let mut json = serde_json::to_string_pretty(&Versioned {
            version: S::VERSION,
            state: &self.state,
        })
        .expect("serialize ledger");
        json.push('\n');
        atomic_write(&self.path, json.as_bytes())
            .map_err(|e| YukiError::Config(format!("{}: {e}", self.path.display())))
    }
}

impl<S: LedgerFormat + Clone> Ledger<S> {
    /// Write ahead: apply `change` (say, mark a record pending) and save it
    /// before the call whose outcome it records. Settle the result with
    /// [`commit`](Self::commit), or [`undo`](Self::undo) it when the call
    /// provably changed nothing. A crash in between leaves the change on disk.
    pub fn write_ahead(&mut self, change: impl FnOnce(&mut S)) -> Result<WriteAhead<S>, YukiError> {
        let before = self.state.clone();
        change(&mut self.state);
        match self.save() {
            Ok(()) => Ok(WriteAhead(before)),
            Err(e) => {
                self.state = before;
                Err(e)
            }
        }
    }

    /// Record the outcome of a write-ahead.
    pub fn commit(
        &mut self,
        _ahead: WriteAhead<S>,
        change: impl FnOnce(&mut S),
    ) -> Result<(), YukiError> {
        change(&mut self.state);
        self.save()
    }

    /// Put the state back as it was before the write-ahead.
    pub fn undo(&mut self, ahead: WriteAhead<S>) -> Result<(), YukiError> {
        self.state = ahead.0;
        self.save()
    }
}

#[derive(Serialize)]
struct Versioned<'a, S> {
    version: u32,
    #[serde(flatten)]
    state: &'a S,
}

/// `.<name>.<suffix>` next to `path`, or `<name>.<suffix>` for a dotfile.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .map_or("ledger".into(), |n| n.to_string_lossy());
    let dot = if name.starts_with('.') { "" } else { "." };
    path.with_file_name(format!("{dot}{name}.{suffix}"))
}

fn lock(path: &Path) -> Result<fs::File, YukiError> {
    let lock = sibling(path, "lock");
    let io = |e: std::io::Error| YukiError::Config(format!("{}: {e}", lock.display()));
    if let Some(dir) = lock.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(io)?;
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock)
        .map_err(io)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => Err(YukiError::Config(format!(
            "another yuki run is working on {}; try again when it has finished",
            path.display()
        ))),
        Err(fs::TryLockError::Error(e)) => Err(io(e)),
    }
}

fn load<S: LedgerFormat>(path: &Path) -> Result<S, YukiError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(S::default()),
        Err(e) => return Err(YukiError::Config(format!("{}: {e}", path.display()))),
    };
    let corrupt = |detail: String| {
        YukiError::Config(format!(
            "{} is not a valid {} ({detail}); refusing to overwrite it. \
             Repair it, or move it aside to start over{}",
            path.display(),
            S::WHAT,
            S::START_OVER
        ))
    };
    let mut raw: Value = serde_json::from_str(&text).map_err(|e| corrupt(e.to_string()))?;
    let version = raw.as_object_mut().and_then(|o| o.remove("version"));
    match version.as_ref().map(Value::as_u64) {
        Some(Some(v)) if v == u64::from(S::VERSION) => {}
        Some(Some(v)) if v > u64::from(S::VERSION) => {
            return Err(YukiError::Config(format!(
                "{} has format version {v}, newer than this yuki understands ({}); upgrade yuki",
                path.display(),
                S::VERSION
            )));
        }
        None if S::UNVERSIONED && raw.is_object() => {}
        _ => return Err(corrupt("missing or unknown \"version\"".into())),
    }
    let state: S = serde_json::from_value(raw).map_err(|e| corrupt(e.to_string()))?;
    state.check().map_err(corrupt)?;
    Ok(state)
}

/// Replace `path` with `bytes` atomically: a temporary file next to it,
/// flushed to disk, renamed over it, then the directory synced, so a crash
/// leaves the old file or the new one, never part of one.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = sibling(path, &format!("tmp-{}", std::process::id()));
    let written = (|| {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        // Make the rename itself durable.
        let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
        fs::File::open(dir.unwrap_or(Path::new(".")))?.sync_all()
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    struct Notes {
        #[serde(default)]
        notes: Vec<String>,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    }

    impl LedgerFormat for Notes {
        const VERSION: u32 = 2;
        const WHAT: &'static str = "note file";
        const START_OVER: &'static str = " (the notes would be lost)";
        fn check(&self) -> Result<(), String> {
            match self.notes.iter().any(String::is_empty) {
                true => Err("an empty note".into()),
                false => Ok(()),
            }
        }
    }

    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    struct Legacy {
        #[serde(default)]
        notes: Vec<String>,
    }

    impl LedgerFormat for Legacy {
        const VERSION: u32 = 1;
        const WHAT: &'static str = "legacy file";
        const START_OVER: &'static str = "";
        const UNVERSIONED: bool = true;
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn saves_versioned_and_keeps_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        fs::write(
            &path,
            r#"{"version": 2, "notes": ["a"], "later": {"x": 1}}"#,
        )
        .unwrap();
        let mut ledger = Ledger::<Notes>::open(&path).unwrap();
        ledger.state_mut().notes.push("b".into());
        ledger.save().unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("{\n  \"version\": 2,"), "{text}");
        assert!(text.contains("\"later\""), "{text}");
        drop(ledger);
        assert_eq!(
            Ledger::<Notes>::peek(&path).unwrap().state().notes,
            ["a", "b"]
        );
        // Only the file and its lock: no temporary file left behind.
        assert_eq!(names(dir.path()), [".notes.json.lock", "notes.json"]);
    }

    #[test]
    fn the_lock_keeps_a_second_opener_out_until_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".state.json");
        let held = Ledger::<Notes>::open(&path).unwrap();
        let err = Ledger::<Notes>::open(&path).unwrap_err().to_string();
        assert!(err.contains("another yuki run"), "{err}");
        // Peeking needs no lock, and a peeked ledger cannot save.
        let peeked = Ledger::<Notes>::peek(&path).unwrap();
        assert!(peeked.save().is_err());
        drop(held);
        Ledger::<Notes>::open(&path).unwrap();
        assert!(dir.path().join(".state.json.lock").exists());
    }

    #[test]
    fn a_corrupt_or_newer_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        for (text, expect) in [
            ("{\"version\": 2, \"notes\": [", "not a valid note file"),
            ("[]", "not a valid note file"),
            ("{\"notes\": []}", "missing or unknown \"version\""),
            ("{\"version\": 2, \"notes\": 3}", "not a valid note file"),
            ("{\"version\": 2, \"notes\": [\"\"]}", "an empty note"),
            ("{\"version\": 3}", "newer than this yuki"),
        ] {
            fs::write(&path, text).unwrap();
            let err = Ledger::<Notes>::open(&path).unwrap_err().to_string();
            assert!(err.contains(expect), "{text}: {err}");
            assert_eq!(fs::read_to_string(&path).unwrap(), text, "left alone");
        }
        // A format that predates versioning reads a file without one.
        fs::write(&path, r#"{"notes": ["old"]}"#).unwrap();
        assert_eq!(
            Ledger::<Legacy>::peek(&path).unwrap().state().notes,
            ["old"]
        );
    }

    #[test]
    fn a_failed_write_leaves_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        atomic_write(&path, b"old").unwrap();
        // A directory where the temporary file would go fails the write
        // before the rename, as a crash mid-write would.
        fs::create_dir(
            dir.path()
                .join(format!(".notes.json.tmp-{}", std::process::id())),
        )
        .unwrap();
        assert!(atomic_write(&path, b"new").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old");
    }

    #[test]
    fn a_write_ahead_is_on_disk_until_committed_or_undone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        let mut ledger = Ledger::<Notes>::open(&path).unwrap();
        let ahead = ledger
            .write_ahead(|s| s.notes.push("pending".into()))
            .unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("pending"));
        ledger.undo(ahead).unwrap();
        assert!(
            Ledger::<Notes>::peek(&path)
                .unwrap()
                .state()
                .notes
                .is_empty()
        );
        let ahead = ledger
            .write_ahead(|s| s.notes.push("pending".into()))
            .unwrap();
        ledger
            .commit(ahead, |s| s.notes[0] = "done".into())
            .unwrap();
        assert_eq!(
            Ledger::<Notes>::peek(&path).unwrap().state().notes,
            ["done"]
        );
    }
}
