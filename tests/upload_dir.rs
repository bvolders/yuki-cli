//! End-to-end: `upload dir` and `upload mark` against a local mock of Yuki.
//!
//! The mock answers Authenticate, SetCurrentDomain, UploadDocument and
//! DocumentsInFolder. What an upload gets depends on its file name:
//! - "broken": a SOAP fault (Yuki rejected it; retried),
//! - "expire": an invalid-session fault (an authentication error; stops the run),
//! - "garbled": HTTP 502 without a SOAP fault (result unknown),
//! - "junk": HTTP 200 without a result element (result unknown),
//! - anything else: a new document ID.
//!
//! The access key "bad-key" is refused at Authenticate.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Output;
use std::sync::{Arc, Mutex};

mod common;

use common::yuki;
use serde_json::Value;
use tempfile::TempDir;

const STATE: &str = ".yuki-sync.json";
const LOCK: &str = ".yuki-sync.lock";

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

fn mock_yuki() -> (String, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let root = format!("http://{}/ws", listener.local_addr().expect("addr"));
    let seen: Log = Arc::default();
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        let mut uploads = 0;
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
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
                    if name.contains("broken") {
                        (500, fault("Server was unable to process request."), entry)
                    } else if name.contains("expire") {
                        (500, fault("Invalid session ID"), entry)
                    } else if name.contains("garbled") {
                        (502, "Bad gateway".to_string(), entry)
                    } else if name.contains("junk") {
                        (200, envelope("<Nothing/>"), entry)
                    } else {
                        uploads += 1;
                        let id = format!("doc-{uploads}");
                        (200, response("UploadDocument", &id), entry)
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
                "SetCurrentDomain" => {
                    let domain = param(&body, "domainID").to_string();
                    (
                        200,
                        response("SetCurrentDomain", ""),
                        format!("SetCurrentDomain {domain}"),
                    )
                }
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
    let err = stderr(&out);
    assert!(err.contains("2 to upload (2 new, 0 to retry)"), "{err}");
    assert!(err.contains("API calls made: 0"), "{err}");

    let seed = run(&home, dir.path(), &["--dry-run", "--seed-from-yuki"]);
    assert!(seed.status.success(), "{}", stderr(&seed));
    assert_eq!(row(&seed, "2026/a.pdf")["Action"], "would-seed");

    assert_eq!(calls(&seen), 0, "{:?}", seen.lock().unwrap());
    assert!(!dir.path().join(STATE).exists());
    assert!(!dir.path().join(LOCK).exists());
}

#[test]
fn a_dry_run_needs_no_config() {
    let home = TempDir::new().unwrap();
    let dir = receipts(&[("a.pdf", "a")]);
    let out = run(&home, dir.path(), &["--dry-run"]);
    assert!(out.status.success(), "{}", stderr(&out));
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
        assert!(err.contains("--yes"), "{err}");
    }
    assert_eq!(calls(&seen), 0);
    assert!(!dir.path().join(STATE).exists());
    assert!(!dir.path().join(LOCK).exists());
}

#[test]
fn uploads_new_files_and_retries_only_the_rejected_one() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[
        ("2026/a.pdf", "a"),
        ("2026/b-broken.pdf", "b"),
        ("2026/c.jpeg", "c"),
        ("2026/notes.txt", "not a receipt"),
        (".hidden/h.pdf", "h"),
        ("_to_delete/old.pdf", "old"),
    ]);
    let out = run(&home, dir.path(), &["--yes"]);
    // One upload failed: the run carries on, then exits non-zero.
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        actions(&out),
        pairs(&[
            ("2026/a.pdf", "uploaded"),
            ("2026/b-broken.pdf", "failed"),
            ("2026/c.jpeg", "uploaded"),
            (".hidden/h.pdf", "excluded"),
            ("_to_delete/old.pdf", "excluded"),
        ])
    );
    assert_eq!(uploads(&seen), ["a.pdf", "b-broken.pdf", "c.jpeg"]);
    let err = stderr(&out);
    assert!(err.contains("API calls made: 4"), "{err}");
    assert!(err.contains("1 of 3 uploads failed"), "{err}");

    let st = state(dir.path());
    assert_eq!(st["version"], 1);
    assert_eq!(st["hash"], "sha256");
    let a = entry_for(dir.path(), "2026/a.pdf").unwrap();
    assert_eq!(a["status"], "uploaded");
    assert_eq!(a["document_id"], "doc-1");
    assert_eq!(a["folder"], "uitzoeken");
    assert_eq!(a["size"], 1);
    assert!(a["uploaded_at"].as_str().unwrap().ends_with('Z'));
    let b = entry_for(dir.path(), "2026/b-broken.pdf").unwrap();
    assert_eq!(b["status"], "failed");
    assert_eq!(b["attempts"], 1);
    assert!(
        b["error"].as_str().unwrap().contains("unable to process"),
        "{b}"
    );
    // Keyed by the sha256 of the content.
    assert!(
        st["files"]
            .get("ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb")
            .is_some(),
        "{st}"
    );
    assert!(!dir.path().join(LOCK).exists(), "the lock is released");

    // The second run retries only the failed file.
    let again = run(&home, dir.path(), &["--yes"]);
    assert_eq!(again.status.code(), Some(1));
    assert_eq!(
        uploads(&seen),
        ["a.pdf", "b-broken.pdf", "c.jpeg", "b-broken.pdf"]
    );

    // Once it is gone there is nothing to do, and Yuki is not contacted.
    std::fs::remove_file(dir.path().join("2026/b-broken.pdf")).unwrap();
    let before = calls(&seen);
    let done = run(&home, dir.path(), &["--yes"]);
    assert!(done.status.success(), "{}", stderr(&done));
    assert!(stderr(&done).contains("API calls made: 0"));
    assert_eq!(calls(&seen), before);
}

