//! End-to-end: `purchase book` and `purchase bookings` against a local Yuki
//! mock. Nothing here reaches the real API. Names and amounts are made up.

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Arc, Mutex};

use common::{Request, RequestLog, actions, bodies, fails, json, ok, response, stderr, yuki};
use serde_json::Value;
use tempfile::TempDir;
use yuki_cli::sync::sha256_hex;

const PDF: &[u8] = b"%PDF-1.4 hosting invoice F2026-0042";

const VAT_CODES: &str = r#"<VATCodes xmlns="">
<VATCode><type>1</type><description>VAT 21%</description></VATCode>
<VATCode><type>11</type><description>VAT 12%</description></VATCode>
<VATCode><type>11</type><description>VAT 21%</description></VATCode>
<VATCode><type>2</type><description>VAT 6%</description></VATCode>
<VATCode><type>11</type><description>Input tax</description></VATCode>
</VATCodes>"#;

/// Yuki's contacts, whatever is searched: the mock leaves the filtering to
/// the CLI.
const CONTACTS: &str = r#"<Contacts xmlns="">
<Contact ID="c-1"><Code>SUP001</Code><Name>Leverancier BV</Name><Type>Company</Type></Contact>
<Contact ID="c-2"><Code></Code><Name>Andere NV</Name><Type>Company</Type></Contact>
</Contacts>"#;

const GL_SCHEME: &str = r#"<GLAccounts xmlns="">
<GLAccount><code>612000</code><description>Hosting</description><type>1</type></GLAccount>
<GLAccount><code>613000</code><description>Software</description><type>1</type></GLAccount>
</GLAccounts>"#;

fn import(succeeded: bool, reference: &str) -> String {
    let (ok, failed) = if succeeded { (1, 0) } else { (0, 1) };
    let message = if succeeded {
        ""
    } else {
        "Contact could not be created"
    };
    format!(
        "<PurchaseInvoicesImportResponse xmlns=\"urn:xmlns:http://www.theyukicompany.com:purchaseinvoicesresponse\">\
         <TotalSucceeded>{ok}</TotalSucceeded><TotalFailed>{failed}</TotalFailed><TotalSkipped>0</TotalSkipped>\
         <Invoice><Succeeded>{succeeded}</Succeeded><Processed>false</Processed>\
         <Reference>{reference}</Reference><Subject>Invoice from Leverancier BV</Subject>\
         <Contact>Leverancier BV</Contact><Message>{message}</Message></Invoice>\
         </PurchaseInvoicesImportResponse>"
    )
}

/// What the mock answers `ProcessPurchaseInvoices` with; status 0 drops
/// the connection.
type Process = Arc<Mutex<(u16, String)>>;

/// A mock of every call `purchase book` makes; `archive` is what
/// `SearchDocuments` finds.
fn mock(archive: &'static str) -> (String, RequestLog, Process) {
    let process: Process = Arc::new(Mutex::new((
        200,
        response("ProcessPurchaseInvoices", &import(true, "F2026-0042")),
    )));
    let answer = Arc::clone(&process);
    let (root, log) = common::mock(move |r: &Request| match r.action.as_str() {
        "Authenticate" => (200, response("Authenticate", "session-1")),
        "AuthenticateClient" => (200, response("AuthenticateClient", "session-2")),
        "SetCurrentDomain" => (200, response("SetCurrentDomain", "")),
        "SearchContacts" => (200, response("SearchContacts", CONTACTS)),
        "ActiveVATCodesList" => (200, response("ActiveVATCodesList", VAT_CODES)),
        "GetGLAccountScheme" => (200, response("GetGLAccountScheme", GL_SCHEME)),
        "SearchDocuments" => (
            200,
            response(
                "SearchDocuments",
                &format!("<Documents xmlns=\"\">{archive}</Documents>"),
            ),
        ),
        "ProcessPurchaseInvoices" => answer.lock().unwrap().clone(),
        // For `upload dir` next to a booking.
        "UploadDocument" => (200, response("UploadDocument", "doc-new")),
        other => panic!("unexpected call {other}"),
    });
    (root, log, process)
}

/// A home with a config on `root`, and the PDF in `inbox/` under it.
fn setup(root: &str, extra_toml: &str) -> (TempDir, PathBuf) {
    let home = common::home_with_config(root, "test-key", extra_toml);
    let inbox = home.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let pdf = inbox.join("F2026-0042.pdf");
    std::fs::write(&pdf, PDF).unwrap();
    (home, pdf)
}

