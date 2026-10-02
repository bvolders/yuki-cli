//! Local sync state for `yuki upload dir`: which files of a directory are
//! already in Yuki, keyed by content hash so a rename or move does not upload
//! a file again.
//!
//! The state lives in `<root>/.yuki-sync.json` and is rewritten atomically
//! (temporary file, fsync, rename, fsync of the directory) after every
//! recorded change. `<root>/.yuki-sync.lock` keeps two runs apart.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::error::YukiError;

/// File name of the state file at the root of the synced directory.
pub const STATE_FILE: &str = ".yuki-sync.json";

/// File name of the lock held while a command changes the state.
pub const LOCK_FILE: &str = ".yuki-sync.lock";

/// The state format this build reads and writes.
pub const STATE_VERSION: u32 = 1;

/// Upload attempts that end in a clean rejection before a file is no longer
/// retried automatically.
pub const MAX_ATTEMPTS: u32 = 3;

/// Extensions `upload dir` picks up, compared case-insensitively.
pub const EXTENSIONS: &[&str] = &["pdf", "jpg", "jpeg", "png"];

/// Excludes that always apply, on top of any `--exclude`.
pub const DEFAULT_EXCLUDES: &[&str] = &["_to_delete", ".*"];

/// What is known about one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Uploaded by `upload dir`.
    Uploaded,
    /// Already in Yuki, recorded by `--seed-from-yuki` or `upload mark`.
    AlreadyInYuki,
    /// Deliberately never uploaded (`upload mark --skip`).
    Skipped,
    /// Yuki rejected the upload; retried up to [`MAX_ATTEMPTS`] times.
    Failed,
    /// The upload may or may not have reached Yuki (a timeout, a server error
    /// without a SOAP fault, an unreadable reply). Never retried automatically:
    /// resolve it with `upload mark --doc-id`, `--forget`, or seeding.
    Unknown,
}

impl Status {
    /// Whether the file is done: never uploaded again.
    pub fn is_settled(self) -> bool {
        matches!(self, Self::Uploaded | Self::AlreadyInYuki | Self::Skipped)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uploaded => "uploaded",
            Self::AlreadyInYuki => "already-in-yuki",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// One file's record, keyed in [`State::files`] by its sha256.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Path relative to the root, `/`-separated, as last seen.
    pub path: String,
    pub size: u64,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
    /// The Yuki folder it was uploaded to or found in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// When `upload dir` uploaded it (UTC, RFC 3339); absent for files found in Yuki.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uploaded_at: Option<String>,
    /// When this record was last written (UTC, RFC 3339).
    pub recorded_at: String,
    /// Upload attempts that did not succeed.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Fields this version does not know, such as a later `matched_transaction`,
    /// kept as they are so rewriting the file never drops them.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Entry {
    /// A fresh record for a file at `path`.
    pub fn new(path: &str, size: u64, status: Status) -> Self {
        Self {
            path: path.to_string(),
            size,
            status,
            document_id: None,
            folder: None,
            uploaded_at: None,
            recorded_at: now_utc(),
            attempts: 0,
            error: None,
            note: None,
            extra: BTreeMap::new(),
        }
    }
}

/// The whole state file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    /// Hash the `files` keys are made with.
    pub hash: String,
    pub files: BTreeMap<String, Entry>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            hash: "sha256".into(),
            files: BTreeMap::new(),
        }
    }
}

