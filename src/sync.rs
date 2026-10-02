//! Local sync state for `yuki upload dir`: which files of a directory are
//! already in Yuki, keyed by content hash so a rename or move does not upload
//! a file again.
//!
//! The state lives in `<root>/.yuki-sync.json` and is rewritten atomically
//! (temporary file, fsync, rename, fsync of the directory). An OS advisory
//! lock on `<root>/.yuki-sync.json.lock` keeps two runs apart; the kernel
//! releases it when the holder exits or dies.

use std::collections::{BTreeMap, HashSet};
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

/// File the advisory lock is taken on.
pub const LOCK_FILE: &str = ".yuki-sync.json.lock";

/// The state format this build reads and writes.
pub const STATE_VERSION: u32 = 1;

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
    /// The upload request provably never reached Yuki; retried next run.
    Failed,
    /// An upload was started and its outcome is not known: it may be in Yuki.
    /// Written before every upload and replaced once Yuki answers, so a
    /// crash, a kill or a timeout leaves it. Never retried automatically:
    /// resolve it with `upload mark` or seeding.
    Pending,
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
            Self::Pending => "pending",
        }
    }
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
    /// When this record was last written (UTC, RFC 3339); for a pending
    /// entry, when the upload was started.
    pub recorded_at: String,
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
    /// The existing record of `hash` updated to `status` for a file at `rel`,
    /// or a fresh one; unknown fields are kept.
    pub fn update(state: &State, hash: &str, rel: &str, size: u64, status: Status) -> Self {
        let mut e = state.files.get(hash).cloned().unwrap_or_else(|| Self {
            path: String::new(),
            size,
            status,
            document_id: None,
            folder: None,
            uploaded_at: None,
            recorded_at: String::new(),
            error: None,
            note: None,
            extra: BTreeMap::new(),
        });
        e.path = rel.to_string();
        e.size = size;
        e.status = status;
        e.recorded_at = now_utc();
        e.error = None;
        e
    }
}

/// The whole state file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    /// Hash the `files` keys are made with.
    pub hash: String,
    pub files: BTreeMap<String, Entry>,
    /// Top-level fields this version does not know, kept as they are.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            hash: "sha256".into(),
            files: BTreeMap::new(),
            extra: BTreeMap::new(),
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

    /// The record at `rel` whose content is no longer in the tree (`present`
    /// holds the hashes scanned): a different file at that path is a changed
    /// version of it, while content that moved elsewhere keeps its record.
    pub fn stale_record_at(&self, rel: &str, present: &HashSet<&str>) -> Option<(&str, &Entry)> {
        self.files
            .iter()
            .find(|(h, e)| {
                e.path == rel && e.status != Status::Failed && !present.contains(h.as_str())
            })
            .map(|(h, e)| (h.as_str(), e))
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
            fs::rename(&tmp, &path)?;
            // Make the rename itself durable.
            fs::File::open(root)?.sync_all()
        })();
        written.map_err(|e| {
            let _ = fs::remove_file(&tmp);
            YukiError::Config(format!("{}: {e}", path.display()))
        })
    }
}

/// The advisory lock on a synced directory, held while the file is open.
#[derive(Debug)]
pub struct Lock(#[allow(dead_code)] fs::File);

impl Lock {
    /// Take the lock of `root`, or refuse when another run holds it.
    pub fn acquire(root: &Path) -> Result<Self, YukiError> {
        let path = root.join(LOCK_FILE);
        let io = |e: std::io::Error| YukiError::Config(format!("{}: {e}", path.display()));
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io)?;
        match file.try_lock() {
            Ok(()) => Ok(Self(file)),
            Err(fs::TryLockError::WouldBlock) => Err(YukiError::Config(format!(
                "another yuki run is working on {}; try again when it has finished",
                root.display()
            ))),
            Err(fs::TryLockError::Error(e)) => Err(io(e)),
        }
    }
}