fn book(home: &TempDir, pdf: &Path, extra: &[&str]) -> Output {
    book_with_env(home, pdf, extra, &[])
}

fn book_with_env(home: &TempDir, pdf: &Path, extra: &[&str], env: &[(&str, &str)]) -> Output {
    let pdf = pdf.to_str().unwrap();
    let mut args = vec![
        "purchase",
        "book",
        pdf,
        "--contact",
        "Leverancier BV",
        "--reference",
        "F2026-0042",
        "--date",
        "2026-10-20",
        "--total",
        "121.00",
        "--vat-amount",
        "21.00",
        "--gl-account",
        "612000",
        "--vat-type",
        "11",
        "--vat-description",
        "vat 21%",
        "--payment-method",
        "credit-card",
        "--output",
        "json",
    ];
    // A flag in `extra` that the defaults give replaces their value.
    let mut rest = extra.iter();
    while let Some(arg) = rest.next() {
        match args.iter().position(|a| a == arg) {
            Some(at) if arg.starts_with("--") && at + 1 < args.len() => {
                args[at + 1] = rest.next().expect("a value for the flag");
            }
            _ => args.push(arg),
        }
    }
    common::yuki_with_env(home, &args, env)
}

const CONFIRMED: &[&str] = &["--yes", "--confirm", "F2026-0042"];

/// The purchase ledger's entries.
fn ledger(home: &TempDir) -> Vec<Value> {
    let path = home.path().join(".config/yuki/purchase-invoices.json");
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str::<Value>(&text).unwrap()["entries"]
            .as_array()
            .unwrap()
            .clone(),
        Err(_) => Vec::new(),
    }
}

/// An empty sync state governing `dir`, as `upload dir` leaves it.
fn sync_state(dir: &Path, files: &str) {
    std::fs::write(
        dir.join(".yuki-sync.json"),
        format!(r#"{{"version": 1, "hash": "sha256", "files": {{{files}}}}}"#),
    )
    .unwrap();
}

fn sync_record(dir: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(dir.join(".yuki-sync.json")).unwrap();
    let state: Value = serde_json::from_str(&text).unwrap();
    state["files"].get(sha256_hex(PDF)).cloned()
}

#[test]
fn a_dry_run_checks_against_yuki_and_prints_the_xml_without_sending() {
    let (root, log, _) = mock("");
    let (home, pdf) = setup(&root, "");
    let out = book(&home, &pdf, &["--dry-run"]);
    ok(&out);
    let xml = String::from_utf8_lossy(&out.stdout).into_owned();
    for fragment in [
        "<Process>false</Process>",
        "<Reference>F2026-0042</Reference>",
        "<InvoiceAmount>121.00</InvoiceAmount>",
        "<DocumentFileName>F2026-0042.pdf</DocumentFileName>",
        "<DocumentBase64><!-- ",
        "<LineAmount>100.00</LineAmount>",
        // Yuki's spelling of the description is what is sent.
        "<LineVATDescription>VAT 21%</LineVATDescription>",
    ] {
        assert!(xml.contains(fragment), "{fragment} in {xml}");
    }
    let calls = actions(&log);
    assert!(
        !calls.iter().any(|a| a == "ProcessPurchaseInvoices"),
        "{calls:?}"
    );
    // One session, on the accounting service, for every check.
    assert_eq!(
        calls.iter().filter(|a| *a == "Authenticate").count(),
        1,
        "{calls:?}"
    );
    for read in [
        "ActiveVATCodesList",
        "GetGLAccountScheme",
        "SearchDocuments",
    ] {
        assert!(calls.iter().any(|a| a == read), "{read} in {calls:?}");
    }
    assert!(ledger(&home).is_empty());
}

#[test]
fn the_xml_validates_against_the_xsd() {
    let Ok(lint) = std::process::Command::new("xmllint")
        .arg("--version")
        .output()
    else {
        eprintln!("xmllint not installed: XSD validation skipped");
        return;
    };
    assert!(lint.status.success());
    let (root, _, _) = mock("");
    let (home, pdf) = setup(&root, "");
    let out = book(
        &home,
        &pdf,
        &[
            "--dry-run",
            "--due-date",
            "2026-11-19",
            "--payment-method",
            "transfer",
            // A supplier Yuki creates, so every contact element is sent.
            "--contact",
            "Nieuwe Leverancier",
            "--subject",
            "Hosting & support",
            "--remarks",
            "via yuki-cli",
            "--country",
            "BE",
            "--vat-number",
            "BE0123456789",
            "--iban",
            "BE68539007547034",
            "--description",
            "Hosting <oktober>",
        ],
    );
    ok(&out);
    // The display form has a comment where the base64 goes: put a value
    // back, as the real document has.
    let shown = String::from_utf8_lossy(&out.stdout).into_owned();
    let start = shown.find("<DocumentBase64>").unwrap() + "<DocumentBase64>".len();
    let end = shown.find("</DocumentBase64>").unwrap();
    let xml = format!("{}JVBERi0x{}", &shown[..start], &shown[end..]);
    let file = home.path().join("doc.xml");
    std::fs::write(&file, xml).unwrap();
    let xsd = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/PurchaseInvoices.xsd"
    );
    let check = std::process::Command::new("xmllint")
        .args(["--noout", "--schema", xsd])
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
}