impl State {
    /// Load the state of `root`; a missing file is an empty state.
    ///
    /// A file that does not parse is an error, never silently replaced: it may
    /// be the only record of what was uploaded.
    pub fn load(root: &Path) -> Result<Self, YukiError> {
        let path = root.join(STATE_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(YukiError::Config(format!("{}: {e}", path.display()))),
        };
        let corrupt = |detail: String| {
            YukiError::Config(format!(
                "{} is not a valid sync state ({detail}); refusing to overwrite it. \
                 Repair it, or move it aside to start over (files already in Yuki \
                 would then need --seed-from-yuki again)",
                path.display()
            ))
        };
        let raw: Value = serde_json::from_str(&text).map_err(|e| corrupt(e.to_string()))?;
        match raw.get("version").and_then(Value::as_u64) {
            Some(v) if v == u64::from(STATE_VERSION) => {}
            Some(v) if v > u64::from(STATE_VERSION) => {
                return Err(YukiError::Config(format!(
                    "{} has format version {v}, newer than this yuki understands \
                     ({STATE_VERSION}); upgrade yuki",
                    path.display()
                )));
            }
            _ => return Err(corrupt("missing or unknown \"version\"".into())),
        }
        let state: Self = serde_json::from_value(raw).map_err(|e| corrupt(e.to_string()))?;
        if state.hash != "sha256" {
            return Err(corrupt(format!("unsupported hash {:?}", state.hash)));
        }
        Ok(state)
    }

    /// Write the state of `root` atomically: a crash leaves the old or the new
    /// file, never a partial one.
    pub fn save(&self, root: &Path) -> Result<(), YukiError> {
        let path = root.join(STATE_FILE);
        let tmp = root.join(format!("{STATE_FILE}.tmp-{}", std::process::id()));
        let mut json = serde_json::to_string_pretty(self).expect("serialize sync state");
        json.push('\n');
        let written = (|| {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(json.as_bytes())?;
            file.sync_all()?;
            fs::rename(&tmp, &path)
        })();
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            return Err(YukiError::Config(format!("{}: {e}", path.display())));
        }
        // Make the rename itself durable.
        #[cfg(unix)]
        if let Ok(dir) = fs::File::open(root) {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

/// The lock on a synced directory, released when dropped.
#[derive(Debug)]
pub struct Lock {
    path: PathBuf,
}

impl Lock {
    /// Take the lock of `root`, or explain who holds it and since when.
    pub fn acquire(root: &Path) -> Result<Self, YukiError> {
        let path = root.join(LOCK_FILE);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                let body = serde_json::json!({"pid": std::process::id(), "created_at": now_utc()});
                let _ = writeln!(file, "{body}");
                let _ = file.sync_all();
                Ok(Self { path })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let holder = fs::read_to_string(&path)
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                    .and_then(|v| v.get("pid").and_then(Value::as_u64))
                    .map_or_else(|| "unknown pid".to_string(), |p| format!("pid {p}"));
                let age = fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .map_or_else(
                        || "of unknown age".to_string(),
                        |d| format!("taken {} ago", human_duration(d.as_secs())),
                    );
                Err(YukiError::Config(format!(
                    "{} is locked by another yuki run ({holder}, {age}). If no other \
                     run is active, the lock is stale: delete {} and try again",
                    root.display(),
                    path.display()
                )))
            }
            Err(e) => Err(YukiError::Config(format!("{}: {e}", path.display()))),
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn human_duration(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m {}s", s / 60, s % 60),
        s if s < 86_400 => format!("{}h {}m", s / 3_600, s % 3_600 / 60),
        s => format!("{}d {}h", s / 86_400, s % 86_400 / 3_600),
    }
}

/// The lowercase hex sha256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A file name in the form both sides of a name match are compared in:
/// Unicode NFC (macOS file systems hand out NFD) and lowercase.
pub fn name_key(name: &str) -> String {
    name.nfc().collect::<String>().to_lowercase()
}

/// The current time in UTC as RFC 3339, to the second.
pub fn now_utc() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    rfc3339_utc(secs)
}

fn rfc3339_utc(epoch_secs: i64) -> String {
    let date = crate::period::date_from_epoch_days(epoch_secs.div_euclid(86_400));
    let s = epoch_secs.rem_euclid(86_400);
    format!(
        "{date}T{:02}:{:02}:{:02}Z",
        s / 3_600,
        s % 3_600 / 60,
        s % 60
    )
}

/// Compiled exclude patterns, matched case-insensitively.
///
/// A pattern without `/` is matched against every path component, so
/// `_to_delete` or `2025` skips a directory of that name at any depth and
/// `*.png` skips every PNG. A pattern with `/` is matched against the whole
/// relative path, where `*` stays within one component and `**` crosses them.
pub struct Excludes {
    patterns: Vec<(String, glob::Pattern)>,
}

impl Excludes {
    /// The defaults plus `extra`.
    pub fn new(extra: &[String]) -> Result<Self, YukiError> {
        let patterns = DEFAULT_EXCLUDES
            .iter()
            .map(|s| (*s).to_string())
            .chain(extra.iter().cloned())
            .map(|p| {
                glob::Pattern::new(&p)
                    .map(|compiled| (p.clone(), compiled))
                    .map_err(|e| YukiError::Config(format!("invalid --exclude {p:?}: {e}")))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { patterns })
    }

    /// The first pattern that excludes `rel` (a `/`-separated relative path).
    pub fn matching(&self, rel: &str) -> Option<&str> {
        let options = glob::MatchOptions {
            case_sensitive: false,
            require_literal_separator: true,
            require_literal_leading_dot: false,
        };
        self.patterns
            .iter()
            .find(|(raw, pattern)| {
                if raw.contains('/') {
                    pattern.matches_with(rel, options)
                } else {
                    rel.split('/')
                        .any(|part| pattern.matches_with(part, options))
                }
            })
            .map(|(raw, _)| raw.as_str())
    }
}

/// A supported file found by [`scan`].
#[derive(Debug, Clone)]
pub struct Found {
    /// Relative to the root, `/`-separated.
    pub rel: String,
    pub path: PathBuf,
    pub size: u64,
    pub hash: String,
}

/// What [`scan`] saw.
#[derive(Debug, Default)]
pub struct Scan {
    /// Supported files, sorted by relative path.
    pub files: Vec<Found>,
    /// Paths skipped, with the reason: an exclude, a symbolic link, a name
    /// that is not UTF-8.
    pub excluded: Vec<(String, String)>,
    /// Supported files or directories that could not be read, with the error.
    pub unreadable: Vec<(String, String)>,
    /// State files below the root, which would make the state ambiguous.
    pub nested_states: Vec<String>,
}

/// Whether `name` has an extension `upload dir` uploads.
pub fn is_supported(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e))
    })
}

