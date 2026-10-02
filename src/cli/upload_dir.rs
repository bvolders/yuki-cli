//! `yuki upload dir` and `yuki upload mark`: idempotent upload of a directory
//! of receipts, tracked in `<dir>/.yuki-sync.json` (see [`crate::sync`]).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::client::archive::{ArchiveClient, ArchiveDocument};
use crate::config::Config;
use crate::error::YukiError;
use crate::folders::folder_id;
use crate::output::{OutputFormat, format_json, format_table, is_tty};
use crate::sync::{self, Entry, Excludes, Found, STATE_FILE, State, Status};

/// Options of `upload dir`.
pub struct DirOptions<'a> {
    pub path: &'a str,
    pub folder: &'a str,
    pub excludes: &'a [String],
    pub max: usize,
    pub dry_run: bool,
    pub seed: bool,
    pub seed_folders: &'a [String],
}

/// How to get the go-ahead for uploads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    /// `--yes`: upload without asking.
    Yes,
    /// Ask on the terminal.
    Prompt,
    /// No terminal and no `--yes`: refuse.
    Refuse,
}

/// How a run that did not hit an error ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// The user answered no at the prompt; nothing was uploaded.
    Aborted,
    /// Uploads were needed but not confirmed; nothing was contacted.
    NeedsConfirmation(String),
    /// Some uploads failed; they are recorded and retried on the next run.
    Failed {
        failed: usize,
        attempted: usize,
    },
}

/// Documents per `DocumentsInFolder` request when seeding.
const SEED_PAGE_SIZE: usize = 500;

/// One output row: what happened to one file.
struct Row {
    path: String,
    action: &'static str,
    doc_id: String,
    error: String,
    note: String,
}

impl Row {
    fn new(path: &str, action: &'static str) -> Self {
        Self {
            path: path.to_string(),
            action,
            doc_id: String::new(),
            error: String::new(),
            note: String::new(),
        }
    }

    fn doc(mut self, id: Option<&str>) -> Self {
        self.doc_id = id.unwrap_or_default().to_string();
        self
    }

    fn note(mut self, note: impl Into<String>) -> Self {
        self.note = note.into();
        self
    }

    fn error(mut self, error: impl Into<String>) -> Self {
        self.error = error.into();
        self
    }
}

fn print_rows(rows: &[Row], format: Option<&str>) {
    let headers: Vec<String> = ["Path", "Action", "Doc ID", "Error", "Note"]
        .iter()
        .map(|h| (*h).to_string())
        .collect();
    let rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                r.path.clone(),
                r.action.to_string(),
                r.doc_id.clone(),
                r.error.clone(),
                r.note.clone(),
            ]
        })
        .collect();
    match OutputFormat::from_flag(format, is_tty()) {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
}

/// The directory to sync, resolved, with the files in it sorted into a plan.
struct Plan {
    root: PathBuf,
    state: State,
    /// Files to upload (new, or failed before), in path order.
    new: Vec<Found>,
    /// Rows for files that need nothing: synced, duplicate, excluded.
    settled: Vec<Row>,
    synced: usize,
    duplicates: usize,
    excluded: usize,
    /// Files whose recorded path changed (renamed or moved): hash and new path.
    moved: Vec<(String, String)>,
}

