use super::*;
use crate::client::sales::ImportedInvoice;

fn pdf_in(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, b"%PDF-1.4 test invoice").unwrap();
    path
}

fn args(file: PathBuf) -> BookArgs {
    BookArgs {
        file,
        contact: "Leverancier BV".into(),
        reference: "F2026-0042".into(),
        date: "2026-10-20".into(),
        total: Cents(12100),
        vat: Cents(2100),
        gl_account: Some("612000".into()),
        vat_type: Some(1),
        vat_description: Some("VAT 21%".into()),
        ..BookArgs::default()
    }
}

fn invalid(args: &BookArgs) -> String {
    match build(args) {
        Err(InvoiceError::InvalidInput(m)) => m,
        other => panic!("expected invalid input, got {other:?}"),
    }
}

/// Assert each tag opens after the previous one: the XSD's `xs:sequence` order.
fn in_order(xml: &str, tags: &[&str]) {
    let mut at = 0;
    for tag in tags {
        let open = format!("<{tag}>");
        let found = xml[at..]
            .find(&open)
            .unwrap_or_else(|| panic!("{open} missing or out of order in:\n{xml}"));
        at += found + open.len();
    }
}

#[test]
fn the_single_line_shortcut_books_total_minus_vat_in_xsd_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(pdf_in(dir.path(), "factuur.pdf"));
    a.due_date = Some("2026-11-19".into());
    a.payment_method = Some(PaymentMethod::CreditCard);
    a.remarks = Some("Tom & Jerry <x>".into());
    a.description = Some("Hosting oktober".into());
    let invoice = build(&a).unwrap();
    assert_eq!(invoice.lines[0].amount, Cents(10000));
    let xml = invoice.to_xml();
    assert!(
        xml.starts_with(r#"<PurchaseInvoices xmlns="urn:xmlns:http://www.theyukicompany.com:purchaseinvoices" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">"#),
        "{xml}"
    );
    in_order(
        &xml,
        &[
            "PurchaseInvoice",
            "Process",
            "Reference",
            "Date",
            "DueDate",
            "InvoiceAmount",
            "InvoiceVATAmount",
            "Currency",
            "PaymentMethod",
            "Remarks",
            "DocumentFileName",
            "DocumentBase64",
            "Contact",
            "FullName",
            "InvoiceLines",
            "InvoiceLine",
            "Description",
            "GLAccountCode",
            "LineAmount",
            "LineVATType",
            "LineVATDescription",
        ],
    );
    for fragment in [
        "<Process>false</Process>",
        "<InvoiceAmount>121.00</InvoiceAmount>",
        "<InvoiceVATAmount>21.00</InvoiceVATAmount>",
        "<PaymentMethod>CreditCard</PaymentMethod>",
        "<Remarks>Tom &amp; Jerry &lt;x&gt;</Remarks>",
        "<LineAmount>100.00</LineAmount>",
        "<LineVATType>1</LineVATType>",
        "<LineVATDescription>VAT 21%</LineVATDescription>",
        &format!(
            "<DocumentBase64>{}</DocumentBase64>",
            BASE64.encode(b"%PDF-1.4 test invoice")
        ),
    ] {
        assert!(xml.contains(fragment), "{fragment} in {xml}");
    }
    assert!(!xml.contains("LineVATPercentage"), "{xml}");
    // The display form elides the PDF and nothing else.
    let shown = invoice.to_display_xml();
    assert!(
        shown.contains("<DocumentBase64><!-- 28 bytes base64 (21-byte PDF) --></DocumentBase64>"),
        "{shown}"
    );
    assert_eq!(
        shown
            .lines()
            .filter(|l| !l.contains("DocumentBase64"))
            .collect::<Vec<_>>(),
        xml.lines()
            .filter(|l| !l.contains("DocumentBase64"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_contact_matched_by_code_sends_only_code_and_name() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(pdf_in(dir.path(), "factuur.pdf"));
    a.country = Some("be".into());
    a.vat_number = Some("BE0123456789".into());
    a.iban = Some("BE68 5390 0754 7034".into());
    let new = build(&a).unwrap().to_xml();
    in_order(
        &new,
        &[
            "Contact",
            "FullName",
            "CountryCode",
            "VATNumber",
            "BankAccount",
        ],
    );
    assert!(
        new.contains("<CountryCode>BE</CountryCode>")
            && new.contains("<BankAccount>BE68539007547034</BankAccount>"),
        "{new}"
    );

    a.contact_code = Some("SUP001".into());
    let coded = build(&a).unwrap().to_xml();
    let contact = &coded[coded.find("<Contact>").unwrap()..coded.find("</Contact>").unwrap()];
    assert!(
        contact.contains("<ContactCode>SUP001</ContactCode>"),
        "{contact}"
    );
    assert!(
        contact.contains("<FullName>Leverancier BV</FullName>"),
        "{contact}"
    );
    for absent in ["CountryCode", "VATNumber", "BankAccount"] {
        assert!(!contact.contains(absent), "{absent} in {contact}");
    }
}

#[test]
fn lines_plus_vat_must_make_the_total_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(pdf_in(dir.path(), "factuur.pdf"));
    a.gl_account = None;
    a.vat_type = None;
    a.vat_description = None;
    a.lines = vec![
        parse_line("Hosting;612000;60.00;1;VAT 21%").unwrap(),
        parse_line(";613000;39.99;1;VAT 21%").unwrap(),
    ];
    let err = invalid(&a);
    assert!(
        err.contains("the lines (99.99 excl. VAT) plus the VAT (21.00) make 120.99, not the total 121.00: off by 0.01"),
        "{err}"
    );
    a.lines[1].amount = Cents(4000);
    let invoice = build(&a).unwrap();
    assert_eq!(invoice.lines.len(), 2);
    assert_eq!(invoice.lines[1].description, None);
    let xml = invoice.to_xml();
    assert_eq!(xml.matches("<InvoiceLine>").count(), 2);
}

#[test]
fn every_local_problem_is_reported_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(pdf_in(dir.path(), "f.pd"));
    a.reference = "  ".into();
    a.total = Cents(0);
    a.vat_type = None;
    a.due_date = Some("2026-10-01".into());
    a.country = Some("Belgium".into());
    let err = invalid(&a);
    for needle in [
        "shorter than the 5 characters",
        "--reference (the supplier's invoice number) is empty",
        "--total 0.00 must be positive",
        "a line needs --gl-account, --vat-type and --vat-description",
        "--due-date 2026-10-01 is before the invoice date 2026-10-20",
        "--country 'BELGIUM' is not an ISO 3166-1 alpha-2 code",
    ] {
        assert!(err.contains(needle), "{needle:?} not in {err}");
    }
    a = args(pdf_in(dir.path(), "factuur.pdf"));
    a.lines = vec![parse_line("x;612000;100.00;1;VAT 21%").unwrap()];
    assert!(invalid(&a).contains("not both"));
}

#[test]
fn only_a_pdf_is_booked() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scan.pdf");
    std::fs::write(&path, b"PNG").unwrap();
    assert!(invalid(&args(path)).contains("not a PDF"));
}