/// The root of the synced tree for `path`: the nearest directory at or above
/// it holding a state file, else `path` itself. Returns (root, path), both
/// canonical.
pub fn resolve_root(path: &Path) -> Result<(PathBuf, PathBuf), YukiError> {
    let base = fs::canonicalize(path)
        .map_err(|e| YukiError::Config(format!("{}: {e}", path.display())))?;
    if !base.is_dir() {
        return Err(YukiError::Config(format!(
            "{}: not a directory",
            path.display()
        )));
    }
    let root = base
        .ancestors()
        .find(|a| a.join(STATE_FILE).is_file())
        .unwrap_or(&base)
        .to_path_buf();
    Ok((root, base))
}

/// Recursively list the supported files under `base` (inside `root`), hashing
/// each one; paths are relative to `root`.
///
/// Symbolic links are reported, never followed. Excluded directories are still
/// walked, to count what they hold and to find nested state files.
pub fn scan(root: &Path, base: &Path, excludes: &Excludes) -> Result<Scan, YukiError> {
    let mut out = Scan::default();
    let prefix = if base == root {
        String::new()
    } else {
        relative_to(root, base).ok_or_else(|| {
            YukiError::Config(format!(
                "{} is not inside {}",
                base.display(),
                root.display()
            ))
        })?
    };
    walk(root, base, &prefix, excludes, &mut out)?;
    out.files.sort_by(|a, b| a.rel.cmp(&b.rel));
    out.excluded.sort();
    out.unreadable.sort();
    out.nested_states.sort();
    Ok(out)
}

