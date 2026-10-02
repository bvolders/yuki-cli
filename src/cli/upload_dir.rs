//! `yuki upload dir` and `yuki upload mark`: idempotent upload of a directory
//! of receipts, tracked in `<root>/.yuki-sync.json` (see [`crate::sync`]).
//!
//! Every upload is written ahead as `pending` and only then sent; Yuki's answer
//! turns it into `uploaded`, or into `failed` when the request provably never
//! left. Anything else (a timeout, a server fault, a crash) leaves it pending,
//! which is never retried automatically.

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
use crate::sync::{self, Entry, Excludes, Found, Lock, STATE_FILE, State, Status, shell_quote};

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
    /// Some files need the user (pending, failed, changed, unreadable).
    NeedsAttention(String),
}

/// Documents per `DocumentsInFolder` request when seeding.
const SEED_PAGE_SIZE: usize = 500;

/// Pages read from one folder at most when seeding.
const SEED_MAX_PAGES: usize = 40;

/// Uploads at the start of a run that all go wrong the same way stop it.
const EARLY_STOP: usize = 3;

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

fn mark_hint(root: &Path, rel: &str) -> String {
    format!(
        "yuki upload mark {}",
        shell_quote(&root.join(rel).display().to_string())
    )
}

fn pending_note(root: &Path, rel: &str) -> String {
    let hint = mark_hint(root, rel);
    format!(
        "upload started, outcome unknown: it may be in Yuki; not retried. Check Yuki, then \
         `{hint} --doc-id <id>`, or `{hint} --forget` to upload it again"
    )
}

/// The directory, its state, and its files sorted into a plan.
struct Plan {
    root: PathBuf,
    state: State,
    /// Files to upload: new ones first, then earlier failures, in path order.
    queue: Vec<Found>,
    /// Pending files: not uploaded, but seeding may find them in Yuki.
    pending: Vec<Found>,
    /// Rows for files that are not queued.
    rows: Vec<Row>,
    synced: usize,
    /// Changed and unreadable files.
    trouble: usize,
    /// Files whose recorded path changed (renamed or moved): hash and new path.
    moved: Vec<(String, String)>,
}

impl Plan {
    /// What needs the user, as phrases; empty when nothing does.
    fn attention(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if !self.pending.is_empty() {
            parts.push(format!("{} pending", self.pending.len()));
        }
        if self.trouble > 0 {
            parts.push(format!("{} changed or unreadable", self.trouble));
        }
        parts
    }

    /// Record paths of files that moved since they were recorded.
    fn refresh_moved(&mut self) {
        for (hash, rel) in std::mem::take(&mut self.moved) {
            if let Some(entry) = self.state.files.get_mut(&hash) {
                entry.path = rel;
            }
        }
    }
}

