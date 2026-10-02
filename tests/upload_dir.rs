//! End-to-end: `upload dir` and `upload mark` against a local mock of Yuki.
//!
//! The mock answers Authenticate, SetCurrentDomain, UploadDocument and
//! DocumentsInFolder. What an upload gets depends on its file name:
//! - "broken": a generic SOAP fault (outcome unknown: stays pending),
//! - "expire": an invalid-session fault (an authentication error; stops the run),
//! - "garbled": HTTP 502 without a SOAP fault (stays pending),
//! - "hang": no answer at all, ever,
//! - anything else: a new document ID.
//!
//! The access key "bad-key" is refused at Authenticate.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod common;

use common::yuki;
use serde_json::Value;
use tempfile::TempDir;

const STATE: &str = ".yuki-sync.json";

fn envelope(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body>{body}</soap:Body></soap:Envelope>"#
    )
}

fn response(op: &str, inner: &str) -> String {
    envelope(&format!(
        r#"<{op}Response xmlns="http://www.theyukicompany.com/"><{op}Result>{inner}</{op}Result></{op}Response>"#
    ))
}

fn fault(message: &str) -> String {
    envelope(&format!(
        "<soap:Fault><faultcode>soap:Server</faultcode><faultstring>{message}</faultstring></soap:Fault>"
    ))
}

fn document(id: &str, date: &str, contact: &str, file: &str) -> String {
    format!(
        "<Document ID=\"{id}\"><Subject>Factuur</Subject><DocumentDate>{date}T00:00:00</DocumentDate>\
         <Amount>1.00</Amount><ContactName>{contact}</ContactName><FileName>{file}</FileName></Document>"
    )
}

/// The raw (still escaped) text of a request parameter.
fn param<'a>(body: &'a str, name: &str) -> &'a str {
    body.split(&format!("<yuki:{name}>"))
        .nth(1)
        .and_then(|rest| rest.split("</yuki:").next())
        .unwrap_or_default()
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

/// What the mock saw: each SOAP action, with the uploaded file name or the
/// folder and offset when it has one.
type Log = Arc<Mutex<Vec<String>>>;

fn serve(mut stream: TcpStream, log: &Log, uploads: &AtomicUsize) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok();
    let (mut action, mut length) = (String::new(), 0usize);
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
            break;
        }
        let lower = header.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        } else if lower.starts_with("soapaction:") {
            action = header["soapaction:".len()..]
                .trim()
                .trim_matches('"')
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok();
    let body = String::from_utf8_lossy(&body);
    let (status, reply, entry) = match action.as_str() {
        "Authenticate" if param(&body, "accessKey") == "bad-key" => {
            (500, fault("Invalid access key"), action.clone())
        }
        "Authenticate" => (200, response("Authenticate", "session-1"), action.clone()),
        "UploadDocument" => {
            let name = param(&body, "fileName").to_string();
            let entry = format!("UploadDocument {name}");
            if name.contains("hang") {
                log.lock().expect("log").push(entry);
                std::thread::sleep(Duration::from_secs(600));
                return;
            } else if name.contains("broken") {
                (500, fault("Server was unable to process request."), entry)
            } else if name.contains("expire") {
                (500, fault("Invalid session ID"), entry)
            } else if name.contains("garbled") {
                (502, "Bad gateway".to_string(), entry)
            } else {
                let n = uploads.fetch_add(1, Ordering::SeqCst) + 1;
                (200, response("UploadDocument", &format!("doc-{n}")), entry)
            }
        }
        "DocumentsInFolder" => {
            let folder = param(&body, "folderID");
            let start = param(&body, "startRecord");
            (
                200,
                documents_in_folder(folder, start),
                format!("DocumentsInFolder {folder} {start}"),
            )
        }
        "SetCurrentDomain" => (
            200,
            response("SetCurrentDomain", ""),
            format!("SetCurrentDomain {}", param(&body, "domainID")),
        ),
        _ => (200, response(&action, ""), action.clone()),
    };
    log.lock().expect("log").push(entry);
    write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Type: text/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    )
    .ok();
}

