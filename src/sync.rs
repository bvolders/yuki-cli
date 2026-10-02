//! Local sync state for `yuki upload dir`: which files of a directory are
//! already in Yuki, keyed by content hash so a rename or move never uploads a
//! file twice.
//!
//! The state lives in `<dir>/.yuki-sync.json` and is rewritten atomically
//! (temporary file, then rename) after every recorded change.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::YukiError;

/// File name of the state file inside the synced directory.
pub const STATE_FILE: &str = ".yuki-sync.json";

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
    /// The last upload attempt failed; the next run retries it.
    Failed,
}

impl Status {
    /// Whether the file is done: never uploaded again.
    pub fn is_settled(self) -> bool {
        !matches!(self, Self::Failed)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uploaded => "uploaded",
            Self::AlreadyInYuki => "already-in-yuki",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

/// One file's record, keyed in [`State::files`] by its sha256.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Path relative to the synced directory, `/`-separated, as last seen.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Fields this version does not know, such as a later `matched_transaction`,
    /// kept as they are so rewriting the file never drops them.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
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
        let version = raw.get("version").and_then(Value::as_u64);
        match version {
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
        let io = |e: std::io::Error| YukiError::Config(format!("{}: {e}", path.display()));
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
            return Err(io(e));
        }
        Ok(())
    }
}

/// The lowercase hex sha256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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

/// Compiled exclude patterns.
///
/// A pattern without `/` is matched against every path component, so `_to_delete`
/// skips that directory anywhere and `*.png` skips every PNG. A pattern with `/`
/// is matched against the whole relative path, where `*` stays within one
/// component and `**` crosses them.
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
            case_sensitive: true,
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
    /// Supported files skipped by an exclude, with the pattern that matched.
    pub excluded: Vec<(String, String)>,
}

/// Whether `name` has an extension `upload dir` uploads.
pub fn is_supported(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e))
    })
}

/// Recursively list the supported files under `root`, hashing each one.
///
/// Symbolic links are not followed. An excluded directory is still walked only
/// to count the supported files it holds, so the plan can say what it skipped.
pub fn scan(root: &Path, excludes: &Excludes) -> Result<Scan, YukiError> {
    let mut out = Scan::default();
    walk(root, "", excludes, &mut out)?;
    out.files.sort_by(|a, b| a.rel.cmp(&b.rel));
    out.excluded.sort();
    Ok(out)
}

fn walk(dir: &Path, prefix: &str, excludes: &Excludes, out: &mut Scan) -> Result<(), YukiError> {
    let io = |p: &Path, e: std::io::Error| YukiError::Config(format!("{}: {e}", p.display()));
    for entry in fs::read_dir(dir).map_err(|e| io(dir, e))? {
        let entry = entry.map_err(|e| io(dir, e))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| io(&path, e))?;
        if kind.is_dir() {
            walk(&path, &rel, excludes, out)?;
        } else if kind.is_file() && is_supported(&name) {
            if let Some(pattern) = excludes.matching(&rel) {
                out.excluded.push((rel, pattern.to_string()));
                continue;
            }
            let bytes = fs::read(&path).map_err(|e| io(&path, e))?;
            out.files.push(Found {
                rel,
                path,
                size: bytes.len() as u64,
                hash: sha256_hex(&bytes),
            });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, status: Status) -> Entry {
        Entry {
            path: path.into(),
            size: 3,
            status,
            document_id: Some("doc-1".into()),
            folder: Some("uitzoeken".into()),
            uploaded_at: None,
            recorded_at: "2026-10-02T08:00:00Z".into(),
            error: None,
            note: None,
            extra: BTreeMap::new(),
        }
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
        assert_eq!(ex.matching("2026/_to_delete/x.pdf"), Some("_to_delete"));
        assert_eq!(ex.matching(".hidden.pdf"), Some(".*"));
        assert_eq!(ex.matching("2026/.cache/x.pdf"), Some(".*"));
        assert_eq!(ex.matching("2026/bol-com/x.pdf"), None);
    }

    #[test]
    fn excludes_with_a_slash_match_the_whole_path() {
        let ex = Excludes::new(&["2026/amazon/*".into(), "*.png".into()]).unwrap();
        assert_eq!(ex.matching("2026/amazon/a.pdf"), Some("2026/amazon/*"));
        // `*` does not cross directories in a path pattern.
        assert_eq!(ex.matching("2026/amazon/old/a.pdf"), None);
        assert_eq!(ex.matching("2026/x/scan.png"), Some("*.png"));
        assert!(Excludes::new(&["[".into()]).is_err());
    }

    #[test]
    fn state_round_trips_and_keeps_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::default();
        let mut e = entry("a.pdf", Status::Uploaded);
        e.extra
            .insert("matched_transaction".into(), Value::String("t-1".into()));
        state.files.insert("abc".into(), e);
        state.save(dir.path()).unwrap();
        let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
        assert!(text.contains("\"matched_transaction\": \"t-1\""), "{text}");
        assert!(text.contains("\"status\": \"uploaded\""), "{text}");
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
    fn scan_hashes_supported_files_and_reports_excluded_ones() {
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
        let scan = scan(root, &Excludes::new(&[]).unwrap()).unwrap();
        let rels: Vec<_> = scan.files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, ["2026/bol/B.JPG", "2026/bol/a.pdf"]);
        assert_eq!(scan.files[1].hash, sha256_hex(b"aaa"));
        assert_eq!(scan.files[1].size, 3);
        assert_eq!(
            scan.excluded,
            vec![
                (".git/x.png".to_string(), ".*".to_string()),
                ("_to_delete/old.pdf".to_string(), "_to_delete".to_string()),
            ]
        );
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