fn plan(path: &str, excludes: &[String]) -> Result<Plan, YukiError> {
    let root =
        std::fs::canonicalize(path).map_err(|e| YukiError::Config(format!("{path}: {e}")))?;
    if !root.is_dir() {
        return Err(YukiError::Config(format!("{path}: not a directory")));
    }
    let excludes = Excludes::new(excludes)?;
    let state = State::load(&root)?;
    let scan = sync::scan(&root, &excludes)?;

    let mut new = Vec::new();
    let mut settled = Vec::new();
    let mut moved = Vec::new();
    let (mut synced, mut duplicates) = (0, 0);
    let mut first_seen: HashMap<String, String> = HashMap::new();
    for file in scan.files {
        if let Some(first) = first_seen.get(&file.hash) {
            duplicates += 1;
            settled.push(Row::new(&file.rel, "duplicate").note(format!("same content as {first}")));
            continue;
        }
        first_seen.insert(file.hash.clone(), file.rel.clone());
        match state.files.get(&file.hash) {
            Some(entry) if entry.status.is_settled() => {
                synced += 1;
                let mut note = entry.status.as_str().to_string();
                if entry.path != file.rel {
                    note.push_str(&format!("; was {}", entry.path));
                    moved.push((file.hash.clone(), file.rel.clone()));
                }
                settled.push(
                    Row::new(&file.rel, "synced")
                        .doc(entry.document_id.as_deref())
                        .note(note),
                );
            }
            _ => new.push(file),
        }
    }
    let excluded = scan.excluded.len();
    for (rel, pattern) in scan.excluded {
        settled.push(Row::new(&rel, "excluded").note(format!("--exclude {pattern}")));
    }
    Ok(Plan {
        root,
        state,
        new,
        settled,
        synced,
        duplicates,
        excluded,
        moved,
    })
}

fn kib(bytes: u64) -> String {
    format!("{:.1} KB", bytes as f64 / 1024.0)
}

/// Print the plan to stderr: counts, then the files that would be uploaded.
fn print_plan(plan: &Plan, opts: &DirOptions<'_>) {
    let retry = |f: &Found| {
        plan.state
            .files
            .get(&f.hash)
            .is_some_and(|e| e.status == Status::Failed)
    };
    eprintln!(
        "Plan for {} (Yuki folder {}):",
        plan.root.display(),
        opts.folder
    );
    eprintln!(
        "  {} new, {} already synced, {} excluded, {} duplicate content",
        plan.new.len(),
        plan.synced,
        plan.excluded,
        plan.duplicates
    );
    if plan.new.is_empty() {
        return;
    }
    eprintln!("  New files:");
    for (i, f) in plan.new.iter().enumerate() {
        let over = if !opts.seed && i >= opts.max {
            "  (over --max, next run)"
        } else {
            ""
        };
        let again = if retry(f) {
            "  (failed before, retry)"
        } else {
            ""
        };
        eprintln!("    {}  {}{again}{over}", f.rel, kib(f.size));
    }
    if !opts.seed && plan.new.len() > opts.max {
        eprintln!(
            "  Uploading at most {} this run (--max); {} wait for the next run.",
            opts.max,
            plan.new.len() - opts.max
        );
    }
}

/// Record paths of files that moved since they were recorded.
fn refresh_moved(plan: &mut Plan) {
    for (hash, rel) in std::mem::take(&mut plan.moved) {
        if let Some(entry) = plan.state.files.get_mut(&hash) {
            entry.path = rel;
        }
    }
}