fn walk(
    root: &Path,
    dir: &Path,
    prefix: &str,
    excludes: &Excludes,
    out: &mut Scan,
) -> Result<(), YukiError> {
    let join = |name: &str| {
        if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        }
    };
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if dir != root && !prefix.is_empty() => {
            out.unreadable.push((format!("{prefix}/"), e.to_string()));
            return Ok(());
        }
        Err(e) => return Err(YukiError::Config(format!("{}: {e}", dir.display()))),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                out.unreadable.push((join("?"), e.to_string()));
                continue;
            }
        };
        let os_name = entry.file_name();
        let Some(name) = os_name.to_str() else {
            out.excluded.push((
                join(&os_name.to_string_lossy()),
                "name is not valid UTF-8".into(),
            ));
            continue;
        };
        let rel = join(name);
        let path = entry.path();
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(e) => {
                out.unreadable.push((rel, e.to_string()));
                continue;
            }
        };
        if name == STATE_FILE && dir != root {
            out.nested_states.push(rel);
            continue;
        }
        if kind.is_symlink() {
            let reason = match excludes.matching(&rel) {
                Some(pattern) => format!("--exclude {pattern}"),
                None => "symbolic link, not followed".to_string(),
            };
            out.excluded.push((rel, reason));
        } else if kind.is_dir() {
            walk(root, &path, &rel, excludes, out)?;
        } else if kind.is_file() && is_supported(name) {
            if let Some(pattern) = excludes.matching(&rel) {
                out.excluded.push((rel, format!("--exclude {pattern}")));
                continue;
            }
            match fs::read(&path) {
                Ok(bytes) => out.files.push(Found {
                    rel,
                    path,
                    size: bytes.len() as u64,
                    hash: sha256_hex(&bytes),
                }),
                Err(e) => out.unreadable.push((rel, e.to_string())),
            }
        }
    }
    Ok(())
}

