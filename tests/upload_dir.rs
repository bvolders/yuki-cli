//! End-to-end: `upload dir` and `upload mark` against a local mock of Yuki.
//!
//! The mock answers Authenticate, SetCurrentDomain, UploadDocument and
//! DocumentsInFolder. What an upload gets depends on its file name:
//! - "broken": a generic SOAP fault (outcome unknown: stays pending),
//! - "expire": an invalid-session fault (stops the run; stays pending),
//! - "denied": HTTP 401 (refused unprocessed: stops the run, nothing recorded),
//! - "garbled": HTTP 502 without a SOAP fault (stays pending),
//! - "hang": no answer at all, ever,
//! - anything else: a new document ID.
//!
//! The access key "bad-key" is refused at Authenticate.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

mod common;

use common::{Request, RequestLog as Log, fails, fault, json, ok, response, stderr, yuki};
use serde_json::Value;
use tempfile::TempDir;

const STATE: &str = ".yuki-sync.json";

fn document(id: &str, date: &str, contact: &str, file: &str) -> String {
    format!(
        "<Document ID=\"{id}\"><Subject>Factuur</Subject><DocumentDate>{date}T00:00:00</DocumentDate>\
         <Amount>1.00</Amount><ContactName>{contact}</ContactName><FileName>{file}</FileName></Document>"
    )
}

/// The archive the seeding tests find. inkoop holds 500 filler documents on
/// its first page, so seeding has to page; verkoop answers every page with
/// the same documents, as an API that ignores the offset would.
fn documents_in_folder(folder: &str, start: &str) -> String {
    let filler = || -> String {
        (0..500)
            .map(|i| {
                document(
                    &format!("fill-{i}"),
                    "2025-01-01",
                    "Filler",
                    &format!("old-{i}.pdf"),
                )
            })
            .collect()
    };
    let docs = match (folder, start) {
        ("1", "0") | ("2", _) => filler(),
        ("1", "500") => [
            document(
                "y-bol",
                "2026-08-30",
                "Bol.com",
                "2026-08-30_bol-com_111.pdf",
            ),
            document(
                "y-vercel",
                "2026-09-07",
                "Vercel Inc",
                "Invoice-REF12345.pdf",
            ),
            document("y-scan-1", "2026-05-01", "Shop", "scan.pdf"),
            document("y-scan-2", "2026-05-02", "Shop", "scan.pdf"),
            document("y-dup", "2026-05-03", "Shop", "dup.pdf"),
            document("y-taken", "2026-05-04", "Shop", "taken.pdf"),
            document("y-pending", "2026-05-06", "Shop", "c-garbled.pdf"),
            // Composed (NFC) here; the local file name is decomposed (NFD).
            document("y-cafe", "2026-05-05", "Caf\u{e9}", "Caf\u{e9}.pdf"),
        ]
        .concat(),
        _ => String::new(),
    };
    response("DocumentsInFolder", &docs)
}

fn reply(r: &Request, uploads: &AtomicUsize) -> (u16, String) {
    match r.action.as_str() {
        "Authenticate" if r.param("accessKey") == "bad-key" => (500, fault("Invalid access key")),
        "Authenticate" => (200, response("Authenticate", "session-1")),
        "UploadDocument" => {
            let name = r.param("fileName");
            if name.contains("hang") {
                std::thread::sleep(Duration::from_secs(600));
                (0, String::new())
            } else if name.contains("broken") {
                (500, fault("Server was unable to process request."))
            } else if name.contains("denied") {
                (401, "Unauthorized".to_string())
            } else if name.contains("expire") {
                (500, fault("Invalid session ID"))
            } else if name.contains("garbled") {
                (502, "Bad gateway".to_string())
            } else {
                let n = uploads.fetch_add(1, Ordering::SeqCst) + 1;
                (200, response("UploadDocument", &format!("doc-{n}")))
            }
        }
        "DocumentsInFolder" => (
            200,
            documents_in_folder(r.param("folderID"), r.param("startRecord")),
        ),
        other => (200, response(other, "")),
    }
}

