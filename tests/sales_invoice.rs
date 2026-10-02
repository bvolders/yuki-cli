//! End-to-end: `sales invoice create` and `templates` against a local Yuki
//! mock. Nothing here reaches the real API. Names and amounts are made up.

mod common;

use common::{RequestLog, mock_yuki, soap_response, yuki};
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
fn mock(result: String) -> (String, RequestLog) {
    mock_yuki(move |action, _| match action {
        "Authenticate" => soap_response("Authenticate", "session-1"),
        "ProcessSalesInvoices" => soap_response("ProcessSalesInvoices", &result),
        other => panic!("unexpected call {other}"),
    })
}

fn home_with_config(root: &str) -> TempDir {
    let home = TempDir::new().expect("temp home");
    let dir = home.path().join(".config/yuki");
    std::fs::create_dir_all(dir.join("invoices")).expect("config dir");
    std::fs::write(
        dir.join("config.toml"),
        format!(
            r#"api_key = "test-key"
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
    std::fs::write(dir.join("invoices/hosting.toml"), TEMPLATE).expect("write template");
    home
}

fn actions(log: &RequestLog) -> Vec<String> {
    log.lock()
        .expect("log")
        .iter()
        .map(|r| r.action.clone())
        .collect()
}

fn sent_body(log: &RequestLog) -> String {
    log.lock()
        .expect("log")
        .iter()
        .find(|r| r.action == "ProcessSalesInvoices")
        .map(|r| r.body.clone())
        .expect("ProcessSalesInvoices was called")
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_template_draft_is_created_with_yes() {
    let (root, log) = mock(import_response(true, false, false, ""));
    let home = home_with_config(&root);
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
    let json: Value = serde_json::from_slice(&output.stdout).expect("JSON stdout");
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
    let home = home_with_config(&root);
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
    let json: Value = serde_json::from_slice(&output.stdout).expect("JSON stdout");
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
        let home = home_with_config(&root);
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
    let home = home_with_config(&root);
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
    let json: Value = serde_json::from_slice(&output.stdout).expect("JSON stdout");
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
    let home = home_with_config(&root);
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
    let home = home_with_config(&root);
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
    let home = home_with_config(&root);
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
    let home = home_with_config("http://127.0.0.1:9/ws");
    let dir = home.path().join(".config/yuki/invoices");
    std::fs::write(dir.join("broken.toml"), "[contact]\n").expect("write template");
    std::fs::write(dir.join("notes.txt"), "not a template").expect("write other file");
    let output = yuki(
        &home,
        &["sales", "invoice", "templates", "--output", "json"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let json: Value = serde_json::from_slice(&output.stdout).expect("JSON stdout");
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
    let (root, log) = mock(import_response(true, false, false, ""));
    let home = home_with_config(&root);
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
    assert!(stderr(&dry).contains("custom PDF hosting.pdf (36 bytes) replaces Yuki's layout"));
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
            "--yes",
            "--output",
            "json",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let json: Value = serde_json::from_slice(&output.stdout).expect("JSON stdout");
    assert_eq!(json["items"][0]["PDF"], "hosting.pdf");
    let body = sent_body(&log);
    assert!(
        body.contains("<DocumentFileName>hosting.pdf</DocumentFileName>"),
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
    let home = home_with_config(&root);
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
    let (root, log) = mock_yuki(|action, _| match action {
        "Authenticate" => soap_response("Authenticate", "session-1"),
        _ => String::new(),
    });
    let home = home_with_config(&root);
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
