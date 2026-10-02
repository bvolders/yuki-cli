//! `yuki upload dir` and `yuki upload mark`: idempotent upload of a directory
//! of receipts, tracked in `<root>/.yuki-sync.json` (see [`crate::sync`]).

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::client::accounting::AccountingClient;
use crate::client::archive::{ArchiveClient, ArchiveDocument};
use crate::config::Config;
use crate::error::YukiError;
use crate::folders::folder_id;
use crate::output::{OutputFormat, format_json, format_table, is_tty};
use crate::sync::{
    self, Entry, Excludes, Found, Lock, MAX_ATTEMPTS, STATE_FILE, State, Status, shell_quote,
};

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

/// How to get the go-ahead for writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    /// `--yes`: go ahead without asking.
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
    /// The user answered no at the prompt; nothing was changed.
    Aborted,
    /// Writes were needed but not confirmed; Yuki was not contacted.
    NeedsConfirmation(String),
    /// The run finished, but some files need the user: failed or unknown
    /// uploads, changed or unreadable files. The message says which.
    NeedsAttention(String),
}

/// Documents per `DocumentsInFolder` request when seeding.
const SEED_PAGE_SIZE: usize = 500;

/// Pages read from one folder at most when seeding.
const SEED_MAX_PAGES: usize = 40;

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

/// Why a file is in the upload queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    New,
    /// Rejected before, this many times.
    Retry(u32),
}

/// The synced tree, with the files under the scanned path sorted into a plan.
struct Plan {
    root: PathBuf,
    base: PathBuf,
    state: State,
    /// Files to upload: new ones first, then retries, each in path order.
    queue: Vec<(Found, Pending)>,
    /// Files that are not uploaded until the user resolves them (unknown, or
    /// failed too often); seeding may still find them in Yuki.
    blocked: Vec<Found>,
    /// Rows for files that are not queued.
    rows: Vec<Row>,
    synced: usize,
    duplicates: usize,
    excluded: usize,
    unknown: usize,
    gave_up: usize,
    changed: usize,
    unreadable: usize,
    /// Files whose recorded path changed (renamed or moved): hash and new path.
    moved: Vec<(String, String)>,
}

impl Plan {
    fn attention(&self) -> Vec<String> {
        let mut parts = Vec::new();
        let mut add = |n: usize, what: &str| {
            if n > 0 {
                parts.push(format!("{n} {what}"));
            }
        };
        add(self.unknown, "with an unknown upload result");
        add(self.gave_up, "failed too often to retry");
        add(self.changed, "changed since recorded");
        add(self.unreadable, "unreadable");
        parts
    }
}

fn mark_hint(root: &Path, rel: &str) -> String {
    format!(
        "yuki upload mark {}",
        shell_quote(&root.join(rel).display().to_string())
    )
}