fn mock_yuki() -> (String, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let root = format!("http://{}/ws", listener.local_addr().expect("addr"));
    let seen: Log = Arc::default();
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        let uploads = Arc::new(AtomicUsize::new(0));
        for stream in listener.incoming().flatten() {
            let (log, uploads) = (Arc::clone(&log), Arc::clone(&uploads));
            std::thread::spawn(move || serve(stream, &log, &uploads));
        }
    });
    (root, seen)
}

fn home_with_key(root: &str, key: &str) -> TempDir {
    let home = TempDir::new().expect("temp home");
    let dir = home.path().join(".config/yuki");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(
        dir.join("config.toml"),
        format!(
            r#"api_key = "{key}"
default_admin = "example"
region = "be"

[administrations.example]
domain_id = "domain-1"
admin_id = "admin-1"
name = "Example BV"
base_url = "{root}"
"#
        ),
    )
    .expect("write config");
    home
}

fn home_with_config(root: &str) -> TempDir {
    home_with_key(root, "test-key")
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

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "JSON stdout ({e}): {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// Action per path from the output rows.
fn actions(output: &Output) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = json(output)["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|r| {
            (
                r["Path"].as_str().unwrap().to_string(),
                r["Action"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    rows.sort();
    rows
}

/// The output row for `path`.
fn row(output: &Output, path: &str) -> Value {
    json(output)["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["Path"] == path)
        .unwrap_or_else(|| panic!("no row for {path}"))
        .clone()
}

fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = expected
        .iter()
        .map(|(p, a)| ((*p).to_string(), (*a).to_string()))
        .collect();
    v.sort();
    v
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

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn uploads(seen: &Log) -> Vec<String> {
    seen.lock()
        .unwrap()
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
    let home = home_with_config(&root);
    let dir = receipts(&[
        ("2026/a.pdf", "a"),
        ("2026/b.PNG", "b"),
        ("_to_delete/old.pdf", "old"),
    ]);
    let out = run(&home, dir.path(), &["--dry-run"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        actions(&out),
        pairs(&[
            ("2026/a.pdf", "would-upload"),
            ("2026/b.PNG", "would-upload"),
            ("_to_delete/old.pdf", "excluded"),
        ])
    );
    assert!(stderr(&out).contains("API calls made: 0"));
    let seed = run(&home, dir.path(), &["--dry-run", "--seed-from-yuki"]);
    assert_eq!(row(&seed, "2026/a.pdf")["Action"], "would-seed");
    assert_eq!(calls(&seen), 0, "{:?}", seen.lock().unwrap());
    assert!(!dir.path().join(STATE).exists());
}

#[test]
fn a_non_interactive_run_without_yes_refuses_before_any_call() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[("a.pdf", "a")]);
    for args in [&[][..], &["--seed-from-yuki"][..]] {
        let out = run(&home, dir.path(), args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let err = stderr(&out);
        assert!(err.contains("\"kind\":\"confirmation_required\""), "{err}");
    }
    assert_eq!(calls(&seen), 0);
    assert!(!dir.path().join(STATE).exists());
}

#[test]
fn uploads_new_files_and_leaves_uncertain_ones_pending() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[
        ("2026/a.pdf", "a"),
        ("2026/b-broken.pdf", "b"),
        ("2026/c-garbled.pdf", "c"),
        ("2026/d.jpeg", "d"),
        ("2026/notes.txt", "not a receipt"),
        (".hidden/h.pdf", "h"),
    ]);
    let out = run(&home, dir.path(), &["--yes"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        actions(&out),
        pairs(&[
            ("2026/a.pdf", "uploaded"),
            ("2026/b-broken.pdf", "pending"),
            ("2026/c-garbled.pdf", "pending"),
            ("2026/d.jpeg", "uploaded"),
            (".hidden/h.pdf", "excluded"),
        ])
    );
    let err = stderr(&out);
    assert!(err.contains("API calls made: 5"), "{err}");
    assert!(err.contains("2 newly pending"), "{err}");
    let a = entry_for(dir.path(), "2026/a.pdf").unwrap();
    assert_eq!(a["status"], "uploaded");
    assert_eq!(a["document_id"], "doc-1");
    assert_eq!(a["folder"], "uitzoeken");
    assert!(a["uploaded_at"].as_str().unwrap().ends_with('Z'));
    let b = entry_for(dir.path(), "2026/b-broken.pdf").unwrap();
    assert_eq!(b["status"], "pending");
    assert!(b["error"].as_str().unwrap().contains("unable to process"));
    let note = row(&out, "2026/b-broken.pdf")["Note"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(note.contains("yuki upload mark '"), "{note}");
    // Keyed by the sha256 of the content.
    assert!(
        state(dir.path())["files"]
            .get("ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb")
            .is_some()
    );

    // Pending files are never retried; the run still needs attention.
    let again = run(&home, dir.path(), &["--yes"]);
    assert_eq!(again.status.code(), Some(1));
    assert!(stderr(&again).contains("2 pending"));
    assert_eq!(uploads(&seen).len(), 4);
    assert!(stderr(&again).contains("API calls made: 0"));

    // Resolved by hand: one is in Yuki, the other goes up again.
    let c = dir.path().join("2026/c-garbled.pdf");
    assert!(mark(&home, &c, &["--doc-id", "y-1"]).status.success());
    let b = dir.path().join("2026/b-broken.pdf");
    assert!(mark(&home, &b, &["--forget"]).status.success());
    let plan = run(&home, dir.path(), &["--dry-run"]);
    assert_eq!(row(&plan, "2026/c-garbled.pdf")["Action"], "synced");
    assert_eq!(row(&plan, "2026/b-broken.pdf")["Action"], "would-upload");
}

#[test]
fn file_names_are_xml_escaped_in_the_request() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[("Tom & Jerry <x>.pdf", "tj")]);
    let out = run(&home, dir.path(), &["--yes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(uploads(&seen), ["Tom &amp; Jerry &lt;x&gt;.pdf"]);
}

/// Wait until the mock has seen `what`.
fn wait_for(seen: &Log, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !seen.lock().unwrap().iter().any(|a| a == what) {
        assert!(Instant::now() < deadline, "mock never saw {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_killed_run_leaves_its_upload_pending_and_releases_the_lock() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
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
    let second = run(&home, dir.path(), &["--yes"]);
    assert_eq!(second.status.code(), Some(1));
    assert!(
        stderr(&second).contains("another yuki run"),
        "{}",
        stderr(&second)
    );

    child.kill().expect("kill yuki");
    child.wait().expect("reap yuki");
    // The kernel released the lock; the pending upload is not retried.
    let after = run(&home, dir.path(), &["--yes"]);
    assert_eq!(after.status.code(), Some(1), "{}", stderr(&after));
    assert_eq!(row(&after, "a-hang.pdf")["Action"], "pending");
    assert_eq!(uploads(&seen), ["a-hang.pdf"]);
}

#[test]
fn the_first_uploads_failing_alike_stop_the_run() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[
        ("a-garbled.pdf", "a"),
        ("b-garbled.pdf", "b"),
        ("c-garbled.pdf", "c"),
        ("d.pdf", "d"),
    ]);
    let out = run(&home, dir.path(), &["--yes"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("first 3 uploads all failed"),
        "{}",
        stderr(&out)
    );
    assert_eq!(row(&out, "d.pdf")["Action"], "not-attempted");
    assert_eq!(uploads(&seen).len(), 3);
}

#[test]
fn a_renamed_or_moved_file_is_not_uploaded_again() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[("in/receipt.pdf", "same bytes")]);
    assert!(run(&home, dir.path(), &["--yes"]).status.success());
    std::fs::create_dir_all(dir.path().join("2026/vendor")).unwrap();
    std::fs::rename(
        dir.path().join("in/receipt.pdf"),
        dir.path().join("2026/vendor/renamed.pdf"),
    )
    .unwrap();
    std::fs::write(dir.path().join("2026/copy.pdf"), "same bytes").unwrap();
    let out = run(&home, dir.path(), &["--yes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(uploads(&seen).len(), 1, "nothing uploaded again");
    assert_eq!(
        actions(&out),
        pairs(&[
            ("2026/copy.pdf", "synced"),
            ("2026/vendor/renamed.pdf", "duplicate"),
        ])
    );
    let st = state(dir.path());
    let files = st["files"].as_object().unwrap();
    assert_eq!(files.values().next().unwrap()["path"], "2026/copy.pdf");
}

#[test]
fn a_changed_file_is_not_uploaded_until_resolved() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[("inv.pdf", "version 1"), ("other.pdf", "o")]);
    assert!(run(&home, dir.path(), &["--yes"]).status.success());
    std::fs::write(dir.path().join("inv.pdf"), "version 2").unwrap();
    // Swapping two recorded files is a move, not a change.
    let (a, b) = (dir.path().join("other.pdf"), dir.path().join("tmp.pdf"));
    std::fs::rename(&a, &b).unwrap();

    let out = run(&home, dir.path(), &["--yes"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let r = row(&out, "inv.pdf");
    assert_eq!(r["Action"], "changed");
    assert!(r["Note"].as_str().unwrap().contains("was doc doc-"), "{r}");
    assert_eq!(row(&out, "tmp.pdf")["Action"], "synced");
    assert_eq!(uploads(&seen).len(), 2);

    let forgot = mark(&home, &dir.path().join("inv.pdf"), &["--forget"]);
    assert_eq!(json(&forgot)["items"][0]["Action"], "forgotten");
    assert!(run(&home, dir.path(), &["--yes"]).status.success());
    assert_eq!(uploads(&seen).len(), 3);
}

#[test]
fn excludes_are_case_insensitive_and_added_to_the_defaults() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[
        ("2025/amazon/deep/a.pdf", "a"),
        ("2026/bol/b.pdf", "b"),
        ("2026/bol/scan.PNG", "s"),
        ("_TO_DELETE/x.pdf", "x"),
    ]);
    let out = run(
        &home,
        dir.path(),
        &["--dry-run", "--exclude", "2025", "--exclude", "*.png"],
    );
    assert_eq!(
        actions(&out),
        pairs(&[
            ("2025/amazon/deep/a.pdf", "excluded"),
            ("2026/bol/b.pdf", "would-upload"),
            ("2026/bol/scan.PNG", "excluded"),
            ("_TO_DELETE/x.pdf", "excluded"),
        ])
    );
    let bad = run(&home, dir.path(), &["--dry-run", "--exclude", "["]);
    assert!(stderr(&bad).contains("invalid --exclude"));
    assert_eq!(calls(&seen), 0);
}

#[test]
fn max_caps_the_uploads_of_one_run() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[("a.pdf", "a"), ("b.pdf", "b"), ("c.pdf", "c")]);
    let out = run(&home, dir.path(), &["--yes", "--max", "2"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(row(&out, "c.pdf")["Action"], "deferred");
    assert_eq!(uploads(&seen), ["a.pdf", "b.pdf"]);
    assert!(
        run(&home, dir.path(), &["--yes", "--max", "2"])
            .status
            .success()
    );
    assert_eq!(uploads(&seen), ["a.pdf", "b.pdf", "c.pdf"]);
}

#[test]
fn an_authentication_error_stops_the_run_and_undoes_its_write_ahead() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[("a.pdf", "a"), ("b-expire.pdf", "b"), ("c.pdf", "c")]);
    let out = run(&home, dir.path(), &["--yes"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert_eq!(
        actions(&out),
        pairs(&[
            ("a.pdf", "uploaded"),
            ("b-expire.pdf", "not-attempted"),
            ("c.pdf", "not-attempted"),
        ])
    );
    assert_eq!(uploads(&seen), ["a.pdf", "b-expire.pdf"]);
    assert!(entry_for(dir.path(), "b-expire.pdf").is_none());
    assert!(stderr(&out).contains("API calls made: 3"));
}

#[test]
fn the_call_count_is_printed_when_authentication_fails() {
    let (root, _seen) = mock_yuki();
    let home = home_with_key(&root, "bad-key");
    let dir = receipts(&[("a.pdf", "a")]);
    let out = run(&home, dir.path(), &["--yes"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("API calls made: 1"));
}

#[test]
fn a_corrupt_state_file_is_refused_and_left_alone() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[("a.pdf", "a"), (STATE, "{\"version\": 1, \"files\": {")]);
    for args in [
        &["--yes"][..],
        &["--dry-run"][..],
        &["--seed-from-yuki", "--yes"][..],
    ] {
        let out = run(&home, dir.path(), args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(
            stderr(&out).contains("refusing to overwrite"),
            "{}",
            stderr(&out)
        );
    }
    assert_eq!(calls(&seen), 0);
    assert_eq!(
        std::fs::read_to_string(dir.path().join(STATE)).unwrap(),
        "{\"version\": 1, \"files\": {"
    );
}

#[test]
fn the_path_must_be_the_sync_root() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[("2026/bol/a.pdf", "a"), ("2026/other/b.pdf", "b")]);
    assert!(run(&home, dir.path(), &["--yes"]).status.success());

    // A subdirectory of a synced directory is refused, pointing at the root.
    let sub = dir.path().join("2026/bol");
    let out = run(&home, &sub, &["--yes"]);
    assert_eq!(out.status.code(), Some(1));
    let top = dir.path().canonicalize().unwrap();
    assert!(
        stderr(&out).contains(&format!("run on {} instead", top.display())),
        "{}",
        stderr(&out)
    );
    // mark finds the root from the file.
    let m = mark(&home, &sub.join("a.pdf"), &["--doc-id", "doc-1"]);
    assert_eq!(json(&m)["items"][0]["Path"], "2026/bol/a.pdf");

    // A state file further down makes the tree ambiguous.
    std::fs::write(dir.path().join("2026/other").join(STATE), "{}").unwrap();
    let nested = run(&home, dir.path(), &["--yes"]);
    assert_eq!(nested.status.code(), Some(1));
    assert!(stderr(&nested).contains("holds other sync states (2026/other/.yuki-sync.json)"));
    assert_eq!(uploads(&seen).len(), 2);
}

#[test]
fn seeding_records_unique_name_matches_after_selecting_the_administration() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
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
    ]);
    // A pending upload, which seeding resolves.
    std::fs::create_dir(dir.path().join("only")).unwrap();
    std::fs::write(dir.path().join("only/c-garbled.pdf"), "pending one").unwrap();
    let first = run(&home, dir.path(), &["--yes", "--exclude", "2026"]);
    assert_eq!(first.status.code(), Some(1), "{}", stderr(&first));
    assert_eq!(row(&first, "only/c-garbled.pdf")["Action"], "pending");
    seen.lock().unwrap().clear();
    let top = dir.path().to_str().unwrap();
    let x = dir.path().join("2026/x/taken.pdf");
    assert!(
        mark(&home, &x, &["--doc-id", "y-taken", "--dir", top])
            .status
            .success()
    );

    let out = run(&home, dir.path(), &["--seed-from-yuki", "--yes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        actions(&out),
        pairs(&[
            ("2026/a/dup.pdf", "ambiguous"),
            ("2026/b/dup.pdf", "ambiguous"),
            ("2026/bol-com/2026-08-30_bol-com_111.pdf", "already-in-yuki"),
            ("2026/cafe/Cafe\u{301}.pdf", "already-in-yuki"),
            ("2026/new/2026-09-30_acme_999.pdf", "not-in-yuki"),
            ("2026/shop/scan.pdf", "ambiguous"),
            (
                "2026/vercel/2026-09-07_vercel_REF12345.pdf",
                "possible-match"
            ),
            ("2026/x/taken.pdf", "synced"),
            ("2026/y/taken.pdf", "ambiguous"),
            ("only/c-garbled.pdf", "already-in-yuki"),
        ])
    );
    let note = |path: &str| row(&out, path)["Note"].as_str().unwrap().to_string();
    assert!(note("2026/a/dup.pdf").contains("2026/a/dup.pdf, 2026/b/dup.pdf"));
    assert!(note("2026/y/taken.pdf").contains("already recorded for 2026/x/taken.pdf"));
    assert!(note("2026/vercel/2026-09-07_vercel_REF12345.pdf").contains("y-vercel"));
    let err = stderr(&out);
    assert!(err.contains("by file name only"), "{err}");
    assert!(err.contains("API calls made: 5"), "{err}");
    assert_eq!(
        *seen.lock().unwrap(),
        [
            "Authenticate",
            "SetCurrentDomain domain-1",
            "DocumentsInFolder 7 0",
            "DocumentsInFolder 1 0",
            "DocumentsInFolder 1 500",
        ]
    );
    let bol = entry_for(dir.path(), "2026/bol-com/2026-08-30_bol-com_111.pdf").unwrap();
    assert_eq!(bol["document_id"], "y-bol");
    assert_eq!(bol["folder"], "inkoop");
    assert_eq!(state(dir.path())["files"].as_object().unwrap().len(), 4);

    // The real run then uploads everything else.
    assert!(run(&home, dir.path(), &["--yes"]).status.success());
    assert_eq!(uploads(&seen).len(), 6);
    assert!(!uploads(&seen).contains(&"c-garbled.pdf".to_string()));
}