#[test]
fn booking_without_a_prompt_needs_the_reference_confirmed() {
    let (root, log, _) = mock("");
    let (home, pdf) = setup(&root, "");
    let out = book(&home, &pdf, &["--yes"]);
    fails(&out, 1, "--confirm F2026-0042");
    fails(&out, 1, "confirmation_required");
    let out = book(&home, &pdf, &["--yes", "--confirm", "F2026-0043"]);
    fails(&out, 1, "not the reference F2026-0042");
    // Without a terminal and without --yes, nothing is sent either.
    let out = book(&home, &pdf, &[]);
    fails(&out, 1, "confirmation_required");
    // Refused before Yuki is even read.
    assert!(actions(&log).is_empty());
    assert!(ledger(&home).is_empty());
}

#[test]
fn a_confirmed_booking_is_written_ahead_settled_and_recorded_for_upload_dir() {
    let (root, log, _) = mock("");
    let (home, pdf) = setup(&root, "");
    sync_state(pdf.parent().unwrap(), "");
    let out = book(&home, &pdf, CONFIRMED);
    ok(&out);
    let shown = json(&out);
    assert_eq!(shown["items"][0]["Succeeded"], "Yes");
    assert_eq!(shown["items"][0]["Reference"], "F2026-0042");

    let calls = actions(&log);
    let process_at = calls
        .iter()
        .position(|a| a == "ProcessPurchaseInvoices")
        .unwrap();
    // The purchase service authenticates on its own, with the key alone.
    assert_eq!(calls[process_at - 1], "Authenticate");
    let requests = log.lock().unwrap().clone();
    assert!(requests[process_at - 1].path.ends_with("/Purchase.asmx"));
    let body = &bodies(&log, "ProcessPurchaseInvoices")[0];
    for fragment in [
        "<yuki:sessionId>session-1</yuki:sessionId>",
        "<yuki:administrationId>admin-1</yuki:administrationId>",
        "<yuki:disableAutoCorrect>false</yuki:disableAutoCorrect>",
        "<yuki:xmlDoc><PurchaseInvoices xmlns=\"urn:xmlns:http://www.theyukicompany.com:purchaseinvoices\"",
    ] {
        assert!(body.contains(fragment), "{fragment} in {body}");
    }
    assert!(body.contains(&base64_of(PDF)), "the PDF is embedded");

    let entries = ledger(&home);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["status"], "booked");
    assert_eq!(entries[0]["hash"], sha256_hex(PDF));
    let record = sync_record(pdf.parent().unwrap()).expect("recorded for upload dir");
    assert_eq!(record["status"], "already-in-yuki");
    assert_eq!(record["path"], "F2026-0042.pdf");

    // The same PDF again, under any reference, is refused before any call.
    let before = actions(&log).len();
    let out = book(
        &home,
        &pdf,
        &[
            "--yes",
            "--confirm",
            "F2026-0099",
            "--reference",
            "F2026-0099",
        ],
    );
    fails(&out, 1, "DUPLICATE");
    assert_eq!(actions(&log).len(), before, "no API call");
}