#[test]
fn file_names_are_xml_escaped_in_the_request() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[("Tom & Jerry <x>.pdf", "tj")]);
    let out = run(&home, dir.path(), &["--yes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(uploads(&seen), ["Tom &amp; Jerry &lt;x&gt;.pdf"]);
    assert_eq!(row(&out, "Tom & Jerry <x>.pdf")["Action"], "uploaded");
}

#[test]
fn retries_queue_after_new_files_and_stop_after_three_attempts() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[("b-broken.pdf", "b")]);
    assert_eq!(run(&home, dir.path(), &["--yes"]).status.code(), Some(1));

    // A retry waits behind new files, whatever their names.
    std::fs::write(dir.path().join("z-new.pdf"), "z").unwrap();
    let out = run(&home, dir.path(), &["--yes", "--max", "1"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(uploads(&seen), ["b-broken.pdf", "z-new.pdf"]);
    assert_eq!(row(&out, "b-broken.pdf")["Action"], "deferred");

    // Attempts two and three, then it is no longer retried.
    for _ in 0..2 {
        assert_eq!(run(&home, dir.path(), &["--yes"]).status.code(), Some(1));
    }
    assert_eq!(
        entry_for(dir.path(), "b-broken.pdf").unwrap()["attempts"],
        3
    );
    let before = uploads(&seen).len();
    let out = run(&home, dir.path(), &["--yes"]);
    assert_eq!(out.status.code(), Some(1), "still needs attention");
    assert_eq!(uploads(&seen).len(), before, "not retried a fourth time");
    let r = row(&out, "b-broken.pdf");
    assert_eq!(r["Action"], "failed");
    assert!(r["Note"].as_str().unwrap().contains("--forget"), "{r}");
    assert!(stderr(&out).contains("failed too often to retry"));

    // --forget puts it back in the queue.
    assert!(
        mark(&home, &dir.path().join("b-broken.pdf"), &["--forget"])
            .status
            .success()
    );
    let _ = run(&home, dir.path(), &["--yes"]);
    assert_eq!(uploads(&seen).len(), before + 1);
}

#[test]
fn an_uncertain_upload_is_recorded_as_unknown_and_never_retried() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[("c-garbled.pdf", "c"), ("d-junk.pdf", "d"), ("e.pdf", "e")]);
    let out = run(&home, dir.path(), &["--yes"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        actions(&out),
        pairs(&[
            ("c-garbled.pdf", "unknown"),
            ("d-junk.pdf", "unknown"),
            ("e.pdf", "uploaded"),
        ])
    );
    for rel in ["c-garbled.pdf", "d-junk.pdf"] {
        assert_eq!(entry_for(dir.path(), rel).unwrap()["status"], "unknown");
    }
    assert!(stderr(&out).contains("2 with an unknown upload result"));

    // The next run uploads nothing, shows them, and still exits 1.
    let again = run(&home, dir.path(), &["--yes"]);
    assert_eq!(again.status.code(), Some(1));
    assert_eq!(uploads(&seen).len(), 3);
    assert_eq!(row(&again, "c-garbled.pdf")["Action"], "unknown");
    let plan = run(&home, dir.path(), &["--dry-run"]);
    assert!(stderr(&plan).contains("2 with an unknown upload result"));

    // Resolved by hand: one is in Yuki, the other is to be uploaded again.
    assert!(
        mark(
            &home,
            &dir.path().join("c-garbled.pdf"),
            &["--doc-id", "y-1"]
        )
        .status
        .success()
    );
    assert!(
        mark(&home, &dir.path().join("d-junk.pdf"), &["--forget"])
            .status
            .success()
    );
    let plan = run(&home, dir.path(), &["--dry-run"]);
    assert_eq!(
        actions(&plan),
        pairs(&[
            ("c-garbled.pdf", "synced"),
            ("d-junk.pdf", "would-upload"),
            ("e.pdf", "synced"),
        ])
    );
}