fn plan(root: PathBuf, base: PathBuf, excludes: &[String]) -> Result<Plan, YukiError> {
    let excludes = Excludes::new(excludes)?;
    let state = State::load(&root)?;
    let scan = sync::scan(&root, &base, &excludes)?;
    if !scan.nested_states.is_empty() {
        return Err(YukiError::Config(format!(
            "{} holds other sync states ({}); sync those directories separately, or merge \
             their records into {} by hand",
            base.display(),
            scan.nested_states.join(", "),
            root.join(STATE_FILE).display()
        )));
    }

    let scanned: HashSet<&str> = scan.files.iter().map(|f| f.hash.as_str()).collect();
    let mut by_path: HashMap<&str, Vec<(&str, &Entry)>> = HashMap::new();
    for (hash, entry) in &state.files {
        by_path
            .entry(entry.path.as_str())
            .or_default()
            .push((hash.as_str(), entry));
    }

    let mut new = Vec::new();
    let mut retries = Vec::new();
    let mut blocked = Vec::new();
    let mut rows = Vec::new();
    let mut moved = Vec::new();
    let (mut synced, mut duplicates, mut unknown, mut gave_up, mut changed) = (0, 0, 0, 0, 0);
    let mut first_seen: HashMap<String, String> = HashMap::new();
    for file in &scan.files {
        if let Some(first) = first_seen.get(&file.hash) {
            duplicates += 1;
            rows.push(Row::new(&file.rel, "duplicate").note(format!("same content as {first}")));
            continue;
        }
        first_seen.insert(file.hash.clone(), file.rel.clone());
        let hint = mark_hint(&root, &file.rel);
        match state.files.get(&file.hash) {
            Some(entry) => {
                if entry.path != file.rel {
                    moved.push((file.hash.clone(), file.rel.clone()));
                }
                let was = if entry.path == file.rel {
                    String::new()
                } else {
                    format!("; was {}", entry.path)
                };
                match entry.status {
                    s if s.is_settled() => {
                        synced += 1;
                        rows.push(
                            Row::new(&file.rel, "synced")
                                .doc(entry.document_id.as_deref())
                                .note(format!("{}{was}", s.as_str())),
                        );
                    }
                    Status::Unknown => {
                        unknown += 1;
                        blocked.push(file.clone());
                        rows.push(
                            Row::new(&file.rel, "unknown")
                                .error(entry.error.clone().unwrap_or_default())
                                .note(format!(
                                    "the upload may have reached Yuki; not retried. Check Yuki, then \
                                     `{hint} --doc-id <id>`, or `{hint} --forget` to upload it again{was}"
                                )),
                        );
                    }
                    _ if entry.attempts >= MAX_ATTEMPTS => {
                        gave_up += 1;
                        blocked.push(file.clone());
                        rows.push(
                            Row::new(&file.rel, "failed")
                                .error(entry.error.clone().unwrap_or_default())
                                .note(format!(
                                    "rejected {} times; no longer retried. `{hint} --forget` \
                                     to try again{was}",
                                    entry.attempts
                                )),
                        );
                    }
                    _ => retries.push((file.clone(), Pending::Retry(entry.attempts))),
                }
            }
            None => {
                let earlier = by_path.get(file.rel.as_str()).and_then(|entries| {
                    entries
                        .iter()
                        .find(|(hash, e)| e.status != Status::Failed && !scanned.contains(hash))
                });
                match earlier {
                    Some((_, e)) => {
                        changed += 1;
                        rows.push(
                            Row::new(&file.rel, "changed")
                                .doc(e.document_id.as_deref())
                                .note(format!(
                                    "content changed since it was recorded as {}{}; not uploaded. \
                                     To upload the new version: `{hint} --forget`; to keep it \
                                     out: `{hint} --skip`",
                                    e.status.as_str(),
                                    e.document_id
                                        .as_deref()
                                        .map(|d| format!(" (was doc {d})"))
                                        .unwrap_or_default()
                                )),
                        );
                    }
                    None => new.push((file.clone(), Pending::New)),
                }
            }
        }
    }
    let excluded = scan.excluded.len();
    for (rel, reason) in &scan.excluded {
        rows.push(Row::new(rel, "excluded").note(reason.clone()));
    }
    let unreadable = scan.unreadable.len();
    for (rel, error) in &scan.unreadable {
        rows.push(Row::new(rel, "error").error(error.clone()));
    }
    new.extend(retries);
    Ok(Plan {
        root,
        base,
        state,
        queue: new,
        blocked,
        rows,
        synced,
        duplicates,
        excluded,
        unknown,
        gave_up,
        changed,
        unreadable,
        moved,
    })
}

fn kib(bytes: u64) -> String {
    format!("{:.1} KB", bytes as f64 / 1024.0)
}