fn ask(question: &str) -> bool {
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// `upload dir`: upload the files of a directory that are not in Yuki yet.
pub async fn dir(
    load_config: impl FnOnce() -> Result<Config, YukiError>,
    admin: Option<&str>,
    opts: DirOptions<'_>,
    confirm: Confirm,
    format: Option<&str>,
    quiet: bool,
) -> Result<Outcome, YukiError> {
    folder_id(opts.folder)?;
    for f in opts.seed_folders {
        folder_id(f)?;
    }
    let mut plan = plan(opts.path, opts.excludes)?;
    if !quiet {
        print_plan(&plan, &opts);
    }

    if opts.dry_run {
        let mut rows: Vec<Row> = plan
            .new
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let action = if opts.seed {
                    "would-seed"
                } else if i < opts.max {
                    "would-upload"
                } else {
                    "deferred"
                };
                Row::new(&f.rel, action)
            })
            .collect();
        rows.append(&mut plan.settled);
        if !quiet {
            if opts.seed {
                eprintln!(
                    "Dry run: --seed-from-yuki would look the new files up in: {}.",
                    seed_folders(&opts).join(", ")
                );
            }
            eprintln!("Dry run: nothing uploaded, nothing written.");
            eprintln!("API calls made: 0");
            print_rows(&rows, format);
        }
        return Ok(Outcome::Done);
    }

    if opts.seed {
        return seed(load_config()?, admin, plan, &opts, format, quiet).await;
    }

    let batch: Vec<Found> = plan.new.iter().take(opts.max).cloned().collect();
    let deferred: Vec<Found> = plan.new.iter().skip(opts.max).cloned().collect();
    if batch.is_empty() {
        if !plan.moved.is_empty() {
            refresh_moved(&mut plan);
            plan.state.save(&plan.root)?;
        }
        if !quiet {
            eprintln!("Nothing to upload.");
            eprintln!("API calls made: 0");
            let mut rows: Vec<Row> = deferred
                .iter()
                .map(|f| Row::new(&f.rel, "deferred"))
                .collect();
            rows.append(&mut plan.settled);
            print_rows(&rows, format);
        }
        return Ok(Outcome::Done);
    }

    let question = format!(
        "Upload {} file{} to Yuki folder {}?",
        batch.len(),
        if batch.len() == 1 { "" } else { "s" },
        opts.folder
    );
    match confirm {
        Confirm::Yes => {}
        Confirm::Refuse => {
            return Ok(Outcome::NeedsConfirmation(format!(
                "upload dir would upload {} files; pass --yes to confirm in non-interactive mode, \
                 or --dry-run to only see the plan",
                batch.len()
            )));
        }
        Confirm::Prompt => {
            if !ask(&question) {
                if !quiet {
                    eprintln!("Aborted; nothing uploaded.");
                }
                return Ok(Outcome::Aborted);
            }
        }
    }

    let config = load_config()?;
    let target = config.target(admin)?;
    let fid = folder_id(opts.folder)?;
    let mut client = ArchiveClient::new().with_api_root(target.api_root);
    let mut calls = 1;
    client.authenticate(target.api_key).await?;
    // Paths are refreshed with the first write.
    refresh_moved(&mut plan);

    let mut rows = Vec::new();
    let mut failed = 0;
    let mut stop: Option<YukiError> = None;
    let total = batch.len();
    for (i, file) in batch.iter().enumerate() {
        if stop.is_some() {
            rows.push(Row::new(&file.rel, "not-attempted"));
            continue;
        }
        let bytes = match std::fs::read(&file.path) {
            Ok(b) if sync::sha256_hex(&b) == file.hash => b,
            Ok(_) => {
                failed += 1;
                rows.push(
                    Row::new(&file.rel, "failed").error("file changed since the plan was made"),
                );
                continue;
            }
            Err(e) => {
                failed += 1;
                rows.push(Row::new(&file.rel, "failed").error(e.to_string()));
                continue;
            }
        };
        let name = file
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&file.rel);
        if !quiet {
            eprint!("[{}/{total}] {} ... ", i + 1, file.rel);
        }
        calls += 1;
        let result = client
            .upload_document(target.admin_id, name, &BASE64.encode(&bytes), fid)
            .await;
        let now = sync::now_utc();
        let mut entry = Entry {
            path: file.rel.clone(),
            size: file.size,
            status: Status::Uploaded,
            document_id: None,
            folder: Some(opts.folder.to_string()),
            uploaded_at: None,
            recorded_at: now.clone(),
            error: None,
            note: None,
            extra: plan
                .state
                .files
                .get(&file.hash)
                .map(|e| e.extra.clone())
                .unwrap_or_default(),
        };
        match result {
            Ok(id) => {
                if !quiet {
                    eprintln!("{id}");
                }
                entry.document_id = Some(id.clone());
                entry.uploaded_at = Some(now);
                plan.state.files.insert(file.hash.clone(), entry);
                if let Err(e) = plan.state.save(&plan.root) {
                    // The upload happened but could not be recorded: stop before
                    // anything else goes unrecorded.
                    return Err(YukiError::Config(format!(
                        "{} was uploaded as document {id}, but the state could not be saved: {e}. \
                         Record it with `yuki upload mark {} --doc-id {id}` once the problem is fixed",
                        file.rel,
                        file.path.display()
                    )));
                }
                rows.push(Row::new(&file.rel, "uploaded").doc(Some(&id)));
            }
            Err(e @ (YukiError::AuthFailed(_) | YukiError::RateLimited)) => {
                if !quiet {
                    eprintln!("stopped: {e}");
                }
                rows.push(Row::new(&file.rel, "not-attempted").error(e.to_string()));
                stop = Some(e);
            }
            Err(e) => {
                if !quiet {
                    eprintln!("failed: {e}");
                }
                failed += 1;
                entry.status = Status::Failed;
                entry.error = Some(e.to_string());
                plan.state.files.insert(file.hash.clone(), entry);
                plan.state.save(&plan.root)?;
                rows.push(Row::new(&file.rel, "failed").error(e.to_string()));
            }
        }
    }
    rows.extend(
        deferred
            .iter()
            .map(|f| Row::new(&f.rel, "deferred").note("over --max")),
    );
    rows.append(&mut plan.settled);
    if !quiet {
        eprintln!("API calls made: {calls}");
        print_rows(&rows, format);
    }
    if let Some(e) = stop {
        return Err(e);
    }
    if failed > 0 {
        return Ok(Outcome::Failed {
            failed,
            attempted: total,
        });
    }
    Ok(Outcome::Done)
}