fn plan(root: PathBuf, excludes: &[String]) -> Result<Plan, YukiError> {
    let excludes = Excludes::new(excludes)?;
    let state = State::load(&root)?;
    let scan = sync::scan(&root, &excludes)?;
    if !scan.nested_states.is_empty() {
        return Err(YukiError::Config(format!(
            "{} holds other sync states ({}); sync each directory on its own, or merge \
             their records into {} by hand",
            root.display(),
            scan.nested_states.join(", "),
            root.join(STATE_FILE).display()
        )));
    }

    let scanned: HashSet<&str> = scan.files.iter().map(|f| f.hash.as_str()).collect();
    // Records whose content is no longer anywhere in the tree, by path: a new
    // hash at such a path is a changed file, not a new one.
    let gone: HashMap<&str, &Entry> = state
        .files
        .iter()
        .filter(|(hash, e)| !scanned.contains(hash.as_str()) && e.status != Status::Failed)
        .map(|(_, e)| (e.path.as_str(), e))
        .collect();

    let (mut new, mut retries, mut pending, mut rows, mut moved) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut synced, mut trouble) = (0, 0);
    let mut first_seen: HashMap<&str, &str> = HashMap::new();
    for file in &scan.files {
        if let Some(first) = first_seen.insert(&file.hash, &file.rel) {
            rows.push(Row::new(&file.rel, "duplicate").note(format!("same content as {first}")));
            first_seen.insert(&file.hash, first);
            continue;
        }
        let Some(entry) = state.files.get(&file.hash) else {
            match gone.get(file.rel.as_str()) {
                Some(e) => {
                    trouble += 1;
                    let hint = mark_hint(&root, &file.rel);
                    rows.push(
                        Row::new(&file.rel, "changed")
                            .doc(e.document_id.as_deref())
                            .note(format!(
                                "content changed since it was recorded as {}{}; not uploaded. \
                                 Upload the new version: `{hint} --forget`; keep it out: \
                                 `{hint} --skip`",
                                e.status.as_str(),
                                e.document_id
                                    .as_deref()
                                    .map(|d| format!(" (was doc {d})"))
                                    .unwrap_or_default()
                            )),
                    );
                }
                None => new.push(file.clone()),
            }
            continue;
        };
        if entry.path != file.rel {
            moved.push((file.hash.clone(), file.rel.clone()));
        }
        match entry.status {
            s if s.is_settled() => {
                synced += 1;
                rows.push(
                    Row::new(&file.rel, "synced")
                        .doc(entry.document_id.as_deref())
                        .note(s.as_str()),
                );
            }
            Status::Pending => {
                pending.push(file.clone());
                rows.push(
                    Row::new(&file.rel, "pending")
                        .error(entry.error.clone().unwrap_or_default())
                        .note(pending_note(&root, &file.rel)),
                );
            }
            _ => retries.push(file.clone()),
        }
    }
    for (rel, reason) in &scan.excluded {
        rows.push(Row::new(rel, "excluded").note(reason.clone()));
    }
    for (rel, error) in &scan.unreadable {
        trouble += 1;
        rows.push(Row::new(rel, "error").error(error.clone()));
    }
    new.extend(retries);
    Ok(Plan {
        root,
        state,
        queue: new,
        pending,
        rows,
        synced,
        trouble,
        moved,
    })
}