/// Print the plan to stderr: counts, then the files that would be uploaded.
fn print_plan(plan: &Plan, opts: &DirOptions<'_>) {
    let scope = if plan.base == plan.root {
        String::new()
    } else {
        format!(", scanning {}", plan.base.display())
    };
    eprintln!(
        "Plan for {} (Yuki folder {}{scope}):",
        plan.root.display(),
        opts.folder
    );
    let retries = plan
        .queue
        .iter()
        .filter(|(_, p)| matches!(p, Pending::Retry(_)))
        .count();
    eprintln!(
        "  {} to upload ({} new, {retries} to retry), {} already synced, {} excluded, {} duplicate content",
        plan.queue.len(),
        plan.queue.len() - retries,
        plan.synced,
        plan.excluded,
        plan.duplicates
    );
    let attention = plan.attention();
    if !attention.is_empty() {
        eprintln!("  Needs attention (see the rows): {}", attention.join(", "));
    }
    if plan.queue.is_empty() {
        return;
    }
    eprintln!("  To upload:");
    for (i, (f, pending)) in plan.queue.iter().enumerate() {
        let over = if !opts.seed && i >= opts.max {
            "  (over --max, next run)"
        } else {
            ""
        };
        let again = match pending {
            Pending::New => String::new(),
            Pending::Retry(n) => format!("  (retry, attempt {} of {MAX_ATTEMPTS})", n + 1),
        };
        eprintln!("    {}  {}{again}{over}", f.rel, kib(f.size));
    }
    if !opts.seed && plan.queue.len() > opts.max {
        eprintln!(
            "  Uploading at most {} this run (--max); {} wait for the next run.",
            opts.max,
            plan.queue.len() - opts.max
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

fn bump(calls: &Cell<usize>) {
    calls.set(calls.get() + 1);
}

/// `upload dir`: upload the files of a directory that are not in Yuki yet.
///
/// Prints the number of API calls made, also when the run ends in an error.
pub async fn dir(
    load_config: impl FnOnce() -> Result<Config, YukiError>,
    admin: Option<&str>,
    opts: DirOptions<'_>,
    confirm: Confirm,
    format: Option<&str>,
    quiet: bool,
) -> Result<Outcome, YukiError> {
    let calls = Cell::new(0);
    let result = run_dir(load_config, admin, &opts, confirm, format, quiet, &calls).await;
    if !quiet {
        eprintln!("API calls made: {}", calls.get());
    }
    result
}

/// How sure it is that a failed upload did not store the file.
enum Verdict {
    /// Authentication or quota: stop the run, record nothing.
    Stop,
    /// Yuki rejected it, or the request never left: safe to retry.
    Rejected,
    /// The request may have been processed: never retry automatically.
    Unknown,
}

fn verdict(e: &YukiError) -> Verdict {
    match e {
        YukiError::AuthFailed(_) | YukiError::RateLimited => Verdict::Stop,
        YukiError::SoapFault { .. } => Verdict::Rejected,
        YukiError::Request(r) if r.is_connect() || r.is_builder() => Verdict::Rejected,
        YukiError::Http { status, .. } if *status < 500 => Verdict::Rejected,
        YukiError::Request(_) | YukiError::Http { .. } | YukiError::Xml(_) => Verdict::Unknown,
        _ => Verdict::Rejected,
    }
}

fn attention_outcome(parts: Vec<String>) -> Outcome {
    if parts.is_empty() {
        Outcome::Done
    } else {
        Outcome::NeedsAttention(format!(
            "files need attention: {}; see the rows",
            parts.join(", ")
        ))
    }
}

async fn run_dir(
    load_config: impl FnOnce() -> Result<Config, YukiError>,
    admin: Option<&str>,
    opts: &DirOptions<'_>,
    confirm: Confirm,
    format: Option<&str>,
    quiet: bool,
    calls: &Cell<usize>,
) -> Result<Outcome, YukiError> {
    folder_id(opts.folder)?;
    for f in opts.seed_folders {
        folder_id(f)?;
    }
    let (root, base) = sync::resolve_root(Path::new(opts.path))?;
    // A dry run writes nothing, so it takes no lock.
    let _lock = if opts.dry_run {
        None
    } else {
        Some(Lock::acquire(&root)?)
    };
    let mut plan = plan(root, base, opts.excludes)?;
    if !quiet {
        print_plan(&plan, opts);
    }

    if opts.dry_run {
        let mut rows: Vec<Row> = if opts.seed {
            plan.queue
                .iter()
                .map(|(f, _)| f)
                .chain(&plan.blocked)
                .map(|f| Row::new(&f.rel, "would-seed"))
                .collect()
        } else {
            plan.queue
                .iter()
                .enumerate()
                .map(|(i, (f, pending))| {
                    let action = match (i < opts.max, pending) {
                        (false, _) => "deferred",
                        (true, Pending::New) => "would-upload",
                        (true, Pending::Retry(_)) => "would-retry",
                    };
                    Row::new(&f.rel, action)
                })
                .collect()
        };
        rows.append(&mut plan.rows);
        if !quiet {
            if opts.seed {
                eprintln!(
                    "Dry run: --seed-from-yuki would look these files up in: {}.",
                    seed_folders(opts).join(", ")
                );
            }
            eprintln!("Dry run: nothing uploaded, nothing written.");
            print_rows(&rows, format);
        }
        return Ok(Outcome::Done);
    }

    if opts.seed {
        return seed(
            load_config,
            admin,
            plan,
            opts,
            confirm,
            format,
            quiet,
            calls,
        )
        .await;
    }

    let batch: Vec<(Found, Pending)> = plan.queue.iter().take(opts.max).cloned().collect();
    let deferred: Vec<Found> = plan
        .queue
        .iter()
        .skip(opts.max)
        .map(|(f, _)| f.clone())
        .collect();
    let deferred_rows = |rows: &mut Vec<Row>| {
        rows.extend(
            deferred
                .iter()
                .map(|f| Row::new(&f.rel, "deferred").note("over --max")),
        );
    };
    if batch.is_empty() {
        if !plan.moved.is_empty() {
            refresh_moved(&mut plan);
            plan.state.save(&plan.root)?;
        }
        if !quiet {
            eprintln!("Nothing to upload.");
            let mut rows = Vec::new();
            deferred_rows(&mut rows);
            rows.append(&mut plan.rows);
            print_rows(&rows, format);
        }
        return Ok(attention_outcome(plan.attention()));
    }

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
            let question = format!(
                "Upload {} file{} to Yuki folder {}?",
                batch.len(),
                if batch.len() == 1 { "" } else { "s" },
                opts.folder
            );
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
    bump(calls);
    client.authenticate(target.api_key).await?;
    // Paths are refreshed with the first write.
    refresh_moved(&mut plan);

    let mut rows = Vec::new();
    let (mut rejected, mut unknown, mut errors) = (0, 0, 0);
    let mut stop: Option<YukiError> = None;
    let total = batch.len();
    for (i, (file, _)) in batch.iter().enumerate() {
        if stop.is_some() {
            rows.push(Row::new(&file.rel, "not-attempted"));
            continue;
        }
        let bytes = match std::fs::read(&file.path) {
            Ok(b) if sync::sha256_hex(&b) == file.hash => b,
            Ok(_) => {
                errors += 1;
                rows.push(
                    Row::new(&file.rel, "error")
                        .error("file changed since the plan was made; run again"),
                );
                continue;
            }
            Err(e) => {
                errors += 1;
                rows.push(Row::new(&file.rel, "error").error(e.to_string()));
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
        bump(calls);
        let result = client
            .upload_document(target.admin_id, name, &BASE64.encode(&bytes), fid)
            .await;
        let mut entry = plan
            .state
            .files
            .get(&file.hash)
            .cloned()
            .unwrap_or_else(|| Entry::new(&file.rel, file.size, Status::Failed));
        entry.path = file.rel.clone();
        entry.size = file.size;
        entry.folder = Some(opts.folder.to_string());
        entry.recorded_at = sync::now_utc();
        match result {
            Ok(id) => {
                if !quiet {
                    eprintln!("{id}");
                }
                entry.status = Status::Uploaded;
                entry.document_id = Some(id.clone());
                entry.uploaded_at = Some(entry.recorded_at.clone());
                entry.error = None;
                plan.state.files.insert(file.hash.clone(), entry);
                if let Err(e) = plan.state.save(&plan.root) {
                    // The upload happened but could not be recorded: stop before
                    // anything else goes unrecorded.
                    return Err(YukiError::Config(format!(
                        "{} was uploaded as document {id}, but the state could not be saved: {e}. \
                         Record it with `{} --doc-id {id}` once the problem is fixed",
                        file.rel,
                        mark_hint(&plan.root, &file.rel)
                    )));
                }
                rows.push(Row::new(&file.rel, "uploaded").doc(Some(&id)));
            }
            Err(e) => {
                match verdict(&e) {
                    Verdict::Stop => {
                        if !quiet {
                            eprintln!("stopped: {e}");
                        }
                        rows.push(Row::new(&file.rel, "not-attempted").error(e.to_string()));
                        stop = Some(e);
                    }
                    Verdict::Rejected => {
                        if !quiet {
                            eprintln!("rejected: {e}");
                        }
                        rejected += 1;
                        entry.status = Status::Failed;
                        entry.attempts += 1;
                        entry.error = Some(e.to_string());
                        let note = if entry.attempts >= MAX_ATTEMPTS {
                            format!(
                                "attempt {} of {MAX_ATTEMPTS}; no longer retried",
                                entry.attempts
                            )
                        } else {
                            format!(
                                "attempt {} of {MAX_ATTEMPTS}; retried next run",
                                entry.attempts
                            )
                        };
                        plan.state.files.insert(file.hash.clone(), entry);
                        plan.state.save(&plan.root)?;
                        rows.push(
                            Row::new(&file.rel, "failed")
                                .error(e.to_string())
                                .note(note),
                        );
                    }
                    Verdict::Unknown => {
                        if !quiet {
                            eprintln!("unknown: {e}");
                        }
                        unknown += 1;
                        entry.status = Status::Unknown;
                        entry.attempts += 1;
                        entry.error = Some(e.to_string());
                        plan.state.files.insert(file.hash.clone(), entry);
                        plan.state.save(&plan.root)?;
                        let hint = mark_hint(&plan.root, &file.rel);
                        rows.push(Row::new(&file.rel, "unknown").error(e.to_string()).note(
                            format!(
                                "may have reached Yuki; not retried. Check Yuki, then \
                                 `{hint} --doc-id <id>`, or `{hint} --forget` to upload it again"
                            ),
                        ));
                    }
                }
            }
        }
    }
    deferred_rows(&mut rows);
    rows.append(&mut plan.rows);
    if !quiet {
        print_rows(&rows, format);
    }
    if let Some(e) = stop {
        return Err(e);
    }
    let mut parts = Vec::new();
    if rejected > 0 {
        parts.push(format!("{rejected} of {total} uploads failed"));
    }
    if unknown > 0 {
        parts.push(format!("{unknown} with an unknown upload result"));
    }
    if errors > 0 {
        parts.push(format!("{errors} could not be read"));
    }
    parts.extend(plan.attention());
    Ok(attention_outcome(parts))
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

/// Every document in `folder`, paged, with guards against an API that
/// ignores the page offset.
async fn fetch_folder(
    client: &ArchiveClient,
    folder: &str,
    calls: &Cell<usize>,
    quiet: bool,
) -> Result<Vec<ArchiveDocument>, YukiError> {
    let fid = folder_id(folder)?;
    let mut docs = Vec::new();
    let mut seen = HashSet::new();
    for page_no in 0..SEED_MAX_PAGES {
        bump(calls);
        let page = client
            .documents_in_folder_page(
                fid,
                "2000-01-01",
                "2099-12-31",
                SEED_PAGE_SIZE,
                page_no * SEED_PAGE_SIZE,
            )
            .await?;
        let received = page.len();
        if page.iter().any(|d| seen.contains(&d.id)) {
            if !quiet {
                eprintln!(
                    "warning: page {} of folder {folder} repeats documents already read; \
                     stopped paging, so the listing may be incomplete",
                    page_no + 1
                );
            }
            return Ok(docs);
        }
        seen.extend(page.iter().map(|d| d.id.clone()));
        docs.extend(page);
        if received < SEED_PAGE_SIZE {
            return Ok(docs);
        }
    }
    Err(YukiError::Config(format!(
        "folder {folder} holds more than {} documents; stopped seeding without recording anything",
        SEED_MAX_PAGES * SEED_PAGE_SIZE
    )))
}

fn name_of(f: &Found) -> String {
    f.path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&f.rel)
        .to_string()
}

fn short_date(doc: &ArchiveDocument) -> &str {
    doc.document_date.get(..10).unwrap_or(&doc.document_date)
}

/// `--seed-from-yuki`: record the files Yuki already has, by file name.
#[allow(clippy::too_many_arguments)]
async fn seed(
    load_config: impl FnOnce() -> Result<Config, YukiError>,
    admin: Option<&str>,
    mut plan: Plan,
    opts: &DirOptions<'_>,
    confirm: Confirm,
    format: Option<&str>,
    quiet: bool,
    calls: &Cell<usize>,
) -> Result<Outcome, YukiError> {
    let candidates: Vec<Found> = plan
        .queue
        .iter()
        .map(|(f, _)| f.clone())
        .chain(plan.blocked.iter().cloned())
        .collect();
    if candidates.is_empty() {
        if !quiet {
            eprintln!("Nothing to seed: every file is recorded.");
            print_rows(&plan.rows, format);
        }
        return Ok(attention_outcome(plan.attention()));
    }
    if confirm == Confirm::Refuse {
        return Ok(Outcome::NeedsConfirmation(
            "--seed-from-yuki records matches in .yuki-sync.json; pass --yes to confirm in \
             non-interactive mode, or --dry-run to only see the plan"
                .into(),
        ));
    }

    let config = load_config()?;
    let target = config.target(admin)?;
    // DocumentsInFolder takes no administration: it reads the session's
    // current domain, so select it first.
    let mut accounting = AccountingClient::new().with_api_root(target.api_root);
    bump(calls);
    accounting.authenticate(target.api_key).await?;
    bump(calls);
    accounting.set_current_domain(target.domain_id).await?;
    let session = accounting
        .session_id()
        .ok_or_else(|| YukiError::AuthFailed("no session after authenticate".into()))?;
    let client = ArchiveClient::new()
        .with_api_root(target.api_root)
        .with_session(session);

    let folders = seed_folders(opts);
    let mut docs: Vec<(String, ArchiveDocument)> = Vec::new();
    for folder in &folders {
        let found = fetch_folder(&client, folder, calls, quiet).await?;
        docs.extend(found.into_iter().map(|d| (folder.clone(), d)));
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
            .entry(sync::name_key(&d.file_name))
            .or_default()
            .push(i);
    }
    // A document is claimed at most once: by the existing state, or by one
    // local file in this run.
    let recorded: HashMap<String, String> = plan
        .state
        .files
        .values()
        .filter_map(|e| e.document_id.clone().map(|d| (d, e.path.clone())))
        .collect();
    let mut taken: HashSet<String> = recorded.keys().cloned().collect();
    let mut claims: BTreeMap<usize, Vec<&Found>> = BTreeMap::new();
    let mut rows = Vec::new();
    let mut unmatched = Vec::new();
    for file in &candidates {
        match by_name
            .get(&sync::name_key(&name_of(file)))
            .map(Vec::as_slice)
        {
            Some([only]) => {
                let doc = &docs[*only].1;
                match recorded.get(&doc.id) {
                    Some(other) => rows.push(
                        Row::new(&file.rel, "ambiguous")
                            .doc(Some(&doc.id))
                            .note(format!(
                                "the Yuki document with this name is already recorded for {other}; not recorded"
                            )),
                    ),
                    None => claims.entry(*only).or_default().push(file),
                }
            }
            Some(several) => {
                taken.extend(several.iter().map(|&i| docs[i].1.id.clone()));
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
    let mut records: Vec<(String, Entry)> = Vec::new();
    for (i, files) in &claims {
        let (folder, doc) = &docs[*i];
        taken.insert(doc.id.clone());
        if let [file] = files.as_slice() {
            let mut entry = plan
                .state
                .files
                .get(&file.hash)
                .cloned()
                .unwrap_or_else(|| Entry::new(&file.rel, file.size, Status::AlreadyInYuki));
            entry.path = file.rel.clone();
            entry.size = file.size;
            entry.status = Status::AlreadyInYuki;
            entry.document_id = Some(doc.id.clone());
            entry.folder = Some(folder.clone());
            entry.recorded_at = sync::now_utc();
            entry.error = None;
            entry.note = Some(format!("seeded: same file name in {folder}"));
            records.push((file.hash.clone(), entry));
            rows.push(
                Row::new(&file.rel, "already-in-yuki")
                    .doc(Some(&doc.id))
                    .note(format!(
                        "file name match in {folder}, dated {}",
                        short_date(doc)
                    )),
            );
        } else {
            let paths: Vec<&str> = files.iter().map(|f| f.rel.as_str()).collect();
            for file in files {
                rows.push(
                    Row::new(&file.rel, "ambiguous")
                        .doc(Some(&doc.id))
                        .note(format!(
                            "{} local files claim this one Yuki document ({}); not recorded",
                            files.len(),
                            paths.join(", ")
                        )),
                );
            }
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
                    .note("no Yuki document with this file name; it will be uploaded"),
            );
        } else {
            rows.push(Row::new(&file.rel, "possible-match").note(format!(
                "not recorded; maybe {}. If it is the same file, run: {} --doc-id <id>",
                candidates.join("; "),
                mark_hint(&plan.root, &file.rel)
            )));
        }
    }
    rows.sort_by(|a, b| a.path.cmp(&b.path));

    if !quiet {
        let count = |a: &str| rows.iter().filter(|r| r.action == a).count();
        eprintln!(
            "Matches: {} already in Yuki; {} ambiguous, {} possible matches to review, {} not in Yuki.",
            count("already-in-yuki"),
            count("ambiguous"),
            count("possible-match"),
            count("not-in-yuki")
        );
        eprintln!(
            "Matching is by file name only (Yuki reports no size or hash): a different file uploaded \
             under the same name is recorded as in Yuki, and a copy uploaded under another name is not \
             found and would be uploaded again. Review the rows; undo a match with \
             `yuki upload mark <file> --forget`."
        );
        rows.append(&mut plan.rows);
        print_rows(&rows, format);
    }
    if records.is_empty() {
        if !quiet {
            eprintln!("Nothing recorded.");
        }
        return Ok(attention_outcome(plan.attention()));
    }
    if confirm == Confirm::Prompt {
        let question = format!(
            "Record {} file{} as already in Yuki?",
            records.len(),
            if records.len() == 1 { "" } else { "s" }
        );
        if !ask(&question) {
            if !quiet {
                eprintln!("Aborted; nothing recorded.");
            }
            return Ok(Outcome::Aborted);
        }
    }
    refresh_moved(&mut plan);
    let recorded_now = records.len();
    let mut seeded_hashes = HashSet::new();
    for (hash, entry) in records {
        seeded_hashes.insert(hash.clone());
        plan.state.files.insert(hash, entry);
    }
    plan.state.save(&plan.root)?;
    if !quiet {
        eprintln!("Recorded {recorded_now} as already-in-yuki.");
    }
    // Unknown files that were found are resolved now; the rest still count.
    plan.unknown = plan
        .blocked
        .iter()
        .filter(|f| {
            !seeded_hashes.contains(&f.hash)
                && plan
                    .state
                    .files
                    .get(&f.hash)
                    .is_some_and(|e| e.status == Status::Unknown)
        })
        .count();
    plan.gave_up = plan
        .blocked
        .iter()
        .filter(|f| {
            !seeded_hashes.contains(&f.hash)
                && plan
                    .state
                    .files
                    .get(&f.hash)
                    .is_some_and(|e| e.status == Status::Failed)
        })
        .count();
    Ok(attention_outcome(plan.attention()))
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

/// `upload mark`: record one file by hand, without contacting Yuki.
///
/// The root is found as for `upload dir`: the nearest directory at or above
/// `--dir` (else the file's directory) that holds a state file. Without
/// `--dir`, one must exist.
pub fn mark(opts: MarkOptions<'_>, format: Option<&str>, quiet: bool) -> Result<(), YukiError> {
    if let Some(folder) = opts.folder {
        folder_id(folder)?;
    }
    let file = std::fs::canonicalize(opts.file)
        .map_err(|e| YukiError::Config(format!("{}: {e}", opts.file)))?;
    let start = match opts.dir {
        Some(dir) => PathBuf::from(dir),
        None => file.parent().map(Path::to_path_buf).unwrap_or_default(),
    };
    let (root, _) = sync::resolve_root(&start)?;
    if opts.dir.is_none() && !root.join(STATE_FILE).is_file() {
        return Err(YukiError::Config(format!(
            "no {STATE_FILE} found in any directory above {}; pass --dir <synced directory>",
            file.display()
        )));
    }
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
    let _lock = Lock::acquire(&root)?;
    let mut state = State::load(&root)?;
    let existing = state.files.get(&hash).cloned();

    let row = match opts.mark {
        Mark::Forget => {
            if let Some(old) = state.files.remove(&hash) {
                state.save(&root)?;
                Row::new(&rel, "forgotten")
                    .doc(old.document_id.as_deref())
                    .note("the next upload dir run treats it as new")
            } else {
                // A changed file: forget the record of the earlier content at
                // this path, so the new version can be uploaded.
                let before = state.files.len();
                let mut doc = None;
                state.files.retain(|_, e| {
                    let keep = e.path != rel;
                    if !keep {
                        doc = doc.take().or(e.document_id.clone());
                    }
                    keep
                });
                if state.files.len() == before {
                    Row::new(&rel, "unchanged").note("no record to forget")
                } else {
                    state.save(&root)?;
                    Row::new(&rel, "forgotten")
                        .doc(doc.as_deref())
                        .note("removed the record of earlier content at this path; the next run uploads this version")
                }
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
                // The same document, whether uploaded here or found in Yuki.
                Some(e)
                    if e.document_id == doc_id
                        && (e.status == status || (doc_id.is_some() && e.status.is_settled())) =>
                {
                    Row::new(&rel, "unchanged")
                        .doc(doc_id.as_deref())
                        .note(format!("already recorded as {}", e.status.as_str()))
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
                    let mut entry =
                        existing.unwrap_or_else(|| Entry::new(&rel, bytes.len() as u64, status));
                    entry.path = rel.clone();
                    entry.size = bytes.len() as u64;
                    entry.status = status;
                    entry.document_id = doc_id.clone();
                    entry.folder = opts.folder.map(str::to_string).or(entry.folder);
                    entry.recorded_at = sync::now_utc();
                    entry.error = None;
                    entry.note = Some(
                        opts.note
                            .map_or_else(|| "marked by hand".to_string(), str::to_string),
                    );
                    state.files.insert(hash, entry);
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
    fn only_a_clean_rejection_is_retried() {
        let fault = YukiError::SoapFault {
            code: "soap:Server".into(),
            message: "bad file".into(),
        };
        assert!(matches!(verdict(&fault), Verdict::Rejected));
        let server = YukiError::Http {
            status: 502,
            body: String::new(),
        };
        assert!(matches!(verdict(&server), Verdict::Unknown));
        let client = YukiError::Http {
            status: 400,
            body: String::new(),
        };
        assert!(matches!(verdict(&client), Verdict::Rejected));
        assert!(matches!(
            verdict(&YukiError::Xml("missing".into())),
            Verdict::Unknown
        ));
        assert!(matches!(
            verdict(&YukiError::AuthFailed("x".into())),
            Verdict::Stop
        ));
        assert!(matches!(verdict(&YukiError::RateLimited), Verdict::Stop));
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