/// The folders `--seed-from-yuki` searches: the given ones, else the upload
/// folder and `inkoop`, where Yuki files purchase documents once processed.
fn seed_folders(opts: &DirOptions<'_>) -> Vec<String> {
    let mut folders: Vec<String> = if opts.seed_folders.is_empty() {
        vec![opts.folder.to_string(), "inkoop".to_string()]
    } else {
        opts.seed_folders.to_vec()
    };
    let mut seen = HashSet::new();
    folders.retain(|f| seen.insert(folder_id(f).unwrap_or(-1)));
    folders
}

/// Parts of a `YYYY-MM-DD_vendor_reference.ext` name, used only to suggest
/// possible matches for review.
struct NameParts<'a> {
    date: &'a str,
    vendor: &'a str,
    reference: &'a str,
}

fn name_parts(file_name: &str) -> Option<NameParts<'_>> {
    let stem = file_name.rsplit_once('.').map_or(file_name, |(s, _)| s);
    let mut parts = stem.splitn(3, '_');
    let date = parts.next()?;
    let vendor = parts.next()?;
    let reference = parts.next()?;
    (crate::period::epoch_days(date).is_some() && date.len() == 10).then_some(NameParts {
        date,
        vendor,
        reference,
    })
}

/// Why `doc` might be the Yuki copy of local `name`, or `None`.
fn possible_match(name: &str, doc: &ArchiveDocument) -> Option<&'static str> {
    let parts = name_parts(name)?;
    let file = doc.file_name.to_lowercase();
    let reference = parts.reference.to_lowercase();
    if reference.len() >= 5
        && (file.contains(&reference)
            || doc.subject.to_lowercase().contains(&reference)
            || doc.reference.to_lowercase().contains(&reference))
    {
        return Some("reference in the Yuki document");
    }
    let vendor = parts
        .vendor
        .split('-')
        .next()
        .unwrap_or_default()
        .to_lowercase();
    if vendor.len() >= 3
        && doc.document_date.get(..10) == Some(parts.date)
        && (doc.contact_name.to_lowercase().contains(&vendor) || file.contains(&vendor))
    {
        return Some("same date and vendor");
    }
    None
}