#[test]
fn a_renamed_or_moved_file_is_not_uploaded_again() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[("in/receipt.pdf", "same bytes")]);
    assert!(run(&home, dir.path(), &["--yes"]).status.success());
    assert_eq!(uploads(&seen).len(), 1);

    std::fs::create_dir_all(dir.path().join("2026/vendor")).unwrap();
    std::fs::rename(
        dir.path().join("in/receipt.pdf"),
        dir.path().join("2026/vendor/renamed.pdf"),
    )
    .unwrap();
    // A second copy under another name is the same content too.
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
    // The record follows the file to its new path (the first one by name).
    let st = state(dir.path());
    let files = st["files"].as_object().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files.values().next().unwrap()["path"], "2026/copy.pdf");
}

#[test]
fn a_changed_file_is_not_uploaded_until_resolved() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[("inv.pdf", "version 1")]);
    assert!(run(&home, dir.path(), &["--yes"]).status.success());
    std::fs::write(dir.path().join("inv.pdf"), "version 2").unwrap();

    let out = run(&home, dir.path(), &["--yes"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let r = row(&out, "inv.pdf");
    assert_eq!(r["Action"], "changed");
    assert_eq!(r["Doc ID"], "doc-1");
    assert!(r["Note"].as_str().unwrap().contains("was doc doc-1"), "{r}");
    assert_eq!(uploads(&seen).len(), 1);

    // Forgetting the old record lets the new version go up.
    let forgot = mark(&home, &dir.path().join("inv.pdf"), &["--forget"]);
    assert!(forgot.status.success(), "{}", stderr(&forgot));
    assert_eq!(json(&forgot)["items"][0]["Action"], "forgotten");
    let out = run(&home, dir.path(), &["--yes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(uploads(&seen).len(), 2);
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
    assert!(out.status.success(), "{}", stderr(&out));
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
    assert_eq!(bad.status.code(), Some(1));
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
    assert_eq!(
        actions(&out),
        pairs(&[
            ("a.pdf", "uploaded"),
            ("b.pdf", "uploaded"),
            ("c.pdf", "deferred")
        ])
    );
    assert_eq!(uploads(&seen), ["a.pdf", "b.pdf"]);
    let next = run(&home, dir.path(), &["--yes", "--max", "2"]);
    assert!(next.status.success());
    assert_eq!(uploads(&seen), ["a.pdf", "b.pdf", "c.pdf"]);
}

#[test]
fn an_authentication_error_stops_the_run() {
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
    let files = state(dir.path())["files"].as_object().unwrap().len();
    assert_eq!(files, 1, "only the upload that happened is recorded");
    assert!(stderr(&out).contains("API calls made: 3"));
}

#[test]
fn the_call_count_is_printed_when_authentication_fails() {
    let (root, _seen) = mock_yuki();
    let home = home_with_key(&root, "bad-key");
    let dir = receipts(&[("a.pdf", "a")]);
    let out = run(&home, dir.path(), &["--yes"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("API calls made: 1"),
        "{}",
        stderr(&out)
    );
}

#[cfg(unix)]
#[test]
fn an_unreadable_file_is_an_error_row_and_the_rest_still_go_up() {
    use std::os::unix::fs::PermissionsExt;
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[("a.pdf", "a"), ("locked.pdf", "l")]);
    let locked = dir.path().join("locked.pdf");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&locked).is_ok() {
        return; // running as root: nothing is unreadable
    }
    let out = run(&home, dir.path(), &["--yes"]);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        actions(&out),
        pairs(&[("a.pdf", "uploaded"), ("locked.pdf", "error")])
    );
    assert_eq!(uploads(&seen), ["a.pdf"]);
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
            stderr(&out).contains("not a valid sync state"),
            "{}",
            stderr(&out)
        );
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
fn a_held_lock_refuses_writes_with_its_age() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[("a.pdf", "a")]);
    std::fs::write(dir.path().join(LOCK), "{\"pid\": 4242}").unwrap();
    for out in [
        run(&home, dir.path(), &["--yes"]),
        run(&home, dir.path(), &["--seed-from-yuki", "--yes"]),
        mark(
            &home,
            &dir.path().join("a.pdf"),
            &["--skip", "--dir", dir.path().to_str().unwrap()],
        ),
    ] {
        assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
        let err = stderr(&out);
        assert!(
            err.contains("locked by another yuki run (pid 4242, taken "),
            "{err}"
        );
        assert!(err.contains(" ago)"), "{err}");
        assert!(err.contains(LOCK), "{err}");
    }
    // A dry run only reads, so it still works.
    assert!(run(&home, dir.path(), &["--dry-run"]).status.success());
    assert_eq!(calls(&seen), 0);
    assert!(
        dir.path().join(LOCK).exists(),
        "someone else's lock is left alone"
    );
}