fn base64_of(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[test]
fn a_pdf_upload_dir_recorded_is_refused_before_any_call() {
    let (root, log, _) = mock("");
    let (home, pdf) = setup(&root, "");
    let hash = sha256_hex(PDF);
    for status in ["uploaded", "pending"] {
        sync_state(
            home.path(),
            &format!(
                r#""{hash}": {{"path": "inbox/F2026-0042.pdf", "size": 1, "status": "{status}", "document_id": "doc-7", "recorded_at": "2026-10-21T10:00:00Z"}}"#
            ),
        );
        let out = book(&home, &pdf, CONFIRMED);
        fails(&out, 1, "DUPLICATE: this PDF");
        fails(&out, 1, "document doc-7");
    }
    assert!(actions(&log).is_empty());
    // A failed upload never reached Yuki: it may be booked.
    sync_state(
        home.path(),
        &format!(
            r#""{hash}": {{"path": "inbox/F2026-0042.pdf", "size": 1, "status": "failed", "recorded_at": "2026-10-21T10:00:00Z"}}"#
        ),
    );
    ok(&book(&home, &pdf, CONFIRMED));
    assert_eq!(
        sync_record(home.path()).unwrap()["status"],
        "already-in-yuki"
    );
}

#[test]
fn a_vat_pair_or_gl_account_yuki_does_not_have_is_refused() {
    let (root, log, _) = mock("");
    let (home, pdf) = setup(&root, "");
    let out = book(
        &home,
        &pdf,
        &[
            "--dry-run",
            "--vat-type",
            "2",
            "--vat-description",
            "VAT 21%",
        ],
    );
    fails(
        &out,
        1,
        "VAT type 2 with description 'VAT 21%' is not an active VAT code",
    );
    fails(&out, 1, "'VAT 21%' is type 1, 11");
    let out = book(&home, &pdf, &["--dry-run", "--gl-account", "999999"]);
    fails(&out, 1, "GL account 999999 is not in the chart of accounts");
    assert!(!actions(&log).iter().any(|a| a == "ProcessPurchaseInvoices"));
}

#[test]
fn lines_that_do_not_make_the_total_are_refused_before_any_call() {
    let (root, log, _) = mock("");
    let (home, pdf) = setup(&root, "");
    let pdf = pdf.to_str().unwrap();
    let out = yuki(
        &home,
        &[
            "purchase",
            "book",
            pdf,
            "--contact",
            "Leverancier BV",
            "--reference",
            "F2026-0042",
            "--date",
            "2026-10-20",
            "--total",
            "121.00",
            "--vat-amount",
            "21.00",
            "--line",
            "Hosting;612000;60.00;1;VAT 21%",
            "--line",
            "Licence;613000;39.00;1;VAT 21%",
            "--payment-method",
            "cash",
            "--dry-run",
        ],
    );
    fails(&out, 1, "make 120.00, not the total 121.00: off by 1.00");
    fails(&out, 1, "invalid_input");
    assert!(actions(&log).is_empty());
}

#[test]
fn an_archive_document_like_it_is_refused_unless_allowed() {
    let (root, log, _) = mock(
        r#"<Document ID="doc-3"><FileName>f2026-0042.PDF</FileName><ContactName>Leverancier BV</ContactName><Folder>Inkoop</Folder><DocumentDate>2026-10-20T00:00:00</DocumentDate></Document>"#,
    );
    let (home, pdf) = setup(&root, "");
    let out = book(&home, &pdf, CONFIRMED);
    fails(
        &out,
        1,
        "DUPLICATE? The Yuki archive holds what looks like this invoice",
    );
    fails(&out, 1, "same file name: document doc-3");
    assert!(!actions(&log).iter().any(|a| a == "ProcessPurchaseInvoices"));
    // Yuki's free text matches the contact, not the file name or reference:
    // the archive is searched by the supplier, and around the invoice date.
    let searches = log.lock().unwrap().clone();
    let searches: Vec<(String, String)> = searches
        .iter()
        .filter(|r| r.action == "SearchDocuments")
        .map(|r| {
            (
                r.param("searchText").to_string(),
                r.param("startDate").to_string(),
            )
        })
        .collect();
    assert!(
        searches.contains(&("leverancier".into(), "0001-01-01".into())),
        "{searches:?}"
    );
    assert!(
        searches.contains(&(String::new(), "2026-10-13".into())),
        "{searches:?}"
    );
    let mut allowed = CONFIRMED.to_vec();
    allowed.push("--allow-archive-match");
    ok(&book(&home, &pdf, &allowed));
}

#[test]
fn an_unknown_outcome_stays_pending_until_resolved() {
    let (root, log, process) = mock("");
    let (home, pdf) = setup(&root, "");
    sync_state(pdf.parent().unwrap(), "");
    *process.lock().unwrap() = (0, String::new());
    let out = book(&home, &pdf, CONFIRMED);
    fails(&out, 1, "outcome_unknown");
    fails(
        &out,
        1,
        "purchase bookings --resolve F2026-0042 --as booked",
    );
    assert_eq!(ledger(&home)[0]["status"], "pending");
    let record = sync_record(pdf.parent().unwrap()).unwrap();
    assert_eq!(
        record["status"], "pending",
        "upload dir must not upload it meanwhile"
    );

    // Pending blocks a retry: it may be in Yuki.
    *process.lock().unwrap() = (
        200,
        response("ProcessPurchaseInvoices", &import(true, "F2026-0042")),
    );
    let out = book(&home, &pdf, CONFIRMED);
    fails(&out, 1, "DUPLICATE: this PDF was sent to Yuki before");
    assert_eq!(bodies(&log, "ProcessPurchaseInvoices").len(), 1);

    // Checked in Yuki: not there. Resolving frees it, and its sync record.
    let out = yuki(
        &home,
        &[
            "purchase",
            "bookings",
            "--resolve",
            "F2026-0042",
            "--as",
            "rejected",
            "--output",
            "json",
        ],
    );
    ok(&out);
    assert_eq!(json(&out)["items"][0]["Status"], "rejected");
    assert!(sync_record(pdf.parent().unwrap()).is_none());
    ok(&book(&home, &pdf, CONFIRMED));
    let entries = ledger(&home);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1]["status"], "booked");
}