/// `--seed-from-yuki`: record the new files Yuki already has, by file name.
async fn seed(
    config: Config,
    admin: Option<&str>,
    mut plan: Plan,
    opts: &DirOptions<'_>,
    format: Option<&str>,
    quiet: bool,
) -> Result<Outcome, YukiError> {
    refresh_moved(&mut plan);
    let mut rows = Vec::new();
    let mut calls = 0;
    if !plan.new.is_empty() {
        let target = config.target(admin)?;
        let mut client = ArchiveClient::new().with_api_root(target.api_root);
        calls += 1;
        client.authenticate(target.api_key).await?;

        let folders = seed_folders(opts);
        let mut docs: Vec<(String, ArchiveDocument)> = Vec::new();
        for folder in &folders {
            let fid = folder_id(folder)?;
            let mut start = 0;
            loop {
                calls += 1;
                let page = client
                    .documents_in_folder_page(
                        fid,
                        "2000-01-01",
                        "2099-12-31",
                        SEED_PAGE_SIZE,
                        start,
                    )
                    .await?;
                let received = page.len();
                docs.extend(page.into_iter().map(|d| (folder.clone(), d)));
                if received < SEED_PAGE_SIZE {
                    break;
                }
                start += received;
            }
        }
        if !quiet {
            eprintln!(
                "Fetched {} documents from Yuki folder{} {}.",
                docs.len(),
                if folders.len() == 1 { "" } else { "s" },
                folders.join(", ")
            );
        }

        let mut by_name: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, (_, d)) in docs.iter().enumerate() {
            by_name
                .entry(d.file_name.to_lowercase())
                .or_default()
                .push(i);
        }
        // Documents already accounted for are never offered as possible matches.
        let mut taken: HashSet<String> = plan
            .state
            .files
            .values()
            .filter_map(|e| e.document_id.clone())
            .collect();
        let name_of = |f: &Found| {
            f.path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&f.rel)
                .to_string()
        };
        let now = sync::now_utc();
        let mut unmatched = Vec::new();
        for file in &plan.new {
            let name = name_of(file);
            match by_name.get(&name.to_lowercase()).map(Vec::as_slice) {
                Some([only]) => {
                    let (folder, doc) = &docs[*only];
                    taken.insert(doc.id.clone());
                    plan.state.files.insert(
                        file.hash.clone(),
                        Entry {
                            path: file.rel.clone(),
                            size: file.size,
                            status: Status::AlreadyInYuki,
                            document_id: Some(doc.id.clone()),
                            folder: Some(folder.clone()),
                            uploaded_at: None,
                            recorded_at: now.clone(),
                            error: None,
                            note: Some(format!("seeded: same file name in {folder}")),
                            extra: BTreeMap::new(),
                        },
                    );
                    rows.push(
                        Row::new(&file.rel, "already-in-yuki")
                            .doc(Some(&doc.id))
                            .note(format!(
                                "file name match in {folder}, dated {}",
                                short_date(doc)
                            )),
                    );
                }
                Some(several) => {
                    for &i in several {
                        taken.insert(docs[i].1.id.clone());
                    }
                    let ids: Vec<String> = several
                        .iter()
                        .map(|&i| format!("{} ({})", docs[i].1.id, docs[i].0))
                        .collect();
                    rows.push(Row::new(&file.rel, "ambiguous").note(format!(
                        "{} Yuki documents share this name: {}; not recorded",
                        several.len(),
                        ids.join(", ")
                    )));
                }
                None => unmatched.push(file),
            }
        }
        for file in unmatched {
            let name = name_of(file);
            let candidates: Vec<String> = docs
                .iter()
                .filter(|(_, d)| !taken.contains(&d.id))
                .filter_map(|(folder, d)| {
                    possible_match(&name, d).map(|why| {
                        format!(
                            "{} {:?} in {folder}, {} ({why})",
                            d.id,
                            d.file_name,
                            short_date(d)
                        )
                    })
                })
                .collect();
            if candidates.is_empty() {
                rows.push(
                    Row::new(&file.rel, "not-in-yuki")
                        .note("no document with this file name; will upload"),
                );
            } else {
                rows.push(Row::new(&file.rel, "possible-match").note(format!(
                    "not recorded; maybe {}. If it is the same file, run: yuki upload mark \"{}\" --doc-id <id>",
                    candidates.join("; "),
                    file.path.display()
                )));
            }
        }
    }
    plan.state.save(&plan.root)?;

    if !quiet {
        let count = |a: &str| rows.iter().filter(|r| r.action == a).count();
        eprintln!(
            "Seeded {} as already-in-yuki; {} ambiguous, {} possible matches to review, {} not in Yuki.",
            count("already-in-yuki"),
            count("ambiguous"),
            count("possible-match"),
            count("not-in-yuki")
        );
        eprintln!(
            "Matching is by file name only (Yuki reports no size or hash): a different file uploaded \
             under the same name is recorded as in Yuki, and a copy uploaded under another name is not \
             found and would be uploaded again. Review the rows below; undo a match with \
             `yuki upload mark <file> --forget`."
        );
        eprintln!("API calls made: {calls}");
        rows.append(&mut plan.settled);
        print_rows(&rows, format);
    }
    Ok(Outcome::Done)
}