#[test]
fn a_subdirectory_syncs_into_the_state_above_it() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[("2026/bol/a.pdf", "a"), ("2026/other/b.pdf", "b")]);
    // Start a state at the top.
    let b = dir.path().join("2026/other/b.pdf");
    let top = dir.path().to_str().unwrap();
    assert!(mark(&home, &b, &["--skip", "--dir", top]).status.success());

    let sub = dir.path().join("2026/bol");
    let out = run(&home, &sub, &["--yes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    // Paths are relative to the root, and only the subdirectory is scanned.
    assert_eq!(actions(&out), pairs(&[("2026/bol/a.pdf", "uploaded")]));
    assert!(!sub.join(STATE).exists());
    assert_eq!(
        entry_for(dir.path(), "2026/bol/a.pdf").unwrap()["status"],
        "uploaded"
    );

    // mark finds the same root from the file.
    let again = mark(&home, &sub.join("a.pdf"), &["--doc-id", "doc-1"]);
    assert_eq!(json(&again)["items"][0]["Path"], "2026/bol/a.pdf");

    // A state file further down makes the tree ambiguous.
    std::fs::write(
        dir.path().join("2026/other").join(STATE),
        "{\"version\": 1, \"hash\": \"sha256\", \"files\": {}}",
    )
    .unwrap();
    let nested = run(&home, dir.path(), &["--yes"]);
    assert_eq!(nested.status.code(), Some(1));
    assert!(
        stderr(&nested).contains("holds other sync states (2026/other/.yuki-sync.json)"),
        "{}",
        stderr(&nested)
    );
    assert_eq!(uploads(&seen).len(), 1);
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
        // Two different local files under the one name Yuki has once.
        ("2026/a/dup.pdf", "dup a"),
        ("2026/b/dup.pdf", "dup b"),
        ("2026/x/taken.pdf", "taken x"),
        ("2026/y/taken.pdf", "taken y"),
        // Decomposed (NFD), as macOS stores names.
        ("2026/cafe/Cafe\u{301}.pdf", "cafe"),
    ]);
    // taken.pdf is already recorded for one file; the other cannot claim it.
    let x = dir.path().join("2026/x/taken.pdf");
    let top = dir.path().to_str().unwrap();
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
        ])
    );
    let note = |path: &str| row(&out, path)["Note"].as_str().unwrap().to_string();
    assert!(note("2026/a/dup.pdf").contains("2026/a/dup.pdf, 2026/b/dup.pdf"));
    assert!(note("2026/y/taken.pdf").contains("already recorded for 2026/x/taken.pdf"));
    assert!(note("2026/shop/scan.pdf").contains("y-scan-1"));
    let vercel = note("2026/vercel/2026-09-07_vercel_REF12345.pdf");
    assert!(vercel.contains("y-vercel"), "{vercel}");
    assert!(vercel.contains("yuki upload mark '"), "{vercel}");
    let err = stderr(&out);
    assert!(err.contains("by file name only"), "{err}");
    // Authenticate, SetCurrentDomain, uitzoeken (one page), inkoop (two pages).
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
    assert_eq!(bol["status"], "already-in-yuki");
    assert_eq!(bol["document_id"], "y-bol");
    assert_eq!(bol["folder"], "inkoop");
    assert!(bol.get("uploaded_at").is_none());
    assert_eq!(
        entry_for(dir.path(), "2026/cafe/Cafe\u{301}.pdf").unwrap()["document_id"],
        "y-cafe"
    );
    // The two seeded files plus the one marked by hand.
    assert_eq!(state(dir.path())["files"].as_object().unwrap().len(), 3);

    // The real run then uploads everything else.
    let up = run(&home, dir.path(), &["--yes"]);
    assert!(up.status.success(), "{}", stderr(&up));
    assert_eq!(uploads(&seen).len(), 6);
    assert!(!uploads(&seen).contains(&"2026-08-30_bol-com_111.pdf".to_string()));
}