/// Print the plan to stderr: counts, then the files that would be uploaded.
fn print_plan(plan: &Plan, opts: &DirOptions<'_>) {
    eprintln!(
        "Plan for {} (Yuki folder {}):",
        plan.root.display(),
        opts.folder
    );
    eprintln!(
        "  {} to upload, {} already synced, {} other",
        plan.queue.len(),
        plan.synced,
        plan.rows.len() - plan.synced
    );
    let attention = plan.attention();
    if !attention.is_empty() {
        eprintln!("  Needs attention (see the rows): {}", attention.join(", "));
    }
    for (i, f) in plan.queue.iter().enumerate() {
        let over = if !opts.seed && i >= opts.max {
            "  (over --max, next run)"
        } else {
            ""
        };
        eprintln!("    {}  {:.1} KB{over}", f.rel, f.size as f64 / 1024.0);
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

/// Ask for, or check, the go-ahead to `what` (e.g. "upload 3 files").
fn go_ahead(confirm: Confirm, what: &str, quiet: bool) -> Option<Outcome> {
    match confirm {
        Confirm::Yes => None,
        Confirm::Refuse => Some(Outcome::NeedsConfirmation(format!(
            "upload dir would {what}; pass --yes to confirm in non-interactive mode, \
             or --dry-run to only see the plan"
        ))),
        Confirm::Prompt if ask(&format!("{}?", capitalise(what))) => None,
        Confirm::Prompt => {
            if !quiet {
                eprintln!("Aborted; nothing changed.");
            }
            Some(Outcome::Aborted)
        }
    }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

fn bump(calls: &Cell<usize>) {
    calls.set(calls.get() + 1);
}

fn outcome(parts: Vec<String>) -> Outcome {
    if parts.is_empty() {
        Outcome::Done
    } else {
        Outcome::NeedsAttention(format!(
            "files need attention: {}; see the rows",
            parts.join(", ")
        ))
    }
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
    let root = sync::sync_root(Path::new(opts.path))?;
    // A dry run writes nothing, so it takes no lock.
    let _lock = (!opts.dry_run).then(|| Lock::acquire(&root)).transpose()?;
    let mut plan = plan(root, opts.excludes)?;
    if !quiet {
        print_plan(&plan, opts);
    }

    if opts.dry_run {
        let mut rows: Vec<Row> = if opts.seed {
            plan.queue
                .iter()
                .chain(&plan.pending)
                .map(|f| Row::new(&f.rel, "would-seed"))
                .collect()
        } else {
            plan.queue
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    Row::new(
                        &f.rel,
                        if i < opts.max {
                            "would-upload"
                        } else {
                            "deferred"
                        },
                    )
                })
                .collect()
        };
        rows.append(&mut plan.rows);
        if !quiet {
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

    let batch: Vec<Found> = plan.queue.iter().take(opts.max).cloned().collect();
    let mut rows: Vec<Row> = plan
        .queue
        .iter()
        .skip(opts.max)
        .map(|f| Row::new(&f.rel, "deferred").note("over --max"))
        .collect();
    if batch.is_empty() {
        if !plan.moved.is_empty() {
            plan.refresh_moved();
            plan.state.save(&plan.root)?;
        }
        if !quiet {
            rows.append(&mut plan.rows);
            print_rows(&rows, format);
        }
        return Ok(outcome(plan.attention()));
    }
    let what = format!(
        "upload {} files to Yuki folder {}",
        batch.len(),
        opts.folder
    );
    if let Some(stop) = go_ahead(confirm, &what, quiet) {
        return Ok(stop);
    }

    let config = load_config()?;
    let target = config.target(admin)?;
    let fid = folder_id(opts.folder)?;
    let mut client = ArchiveClient::new().with_api_root(target.api_root);
    bump(calls);
    client.authenticate(target.api_key).await?;
    plan.refresh_moved();

    let (mut failed, mut pending) = (0, 0);
    let mut early: Vec<String> = Vec::new();
    let mut stop: Option<YukiError> = None;
    for (i, file) in batch.iter().enumerate() {
        if stop.is_some() {
            rows.push(Row::new(&file.rel, "not-attempted"));
            continue;
        }
        let bytes = match std::fs::read(&file.path) {
            Ok(b) if sync::sha256_hex(&b) == file.hash => b,
            Ok(_) => {
                failed += 1;
                rows.push(Row::new(&file.rel, "error").error("changed since the plan; run again"));
                continue;
            }
            Err(e) => {
                failed += 1;
                rows.push(Row::new(&file.rel, "error").error(e.to_string()));
                continue;
            }
        };
        let name = file
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&file.rel);
        // Write ahead: from here until Yuki answers, the file may be in Yuki.
        let before = plan.state.files.get(&file.hash).cloned();
        let mut entry = Entry::update(
            &plan.state,
            &file.hash,
            &file.rel,
            file.size,
            Status::Pending,
        );
        entry.folder = Some(opts.folder.to_string());
        plan.state.files.insert(file.hash.clone(), entry.clone());
        plan.state.save(&plan.root)?;

        if !quiet {
            eprint!("[{}/{}] {} ... ", i + 1, batch.len(), file.rel);
        }
        bump(calls);
        let result = client
            .upload_document(target.admin_id, name, &BASE64.encode(&bytes), fid)
            .await;
        let row = match result {
            Ok(id) => {
                if !quiet {
                    eprintln!("{id}");
                }
                entry.status = Status::Uploaded;
                entry.document_id = Some(id.clone());
                entry.uploaded_at = Some(sync::now_utc());
                Row::new(&file.rel, "uploaded").doc(Some(&id))
            }
            Err(e) => {
                if !quiet {
                    eprintln!("{e}");
                }
                entry.error = Some(e.to_string());
                match &e {
                    // Refused before anything was stored: undo the write-ahead
                    // and stop, as every further call would be refused too.
                    YukiError::AuthFailed(_) | YukiError::RateLimited => {
                        match before {
                            Some(b) => plan.state.files.insert(file.hash.clone(), b),
                            None => plan.state.files.remove(&file.hash),
                        };
                        plan.state.save(&plan.root)?;
                        rows.push(Row::new(&file.rel, "not-attempted").error(e.to_string()));
                        stop = Some(e);
                        continue;
                    }
                    YukiError::Request(r) if r.is_connect() || r.is_builder() => {
                        failed += 1;
                        entry.status = Status::Failed;
                        Row::new(&file.rel, "failed")
                            .error(e.to_string())
                            .note("never reached Yuki; retried next run")
                    }
                    _ => {
                        pending += 1;
                        Row::new(&file.rel, "pending")
                            .error(e.to_string())
                            .note(pending_note(&plan.root, &file.rel))
                    }
                }
            }
        };
        let ok = row.action == "uploaded";
        let error = row.error.clone();
        plan.state.files.insert(file.hash.clone(), entry);
        if let Err(e) = plan.state.save(&plan.root) {
            // The entry stays pending on disk, which is the safe reading.
            return Err(YukiError::Config(format!(
                "{} could not record the result for {} ({}): {e}; it stays pending",
                STATE_FILE, file.rel, row.action
            )));
        }
        rows.push(row);
        // A run whose first uploads all go wrong the same way is systemic.
        if i < EARLY_STOP && !ok && early.len() == i {
            early.push(error);
            if early.len() == EARLY_STOP && early.iter().all(|e| *e == early[0]) {
                if !quiet {
                    eprintln!(
                        "stopping: the first {EARLY_STOP} uploads all failed with: {}",
                        early[0]
                    );
                }
                stop = Some(YukiError::Config(format!(
                    "stopped after the first {EARLY_STOP} uploads all failed with: {}",
                    early[0]
                )));
            }
        }
    }
    rows.append(&mut plan.rows);
    if !quiet {
        print_rows(&rows, format);
    }
    if let Some(e) = stop {
        return Err(e);
    }
    let mut parts = Vec::new();
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if pending > 0 {
        parts.push(format!("{pending} newly pending"));
    }
    parts.extend(plan.attention());
    Ok(outcome(parts))
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

/// Why `doc` might be the Yuki copy of local `name` (`YYYY-MM-DD_vendor_ref.ext`),
/// or `None`. Used only to suggest matches for review.
fn possible_match(name: &str, doc: &ArchiveDocument) -> Option<&'static str> {
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    let mut parts = stem.splitn(3, '_');
    let (date, vendor, reference) = (parts.next()?, parts.next()?, parts.next()?);
    crate::period::epoch_days(date).filter(|_| date.len() == 10)?;
    let file = doc.file_name.to_lowercase();
    let reference = reference.to_lowercase();
    if reference.len() >= 5
        && [
            &file,
            &doc.subject.to_lowercase(),
            &doc.reference.to_lowercase(),
        ]
        .iter()
        .any(|s| s.contains(&reference))
    {
        return Some("reference in the Yuki document");
    }
    let vendor = vendor.split('-').next().unwrap_or_default().to_lowercase();
    (vendor.len() >= 3
        && doc.document_date.get(..10) == Some(date)
        && (doc.contact_name.to_lowercase().contains(&vendor) || file.contains(&vendor)))
    .then_some("same date and vendor")
}

/// Every document in `folder`. Any paging anomaly is an error: a listing that
/// may be incomplete must not decide what is already in Yuki.
async fn fetch_folder(
    client: &ArchiveClient,
    folder: &str,
    calls: &Cell<usize>,
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
        let full = page.len() == SEED_PAGE_SIZE;
        for doc in page {
            if !seen.insert(doc.id.clone()) {
                return Err(YukiError::Config(format!(
                    "Yuki listed document {} twice while paging folder {folder}, so the listing \
                     cannot be trusted; nothing recorded",
                    doc.id
                )));
            }
            docs.push(doc);
        }
        if !full {
            return Ok(docs);
        }
    }
    Err(YukiError::Config(format!(
        "folder {folder} holds more than {} documents, more than seeding reads; nothing recorded",
        SEED_MAX_PAGES * SEED_PAGE_SIZE
    )))
}

fn name_of(f: &Found) -> &str {
    f.path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&f.rel)
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
    let candidates: Vec<&Found> = plan.queue.iter().chain(&plan.pending).collect();
    if candidates.is_empty() {
        if !quiet {
            print_rows(&plan.rows, format);
        }
        return Ok(outcome(plan.attention()));
    }
    if confirm == Confirm::Refuse {
        return Ok(go_ahead(confirm, "record seeding matches", quiet).expect("refused"));
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
    let session = accounting.session_id().unwrap_or_default();
    let client = ArchiveClient::new()
        .with_api_root(target.api_root)
        .with_session(session);
    let mut docs: Vec<(String, ArchiveDocument)> = Vec::new();
    for folder in seed_folders(opts) {
        let found = fetch_folder(&client, &folder, calls).await?;
        docs.extend(found.into_iter().map(|d| (folder.clone(), d)));
    }

    let mut by_name: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, (_, d)) in docs.iter().enumerate() {
        by_name
            .entry(sync::name_key(&d.file_name))
            .or_default()
            .push(i);
    }
    // A document is claimed at most once: by the state, or by one file now.
    let recorded: HashMap<&str, &str> = plan
        .state
        .files
        .values()
        .filter_map(|e| Some((e.document_id.as_deref()?, e.path.as_str())))
        .collect();
    let mut taken: HashSet<&str> = recorded.keys().copied().collect();
    let mut claims: BTreeMap<usize, Vec<&Found>> = BTreeMap::new();
    let mut rows = Vec::new();
    let mut unmatched = Vec::new();
    for &file in &candidates {
        match by_name
            .get(&sync::name_key(name_of(file)))
            .map(Vec::as_slice)
        {
            None => unmatched.push(file),
            Some([only]) => match recorded.get(docs[*only].1.id.as_str()) {
                Some(other) => rows.push(Row::new(&file.rel, "ambiguous").note(format!(
                    "its Yuki document {} is already recorded for {other}; not recorded",
                    docs[*only].1.id
                ))),
                None => claims.entry(*only).or_default().push(file),
            },
            Some(several) => {
                taken.extend(several.iter().map(|&i| docs[i].1.id.as_str()));
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
        }
    }
    let mut records = Vec::new();
    for (&i, files) in &claims {
        let (folder, doc) = &docs[i];
        taken.insert(&doc.id);
        if let [file] = files.as_slice() {
            let mut entry = Entry::update(
                &plan.state,
                &file.hash,
                &file.rel,
                file.size,
                Status::AlreadyInYuki,
            );
            entry.document_id = Some(doc.id.clone());
            entry.folder = Some(folder.clone());
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
        let maybe: Vec<String> = docs
            .iter()
            .filter(|(_, d)| !taken.contains(d.id.as_str()))
            .filter_map(|(folder, d)| {
                let why = possible_match(name_of(file), d)?;
                Some(format!(
                    "{} {:?} in {folder}, {} ({why})",
                    d.id,
                    d.file_name,
                    short_date(d)
                ))
            })
            .collect();
        rows.push(if maybe.is_empty() {
            Row::new(&file.rel, "not-in-yuki").note("no Yuki document with this file name")
        } else {
            Row::new(&file.rel, "possible-match").note(format!(
                "not recorded; maybe {}. If it is the same file: {} --doc-id <id>",
                maybe.join("; "),
                mark_hint(&plan.root, &file.rel)
            ))
        });
    }
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    // Pending files were candidates and have a seeding row of their own.
    plan.rows.retain(|r| r.action != "pending");
    if !quiet {
        eprintln!(
            "Matching is by file name only (Yuki reports no size or hash): a different file \
             uploaded under the same name is recorded as in Yuki, and a copy uploaded under \
             another name is not found. Undo a match with `yuki upload mark <file> --forget`."
        );
        rows.append(&mut plan.rows);
        print_rows(&rows, format);
    }
    if !records.is_empty() {
        let what = format!("record {} files as already in Yuki", records.len());
        if let Some(stop) = go_ahead(confirm, &what, quiet) {
            return Ok(stop);
        }
        plan.refresh_moved();
        for (hash, entry) in records {
            plan.pending.retain(|f| f.hash != hash);
            plan.state.files.insert(hash, entry);
        }
        plan.state.save(&plan.root)?;
    }
    Ok(outcome(plan.attention()))
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
/// The root is `--dir` (which must be a valid sync root), else the nearest
/// directory above the file holding a state file.
pub fn mark(opts: MarkOptions<'_>, format: Option<&str>, quiet: bool) -> Result<(), YukiError> {
    if let Some(folder) = opts.folder {
        folder_id(folder)?;
    }
    let file = std::fs::canonicalize(opts.file)
        .map_err(|e| YukiError::Config(format!("{}: {e}", opts.file)))?;
    let parent = file.parent().unwrap_or(Path::new("/"));
    let root = match opts.dir {
        Some(dir) => sync::sync_root(Path::new(dir))?,
        None => sync::find_root(parent)?.ok_or_else(|| {
            YukiError::Config(format!(
                "no {STATE_FILE} found in any directory above {}; pass --dir <synced directory>",
                file.display()
            ))
        })?,
    };
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
            // The file's own record, else (a changed file) the record of the
            // earlier content at its path.
            let before = state.files.len();
            let mut doc = existing.as_ref().and_then(|e| e.document_id.clone());
            if existing.is_some() {
                state.files.remove(&hash);
            } else {
                state.files.retain(|_, e| {
                    let keep = e.path != rel;
                    if !keep {
                        doc = doc.take().or(e.document_id.clone());
                    }
                    keep
                });
            }
            if state.files.len() == before {
                Row::new(&rel, "unchanged").note("no record to forget")
            } else {
                state.save(&root)?;
                Row::new(&rel, "forgotten")
                    .doc(doc.as_deref())
                    .note("the next upload dir run uploads it")
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
            let claimed_by = doc_id.as_deref().and_then(|id| {
                state
                    .files
                    .iter()
                    .find(|(h, e)| **h != hash && e.document_id.as_deref() == Some(id))
                    .map(|(_, e)| e.path.clone())
            });
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
                _ if claimed_by.is_some() && !opts.force => {
                    return Err(YukiError::Config(format!(
                        "document {} is already recorded for {}; pass --force to record it for {rel} too",
                        doc_id.unwrap_or_default(),
                        claimed_by.unwrap_or_default()
                    )));
                }
                _ => {
                    let mut entry = Entry::update(&state, &hash, &rel, bytes.len() as u64, status);
                    entry.document_id = doc_id.clone();
                    entry.folder = opts.folder.map(str::to_string).or(entry.folder);
                    entry.note = Some(opts.note.unwrap_or("marked by hand").to_string());
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
        let by_day = doc("2", "2026-08-17", "Github B.V.", "github-receipt.pdf");
        assert_eq!(possible_match(gh, &by_day), Some("same date and vendor"));
        // Another vendor that day, or the vendor another day, is no candidate.
        let telenet = doc("3", "2026-08-17", "Telenet", "t.pdf");
        assert_eq!(possible_match(gh, &telenet), None);
        let later = doc("4", "2026-08-18", "Github B.V.", "g.pdf");
        assert_eq!(possible_match(gh, &later), None);
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