/// `path` relative to `root`, `/`-separated, or `None` when it is outside it.
pub fn relative_to(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<&str> = rel
        .components()
        .map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect::<Option<_>>()?;
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// `s` quoted for a POSIX shell: single quotes, with `'` written as `'\''`.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, status: Status) -> Entry {
        let mut e = Entry::new(path, 3, status);
        e.document_id = Some("doc-1".into());
        e
    }

    #[test]
    fn rfc3339_formats_utc_seconds() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(1_790_000_000), "2026-09-21T14:13:20Z");
    }

    #[test]
    fn sha256_matches_a_known_digest() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn name_keys_ignore_unicode_form_and_case() {
        // "café" composed (NFC) and decomposed (NFD, as macOS stores names).
        assert_eq!(name_key("Caf\u{e9}.PDF"), name_key("cafe\u{301}.pdf"));
    }

    #[test]
    fn shell_quote_survives_single_quotes() {
        assert_eq!(shell_quote("a b.pdf"), "'a b.pdf'");
        assert_eq!(shell_quote("it's.pdf"), r"'it'\''s.pdf'");
    }

    #[test]
    fn supported_extensions_are_case_insensitive() {
        for name in ["a.pdf", "a.PDF", "b.Jpg", "c.jpeg", "d.PNG"] {
            assert!(is_supported(name), "{name}");
        }
        for name in ["a.txt", "pdf", ".pdf", "a.pdf.bak", "receipts-log.csv"] {
            assert!(!is_supported(name), "{name}");
        }
    }

    #[test]
    fn default_excludes_skip_dot_and_to_delete_paths() {
        let ex = Excludes::new(&[]).unwrap();
        assert_eq!(ex.matching("_to_delete/x.pdf"), Some("_to_delete"));
        assert_eq!(ex.matching("2026/_TO_DELETE/x.pdf"), Some("_to_delete"));
        assert_eq!(ex.matching(".hidden.pdf"), Some(".*"));
        assert_eq!(ex.matching("2026/.cache/x.pdf"), Some(".*"));
        assert_eq!(ex.matching("2026/bol-com/x.pdf"), None);
    }

    #[test]
    fn excludes_by_component_or_by_path() {
        let ex = Excludes::new(&["2025".into(), "2026/amazon/*".into(), "*.png".into()]).unwrap();
        // A bare name excludes that directory at any depth, recursively.
        assert_eq!(ex.matching("2025/a/b/c.pdf"), Some("2025"));
        assert_eq!(ex.matching("2026/amazon/a.pdf"), Some("2026/amazon/*"));
        // `*` does not cross directories in a path pattern; `**` does.
        assert_eq!(ex.matching("2026/amazon/old/a.pdf"), None);
        assert_eq!(ex.matching("2026/x/scan.PNG"), Some("*.png"));
        let deep = Excludes::new(&["2026/amazon/**".into()]).unwrap();
        assert_eq!(
            deep.matching("2026/amazon/old/a.pdf"),
            Some("2026/amazon/**")
        );
        assert!(Excludes::new(&["[".into()]).is_err());
    }

    #[test]
    fn state_round_trips_and_keeps_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::default();
        let mut e = entry("a.pdf", Status::Unknown);
        e.attempts = 2;
        e.extra
            .insert("matched_transaction".into(), Value::String("t-1".into()));
        state.files.insert("abc".into(), e);
        state.save(dir.path()).unwrap();
        let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
        assert!(text.contains("\"matched_transaction\": \"t-1\""), "{text}");
        assert!(text.contains("\"status\": \"unknown\""), "{text}");
        assert!(text.contains("\"attempts\": 2"), "{text}");
        assert_eq!(State::load(dir.path()).unwrap(), state);
        // No temporary file is left behind.
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from(STATE_FILE)]);
    }

    #[test]
    fn a_missing_state_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(State::load(dir.path()).unwrap(), State::default());
    }

    #[test]
    fn a_corrupt_or_newer_state_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        for (text, expect) in [
            ("{\"version\": 1, \"files\": {", "not a valid sync state"),
            ("[]", "not a valid sync state"),
            ("{\"files\": {}}", "not a valid sync state"),
            (
                "{\"version\": 1, \"hash\": \"sha256\", \"files\": {\"x\": {\"path\": 3}}}",
                "not a valid sync state",
            ),
            (
                "{\"version\": 9, \"hash\": \"sha256\", \"files\": {}}",
                "newer than this yuki",
            ),
        ] {
            fs::write(&path, text).unwrap();
            let err = State::load(dir.path()).unwrap_err().to_string();
            assert!(err.contains(expect), "{text}: {err}");
        }
    }

    #[test]
    fn a_failed_save_leaves_the_old_state_intact() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::default();
        state
            .files
            .insert("abc".into(), entry("a.pdf", Status::Uploaded));
        state.save(dir.path()).unwrap();
        let before = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
        // A directory where the temporary file would go makes the write fail
        // before the rename, as a crash mid-write would.
        let tmp = dir
            .path()
            .join(format!("{STATE_FILE}.tmp-{}", std::process::id()));
        fs::create_dir(&tmp).unwrap();
        state
            .files
            .insert("def".into(), entry("b.pdf", Status::Uploaded));
        assert!(state.save(dir.path()).is_err());
        let after = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn the_lock_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let lock = Lock::acquire(dir.path()).unwrap();
        let text = fs::read_to_string(dir.path().join(LOCK_FILE)).unwrap();
        assert!(
            text.contains(&format!("\"pid\":{}", std::process::id())),
            "{text}"
        );
        let err = Lock::acquire(dir.path()).unwrap_err().to_string();
        assert!(err.contains("locked by another yuki run"), "{err}");
        assert!(err.contains("ago"), "{err}");
        assert!(err.contains(LOCK_FILE), "{err}");
        drop(lock);
        assert!(!dir.path().join(LOCK_FILE).exists());
        assert!(Lock::acquire(dir.path()).is_ok());
    }

    #[test]
    fn scan_hashes_supported_files_and_reports_what_it_skips() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("2026/bol")).unwrap();
        fs::create_dir_all(root.join("_to_delete")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("2026/bol/a.pdf"), b"aaa").unwrap();
        fs::write(root.join("2026/bol/B.JPG"), b"bbb").unwrap();
        fs::write(root.join("2026/notes.txt"), b"x").unwrap();
        fs::write(root.join("_to_delete/old.pdf"), b"old").unwrap();
        fs::write(root.join(".git/x.png"), b"git").unwrap();
        fs::write(root.join(".DS_Store"), b"ds").unwrap();
        fs::write(root.join(STATE_FILE), b"{}").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("2026/bol/a.pdf"), root.join("2026/link.pdf"))
            .unwrap();
        let scan = scan(root, root, &Excludes::new(&[]).unwrap()).unwrap();
        let rels: Vec<_> = scan.files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, ["2026/bol/B.JPG", "2026/bol/a.pdf"]);
        assert_eq!(scan.files[1].hash, sha256_hex(b"aaa"));
        assert_eq!(scan.files[1].size, 3);
        let mut expected = vec![
            (".git/x.png".to_string(), "--exclude .*".to_string()),
            (
                "_to_delete/old.pdf".to_string(),
                "--exclude _to_delete".to_string(),
            ),
        ];
        #[cfg(unix)]
        expected.insert(
            1,
            (
                "2026/link.pdf".to_string(),
                "symbolic link, not followed".to_string(),
            ),
        );
        assert_eq!(scan.excluded, expected);
        assert!(scan.nested_states.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn scan_reports_unreadable_files_and_non_utf8_names() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("ok.pdf"), b"ok").unwrap();
        fs::write(root.join("locked.pdf"), b"no").unwrap();
        fs::set_permissions(root.join("locked.pdf"), fs::Permissions::from_mode(0o000)).unwrap();
        let bad = std::ffi::OsStr::from_bytes(b"bad\xff.pdf");
        // Some file systems (APFS) refuse non-UTF-8 names; test what we can.
        let made_bad = fs::write(root.join(bad), b"x").is_ok();
        // Root reads anything; only expect an error when the mode holds.
        let unreadable = fs::File::open(root.join("locked.pdf")).is_err();
        let scan = scan(root, root, &Excludes::new(&[]).unwrap()).unwrap();
        fs::set_permissions(root.join("locked.pdf"), fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(scan.files.len(), if unreadable { 1 } else { 2 });
        if unreadable {
            assert_eq!(scan.unreadable.len(), 1, "{:?}", scan.unreadable);
            assert_eq!(scan.unreadable[0].0, "locked.pdf");
        }
        if made_bad {
            assert!(
                scan.excluded
                    .iter()
                    .any(|(_, why)| why == "name is not valid UTF-8"),
                "{:?}",
                scan.excluded
            );
        }
    }

    #[test]
    fn scan_of_a_subdirectory_keeps_paths_relative_to_the_root_and_finds_nested_states() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("2026/bol/inner")).unwrap();
        fs::write(root.join("2026/bol/a.pdf"), b"a").unwrap();
        fs::write(root.join("2026/top.pdf"), b"t").unwrap();
        fs::write(root.join("2026/bol/inner").join(STATE_FILE), b"{}").unwrap();
        let base = root.join("2026/bol");
        let scan = scan(&root, &base, &Excludes::new(&[]).unwrap()).unwrap();
        let rels: Vec<_> = scan.files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, ["2026/bol/a.pdf"]);
        assert_eq!(scan.nested_states, ["2026/bol/inner/.yuki-sync.json"]);
    }

    #[test]
    fn the_root_is_the_nearest_directory_with_a_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().canonicalize().unwrap();
        fs::create_dir_all(top.join("a/b")).unwrap();
        assert_eq!(resolve_root(&top.join("a/b")).unwrap().0, top.join("a/b"));
        fs::write(top.join(STATE_FILE), b"{}").unwrap();
        let (root, base) = resolve_root(&top.join("a/b")).unwrap();
        assert_eq!((root, base), (top.clone(), top.join("a/b")));
        assert!(resolve_root(&top.join("missing")).is_err());
    }

    #[test]
    fn relative_paths_stay_inside_the_root() {
        let root = Path::new("/r");
        assert_eq!(
            relative_to(root, Path::new("/r/a/b.pdf")).as_deref(),
            Some("a/b.pdf")
        );
        assert_eq!(relative_to(root, Path::new("/other/b.pdf")), None);
        assert_eq!(relative_to(root, Path::new("/r")), None);
    }
}
