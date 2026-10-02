//! End-to-end: `documents download` and `invoices document` write the real
//! file from a local Yuki mock, under its own name or --out.

mod common;

use common::{RequestLog, actions, json, response, yuki};
use tempfile::TempDir;

/// `%PDF-1.4 made-up` in base64, with a line break as Yuki may send it.
const PDF_BASE64: &str = "JVBERi0xLjQg\nbWFkZS11cA==";
const PDF: &[u8] = b"%PDF-1.4 made-up";

fn mock() -> (String, RequestLog) {
    common::mock(|r| match r.action.as_str() {
        "Authenticate" => (200, response("Authenticate", "session-1")),
        "FindDocument" => (
            200,
            response(
                "FindDocument",
                r#"<Document ID="doc-1"><Subject>Invoice</Subject><FileName>Invoice 2026-19.pdf</FileName></Document>"#,
            ),
        ),
        "DocumentBinaryData" => (200, response("DocumentBinaryData", PDF_BASE64)),
        "GetTransactionDocument" => (
            200,
            response(
                "GetTransactionDocument",
                &format!(
                    "<fileName>../Factuur &amp; co.pdf</fileName><filedata>{PDF_BASE64}</filedata>"
                ),
            ),
        ),
        other => panic!("unexpected call {other}"),
    })
}

fn home_with_config(root: &str) -> TempDir {
    common::home_with_config(root, "test-key", "")
}

#[test]
fn download_saves_the_file_under_its_archive_name() {
    let (root, log) = mock();
    let home = home_with_config(&root);
    let dir = home.path().join("out");
    std::fs::create_dir(&dir).unwrap();
    let output = yuki(
        &home,
        &[
            "documents",
            "download",
            "doc-1",
            "--out",
            dir.to_str().unwrap(),
            "-o",
            "json",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let saved = dir.join("Invoice 2026-19.pdf");
    assert_eq!(std::fs::read(&saved).unwrap(), PDF);
    let json = json(&output);
    assert_eq!(json["items"][0]["Bytes"], PDF.len().to_string());
    assert_eq!(
        actions(&log),
        ["Authenticate", "FindDocument", "DocumentBinaryData"]
    );

    // Never over an existing file.
    let again = yuki(
        &home,
        &[
            "documents",
            "download",
            "doc-1",
            "--out",
            dir.to_str().unwrap(),
        ],
    );
    assert_eq!(again.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&again.stderr).contains("remove it or pass --out"));
}

#[test]
fn download_to_a_file_needs_no_lookup() {
    let (root, log) = mock();
    let home = home_with_config(&root);
    let file = home.path().join("copy.pdf");
    let output = yuki(
        &home,
        &[
            "documents",
            "download",
            "doc-1",
            "--out",
            file.to_str().unwrap(),
        ],
    );
    assert!(output.status.success());
    assert_eq!(std::fs::read(&file).unwrap(), PDF);
    assert_eq!(actions(&log), ["Authenticate", "DocumentBinaryData"]);
}

#[test]
fn invoices_document_saves_the_file_instead_of_printing_it() {
    let (root, _log) = mock();
    let home = home_with_config(&root);
    let dir = home.path().join("out");
    std::fs::create_dir(&dir).unwrap();
    let output = yuki(
        &home,
        &[
            "invoices",
            "document",
            "tx-1",
            "--out",
            dir.to_str().unwrap(),
            "-o",
            "json",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The name's directory part is dropped, so it stays inside --out.
    assert_eq!(std::fs::read(dir.join("Factuur & co.pdf")).unwrap(), PDF);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("JVBERi"), "{stdout}");
}