fn mock_yuki() -> (String, Log) {
    let uploads = Arc::new(AtomicUsize::new(0));
    common::mock(move |r| reply(r, &uploads))
}

/// What the mock saw: each SOAP action, with the uploaded file name or the
/// folder and offset when it has one.
fn summary(seen: &Log) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|r| match r.action.as_str() {
            "UploadDocument" => format!("UploadDocument {}", r.param("fileName")),
            "DocumentsInFolder" => format!(
                "DocumentsInFolder {} {}",
                r.param("folderID"),
                r.param("startRecord")
            ),
            "SetCurrentDomain" => format!("SetCurrentDomain {}", r.param("domainID")),
            other => other.to_string(),
        })
        .collect()
}

fn home(root: &str) -> TempDir {
    common::home_with_config(root, "test-key", "")
}

/// A receipts directory holding `files` (relative path, content).
fn receipts(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().expect("receipts dir");
    for (rel, content) in files {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    dir
}

fn run(home: &TempDir, dir: &Path, extra: &[&str]) -> Output {
    let dir = dir.to_str().unwrap();
    let mut args = vec!["upload", "dir", dir];
    args.extend_from_slice(extra);
    yuki(home, &args)
}

fn mark(home: &TempDir, file: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["upload", "mark", file.to_str().unwrap()];
    args.extend_from_slice(extra);
    yuki(home, &args)
}

/// The output row for `path`.
#[track_caller]
fn row(output: &Output, path: &str) -> Value {
    json(output)["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["Path"] == path)
        .unwrap_or_else(|| panic!("no row for {path}"))
        .clone()
}

/// The rows are exactly these (path, action) pairs, in any order.
#[track_caller]
fn assert_actions(output: &Output, expected: &[(&str, &str)]) {
    let mut got: Vec<(String, String)> = json(output)["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|r| {
            (
                r["Path"].as_str().unwrap().into(),
                r["Action"].as_str().unwrap().into(),
            )
        })
        .collect();
    let mut want: Vec<(String, String)> = expected
        .iter()
        .map(|(p, a)| ((*p).into(), (*a).into()))
        .collect();
    got.sort();
    want.sort();
    assert_eq!(got, want);
}

fn state(dir: &Path) -> Value {
    let text = std::fs::read_to_string(dir.join(STATE)).expect("state file");
    serde_json::from_str(&text).expect("state JSON")
}

/// The state entry recorded for the file at `rel`.
fn entry_for(dir: &Path, rel: &str) -> Option<Value> {
    state(dir)["files"]
        .as_object()
        .unwrap()
        .values()
        .find(|e| e["path"] == rel)
        .cloned()
}

fn uploads(seen: &Log) -> Vec<String> {
    summary(seen)
        .iter()
        .filter_map(|a| a.strip_prefix("UploadDocument ").map(str::to_string))
        .collect()
}

fn calls(seen: &Log) -> usize {
    seen.lock().unwrap().len()
}

#[test]
fn a_dry_run_makes_no_calls_and_writes_nothing() {
    let (root, seen) = mock_yuki();
    // The config points at the live mock, so any call would be seen.
    let home = home(&root);
    let dir = receipts(&[
        ("2026/a.pdf", "a"),
        ("2026/b.PNG", "b"),
        ("2025/deep/c.pdf", "c"),
        ("_TO_DELETE/old.pdf", "old"),
    ]);
    let out = run(&home, dir.path(), &["--dry-run", "--exclude", "./2025/"]);
    ok(&out);
    assert_actions(
        &out,
        &[
            ("2026/a.pdf", "would-upload"),
            ("2026/b.PNG", "would-upload"),
            ("2025/deep/c.pdf", "excluded"),
            ("_TO_DELETE/old.pdf", "excluded"),
        ],
    );
    assert!(stderr(&out).contains("API calls made: 0"));
    let seed = run(&home, dir.path(), &["--dry-run", "--seed-from-yuki"]);
    assert_eq!(row(&seed, "2026/a.pdf")["Action"], "would-seed");
    fails(
        &run(&home, dir.path(), &["--dry-run", "--exclude", "["]),
        1,
        "invalid --exclude",
    );
    assert_eq!(calls(&seen), 0, "{:?}", seen.lock().unwrap());
    assert!(!dir.path().join(STATE).exists());
}

#[test]
fn a_non_interactive_run_without_yes_refuses_before_any_call() {
    let (root, seen) = mock_yuki();
    let home = home(&root);
    let dir = receipts(&[("a.pdf", "a")]);
    for args in [&[][..], &["--seed-from-yuki"][..]] {
        fails(
            &run(&home, dir.path(), args),
            1,
            "\"kind\":\"confirmation_required\"",
        );
    }
    assert_eq!(calls(&seen), 0);
    assert!(!dir.path().join(STATE).exists());
}

#[test]
fn uploads_new_files_and_leaves_uncertain_ones_pending() {
    let (root, seen) = mock_yuki();
    let home = home(&root);
    let dir = receipts(&[
        ("2026/a.pdf", "a"),
        ("2026/b-broken.pdf", "b"),
        ("2026/c-garbled.pdf", "c"),
        ("2026/d.jpeg", "d"),
        ("2026/notes.txt", "not a receipt"),
        (".hidden/h.pdf", "h"),
    ]);
    let out = run(&home, dir.path(), &["--yes"]);
    fails(&out, 1, "2 newly pending");
    assert_actions(
        &out,
        &[
            ("2026/a.pdf", "uploaded"),
            ("2026/b-broken.pdf", "pending"),
            ("2026/c-garbled.pdf", "pending"),
            ("2026/d.jpeg", "uploaded"),
            (".hidden/h.pdf", "excluded"),
        ],
    );
    assert!(stderr(&out).contains("API calls made: 5"));
    let a = entry_for(dir.path(), "2026/a.pdf").unwrap();
    assert_eq!(
        (&a["status"], &a["document_id"], &a["folder"]),
        (&"uploaded".into(), &"doc-1".into(), &"uitzoeken".into())
    );
    assert!(a["uploaded_at"].as_str().unwrap().ends_with('Z'));
    let b = entry_for(dir.path(), "2026/b-broken.pdf").unwrap();
    assert_eq!(b["status"], "pending");
    assert!(b["error"].as_str().unwrap().contains("unable to process"));
    let note = row(&out, "2026/b-broken.pdf")["Note"].to_string();
    assert!(note.contains("yuki upload mark '"), "{note}");
    // Keyed by the sha256 of the content.
    assert!(
        state(dir.path())["files"]
            .get("ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb")
            .is_some()
    );

    // Pending files are never retried; the run still needs attention.
    let again = run(&home, dir.path(), &["--yes"]);
    fails(&again, 1, "2 pending");
    assert!(stderr(&again).contains("API calls made: 0"));
    assert_eq!(uploads(&seen).len(), 4);

    // Resolved by hand: one is in Yuki, the other goes up again.
    ok(&mark(
        &home,
        &dir.path().join("2026/c-garbled.pdf"),
        &["--doc-id", "y-1"],
    ));
    ok(&mark(
        &home,
        &dir.path().join("2026/b-broken.pdf"),
        &["--forget"],
    ));
    let plan = run(&home, dir.path(), &["--dry-run"]);
    assert_eq!(row(&plan, "2026/c-garbled.pdf")["Action"], "synced");
    assert_eq!(row(&plan, "2026/b-broken.pdf")["Action"], "would-upload");
}

#[test]
fn file_names_are_xml_escaped_in_the_request() {
    let (root, seen) = mock_yuki();
    let home = home(&root);
    let dir = receipts(&[("Tom & Jerry <x>.pdf", "tj")]);
    ok(&run(&home, dir.path(), &["--yes"]));
    assert_eq!(uploads(&seen), ["Tom &amp; Jerry &lt;x&gt;.pdf"]);
}

/// Wait until the mock has seen `what`.
fn wait_for(seen: &Log, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !summary(seen).iter().any(|a| a == what) {
        assert!(Instant::now() < deadline, "mock never saw {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_killed_run_leaves_its_upload_pending_and_releases_the_lock() {
    let (root, seen) = mock_yuki();
    let home = home(&root);
    let dir = receipts(&[("a-hang.pdf", "a")]);
    let mut child = Command::new(env!("CARGO_BIN_EXE_yuki"))
        .args(["upload", "dir", dir.path().to_str().unwrap(), "--yes"])
        .env("HOME", home.path())
        .env_remove("YUKI_REGION")
        .env_remove("YUKI_BASE_URL")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn yuki");
    wait_for(&seen, "UploadDocument a-hang.pdf");

    // Written ahead: the upload is pending before Yuki has answered.
    assert_eq!(
        entry_for(dir.path(), "a-hang.pdf").unwrap()["status"],
        "pending"
    );
    // The holder's OS lock keeps a second run out.
    fails(&run(&home, dir.path(), &["--yes"]), 1, "another yuki run");
    // A run below the synced root is refused.
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    fails(&run(&home, &sub, &["--yes"]), 1, "run on");

    child.kill().expect("kill yuki");
    child.wait().expect("reap yuki");
    // The kernel released the lock; the pending upload is not retried.
    let after = run(&home, dir.path(), &["--yes"]);
    fails(&after, 1, "1 pending");
    assert_eq!(row(&after, "a-hang.pdf")["Action"], "pending");
    assert_eq!(uploads(&seen), ["a-hang.pdf"]);
}

#[test]
fn the_first_uploads_failing_alike_stop_the_run() {
    let (root, seen) = mock_yuki();
    let home = home(&root);
    let dir = receipts(&[
        ("a-garbled.pdf", "a"),
        ("b-garbled.pdf", "b"),
        ("c-garbled.pdf", "c"),
        ("d.pdf", "d"),
    ]);
    let out = run(&home, dir.path(), &["--yes"]);
    fails(&out, 1, "first 3 uploads all failed");
    assert_eq!(row(&out, "d.pdf")["Action"], "not-attempted");
    assert_eq!(uploads(&seen).len(), 3);
}

#[test]
fn a_renamed_or_moved_file_is_not_uploaded_again() {
    let (root, seen) = mock_yuki();
    let home = home(&root);
    let dir = receipts(&[("in/receipt.pdf", "same bytes")]);
    ok(&run(&home, dir.path(), &["--yes"]));
    std::fs::create_dir_all(dir.path().join("2026/vendor")).unwrap();
    std::fs::rename(
        dir.path().join("in/receipt.pdf"),
        dir.path().join("2026/vendor/renamed.pdf"),
    )
    .unwrap();
    std::fs::write(dir.path().join("2026/copy.pdf"), "same bytes").unwrap();
    let out = run(&home, dir.path(), &["--yes"]);
    ok(&out);
    assert_eq!(uploads(&seen).len(), 1, "nothing uploaded again");
    assert_actions(
        &out,
        &[
            ("2026/copy.pdf", "synced"),
            ("2026/vendor/renamed.pdf", "duplicate"),
        ],
    );
    let st = state(dir.path());
    assert_eq!(
        st["files"].as_object().unwrap().values().next().unwrap()["path"],
        "2026/copy.pdf"
    );
}

#[test]
fn forget_by_path_keeps_the_record_of_content_that_moved() {
    let (root, seen) = mock_yuki();
    let home = home(&root);
    let dir = receipts(&[("a.pdf", "first")]);
    ok(&run(&home, dir.path(), &["--yes"]));
    // a.pdf is renamed to b.pdf, and a new a.pdf appears.
    std::fs::rename(dir.path().join("a.pdf"), dir.path().join("b.pdf")).unwrap();
    std::fs::write(dir.path().join("a.pdf"), "second").unwrap();

    let forgot = mark(&home, &dir.path().join("a.pdf"), &["--forget"]);
    assert_eq!(json(&forgot)["items"][0]["Action"], "unchanged");
    let out = run(&home, dir.path(), &["--yes"]);
    assert_actions(&out, &[("a.pdf", "uploaded"), ("b.pdf", "synced")]);
    assert_eq!(
        uploads(&seen),
        ["a.pdf", "a.pdf"],
        "b.pdf is not uploaded again"
    );
}

#[test]
fn a_changed_file_is_not_uploaded_until_resolved() {
    let (root, seen) = mock_yuki();
    let home = home(&root);
    let dir = receipts(&[("inv.pdf", "version 1"), ("other.pdf", "o")]);
    ok(&run(&home, dir.path(), &["--yes"]));
    std::fs::write(dir.path().join("inv.pdf"), "version 2").unwrap();
    // A rename alongside is a move, not a change.
    std::fs::rename(dir.path().join("other.pdf"), dir.path().join("tmp.pdf")).unwrap();

    let out = run(&home, dir.path(), &["--yes"]);
    fails(&out, 1, "1 changed or unreadable");
    let r = row(&out, "inv.pdf");
    assert_eq!(r["Action"], "changed");
    assert!(r["Note"].to_string().contains("was doc doc-"), "{r}");
    assert_eq!(row(&out, "tmp.pdf")["Action"], "synced");
    assert_eq!(uploads(&seen).len(), 2);

    let forgot = mark(&home, &dir.path().join("inv.pdf"), &["--forget"]);
    assert_eq!(json(&forgot)["items"][0]["Action"], "forgotten");
    ok(&run(&home, dir.path(), &["--yes"]));
    assert_eq!(uploads(&seen).len(), 3);
}

#[test]
fn max_caps_the_uploads_of_one_run() {
    let (root, seen) = mock_yuki();
    let home = home(&root);
    let dir = receipts(&[("a.pdf", "a"), ("b.pdf", "b"), ("c.pdf", "c")]);
    let out = run(&home, dir.path(), &["--yes", "--max", "2"]);
    ok(&out);
    assert_eq!(row(&out, "c.pdf")["Action"], "deferred");
    assert_eq!(uploads(&seen), ["a.pdf", "b.pdf"]);
    ok(&run(&home, dir.path(), &["--yes", "--max", "2"]));
    assert_eq!(uploads(&seen), ["a.pdf", "b.pdf", "c.pdf"]);
}

#[test]
fn an_http_refusal_stops_the_run_and_undoes_its_write_ahead() {
    let (root, seen) = mock_yuki();
    let home = home(&root);
    let dir = receipts(&[("a.pdf", "a"), ("b-denied.pdf", "b"), ("c.pdf", "c")]);
    let out = run(&home, dir.path(), &["--yes"]);
    fails(&out, 2, "API calls made: 3");
    assert_actions(
        &out,
        &[
            ("a.pdf", "uploaded"),
            ("b-denied.pdf", "not-attempted"),
            ("c.pdf", "not-attempted"),
        ],
    );
    assert_eq!(uploads(&seen), ["a.pdf", "b-denied.pdf"]);
    assert!(entry_for(dir.path(), "b-denied.pdf").is_none());
}

#[test]
fn an_auth_fault_stops_the_run_but_stays_pending() {
    let (root, seen) = mock_yuki();
    let home = home(&root);
    let dir = receipts(&[("b-expire.pdf", "b"), ("c.pdf", "c")]);
    let out = run(&home, dir.path(), &["--yes"]);
    fails(&out, 2, "Invalid session");
    assert_actions(
        &out,
        &[("b-expire.pdf", "pending"), ("c.pdf", "not-attempted")],
    );
    assert_eq!(uploads(&seen), ["b-expire.pdf"]);
    assert_eq!(
        entry_for(dir.path(), "b-expire.pdf").unwrap()["status"],
        "pending"
    );
}

#[test]
fn the_call_count_is_printed_when_authentication_fails() {
    let (root, _seen) = mock_yuki();
    let home = common::home_with_config(&root, "bad-key", "");
    let dir = receipts(&[("a.pdf", "a")]);
    fails(&run(&home, dir.path(), &["--yes"]), 2, "API calls made: 1");
}

#[test]
fn a_corrupt_state_file_is_refused_and_left_alone() {
    let (root, seen) = mock_yuki();
    let home = home(&root);
    let corrupt = "{\"version\": 1, \"files\": {";
    let dir = receipts(&[("a.pdf", "a"), (STATE, corrupt)]);
    for args in [
        &["--yes"][..],
        &["--dry-run"][..],
        &["--seed-from-yuki", "--yes"][..],
    ] {
        fails(&run(&home, dir.path(), args), 1, "refusing to overwrite");
    }
    assert_eq!(calls(&seen), 0);
    assert_eq!(
        std::fs::read_to_string(dir.path().join(STATE)).unwrap(),
        corrupt
    );
}

#[test]
fn the_path_must_be_the_sync_root() {
    let (root, seen) = mock_yuki();
    let home = home(&root);
    let dir = receipts(&[("2026/bol/a.pdf", "a"), ("2026/other/b.pdf", "b")]);
    ok(&run(&home, dir.path(), &["--yes"]));

    // A subdirectory of a synced directory is refused, pointing at the root.
    let sub = dir.path().join("2026/bol");
    let top = dir.path().canonicalize().unwrap();
    fails(
        &run(&home, &sub, &["--yes"]),
        1,
        &format!("run on {} instead", top.display()),
    );
    // mark finds the root from the file.
    let m = mark(&home, &sub.join("a.pdf"), &["--doc-id", "doc-1"]);
    assert_eq!(json(&m)["items"][0]["Path"], "2026/bol/a.pdf");

    // A state file further down makes the tree ambiguous.
    std::fs::write(dir.path().join("2026/other").join(STATE), "{}").unwrap();
    let nested = run(&home, dir.path(), &["--yes"]);
    fails(
        &nested,
        1,
        "holds other sync states (2026/other/.yuki-sync.json)",
    );
    assert_eq!(uploads(&seen).len(), 2);
}

#[test]
fn seeding_records_unique_name_matches_after_selecting_the_administration() {
    let (root, seen) = mock_yuki();
    let home = home(&root);
    let dir = receipts(&[
        ("2026/bol-com/2026-08-30_bol-com_111.pdf", "bol"),
        ("2026/vercel/2026-09-07_vercel_REF12345.pdf", "vercel"),
        ("2026/shop/scan.pdf", "scan"),
        ("2026/new/2026-09-30_acme_999.pdf", "new"),
        ("2026/a/dup.pdf", "dup a"),
        ("2026/b/dup.pdf", "dup b"),
        ("2026/x/taken.pdf", "taken x"),
        ("2026/y/taken.pdf", "taken y"),
        ("2026/cafe/Cafe\u{301}.pdf", "cafe"),
        ("only/c-garbled.pdf", "pending one"),
    ]);
    // A pending upload, which seeding resolves.
    let first = run(&home, dir.path(), &["--yes", "--exclude", "2026"]);
    fails(&first, 1, "1 newly pending");
    seen.lock().unwrap().clear();
    let x = dir.path().join("2026/x/taken.pdf");
    ok(&mark(&home, &x, &["--doc-id", "y-taken"]));

    let out = run(&home, dir.path(), &["--seed-from-yuki", "--yes"]);
    ok(&out);
    assert_actions(
        &out,
        &[
            ("2026/a/dup.pdf", "ambiguous"),
            ("2026/b/dup.pdf", "ambiguous"),
            ("2026/bol-com/2026-08-30_bol-com_111.pdf", "already-in-yuki"),
            ("2026/cafe/Cafe\u{301}.pdf", "already-in-yuki"),
            ("2026/new/2026-09-30_acme_999.pdf", "not-in-yuki"),
            ("2026/shop/scan.pdf", "ambiguous"),
            (
                "2026/vercel/2026-09-07_vercel_REF12345.pdf",
                "possible-match",
            ),
            ("2026/x/taken.pdf", "synced"),
            ("2026/y/taken.pdf", "ambiguous"),
            ("only/c-garbled.pdf", "already-in-yuki"),
        ],
    );
    let note = |path: &str| row(&out, path)["Note"].to_string();
    assert!(note("2026/a/dup.pdf").contains("2026/a/dup.pdf, 2026/b/dup.pdf"));
    assert!(note("2026/y/taken.pdf").contains("already recorded for 2026/x/taken.pdf"));
    assert!(note("2026/vercel/2026-09-07_vercel_REF12345.pdf").contains("y-vercel"));
    let err = stderr(&out);
    assert!(err.contains("by file name only"), "{err}");
    assert!(err.contains("API calls made: 6"), "{err}");
    // Paging ends on an empty page, advancing by what each page held.
    assert_eq!(
        summary(&seen),
        [
            "Authenticate",
            "SetCurrentDomain domain-1",
            "DocumentsInFolder 7 0",
            "DocumentsInFolder 1 0",
            "DocumentsInFolder 1 500",
            "DocumentsInFolder 1 508",
        ]
    );
    let bol = entry_for(dir.path(), "2026/bol-com/2026-08-30_bol-com_111.pdf").unwrap();
    assert_eq!(
        (&bol["document_id"], &bol["folder"]),
        (&"y-bol".into(), &"inkoop".into())
    );
    assert_eq!(state(dir.path())["files"].as_object().unwrap().len(), 4);

    // The real run then uploads everything else.
    ok(&run(&home, dir.path(), &["--yes"]));
    assert_eq!(uploads(&seen).len(), 6);
    assert!(!uploads(&seen).contains(&"c-garbled.pdf".to_string()));
}

#[test]
fn seeding_records_nothing_when_paging_looks_wrong() {
    let (root, seen) = mock_yuki();
    let home = home(&root);
    let dir = receipts(&[("old-3.pdf", "o")]);
    let args = ["--seed-from-yuki", "--yes", "--seed-folder", "verkoop"];
    fails(
        &run(&home, dir.path(), &args),
        1,
        "listed document fill-0 twice",
    );
    assert!(!dir.path().join(STATE).exists());
    assert_eq!(calls(&seen), 4);
}

#[test]
fn mark_records_a_file_by_hand_without_contacting_yuki() {
    let home = TempDir::new().unwrap();
    let dir = receipts(&[("2026/supabase/inv.pdf", "inv"), ("2026/other.pdf", "o")]);
    let file = dir.path().join("2026/supabase/inv.pdf");
    let other = dir.path().join("2026/other.pdf");

    fails(&mark(&home, &file, &["--doc-id", "d-1"]), 1, "pass --dir");
    let top = dir.path().to_str().unwrap();
    let out = mark(
        &home,
        &file,
        &["--doc-id", "d-1", "--dir", top, "--folder", "inkoop"],
    );
    assert_eq!(json(&out)["items"][0]["Action"], "recorded");
    let e = entry_for(dir.path(), "2026/supabase/inv.pdf").unwrap();
    assert_eq!(
        (&e["status"], &e["folder"]),
        (&"already-in-yuki".into(), &"inkoop".into())
    );

    let same = mark(&home, &file, &["--doc-id", "d-1"]);
    assert_eq!(json(&same)["items"][0]["Action"], "unchanged");
    fails(&mark(&home, &file, &["--doc-id", "d-2"]), 1, "--force");
    ok(&mark(&home, &file, &["--doc-id", "d-2", "--force"]));

    // One document cannot be recorded for two files without --force.
    let twice = mark(&home, &other, &["--doc-id", "d-2"]);
    fails(
        &twice,
        1,
        "document d-2 is already recorded for 2026/supabase/inv.pdf",
    );
    ok(&mark(&home, &other, &["--doc-id", "d-2", "--force"]));

    ok(&mark(
        &home,
        &other,
        &["--skip", "--note", "private", "--force"],
    ));
    let e = entry_for(dir.path(), "2026/other.pdf").unwrap();
    assert_eq!(
        (&e["status"], &e["note"]),
        (&"skipped".into(), &"private".into())
    );
    let forgot = mark(&home, &file, &["--forget"]);
    assert_eq!(json(&forgot)["items"][0]["Action"], "forgotten");
    let plan = run(&home, dir.path(), &["--dry-run"]);
    assert_actions(
        &plan,
        &[
            ("2026/other.pdf", "synced"),
            ("2026/supabase/inv.pdf", "would-upload"),
        ],
    );

    assert_eq!(mark(&home, &file, &[]).status.code(), Some(2));
    assert_eq!(
        mark(&home, &file, &["--skip", "--forget"]).status.code(),
        Some(2)
    );
}
