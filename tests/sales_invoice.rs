//! End-to-end: `sales invoice create` and `templates` against a local Yuki
//! mock. Nothing here reaches the real API. Names and amounts are made up.

mod common;

use common::{RequestLog, actions, bodies, fault, json, response, stderr, yuki};
use serde_json::Value;
use tempfile::TempDir;

/// A monthly template for an existing contact, at Belgian 21% VAT.
const TEMPLATE: &str = r#"
subject = "Managed hosting"
due_days = 30

[contact]
code = "C0042"

[[lines]]
description = "Managed hosting"
qty = 1
price = 100.00
vat_percentage = 21
vat_type = 1
gl_account = "700000"
"#;

/// An ad-hoc invoice for a new contact.
const AD_HOC: &str = r#"
subject = "Workshop & follow-up"
date = 2026-10-01

[contact]
name = "New Customer BV"
country = "BE"
email = "billing@example.be"

[[lines]]
description = "Workshop"
qty = 2
price = "450.00"
vat_percentage = 21
vat_type = 1
"#;

fn import_response(succeeded: bool, processed: bool, email_sent: bool, reference: &str) -> String {
    let (ok, failed) = if succeeded { (1, 0) } else { (0, 1) };
    let message = if succeeded { "" } else { "Contact not found" };
    format!(
        "<SalesInvoicesImportResponse xmlns=\"urn:xmlns:http://www.theyukicompany.com:salesinvoicesresponse\">\
         <TotalSucceeded>{ok}</TotalSucceeded><TotalFailed>{failed}</TotalFailed><TotalSkipped>0</TotalSkipped>\
         <Invoice><Succeeded>{succeeded}</Succeeded><Processed>{processed}</Processed>\
         <EmailSent>{email_sent}</EmailSent><Reference>{reference}</Reference>\
         <Subject>Managed hosting</Subject><Message>{message}</Message></Invoice>\
         </SalesInvoicesImportResponse>"
    )
}

/// A mock answering Authenticate and, with `result`, ProcessSalesInvoices.
/// The sales archive: Yuki's own invoice PDFs and a timesheet.
const SALES_ARCHIVE: &str = r#"<Documents xmlns="">
<Document ID="d-19"><FileName>Invoice 2026-19.pdf</FileName></Document>
<Document ID="d-9"><FileName>Invoice 2026-9.pdf</FileName></Document>
<Document ID="d-t"><FileName>uren januari 2026.xlsx</FileName></Document>
</Documents>"#;

fn mock(result: String) -> (String, RequestLog) {
    common::mock(move |r| match r.action.as_str() {
        "Authenticate" => (200, response("Authenticate", "session-1")),
        "ProcessSalesInvoices" => (200, response("ProcessSalesInvoices", &result)),
        "SetCurrentDomain" => (200, response("SetCurrentDomain", "")),
        "DocumentsInFolder" => (200, response("DocumentsInFolder", SALES_ARCHIVE)),
        other => panic!("unexpected call {other}"),
    })
}

fn home(root: &str) -> TempDir {
    let home = common::home_with_config(root, "test-key", "");
    let dir = home.path().join(".config/yuki/invoices");
    std::fs::create_dir_all(&dir).expect("invoices dir");
    std::fs::write(dir.join("hosting.toml"), TEMPLATE).expect("write template");
    home
}

fn sent_body(log: &RequestLog) -> String {
    bodies(log, "ProcessSalesInvoices")
        .into_iter()
        .next()
        .expect("ProcessSalesInvoices was called")
}