#[test]
fn resolving_as_booked_records_the_pdf_for_upload_dir() {
    let (root, _, process) = mock("");
    let (home, pdf) = setup(&root, "");
    sync_state(pdf.parent().unwrap(), "");
    *process.lock().unwrap() = (500, common::fault("Server was unable to process request."));
    fails(&book(&home, &pdf, CONFIRMED), 1, "outcome_unknown");
    ok(&yuki(
        &home,
        &[
            "purchase",
            "bookings",
            "--resolve",
            "F2026-0042",
            "--as",
            "booked",
        ],
    ));
    assert_eq!(ledger(&home)[0]["status"], "booked");
    assert_eq!(
        sync_record(pdf.parent().unwrap()).unwrap()["status"],
        "already-in-yuki"
    );
}

#[test]
fn a_refusal_or_rejection_frees_the_invoice_and_hints_at_an_api_client() {
    let (root, _, process) = mock("");
    let (home, pdf) = setup(&root, "");
    sync_state(pdf.parent().unwrap(), "");
    *process.lock().unwrap() = (403, String::new());
    let out = book(&home, &pdf, CONFIRMED);
    fails(&out, 2, "support.accounting@nmbrs.com");
    assert_eq!(ledger(&home)[0]["status"], "rejected");
    assert!(
        sync_record(pdf.parent().unwrap()).is_none(),
        "nothing reached Yuki"
    );

    *process.lock().unwrap() = (200, response("ProcessPurchaseInvoices", &import(false, "")));
    let out = book(&home, &pdf, CONFIRMED);
    fails(&out, 1, "invoice_rejected");
    fails(&out, 1, "Contact could not be created");
    assert_eq!(ledger(&home)[1]["status"], "rejected");
    assert!(sync_record(pdf.parent().unwrap()).is_none());
}

