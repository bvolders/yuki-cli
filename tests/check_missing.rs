//! End-to-end: `check missing` against a local mock of Yuki Belgium. The
//! fixture is Yuki's "Betalingen waarvan factuur nog ontbreekt" for Vercel
//! Inc as the UI showed it: seven card payments no invoice settles, and the
//! open invoice 2E81616A-0008.

mod common;

use common::{RequestLog, actions, json, ok, response, stderr, yuki};
use serde_json::Value;
use tempfile::TempDir;

fn item(id: &str, contact: &str, date: &str, open: &str, kind: (&str, &str), rest: &str) -> String {
    let (type_id, type_name) = kind;
    format!(
        "<Item ID=\"{id}\"><Date>{date}</Date>{rest}<Contact>{contact}</Contact>\
         <OpenAmount>{open}</OpenAmount><OriginalAmount>{open}</OriginalAmount>\
         <Type ID=\"{type_id}\">{type_name}</Type><DueDate /><Country>US</Country></Item>"
    )
}

const CARD: (&str, &str) = ("", "Creditcardbetaling");
const INVOICE: (&str, &str) = ("2", "Aankoopfactuur");

fn payment(id: &str, contact: &str, date: &str, open: &str, description: &str) -> String {
    item(
        id,
        contact,
        date,
        open,
        CARD,
        &format!("<Description>{description}</Description><Reference /><PaymentMethod />"),
    )
}

/// The Vercel case, plus a supplier with only an open invoice (not listed)
/// and one paid by bank transfer.
fn outstanding() -> String {
    let items = [
        payment("v1", "Vercel Inc", "2026-02-08", "-17.27", "vercel"),
        payment("v2", "Vercel Inc", "2026-03-08", "-39.01", "vercel inc"),
        payment("v3", "Vercel Inc", "2026-04-08", "-36.15", "vercel"),
        payment("v4", "Vercel Inc", "2026-05-08", "-17.89", "vercel inc"),
        payment("v5", "Vercel Inc", "2026-07-08", "-17.79", "Vercrel inc"),
        payment(
            "v6",
            "Vercel Inc",
            "2026-08-07",
            "-17.65",
            "VERCEL INC. COVINA C Koers (1 EUR = 1,133144476 USD)",
        ),
        payment(
            "v7",
            "Vercel Inc",
            "2026-09-07",
            "-17.52",
            "VERCEL INC. Bedrag 20 USD COVINA C Koers (1 EUR = 1,141552511 USD)",
        ),
        item(
            "v8",
            "Vercel Inc",
            "2026-09-07",
            "17.22",
            INVOICE,
            "<Description>Factuur van Vercel Inc, Bureaubenodigdheden en drukwerk</Description>\
             <Reference>2E81616A-0008</Reference><PaymentMethod>Bancontact</PaymentMethod>",
        ),
        item(
            "g1",
            "Ge-fibo",
            "2026-09-30",
            "446.56",
            INVOICE,
            "<Description>Factuur van Ge-fibo</Description><Reference>F-1</Reference>",
        ),
        item(
            "a1",
            "Alpha &amp; Co",
            "2026-08-14",
            "-88.90",
            ("", "Banktransactie"),
            "<Description>Overschrijving | ALPHA</Description><Reference />",
        ),
    ];
    response(
        "OutstandingCreditorItemsByDateOutstanding",
        &format!(
            "<OutstandingCreditorItems xmlns=\"\">{}</OutstandingCreditorItems>",
            items.concat()
        ),
    )
}

fn mock() -> (String, RequestLog) {
    common::mock(|r| match r.action.as_str() {
        "Authenticate" => (200, response("Authenticate", "session-1")),
        "OutstandingCreditorItemsByDateOutstanding" => (200, outstanding()),
        other => (200, response(other, "")),
    })
}