#[test]
fn a_template_draft_is_created_with_yes() {
    let (root, log) = mock(import_response(true, false, false, ""));
    let home = home(&root);
    let output = yuki(
        &home,
        &[
            "sales",
            "invoice",
            "create",
            "--template",
            "hosting",
            "--qty",
            "3",
            "--date",
            "2026-10-01",
            "--yes",
            "--output",
            "json",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let json = json(&output);
    assert_eq!(json["total"], 1);
    assert_eq!(json["items"][0]["Succeeded"], "Yes");
    assert_eq!(json["items"][0]["Processed"], "No");

    // Authenticate, then the import: no SetCurrentDomain, nothing else.
    assert_eq!(actions(&log), ["Authenticate", "ProcessSalesInvoices"]);
    let body = sent_body(&log);
    for fragment in [
        "<yuki:sessionId>session-1</yuki:sessionId>",
        "<yuki:administrationId>admin-1</yuki:administrationId>",
        "<yuki:xmlDoc><SalesInvoices xmlns=\"urn:xmlns:http://www.theyukicompany.com:salesinvoices\"",
        "<Process>false</Process>",
        "<EmailToCustomer>false</EmailToCustomer>",
        "<ContactCode>C0042</ContactCode>",
        "<ProductQuantity>3</ProductQuantity>",
        "<DueDate>2026-10-31</DueDate>",
    ] {
        assert!(body.contains(fragment), "{fragment} missing from:\n{body}");
    }
    // The preview went to stderr, stating the mode and the totals.
    let err = stderr(&output);
    assert!(err.contains("DRAFT (Process=false)"), "{err}");
    assert!(err.contains("363.00 EUR"), "{err}");
}

#[test]
fn send_email_books_and_emails_the_invoice() {
    // The result arrives as escaped XML text this time.
    let escaped = import_response(true, true, true, "2026-0042")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let (root, log) = mock(escaped);
    let home = home(&root);
    let file = home.path().join("adhoc.toml");
    std::fs::write(&file, AD_HOC).expect("write invoice");
    let output = yuki(
        &home,
        &[
            "sales",
            "invoice",
            "create",
            "--file",
            file.to_str().unwrap(),
            "--send",
            "email",
            "--yes",
            "--output",
            "json",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let json = json(&output);
    let row = &json["items"][0];
    assert_eq!(
        (&row["Processed"], &row["Email Sent"], &row["Reference"]),
        (
            &Value::from("Yes"),
            &Value::from("Yes"),
            &Value::from("2026-0042")
        )
    );
    let body = sent_body(&log);
    for fragment in [
        "<Subject>Workshop &amp; follow-up</Subject>",
        "<Process>true</Process>",
        "<EmailToCustomer>true</EmailToCustomer>",
        "<SentToPeppol>false</SentToPeppol>",
        "<Date>2026-10-01</Date>",
        "<FullName>New Customer BV</FullName>",
        "<CountryCode>BE</CountryCode>",
    ] {
        assert!(body.contains(fragment), "{fragment} missing from:\n{body}");
    }
    // Escaped exactly once, inside a document that is not itself escaped.
    assert!(
        !body.contains("&amp;amp;") && !body.contains("&lt;SalesInvoices"),
        "{body}"
    );
    assert!(stderr(&output).contains("BOOK AND SEND BY EMAIL"));
}

#[test]
fn a_send_yuki_did_not_carry_out_fails_even_when_quiet() {
    for (processed, email_sent, expected) in [
        (false, false, "did not book it"),
        (true, false, "did not email it"),
    ] {
        let (root, _log) = mock(import_response(true, processed, email_sent, "2026-0043"));
        let home = home(&root);
        let output = yuki(
            &home,
            &[
                "sales",
                "invoice",
                "create",
                "--template",
                "hosting",
                "--send",
                "email",
                "--yes",
                "--quiet",
            ],
        );
        assert_eq!(output.status.code(), Some(1), "{expected}");
        let err = stderr(&output);
        assert!(err.contains("\"kind\":\"invoice_rejected\""), "{err}");
        assert!(err.contains(expected), "{err}");
    }
}

#[test]
fn a_rejected_invoice_exits_non_zero_after_printing_yukis_answer() {
    let (root, _log) = mock(import_response(false, false, false, ""));
    let home = home(&root);
    let output = yuki(
        &home,
        &[
            "sales",
            "invoice",
            "create",
            "--template",
            "hosting",
            "--yes",
            "--output",
            "json",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let json = json(&output);
    assert_eq!(json["items"][0]["Message"], "Contact not found");
    let err = stderr(&output);
    let envelope: Value =
        serde_json::from_str(err.lines().last().expect("stderr")).expect("error envelope");
    assert_eq!(envelope["error"]["kind"], "invoice_rejected");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Contact not found"),
        "{err}"
    );
}

#[test]
fn a_dry_run_prints_the_document_and_makes_no_call() {
    let (root, log) = mock(String::new());
    let home = home(&root);
    let output = yuki(
        &home,
        &[
            "sales",
            "invoice",
            "create",
            "--template",
            "hosting",
            "--send",
            "both",
            "--date",
            "2026-10-01",
            "--dry-run",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let xml = String::from_utf8_lossy(&output.stdout);
    assert!(
        xml.starts_with(
            "<SalesInvoices xmlns=\"urn:xmlns:http://www.theyukicompany.com:salesinvoices\""
        ),
        "{xml}"
    );
    assert!(xml.contains("<SentToPeppol>true</SentToPeppol>"), "{xml}");
    assert!(stderr(&output).contains("Dry run: nothing was sent to Yuki"));
    assert!(actions(&log).is_empty(), "{:?}", actions(&log));
}

#[test]
fn a_dry_run_needs_no_configuration() {
    let home = TempDir::new().expect("temp home");
    let file = home.path().join("adhoc.toml");
    std::fs::write(&file, AD_HOC).expect("write invoice");
    let output = yuki(
        &home,
        &[
            "sales",
            "invoice",
            "create",
            "--file",
            file.to_str().unwrap(),
            "--dry-run",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("<Process>false</Process>"));
}

#[test]
fn without_a_terminal_it_refuses_unless_yes_is_given() {
    let (root, log) = mock(String::new());
    let home = home(&root);
    let output = yuki(
        &home,
        &["sales", "invoice", "create", "--template", "hosting"],
    );
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(err.contains("\"kind\":\"confirmation_required\""), "{err}");
    assert!(err.contains("pass --yes"), "{err}");
    assert!(actions(&log).is_empty(), "{:?}", actions(&log));
}

#[test]
fn an_invalid_file_is_reported_before_anything_else() {
    let (root, log) = mock(String::new());
    let home = home(&root);
    let file = home.path().join("bad.toml");
    std::fs::write(&file, AD_HOC.replace("country = \"BE\"\n", "")).expect("write invoice");
    let output = yuki(
        &home,
        &[
            "sales",
            "invoice",
            "create",
            "--file",
            file.to_str().unwrap(),
            "--yes",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(err.contains("contact.country is required"), "{err}");
    assert!(err.contains("\"kind\":\"invalid_input\""), "{err}");
    assert!(actions(&log).is_empty());
}

#[test]
fn templates_lists_valid_and_invalid_templates() {
    let home = home("http://127.0.0.1:9/ws");
    let dir = home.path().join(".config/yuki/invoices");
    std::fs::write(dir.join("broken.toml"), "[contact]\n").expect("write template");
    std::fs::write(dir.join("notes.txt"), "not a template").expect("write other file");
    let output = yuki(
        &home,
        &["sales", "invoice", "templates", "--output", "json"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let json = json(&output);
    let items = json["items"].as_array().expect("items");
    assert_eq!(items.len(), 2, "{json}");
    assert_eq!(items[0]["Name"], "broken");
    assert!(
        items[0]["Status"].as_str().unwrap().starts_with("invalid:"),
        "{json}"
    );
    assert_eq!(items[1]["Name"], "hosting");
    assert_eq!(items[1]["Customer"], "code C0042");
    assert_eq!(items[1]["Net"], "100.00");
    assert_eq!(items[1]["Status"], "ok");
}

#[test]
fn a_custom_pdf_goes_into_the_envelope_but_not_into_the_dry_run_output() {
    let (root, log) = mock(import_response(true, true, false, "2026-20"));
    let home = home(&root);
    let dir = home.path().join(".config/yuki/invoices");
    let pdf = b"%PDF-1.7\nmade-up invoice body\n%%EOF\n";
    std::fs::write(dir.join("hosting.pdf"), pdf).expect("write pdf");
    let pdf_path = dir.join("hosting.pdf");
    let pdf_arg = pdf_path.to_str().unwrap();
    let base64 = "JVBERi0xLjcKbWFkZS11cCBpbnZvaWNlIGJvZHkKJSVFT0YK";

    let dry = yuki(
        &home,
        &[
            "sales",
            "invoice",
            "create",
            "--template",
            "hosting",
            "--pdf",
            pdf_arg,
            "--book",
            "--number",
            "2026-20",
            "--date",
            "2026-10-02",
            "--dry-run",
        ],
    );
    assert!(dry.status.success(), "{}", stderr(&dry));
    let xml = String::from_utf8_lossy(&dry.stdout);
    assert!(!xml.contains(base64), "{xml}");
    assert!(
        xml.contains("<!-- 48 bytes base64 (36-byte PDF) -->"),
        "{xml}"
    );
    assert!(stderr(&dry).contains(
        "custom PDF hosting.pdf (36 bytes), stored as Invoice 2026-20.pdf, replaces Yuki's layout"
    ));
    assert!(actions(&log).is_empty());

    let output = yuki(
        &home,
        &[
            "sales",
            "invoice",
            "create",
            "--template",
            "hosting",
            "--pdf",
            pdf_arg,
            "--book",
            "--number",
            "2026-20",
            "--date",
            "2026-10-02",
            "--yes",
            "--output",
            "json",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let json = json(&output);
    // The archive file is named after the number, not the local file.
    assert_eq!(json["items"][0]["PDF"], "Invoice 2026-20.pdf");
    let body = sent_body(&log);
    assert!(
        body.contains("<DocumentFileName>Invoice 2026-20.pdf</DocumentFileName>"),
        "{body}"
    );
    assert!(
        body.contains(&format!("<DocumentBase64>{base64}</DocumentBase64>")),
        "{body}"
    );
}

#[test]
fn a_template_with_a_pdf_is_refused() {
    let (root, log) = mock(String::new());
    let home = home(&root);
    let dir = home.path().join(".config/yuki/invoices");
    std::fs::write(
        dir.join("monthly.toml"),
        format!("pdf = \"x.pdf\"\n{TEMPLATE}"),
    )
    .expect("write template");
    let output = yuki(
        &home,
        &[
            "sales",
            "invoice",
            "create",
            "--template",
            "monthly",
            "--dry-run",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("a template can't carry a PDF; pass --pdf per invoice"));
    assert!(actions(&log).is_empty());
}

#[test]
fn a_lost_answer_warns_that_the_invoice_may_exist() {
    // The mock drops the connection once it has read the request.
    let (root, log) = common::mock(|r| match r.action.as_str() {
        "Authenticate" => (200, response("Authenticate", "session-1")),
        _ => (0, String::new()),
    });
    let home = home(&root);
    let output = yuki(
        &home,
        &[
            "sales",
            "invoice",
            "create",
            "--template",
            "hosting",
            "--yes",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(err.contains("\"kind\":\"outcome_unknown\""), "{err}");
    assert!(
        err.contains("the invoice may already have been created in Yuki — check 'To be sent'/'Sales' before retrying"),
        "{err}"
    );
    assert_eq!(actions(&log), ["Authenticate", "ProcessSalesInvoices"]);
}

#[test]
fn number_auto_takes_the_next_number_from_the_sales_archive() {
    let (root, log) = mock(import_response(true, true, false, "2026-20"));
    let home = home(&root);
    let output = yuki(
        &home,
        &[
            "sales",
            "invoice",
            "create",
            "--template",
            "hosting",
            "--date",
            "2026-10-02",
            "--number",
            "auto",
            "--book",
            "--yes",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        actions(&log),
        [
            "Authenticate",
            "SetCurrentDomain",
            "DocumentsInFolder",
            "Authenticate",
            "ProcessSalesInvoices"
        ]
    );
    // Only the invoice year is read from the archive.
    let list = log.lock().unwrap()[2].body.clone();
    assert!(
        list.contains("<yuki:startDate>2026-01-01</yuki:startDate>")
            && list.contains("<yuki:endDate>2026-12-31</yuki:endDate>"),
        "{list}"
    );
    let body = sent_body(&log);
    assert!(body.contains("<Reference>2026-20</Reference>"), "{body}");
    assert!(body.contains("<Process>true</Process>"), "{body}");
    assert!(
        body.contains("<EmailToCustomer>false</EmailToCustomer>"),
        "{body}"
    );
    let err = stderr(&output);
    assert!(err.contains("BOOKS IMMEDIATELY"), "{err}");
    assert!(err.contains("Number         2026-20"), "{err}");
}

#[test]
fn a_number_already_in_the_archive_is_refused_before_any_write() {
    let (root, log) = mock(String::new());
    let home = home(&root);
    let output = yuki(
        &home,
        &[
            "sales",
            "invoice",
            "create",
            "--template",
            "hosting",
            "--number",
            "2026-19",
            "--book",
            "--yes",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(
        err.contains("invoice number 2026-19 is already in the sales archive"),
        "{err}"
    );
    assert!(err.contains("\"kind\":\"invalid_input\""), "{err}");
    assert!(!actions(&log).contains(&"ProcessSalesInvoices".to_string()));
}

#[test]
fn a_dry_run_cannot_pick_an_automatic_number() {
    let home = TempDir::new().expect("temp home");
    let file = home.path().join("adhoc.toml");
    std::fs::write(&file, AD_HOC).expect("write invoice");
    let output = yuki(
        &home,
        &[
            "sales",
            "invoice",
            "create",
            "--file",
            file.to_str().unwrap(),
            "--number",
            "auto",
            "--book",
            "--dry-run",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("run `sales invoice prepare --number auto`"));
}

#[test]
fn prepare_and_create_agree_on_every_figure() {
    let (root, log) = mock(String::new());
    let home = home(&root);
    let args = [
        "--template",
        "hosting",
        "--qty",
        "2.5",
        "--price",
        "85.10",
        "--date",
        "2026-09-30",
        "--number",
        "auto",
    ];
    let mut prepare = vec!["sales", "invoice", "prepare"];
    prepare.extend_from_slice(&args);
    let output = yuki(&home, &prepare);
    assert!(output.status.success(), "{}", stderr(&output));
    let json = json(&output);
    assert_eq!(json["number"], "2026-20");
    assert_eq!(json["date"]["text"], "30 september 2026");
    assert_eq!(json["due_date"]["iso"], "2026-10-30");
    assert_eq!(json["totals"]["net"], "212.75");
    assert_eq!(json["totals"]["vat"], "44.68");
    assert_eq!(json["totals"]["gross"], "257.43");
    assert_eq!(json["payment_reference"], "+++202/6000/02014+++");
    // Only the archive was read.
    assert_eq!(
        actions(&log),
        ["Authenticate", "SetCurrentDomain", "DocumentsInFolder"]
    );

    // create with the same inputs and the prepared number books the same.
    let number = json["number"].as_str().unwrap();
    let mut create = vec!["sales", "invoice", "create", "--dry-run", "--book"];
    create.extend_from_slice(&args[..args.len() - 2]);
    create.extend_from_slice(&["--number", number]);
    let output = yuki(&home, &create);
    assert!(output.status.success(), "{}", stderr(&output));
    // Expected values worked out by hand, not by the code under test:
    // 2.5 × 85.10 = 212.75 net; 21% of it 44.6775 → 44.68; gross 257.43.
    let xml = String::from_utf8_lossy(&output.stdout);
    for fragment in [
        "<Reference>2026-20</Reference>",
        "<Date>2026-09-30</Date>",
        "<DueDate>2026-10-30</DueDate>",
        "<ProductQuantity>2.5</ProductQuantity>",
        "<SalesPrice>85.10</SalesPrice>",
        "<VATPercentage>21</VATPercentage>",
    ] {
        assert!(xml.contains(fragment), "{fragment} missing from {xml}");
    }
    let preview = stderr(&output);
    for total in ["212.75 EUR", "257.43 EUR"] {
        assert!(preview.contains(total), "{total}: {preview}");
    }
    assert!(preview.contains("44.68 EUR  (21% on 212.75)"), "{preview}");
}

/// The local ledger of numbers given out, as `sales invoice numbers` lists it.
fn ledger_rows(home: &TempDir) -> Vec<Value> {
    let output = yuki(home, &["sales", "invoice", "numbers", "--output", "json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let json = json(&output);
    json["items"].as_array().cloned().unwrap_or_default()
}

fn book_number(home: &TempDir, number: &str) -> std::process::Output {
    yuki(
        home,
        &[
            "sales",
            "invoice",
            "create",
            "--template",
            "hosting",
            "--number",
            number,
            "--book",
            "--yes",
            "--quiet",
        ],
    )
}

#[test]
fn the_ledger_records_a_booking_and_blocks_the_number() {
    let (root, _log) = mock(import_response(true, true, false, "2026-20"));
    let home = home(&root);
    let output = book_number(&home, "2026-20");
    assert!(output.status.success(), "{}", stderr(&output));
    // Quiet, a booking still says so in one line.
    let err = stderr(&output);
    assert!(
        err.contains("BOOKS IMMEDIATELY: 2026-20 code C0042 121.00 EUR"),
        "{err}"
    );
    let rows = ledger_rows(&home);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (&rows[0]["Number"], &rows[0]["Status"]),
        (&Value::from("2026-20"), &Value::from("booked"))
    );
    assert!(!rows[0]["Booked"].as_str().unwrap().is_empty());

    // The archive does not show 2026-20 yet; the ledger still refuses it,
    // and auto skips it.
    let again = book_number(&home, "2026-20");
    assert_eq!(again.status.code(), Some(1));
    assert!(stderr(&again).contains("invoice number 2026-20 was already given out: booked"));
    let prepare = yuki(
        &home,
        &[
            "sales",
            "invoice",
            "prepare",
            "--template",
            "hosting",
            "--date",
            "2026-10-02",
            "--number",
            "auto",
        ],
    );
    let json = json(&prepare);
    assert_eq!(json["number"], "2026-21");
}

#[test]
fn a_clean_rejection_frees_the_number() {
    let (root, _log) = mock(import_response(false, false, false, ""));
    let home = home(&root);
    let output = book_number(&home, "2026-20");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(ledger_rows(&home)[0]["Status"], "rejected");
    let prepare = yuki(
        &home,
        &[
            "sales",
            "invoice",
            "prepare",
            "--template",
            "hosting",
            "--date",
            "2026-10-02",
            "--number",
            "auto",
        ],
    );
    let json = json(&prepare);
    assert_eq!(json["number"], "2026-20");
}

/// A mock whose ProcessSalesInvoices fails with `status` and `reply`.
fn failing(status: u16, reply: String) -> (String, RequestLog) {
    common::mock(move |r| match r.action.as_str() {
        "Authenticate" => (200, response("Authenticate", "session-1")),
        "ProcessSalesInvoices" => (status, reply.clone()),
        "DocumentsInFolder" => (200, response("DocumentsInFolder", SALES_ARCHIVE)),
        other => (200, response(other, "")),
    })
}

#[test]
fn whether_the_call_may_have_been_processed_decides_the_number() {
    // A SOAP fault may come after processing: the number stays pending.
    let (root, _log) = failing(500, fault("Server was unable to process request."));
    let faulted = home(&root);
    let err = stderr(&book_number(&faulted, "2026-20"));
    assert!(err.contains("\"kind\":\"outcome_unknown\""), "{err}");
    assert_eq!(ledger_rows(&faulted)[0]["Status"], "pending");

    // HTTP 401 is refused unprocessed: the number is free again.
    let (root, _log) = failing(401, "Unauthorized".into());
    let refused = home(&root);
    let err = stderr(&book_number(&refused, "2026-20"));
    assert!(err.contains("\"kind\":\"auth_failed\""), "{err}");
    assert_eq!(ledger_rows(&refused)[0]["Status"], "rejected");
}

#[test]
fn an_unknown_outcome_keeps_the_number_pending_until_resolved() {
    let (root, _log) = common::mock(|r| match r.action.as_str() {
        "Authenticate" => (200, response("Authenticate", "session-1")),
        "SetCurrentDomain" => (200, response("SetCurrentDomain", "")),
        "DocumentsInFolder" => (200, response("DocumentsInFolder", SALES_ARCHIVE)),
        _ => (0, String::new()),
    });
    let home = home(&root);
    let output = book_number(&home, "2026-20");
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(err.contains("\"kind\":\"outcome_unknown\""), "{err}");
    assert!(err.contains("stays pending in the ledger"), "{err}");
    assert_eq!(ledger_rows(&home)[0]["Status"], "pending");

    let again = book_number(&home, "2026-20");
    assert!(stderr(&again).contains("already given out: pending (outcome unknown"));

    // Checked in Yuki: it was not created.
    let resolve = yuki(
        &home,
        &[
            "sales",
            "invoice",
            "numbers",
            "--resolve",
            "2026-20",
            "rejected",
            "--output",
            "json",
        ],
    );
    assert!(resolve.status.success(), "{}", stderr(&resolve));
    assert_eq!(ledger_rows(&home)[0]["Status"], "rejected");
    let bad = yuki(
        &home,
        &[
            "sales",
            "invoice",
            "numbers",
            "--resolve",
            "2026-20",
            "maybe",
        ],
    );
    assert_eq!(bad.status.code(), Some(1));
}