#[test]
fn an_ambiguous_answer_never_frees_the_invoice() {
    let (root, log, process) = mock("");
    let (home, pdf) = setup(&root, "");
    sync_state(pdf.parent().unwrap(), "");
    // TotalSucceeded=1, but the invoice's Succeeded is empty.
    let ambiguous = import(true, "F2026-0042")
        .replace("<Succeeded>true</Succeeded>", "<Succeeded></Succeeded>");
    *process.lock().unwrap() = (200, response("ProcessPurchaseInvoices", &ambiguous));
    let out = book(&home, &pdf, CONFIRMED);
    fails(&out, 1, "outcome_unknown");
    assert_eq!(ledger(&home)[0]["status"], "pending");
    assert_eq!(
        sync_record(pdf.parent().unwrap()).unwrap()["status"],
        "pending"
    );
    // So a re-run cannot book it a second time.
    *process.lock().unwrap() = (
        200,
        response("ProcessPurchaseInvoices", &import(true, "F2026-0042")),
    );
    fails(&book(&home, &pdf, CONFIRMED), 1, "DUPLICATE");
    assert_eq!(bodies(&log, "ProcessPurchaseInvoices").len(), 1);
}

#[test]
fn another_reference_back_stays_pending() {
    let (root, _, process) = mock("");
    let (home, pdf) = setup(&root, "");
    *process.lock().unwrap() = (
        200,
        response("ProcessPurchaseInvoices", &import(true, "OTHER")),
    );
    let out = book(&home, &pdf, CONFIRMED);
    fails(&out, 1, "reference_mismatch");
    let entry = &ledger(&home)[0];
    assert_eq!(entry["status"], "pending");
    assert!(
        entry["note"]
            .as_str()
            .unwrap()
            .contains("REFERENCE MISMATCH")
    );
}

#[test]
fn a_configured_api_client_authenticates_with_authenticate_client() {
    let (root, log, _) = mock("");
    let (home, pdf) = setup(
        &root,
        "client_id = \"cid-1\"\nclient_secret = \"s3cret-value\"\n",
    );
    let out = book(&home, &pdf, CONFIRMED);
    ok(&out);
    let auth = &bodies(&log, "AuthenticateClient")[0];
    for fragment in [
        "<yuki:clientID>cid-1</yuki:clientID>",
        "<yuki:clientSecret>s3cret-value</yuki:clientSecret>",
        "<yuki:accessKey>test-key</yuki:accessKey>",
    ] {
        assert!(auth.contains(fragment), "{fragment} in {auth}");
    }
    assert!(
        bodies(&log, "ProcessPurchaseInvoices")[0]
            .contains("<yuki:sessionId>session-2</yuki:sessionId>")
    );
    // The checks keep the plain key: only the purchase service changes.
    assert_eq!(bodies(&log, "Authenticate").len(), 1);
    let printed = format!("{}{}", String::from_utf8_lossy(&out.stdout), stderr(&out));
    assert!(!printed.contains("s3cret-value"), "{printed}");

    // From the environment, without a config entry.
    let (root, log, _) = mock("");
    let (home, pdf) = setup(&root, "");
    let env = [
        ("YUKI_CLIENT_ID", "env-id"),
        ("YUKI_CLIENT_SECRET", "env-secret"),
    ];
    ok(&book_with_env(&home, &pdf, CONFIRMED, &env));
    assert!(
        bodies(&log, "AuthenticateClient")[0].contains("<yuki:clientID>env-id</yuki:clientID>")
    );
    let shown = yuki(&home, &["config", "show", "--output", "json"]);
    let text = String::from_utf8_lossy(&shown.stdout).into_owned();
    assert!(!text.contains("secret-value"), "{text}");
}

/// `upload dir` on `dir`, confirmed, as JSON rows by path.
fn upload_dir(home: &TempDir, dir: &Path) -> Vec<Value> {
    let out = yuki(
        home,
        &[
            "upload",
            "dir",
            dir.to_str().unwrap(),
            "--yes",
            "--output",
            "json",
        ],
    );
    // A pending file fails the run (it needs attention); the rows still come.
    assert!(out.status.code().is_some(), "{}", stderr(&out));
    json(&out)["items"].as_array().unwrap().clone()
}

fn row<'a>(rows: &'a [Value], path: &str) -> &'a Value {
    rows.iter()
        .find(|r| r["Path"] == path)
        .unwrap_or_else(|| panic!("{path} in {rows:?}"))
}