fn rows(out: &Value) -> Vec<Vec<String>> {
    out["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|r| {
            ["Supplier", "Kind", "Date", "Open", "Reference", "FX"]
                .iter()
                .map(|k| r[*k].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .collect()
}

#[test]
fn check_missing_reproduces_yukis_list_for_vercel() {
    let (root, seen) = mock();
    let home = common::home_with_config(&root, "test-key", "");
    let out = yuki(
        &home,
        &[
            "check",
            "missing",
            "--contact",
            "vercel",
            "--output",
            "json",
        ],
    );
    ok(&out);
    let json = json(&out);
    let got = rows(&json);
    let expect = [
        ("payment", "2026-02-08", "-17.27", "", ""),
        ("payment", "2026-03-08", "-39.01", "", ""),
        ("payment", "2026-04-08", "-36.15", "", ""),
        ("payment", "2026-05-08", "-17.89", "", ""),
        ("payment", "2026-07-08", "-17.79", "", ""),
        ("payment", "2026-08-07", "-17.65", "", "~20.00 USD"),
        ("payment", "2026-09-07", "-17.52", "", "20.00 USD"),
        ("open invoice", "2026-09-07", "17.22", "2E81616A-0008", ""),
    ];
    assert_eq!(got.len(), expect.len(), "{json}");
    for (row, (kind, date, open, reference, fx)) in got.iter().zip(expect) {
        assert_eq!(row, &["Vercel Inc", kind, date, open, reference, fx]);
    }
    assert_eq!(json["summary"]["payments"], 7);
    assert_eq!(json["summary"]["payments_open"], "-163.28");
    assert_eq!(json["summary"]["open_invoices"], 1);
    assert_eq!(json["summary"]["invoices_open"], "17.22");
    assert!(
        stderr(&out).contains("API calls made: 3"),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        actions(&seen),
        [
            "Authenticate",
            "SetCurrentDomain",
            "OutstandingCreditorItemsByDateOutstanding"
        ]
    );
    let request = seen.lock().expect("log")[2].clone();
    assert_eq!(request.param("includeBankTransactions"), "true");
    assert_eq!(request.param("administrationID"), "admin-1");
}

#[test]
fn check_missing_lists_only_suppliers_with_payments_in_the_period() {
    let (root, _) = mock();
    let home = common::home_with_config(&root, "test-key", "");
    let out = yuki(
        &home,
        &[
            "check", "missing", "--period", "2026-Q3", "--output", "json",
        ],
    );
    ok(&out);
    let json = json(&out);
    let got = rows(&json);
    let suppliers: Vec<(&str, &str)> = got.iter().map(|r| (r[0].as_str(), r[2].as_str())).collect();
    // Ge-fibo has an open invoice but no payment: not listed. Vercel's
    // invoice is listed with its Q3 payments.
    assert_eq!(
        suppliers,
        [
            ("Alpha & Co", "2026-08-14"),
            ("Vercel Inc", "2026-07-08"),
            ("Vercel Inc", "2026-08-07"),
            ("Vercel Inc", "2026-09-07"),
            ("Vercel Inc", "2026-09-07"),
        ]
    );
    assert_eq!(json["summary"]["suppliers"], 2);
    assert_eq!(json["suppliers"][1]["Payments"], "3");
}

#[test]
fn check_missing_finds_local_receipts_without_writing_to_the_folder() {
    let (root, _) = mock();
    let home = common::home_with_config(&root, "test-key", "");
    let receipts = TempDir::new().expect("receipts dir");
    let dir = receipts.path().join("2026/vercel");
    std::fs::create_dir_all(&dir).expect("vendor dir");
    std::fs::create_dir_all(receipts.path().join("_to_delete")).expect("to delete");
    let files = [
        "2026-07-07_vercel_2E81616A-0006.pdf",
        "2026-08-07_vercel_2E81616A-0007.pdf",
        "2026-09-07_vercel_2E81616A-0008.pdf",
    ];
    for (i, name) in files.iter().enumerate() {
        std::fs::write(dir.join(name), format!("%PDF receipt {i}")).expect("receipt");
    }
    std::fs::write(
        receipts.path().join("_to_delete/2026-09-07_vercel_x.pdf"),
        "%PDF old",
    )
    .expect("old");
    std::fs::write(
        receipts.path().join("receipts-log.csv"),
        "invoice_date,vendor,invoice_number,amount,currency,document_type,filename,collected_on\n\
         2026-07-07,Vercel,2E81616A-0006,20.00,USD,invoice,2026/vercel/2026-07-07_vercel_2E81616A-0006.pdf,2026-09-01\n\
         2026-08-07,Vercel,2E81616A-0007,20.00,USD,invoice,2026/vercel/2026-08-07_vercel_2E81616A-0007.pdf,2026-09-01\n\
         2026-09-07,Vercel,2E81616A-0008,20.00,USD,invoice,2026/vercel/2026-09-07_vercel_2E81616A-0008.pdf,2026-09-21\n",
    )
    .expect("log");
    let uploaded = yuki_cli::sync::sha256_hex(b"%PDF receipt 1");
    let state = format!(
        r#"{{"version":1,"hash":"sha256","files":{{"{uploaded}":{{"path":"2026/vercel/{}","size":14,"status":"uploaded","document_id":"f7862e61","recorded_at":"2026-10-02T10:00:00Z"}}}}}}"#,
        files[1]
    );
    std::fs::write(receipts.path().join(".yuki-sync.json"), &state).expect("state");
    let before = listing(receipts.path());

    let out = yuki(
        &home,
        &[
            "check",
            "missing",
            "--contact",
            "vercel",
            "--receipts",
            receipts.path().to_str().unwrap(),
            "--output",
            "json",
        ],
    );
    ok(&out);
    let json = json(&out);
    let receipt: Vec<&str> = json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["Local receipt"].as_str().unwrap())
        .collect();
    assert_eq!(
        receipt,
        [
            "",
            "",
            "",
            "",
            // No FX in "Vercrel inc": the USD receipt matches on vendor and date.
            "2026/vercel/2026-07-07_vercel_2E81616A-0006.pdf (vendor and date only)",
            "2026/vercel/2026-08-07_vercel_2E81616A-0007.pdf (uploaded, awaiting Yuki)",
            "2026/vercel/2026-09-07_vercel_2E81616A-0008.pdf",
            "",
        ]
    );
    assert_eq!(json["summary"]["local_receipts"], 3);
    assert_eq!(json["suppliers"][0]["Local receipts"], "3");
    assert_eq!(
        listing(receipts.path()),
        before,
        "the folder was written to"
    );
    assert_eq!(
        std::fs::read_to_string(receipts.path().join(".yuki-sync.json")).unwrap(),
        state
    );
}

#[test]
fn check_missing_table_ends_with_a_summary() {
    let (root, _) = mock();
    let home = common::home_with_config(&root, "test-key", "");
    let out = yuki(&home, &["check", "missing", "--output", "table"]);
    ok(&out);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("8 payments without an invoice (open -252.18 EUR) at 2 suppliers; 1 open invoices (open 17.22 EUR)"),
        "{text}"
    );
    assert!(text.contains("Payments open"), "{text}");
}

#[test]
fn check_missing_refuses_a_receipts_path_that_is_no_folder() {
    let (root, seen) = mock();
    let home = common::home_with_config(&root, "test-key", "");
    let out = yuki(
        &home,
        &["check", "missing", "--receipts", "/nonexistent/receipts"],
    );
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("/nonexistent/receipts"),
        "{}",
        stderr(&out)
    );
    assert!(
        actions(&seen).is_empty(),
        "no API call before the folder is read"
    );
}

/// Every path under `dir` with its size, sorted.
fn listing(dir: &std::path::Path) -> Vec<(String, u64)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(String, u64)>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                walk(&path, out);
            }
            out.push((path.display().to_string(), meta.len()));
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}