#[test]
fn seeding_stops_paging_when_a_page_repeats() {
    let (root, seen) = mock_yuki();
    let home = home_with_config(&root);
    let dir = receipts(&[("old-3.pdf", "o")]);
    let out = run(
        &home,
        dir.path(),
        &["--seed-from-yuki", "--yes", "--seed-folder", "verkoop"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("repeats documents already read"));
    assert_eq!(row(&out, "old-3.pdf")["Action"], "already-in-yuki");
    assert_eq!(calls(&seen), 4, "{:?}", seen.lock().unwrap());
}

#[test]
fn mark_records_a_file_by_hand_without_contacting_yuki() {
    let home = TempDir::new().unwrap();
    let dir = receipts(&[("2026/supabase/inv.pdf", "inv"), ("2026/other.pdf", "o")]);
    let file = dir.path().join("2026/supabase/inv.pdf");

    // No state file anywhere above it yet: --dir is needed.
    let out = mark(&home, &file, &["--doc-id", "d-1"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("pass --dir"), "{}", stderr(&out));

    let root = dir.path().to_str().unwrap();
    let out = mark(
        &home,
        &file,
        &["--doc-id", "d-1", "--dir", root, "--folder", "inkoop"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(json(&out)["items"][0]["Action"], "recorded");
    let e = entry_for(dir.path(), "2026/supabase/inv.pdf").unwrap();
    assert_eq!(e["status"], "already-in-yuki");
    assert_eq!(e["document_id"], "d-1");
    assert_eq!(e["folder"], "inkoop");
    assert!(!dir.path().join(LOCK).exists());

    // Now the state file is found from the file itself; the same mark is a no-op.
    let same = mark(&home, &file, &["--doc-id", "d-1"]);
    assert!(same.status.success());
    assert_eq!(json(&same)["items"][0]["Action"], "unchanged");

    // Another document ID needs --force.
    let other = mark(&home, &file, &["--doc-id", "d-2"]);
    assert_eq!(other.status.code(), Some(1));
    assert!(stderr(&other).contains("--force"));
    let forced = mark(&home, &file, &["--doc-id", "d-2", "--force"]);
    assert!(forced.status.success());
    assert_eq!(
        entry_for(dir.path(), "2026/supabase/inv.pdf").unwrap()["document_id"],
        "d-2"
    );

    // The dry run sees it as synced.
    let plan = run(&home, dir.path(), &["--dry-run"]);
    assert_eq!(
        actions(&plan),
        pairs(&[
            ("2026/other.pdf", "would-upload"),
            ("2026/supabase/inv.pdf", "synced")
        ])
    );

    // --skip keeps a file out for good; --forget drops a record.
    let other = dir.path().join("2026/other.pdf");
    assert!(
        mark(&home, &other, &["--skip", "--note", "private"])
            .status
            .success()
    );
    let e = entry_for(dir.path(), "2026/other.pdf").unwrap();
    assert_eq!(
        (e["status"].as_str(), e["note"].as_str()),
        (Some("skipped"), Some("private"))
    );
    let forgot = mark(&home, &file, &["--forget"]);
    assert!(forgot.status.success());
    assert_eq!(json(&forgot)["items"][0]["Action"], "forgotten");
    assert!(entry_for(dir.path(), "2026/supabase/inv.pdf").is_none());
    let plan = run(&home, dir.path(), &["--dry-run"]);
    assert_eq!(
        actions(&plan),
        pairs(&[
            ("2026/other.pdf", "synced"),
            ("2026/supabase/inv.pdf", "would-upload")
        ])
    );

    // Exactly one of --doc-id, --skip, --forget.
    assert_eq!(mark(&home, &file, &[]).status.code(), Some(2));
    assert_eq!(
        mark(&home, &file, &["--skip", "--forget"]).status.code(),
        Some(2)
    );
}