fn short_date(doc: &ArchiveDocument) -> &str {
    doc.document_date.get(..10).unwrap_or(&doc.document_date)
}

/// What `upload mark` records.
pub enum Mark<'a> {
    /// The file is in Yuki as this document.
    Document(&'a str),
    /// Never upload the file.
    Skip,
    /// Drop the record, so the next run treats the file as new.
    Forget,
}

/// Options of `upload mark`.
pub struct MarkOptions<'a> {
    pub file: &'a str,
    pub mark: Mark<'a>,
    pub folder: Option<&'a str>,
    pub note: Option<&'a str>,
    pub dir: Option<&'a str>,
    pub force: bool,
}

/// The synced directory holding `file`: `--dir`, else the nearest ancestor
/// with a state file.
fn find_root(file: &Path, dir: Option<&str>) -> Result<PathBuf, YukiError> {
    if let Some(dir) = dir {
        return std::fs::canonicalize(dir).map_err(|e| YukiError::Config(format!("{dir}: {e}")));
    }
    file.ancestors()
        .skip(1)
        .find(|a| a.join(STATE_FILE).is_file())
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            YukiError::Config(format!(
                "no {STATE_FILE} found in any directory above {}; pass --dir <synced directory>",
                file.display()
            ))
        })
}

/// `upload mark`: record one file by hand, without contacting Yuki.
pub fn mark(opts: MarkOptions<'_>, format: Option<&str>, quiet: bool) -> Result<(), YukiError> {
    if let Some(folder) = opts.folder {
        folder_id(folder)?;
    }
    let file = std::fs::canonicalize(opts.file)
        .map_err(|e| YukiError::Config(format!("{}: {e}", opts.file)))?;
    let root = find_root(&file, opts.dir)?;
    let rel = sync::relative_to(&root, &file).ok_or_else(|| {
        YukiError::Config(format!(
            "{} is not inside {}",
            file.display(),
            root.display()
        ))
    })?;
    let bytes =
        std::fs::read(&file).map_err(|e| YukiError::Config(format!("{}: {e}", file.display())))?;
    let hash = sync::sha256_hex(&bytes);
    let mut state = State::load(&root)?;
    let existing = state.files.get(&hash).cloned();

    let row = match opts.mark {
        Mark::Forget => {
            if state.files.remove(&hash).is_none() {
                Row::new(&rel, "unchanged").note("no record to forget")
            } else {
                state.save(&root)?;
                Row::new(&rel, "forgotten")
                    .doc(existing.as_ref().and_then(|e| e.document_id.as_deref()))
                    .note("the next upload dir run treats it as new")
            }
        }
        Mark::Document(_) | Mark::Skip => {
            let (status, doc_id) = match opts.mark {
                Mark::Document(id) => (Status::AlreadyInYuki, Some(id.trim().to_string())),
                _ => (Status::Skipped, None),
            };
            if doc_id.as_deref() == Some("") {
                return Err(YukiError::Config("--doc-id is empty".into()));
            }
            match &existing {
                Some(e) if e.status == status && e.document_id == doc_id => {
                    Row::new(&rel, "unchanged")
                        .doc(doc_id.as_deref())
                        .note(format!("already recorded as {}", status.as_str()))
                }
                Some(e) if e.status.is_settled() && !opts.force => {
                    return Err(YukiError::Config(format!(
                        "{rel} is already recorded as {}{}; pass --force to replace the record",
                        e.status.as_str(),
                        e.document_id
                            .as_deref()
                            .map(|d| format!(" (document {d})"))
                            .unwrap_or_default()
                    )));
                }
                _ => {
                    state.files.insert(
                        hash,
                        Entry {
                            path: rel.clone(),
                            size: bytes.len() as u64,
                            status,
                            document_id: doc_id.clone(),
                            folder: opts.folder.map(str::to_string),
                            uploaded_at: None,
                            recorded_at: sync::now_utc(),
                            error: None,
                            note: Some(
                                opts.note
                                    .map_or_else(|| "marked by hand".to_string(), str::to_string),
                            ),
                            extra: existing.map(|e| e.extra).unwrap_or_default(),
                        },
                    );
                    state.save(&root)?;
                    Row::new(&rel, "recorded")
                        .doc(doc_id.as_deref())
                        .note(status.as_str())
                }
            }
        }
    };
    if !quiet {
        print_rows(&[row], format);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: &str, date: &str, contact: &str, file: &str) -> ArchiveDocument {
        ArchiveDocument {
            id: id.into(),
            subject: String::new(),
            document_date: format!("{date}T00:00:00"),
            amount: String::new(),
            folder: String::new(),
            contact_name: contact.into(),
            file_name: file.into(),
            reference: String::new(),
        }
    }

    #[test]
    fn possible_matches_by_reference_or_date_and_vendor() {
        let name = "2026-09-07_vercel_2E81616A-0008.pdf";
        let by_ref = doc("1", "2026-09-07", "Vercel Inc", "Invoice-2E81616A-0008.pdf");
        assert_eq!(
            possible_match(name, &by_ref),
            Some("reference in the Yuki document")
        );
        let gh = "2026-08-17_github_INV151037734.pdf";
        let by_day = doc(
            "2",
            "2026-08-17",
            "Github B.V.",
            "github-receipt-2026-08-17.pdf",
        );
        assert_eq!(possible_match(gh, &by_day), Some("same date and vendor"));
        // Another vendor that day, or the vendor another day, is no candidate.
        assert_eq!(
            possible_match(gh, &doc("3", "2026-08-17", "Telenet", "t.pdf")),
            None
        );
        assert_eq!(
            possible_match(gh, &doc("4", "2026-08-18", "Github B.V.", "g.pdf")),
            None
        );
        // Names not in the date_vendor_reference shape get no suggestions.
        assert_eq!(possible_match("scan.pdf", &by_ref), None);
    }

    #[test]
    fn seed_folders_default_to_the_upload_folder_and_inkoop() {
        let opts = DirOptions {
            path: ".",
            folder: "uitzoeken",
            excludes: &[],
            max: 25,
            dry_run: false,
            seed: true,
            seed_folders: &[],
        };
        assert_eq!(seed_folders(&opts), ["uitzoeken", "inkoop"]);
        let inkoop = DirOptions {
            folder: "purchase",
            ..opts
        };
        assert_eq!(seed_folders(&inkoop), ["purchase"]);
    }
}