#[test]
fn upload_dir_never_uploads_a_pdf_purchase_book_sent() {
    let (root, log, process) = mock("");
    let (home, pdf) = setup(&root, "");
    // Booked from outside any synced directory...
    ok(&book(&home, &pdf, CONFIRMED));
    // ...then copied into one, with another receipt.
    let receipts = home.path().join("receipts");
    std::fs::create_dir_all(&receipts).unwrap();
    std::fs::copy(&pdf, receipts.join("copy.pdf")).unwrap();
    std::fs::write(receipts.join("other.pdf"), b"%PDF-1.4 other").unwrap();
    let rows = upload_dir(&home, &receipts);
    assert_eq!(row(&rows, "copy.pdf")["Action"], "booked");
    assert_eq!(row(&rows, "other.pdf")["Action"], "uploaded");
    let uploads = bodies(&log, "UploadDocument");
    assert_eq!(uploads.len(), 1);
    assert!(uploads[0].contains("other.pdf"));
    // Recorded as in Yuki, so it stays out for good.
    let record = sync_record(&receipts).unwrap();
    assert_eq!(record["status"], "already-in-yuki");
    assert!(record["note"].as_str().unwrap().contains("F2026-0042"));

    // A booking whose outcome is unknown is kept out too, as pending.
    let second = home.path().join("inbox/second.pdf");
    std::fs::write(&second, b"%PDF-1.4 second invoice").unwrap();
    *process.lock().unwrap() = (0, String::new());
    let out = book(
        &home,
        &second,
        &[
            "--reference",
            "F2026-0050",
            "--date",
            "2026-10-21",
            "--yes",
            "--confirm",
            "F2026-0050",
        ],
    );
    fails(&out, 1, "outcome_unknown");
    std::fs::copy(&second, receipts.join("second.pdf")).unwrap();
    let rows = upload_dir(&home, &receipts);
    assert_eq!(row(&rows, "second.pdf")["Action"], "pending");
    assert_eq!(
        bodies(&log, "UploadDocument").len(),
        1,
        "nothing more uploaded"
    );
}

#[test]
fn resolving_as_booked_records_a_pdf_without_a_sync_record() {
    let (root, _, process) = mock("");
    let (home, pdf) = setup(&root, "");
    *process.lock().unwrap() = (0, String::new());
    fails(&book(&home, &pdf, CONFIRMED), 1, "outcome_unknown");
    // The directory became a synced one after the booking was sent.
    sync_state(pdf.parent().unwrap(), "");
    ok(&yuki(
        &home,
        &[
            "purchase",
            "bookings",
            "--resolve",
            "F2026-0042",
            "--as",
            "booked",
        ],
    ));
    let record = sync_record(pdf.parent().unwrap()).expect("recorded");
    assert_eq!(record["status"], "already-in-yuki");
}

#[test]
fn a_respelled_or_three_of_four_duplicate_is_refused() {
    let (root, log, process) = mock("");
    let (home, pdf) = setup(&root, "");
    ok(&book(&home, &pdf, CONFIRMED));
    let rescan = home.path().join("inbox/rescan.pdf");
    std::fs::write(&rescan, b"%PDF-1.4 the same invoice, scanned again").unwrap();
    // The same reference spelled otherwise, from "Leverancier B.V.".
    let out = book(
        &home,
        &rescan,
        &[
            "--contact",
            "leverancier b.v.",
            "--reference",
            "F 2026/0042",
            "--date",
            "2026-10-01",
            "--yes",
            "--confirm",
            "F 2026/0042",
        ],
    );
    fails(&out, 1, "DUPLICATE: this supplier's reference");
    // Same supplier, date and total under another reference.
    let other_ref = [
        "--reference",
        "F2026-0099",
        "--yes",
        "--confirm",
        "F2026-0099",
    ];
    let out = book(&home, &rescan, &other_ref);
    fails(&out, 1, "--allow-duplicate");
    assert_eq!(bodies(&log, "ProcessPurchaseInvoices").len(), 1);
    let mut allowed = other_ref.to_vec();
    allowed.push("--allow-duplicate");
    *process.lock().unwrap() = (
        200,
        response("ProcessPurchaseInvoices", &import(true, "F2026-0099")),
    );
    ok(&book(&home, &rescan, &allowed));
}

/// `purchase book` with exactly `args` after the file.
fn book_raw(home: &TempDir, pdf: &Path, args: &[&str]) -> Output {
    let mut all = vec!["purchase", "book", pdf.to_str().unwrap()];
    all.extend_from_slice(args);
    yuki(home, &all)
}