/// The lowercase hex sha256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The size and sha256 of the file at `path`, streamed rather than read whole.
fn hash_file(path: &Path) -> std::io::Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let size = std::io::copy(
        &mut std::io::BufReader::new(fs::File::open(path)?),
        &mut hasher,
    )?;
    Ok((size, format!("{:x}", hasher.finalize())))
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
    /// The defaults plus `extra`. A leading `./` and a trailing `/` are
    /// dropped, and patterns are compared in Unicode NFC like the paths.
    pub fn new(extra: &[String]) -> Result<Self, YukiError> {
        let patterns = DEFAULT_EXCLUDES
            .iter()
            .map(|s| (*s).to_string())
            .chain(extra.iter().map(|p| {
                let p = p.strip_prefix("./").unwrap_or(p);
                p.strip_suffix('/').unwrap_or(p).nfc().collect()
            }))
            .map(|p: String| {
                glob::Pattern::new(&p)
                    .map(|compiled| (p.clone(), compiled))
                    .map_err(|e| YukiError::Config(format!("invalid --exclude {p:?}: {e}")))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { patterns })
    }

    /// The first pattern that excludes `rel` (a `/`-separated relative path).
    pub fn matching(&self, rel: &str) -> Option<&str> {
        let rel: String = rel.nfc().collect();
        let rel = rel.as_str();
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

fn canonical_dir(path: &Path) -> Result<PathBuf, YukiError> {
    let dir = fs::canonicalize(path)
        .map_err(|e| YukiError::Config(format!("{}: {e}", path.display())))?;
    if !dir.is_dir() {
        return Err(YukiError::Config(format!(
            "{}: not a directory",
            path.display()
        )));
    }
    Ok(dir)
}

/// `path` as a sync root: a directory with no state file above it.
pub fn sync_root(path: &Path) -> Result<PathBuf, YukiError> {
    let root = canonical_dir(path)?;
    if let Some(above) = root
        .ancestors()
        .skip(1)
        .find(|a| a.join(STATE_FILE).is_file())
    {
        return Err(YukiError::Config(format!(
            "{} is inside the synced directory {}; run on {} instead",
            root.display(),
            above.display(),
            above.display()
        )));
    }
    Ok(root)
}

/// The nearest directory at or above `start` that holds a state file.
pub fn find_root(start: &Path) -> Result<Option<PathBuf>, YukiError> {
    let start = canonical_dir(start)?;
    Ok(start
        .ancestors()
        .find(|a| a.join(STATE_FILE).is_file())
        .map(Path::to_path_buf))
}

/// Recursively list the supported files under `root`, hashing each one.
///
/// Symbolic links are reported, never followed. Excluded directories are still
/// walked, to list what they hold and to find nested state files.
pub fn scan(root: &Path, excludes: &Excludes) -> Result<Scan, YukiError> {
    let mut out = Scan::default();
    fs::read_dir(root).map_err(|e| YukiError::Config(format!("{}: {e}", root.display())))?;
    walk(root, "", excludes, &mut out);
    out.files.sort_by(|a, b| a.rel.cmp(&b.rel));
    out.excluded.sort();
    out.unreadable.sort();
    out.nested_states.sort();
    Ok(out)
}

fn walk(dir: &Path, prefix: &str, excludes: &Excludes, out: &mut Scan) {
    let join = |name: &str| {
        if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        }
    };
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            out.unreadable.push((format!("{prefix}/"), e.to_string()));
            return;
        }
    };
    for entry in entries.flatten() {
        let os_name = entry.file_name();
        let Some(name) = os_name.to_str() else {
            let rel = join(&os_name.to_string_lossy());
            out.excluded.push((rel, "name is not valid UTF-8".into()));
            continue;
        };
        let rel = join(name);
        let Ok(kind) = entry.file_type() else {
            out.unreadable.push((rel, "cannot read file type".into()));
            continue;
        };
        if name == STATE_FILE && !prefix.is_empty() {
            out.nested_states.push(rel);
        } else if kind.is_symlink() {
            let reason = excludes.matching(&rel).map_or_else(
                || "symbolic link, not followed".to_string(),
                |p| format!("--exclude {p}"),
            );
            out.excluded.push((rel, reason));
        } else if kind.is_dir() {
            walk(&entry.path(), &rel, excludes, out);
        } else if kind.is_file() && is_supported(name) {
            if let Some(pattern) = excludes.matching(&rel) {
                out.excluded.push((rel, format!("--exclude {pattern}")));
                continue;
            }
            let path = entry.path();
            match hash_file(&path) {
                Ok((size, hash)) => out.files.push(Found {
                    rel,
                    path,
                    size,
                    hash,
                }),
                Err(e) => out.unreadable.push((rel, e.to_string())),
            }
        }
    }
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
        let mut e = Entry::update(&State::default(), "x", path, 3, status);
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
    fn excludes_by_component_or_by_path_ignoring_case() {
        let ex = Excludes::new(&["2025".into(), "2026/amazon/*".into(), "*.png".into()]).unwrap();
        assert_eq!(ex.matching("_TO_DELETE/x.pdf"), Some("_to_delete"));
        assert_eq!(ex.matching("2026/.cache/x.pdf"), Some(".*"));
        // A bare name excludes that directory at any depth, recursively.
        assert_eq!(ex.matching("2025/a/b/c.pdf"), Some("2025"));
        assert_eq!(ex.matching("2026/amazon/a.pdf"), Some("2026/amazon/*"));
        // `*` does not cross directories in a path pattern; `**` does.
        assert_eq!(ex.matching("2026/amazon/old/a.pdf"), None);
        assert_eq!(ex.matching("2026/x/scan.PNG"), Some("*.png"));
        assert_eq!(ex.matching("2026/bol-com/x.pdf"), None);
        // Spelled as a path, as Unicode NFD, it still matches.
        let spelled = Excludes::new(&["./2025/".into(), "cafe\u{301}".into()]).unwrap();
        assert_eq!(spelled.matching("2025/a.pdf"), Some("2025"));
        assert_eq!(spelled.matching("Caf\u{e9}/a.pdf"), Some("caf\u{e9}"));
        assert_eq!(spelled.matching("cafe\u{301}/a.pdf"), Some("caf\u{e9}"));
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
        state
            .extra
            .insert("note".into(), Value::String("kept".into()));
        let mut e = entry("a.pdf", Status::Pending);
        e.extra
            .insert("matched_transaction".into(), Value::String("t-1".into()));
        state.files.insert("abc".into(), e);
        state.save(dir.path()).unwrap();
        let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
        assert!(text.contains("\"matched_transaction\": \"t-1\""), "{text}");
        assert!(text.contains("\"status\": \"pending\""), "{text}");
        assert!(text.contains("\"note\": \"kept\""), "{text}");
        let loaded = State::load(dir.path()).unwrap();
        assert_eq!(loaded, state);
        // An update keeps the unknown fields.
        let updated = Entry::update(&loaded, "abc", "b.pdf", 3, Status::Uploaded);
        assert_eq!(updated.extra.len(), 1);
        // No temporary file is left behind.
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from(STATE_FILE)]);
    }

    #[test]
    fn a_corrupt_or_newer_state_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(State::load(dir.path()).unwrap(), State::default());
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
    fn the_lock_is_exclusive_until_released() {
        let dir = tempfile::tempdir().unwrap();
        let lock = Lock::acquire(dir.path()).unwrap();
        let err = Lock::acquire(dir.path()).unwrap_err().to_string();
        assert!(err.contains("another yuki run"), "{err}");
        drop(lock);
        assert!(Lock::acquire(dir.path()).is_ok());
    }

    #[test]
    fn scan_hashes_supported_files_and_reports_what_it_skips() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("2026/bol/inner")).unwrap();
        fs::create_dir_all(root.join("_to_delete")).unwrap();
        fs::write(root.join("2026/bol/a.pdf"), b"aaa").unwrap();
        fs::write(root.join("2026/bol/B.JPG"), b"bbb").unwrap();
        fs::write(root.join("2026/notes.txt"), b"x").unwrap();
        fs::write(root.join("_to_delete/old.pdf"), b"old").unwrap();
        fs::write(root.join(".DS_Store"), b"ds").unwrap();
        fs::write(root.join(STATE_FILE), b"{}").unwrap();
        fs::write(root.join("2026/bol/inner").join(STATE_FILE), b"{}").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("2026/bol/a.pdf"), root.join("2026/link.pdf"))
            .unwrap();
        let scan = scan(root, &Excludes::new(&[]).unwrap()).unwrap();
        let rels: Vec<_> = scan.files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, ["2026/bol/B.JPG", "2026/bol/a.pdf"]);
        assert_eq!(scan.files[1].hash, sha256_hex(b"aaa"));
        assert_eq!(scan.files[1].size, 3);
        let reasons: Vec<_> = scan
            .excluded
            .iter()
            .map(|(r, w)| format!("{r}: {w}"))
            .collect();
        assert!(reasons.contains(&"_to_delete/old.pdf: --exclude _to_delete".to_string()));
        #[cfg(unix)]
        assert!(reasons.contains(&"2026/link.pdf: symbolic link, not followed".to_string()));
        assert_eq!(scan.nested_states, ["2026/bol/inner/.yuki-sync.json"]);
    }

    #[test]
    fn a_root_must_not_be_inside_another_synced_directory() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().canonicalize().unwrap();
        fs::create_dir_all(top.join("a/b")).unwrap();
        assert_eq!(sync_root(&top.join("a/b")).unwrap(), top.join("a/b"));
        assert_eq!(find_root(&top.join("a/b")).unwrap(), None);
        fs::write(top.join(STATE_FILE), b"{}").unwrap();
        let err = sync_root(&top.join("a/b")).unwrap_err().to_string();
        assert!(
            err.contains(&format!("run on {} instead", top.display())),
            "{err}"
        );
        assert_eq!(find_root(&top.join("a/b")).unwrap(), Some(top.clone()));
        assert_eq!(sync_root(&top).unwrap(), top);
        assert!(sync_root(&top.join("missing")).is_err());
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