#[test]
fn seeding_records_nothing_when_paging_looks_wrong() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[("old-3.pdf", "o")]);
    let out = run(
        &home,
        dir.path(),
        &["--seed-from-yuki", "--yes", "--seed-folder", "verkoop"],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("listed document fill-0 twice"),
        "{}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("nothing recorded"));
    assert!(!dir.path().join(STATE).exists());
    assert_eq!(calls(&seen), 4);
}

#[test]
fn mark_records_a_file_by_hand_without_contacting_yuki() {
    let home = TempDir::new().unwrap();
    let dir = receipts(&[("2026/supabase/inv.pdf", "inv"), ("2026/other.pdf", "o")]);
    let file = dir.path().join("2026/supabase/inv.pdf");
    let other = dir.path().join("2026/other.pdf");

    let out = mark(&home, &file, &["--doc-id", "d-1"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("pass --dir"), "{}", stderr(&out));

    let top = dir.path().to_str().unwrap();
    let out = mark(
        &home,
        &file,
        &["--doc-id", "d-1", "--dir", top, "--folder", "inkoop"],
    );
    assert_eq!(json(&out)["items"][0]["Action"], "recorded");
    let e = entry_for(dir.path(), "2026/supabase/inv.pdf").unwrap();
    assert_eq!(
        (e["status"].as_str(), e["folder"].as_str()),
        (Some("already-in-yuki"), Some("inkoop"))
    );

    let same = mark(&home, &file, &["--doc-id", "d-1"]);
    assert_eq!(json(&same)["items"][0]["Action"], "unchanged");
    let replace = mark(&home, &file, &["--doc-id", "d-2"]);
    assert!(stderr(&replace).contains("--force"));
    assert!(
        mark(&home, &file, &["--doc-id", "d-2", "--force"])
            .status
            .success()
    );

    // One document cannot be recorded for two files without --force.
    let twice = mark(&home, &other, &["--doc-id", "d-2"]);
    assert_eq!(twice.status.code(), Some(1));
    assert!(
        stderr(&twice).contains("document d-2 is already recorded for 2026/supabase/inv.pdf"),
        "{}",
        stderr(&twice)
    );
    assert!(
        mark(&home, &other, &["--doc-id", "d-2", "--force"])
            .status
            .success()
    );

    assert!(
        mark(&home, &other, &["--skip", "--note", "private", "--force"])
            .status
            .success()
    );
    let e = entry_for(dir.path(), "2026/other.pdf").unwrap();
    assert_eq!(
        (e["status"].as_str(), e["note"].as_str()),
        (Some("skipped"), Some("private"))
    );
    let forgot = mark(&home, &file, &["--forget"]);
    assert_eq!(json(&forgot)["items"][0]["Action"], "forgotten");
    let plan = run(&home, dir.path(), &["--dry-run"]);
    assert_eq!(
        actions(&plan),
        pairs(&[
            ("2026/other.pdf", "synced"),
            ("2026/supabase/inv.pdf", "would-upload")
        ])
    );

    assert_eq!(mark(&home, &file, &[]).status.code(), Some(2));
    assert_eq!(
        mark(&home, &file, &["--skip", "--forget"]).status.code(),
        Some(2)
    );
}