const MINIMAL: &[&str] = &[
    "--reference",
    "F2026-0042",
    "--date",
    "2026-10-20",
    "--total",
    "121.00",
    "--vat-amount",
    "21.00",
    "--gl-account",
    "612000",
    "--vat-type",
    "1",
    "--vat-description",
    "VAT 21%",
];

#[test]
fn a_contact_code_alone_identifies_the_supplier() {
    let (root, log, _) = mock("");
    let (home, pdf) = setup(&root, "");
    let mut unknown = MINIMAL.to_vec();
    unknown.extend([
        "--contact-code",
        "NOPE",
        "--payment-method",
        "cash",
        "--dry-run",
    ]);
    fails(
        &book_raw(&home, &pdf, &unknown),
        1,
        "no contact has code NOPE",
    );

    let mut args = MINIMAL.to_vec();
    args.extend([
        "--contact-code",
        "SUP001",
        "--payment-method",
        "transfer",
        "--iban",
        "BE68 5390 0754 7034",
        "--yes",
        "--confirm",
        "F2026-0042",
    ]);
    ok(&book_raw(&home, &pdf, &args));
    let body = &bodies(&log, "ProcessPurchaseInvoices")[0];
    assert!(body.contains("<ContactCode>SUP001</ContactCode>"), "{body}");
    assert!(body.contains("<BankAccount>BE68539007547034</BankAccount>"));
    assert!(
        !body.contains("<FullName>"),
        "Yuki would overwrite the name: {body}"
    );
    // The ledger knows the supplier by Yuki's name.
    assert_eq!(ledger(&home)[0]["contact"], "Leverancier BV");
}

#[test]
fn a_new_supplier_is_said_so_and_needs_a_country() {
    let (root, _, _) = mock("");
    let (home, pdf) = setup(&root, "");
    let out = book(
        &home,
        &pdf,
        &["--contact", "Nieuwe Leverancier", "--dry-run"],
    );
    fails(&out, 1, "no contact named 'Nieuwe Leverancier' in Yuki");
    fails(&out, 1, "--country");
    let out = book(
        &home,
        &pdf,
        &[
            "--contact",
            "Nieuwe Leverancier",
            "--country",
            "BE",
            "--dry-run",
        ],
    );
    ok(&out);
    assert!(stderr(&out).contains("!! NEW supplier will be created in Yuki"));
    let xml = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(xml.contains("<ContactType>Company</ContactType>"), "{xml}");
    // An existing supplier, spelled otherwise, is matched and not recreated.
    let out = book(&home, &pdf, &["--contact", "leverancier b.v.", "--dry-run"]);
    ok(&out);
    let xml = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(xml.contains("<FullName>Leverancier BV</FullName>"), "{xml}");
    assert!(!xml.contains("ContactType"), "{xml}");
}

#[test]
fn the_payment_method_is_required() {
    let (root, log, _) = mock("");
    let (home, pdf) = setup(&root, "");
    let mut args = MINIMAL.to_vec();
    args.extend(["--contact", "Leverancier BV", "--dry-run"]);
    fails(&book_raw(&home, &pdf, &args), 2, "--payment-method");
    args.extend(["--payment-method", "transfer"]);
    fails(
        &book_raw(&home, &pdf, &args),
        1,
        "--payment-method transfer needs --iban",
    );
    assert!(actions(&log).is_empty());
}

#[test]
fn a_vat_amount_the_rates_do_not_give_is_refused() {
    let (root, log, _) = mock("");
    let (home, pdf) = setup(&root, "");
    // 101.00 + 20.00: 21% of 101.00 is 21.21.
    let out = book(
        &home,
        &pdf,
        &[
            "--vat-amount",
            "20.00",
            "--vat-type",
            "1",
            "--vat-description",
            "VAT 21%",
            "--dry-run",
        ],
    );
    fails(
        &out,
        1,
        "--vat-amount 20.00 does not match the lines: at their VAT rates it is 21.21",
    );
    // A code without a plain rate is not checked, and the preview says so.
    let out = book(
        &home,
        &pdf,
        &[
            "--vat-amount",
            "20.00",
            "--vat-type",
            "11",
            "--vat-description",
            "Input tax",
            "--dry-run",
        ],
    );
    ok(&out);
    assert!(stderr(&out).contains("VAT amount not checked"));
    assert!(!actions(&log).iter().any(|a| a == "ProcessPurchaseInvoices"));
}