#[test]
fn a_line_has_five_fields() {
    let line = parse_line(" Hosting ; 612000 ; 100.5 ; 11 ; VAT 21% ").unwrap();
    assert_eq!(
        line,
        LineArg {
            description: Some("Hosting".into()),
            gl_account: "612000".into(),
            amount: Cents(10050),
            vat_type: 11,
            vat_description: "VAT 21%".into(),
        }
    );
    for (bad, why) in [
        ("Hosting;612000;100", "5 fields"),
        ("a;b;c;d;e;f", "5 fields"),
        ("x;;100;1;VAT 21%", "GL account is empty"),
        ("x;612000;100;1;", "VAT description is empty"),
        ("x;612000;1,5;1;VAT 21%", "amount"),
        ("x;612000;1.005;1;VAT 21%", "amount"),
        ("x;612000;100;one;VAT 21%", "not a number"),
    ] {
        let err = parse_line(bad).unwrap_err();
        assert!(err.contains(why), "{bad}: {err}");
    }
}

fn code(code: &str, description: &str) -> VatCode {
    VatCode {
        code: code.into(),
        description: description.into(),
    }
}

#[test]
fn a_vat_type_is_checked_with_its_description() {
    // Belgian VAT types are ambiguous: 11 is several codes.
    let codes = [
        code("1", "VAT 21%"),
        code("11", "VAT 12%"),
        code("11", "Input tax"),
        code("11", "VAT 21%"),
        code("2", "VAT 6%"),
    ];
    assert_eq!(
        vat_code(&codes, 11, "vat 21%").unwrap().description,
        "VAT 21%"
    );
    let err = vat_code(&codes, 11, "VAT 6%").unwrap_err();
    assert!(
        err.contains("type 11 is: 'VAT 12%', 'Input tax', 'VAT 21%'")
            && err.contains("'VAT 6%' is type 2"),
        "{err}"
    );
    let err = vat_code(&codes, 99, "Whatever").unwrap_err();
    assert!(
        err.starts_with("VAT type 99 with description 'Whatever' is not an active VAT code"),
        "{err}"
    );
}

fn doc(id: &str, file: &str, contact: &str, reference: &str) -> ArchiveDocument {
    ArchiveDocument {
        id: id.into(),
        subject: String::new(),
        document_date: "2026-10-20T00:00:00".into(),
        amount: String::new(),
        folder: "Inkoop".into(),
        contact_name: contact.into(),
        file_name: file.into(),
        reference: reference.into(),
    }
}

#[test]
fn archive_documents_match_by_file_name_or_reference_and_supplier() {
    let dir = tempfile::tempdir().unwrap();
    let invoice = build(&args(pdf_in(dir.path(), "Factuur.pdf"))).unwrap();
    let docs = [
        doc("1", "factuur.PDF", "", ""),
        doc("2", "other.pdf", "Leverancier", "f2026-0042"),
        doc("3", "other.pdf", "Someone else", "F2026-0042"),
        doc("4", "x.pdf", "Leverancier BV", "F2026-0043"),
        doc("1", "factuur.PDF", "", ""),
    ];
    let found = archive_matches(&invoice, &docs);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        found[0].starts_with("same file name: document 1"),
        "{found:?}"
    );
    assert!(
        found[1].starts_with("same reference and supplier: document 2"),
        "{found:?}"
    );
}

#[test]
fn the_sync_state_refuses_a_pdf_in_yuki_or_pending() {
    let spot = SyncSpot {
        root: PathBuf::from("/r"),
        rel: "a/f.pdf".into(),
    };
    let mut state = SyncState::default();
    assert!(check_sync(&state, &spot, "h").is_ok());
    for (status, ok) in [
        (SyncStatus::Uploaded, false),
        (SyncStatus::AlreadyInYuki, false),
        (SyncStatus::Pending, false),
        (SyncStatus::Failed, true),
        (SyncStatus::Skipped, true),
    ] {
        state.files.insert(
            "h".into(),
            SyncEntry {
                path: "a/f.pdf".into(),
                status,
                document_id: Some("doc-9".into()),
                ..SyncEntry::default()
            },
        );
        match check_sync(&state, &spot, "h") {
            Ok(()) => assert!(ok, "{status:?} allowed"),
            Err(e) => {
                assert!(!ok, "{status:?} refused: {e}");
                let e = e.to_string();
                assert!(
                    e.starts_with("DUPLICATE: this PDF") && e.contains("document doc-9"),
                    "{e}"
                );
            }
        }
    }
}

#[test]
fn confirm_names_the_reference() {
    assert!(check_confirm("F-1", None, false).is_ok());
    assert!(check_confirm("F-1", Some("F-1"), true).is_ok());
    let err = check_confirm("F-1", None, true).unwrap_err();
    assert!(matches!(&err, InvoiceError::ConfirmationRequired(m) if m.contains("--confirm F-1")));
    let err = check_confirm("F-1", Some("F-2"), false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("not the reference F-1"), "{err}");
}

fn imported(succeeded: bool, processed: bool, reference: &str) -> PurchaseInvoicesImport {
    PurchaseInvoicesImport {
        total_succeeded: u32::from(succeeded),
        total_failed: u32::from(!succeeded),
        total_skipped: 0,
        invoices: vec![ImportedInvoice {
            succeeded,
            processed,
            reference: reference.into(),
            message: if succeeded { "" } else { "Contact not found" }.into(),
            ..ImportedInvoice::default()
        }],
    }
}

#[test]
fn yukis_answer_is_judged() {
    let dir = tempfile::tempdir().unwrap();
    let mut invoice = build(&args(pdf_in(dir.path(), "factuur.pdf"))).unwrap();
    assert_eq!(
        verdict(&imported(true, false, "f2026-0042"), &invoice),
        Verdict::Done(None)
    );
    assert!(matches!(
        verdict(&imported(false, false, ""), &invoice),
        Verdict::Rejected { freed: true, message } if message.contains("Contact not found")
    ));
    assert!(matches!(
        verdict(&PurchaseInvoicesImport::default(), &invoice),
        Verdict::Rejected { freed: false, .. }
    ));
    assert!(matches!(
        verdict(&imported(true, false, "OTHER"), &invoice),
        Verdict::ReferenceMismatch(m) if m.contains("reference OTHER, not F2026-0042")
    ));
    invoice.process = true;
    assert!(matches!(
        verdict(&imported(true, false, "F2026-0042"), &invoice),
        Verdict::Done(Some(_))
    ));
}

#[test]
fn a_refusal_under_the_access_key_alone_hints_at_an_api_client() {
    let refused = "Client not authorized for this webservice".to_string();
    assert!(with_hint(refused.clone(), true).contains("support.accounting@nmbrs.com"));
    assert_eq!(with_hint(refused.clone(), false), refused);
    assert_eq!(with_hint("Unknown GL".into(), true), "Unknown GL");
    let e = hinted(YukiError::Unauthorized(403), true).to_string();
    assert!(e.contains("HTTP 403") && e.contains("Client ID"), "{e}");
}
