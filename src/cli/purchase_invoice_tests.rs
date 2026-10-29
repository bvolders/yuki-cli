use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use super::*;
use crate::client::sales::ImportedInvoice;
use crate::sync::Entry as SyncEntry;

fn pdf_in(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, b"%PDF-1.4 test invoice").unwrap();
    path
}

fn args(file: PathBuf) -> BookArgs {
    BookArgs {
        file,
        contact: Some("Leverancier BV".into()),
        reference: "F2026-0042".into(),
        date: "2026-10-20".into(),
        total: Cents(12100),
        vat: Cents(2100),
        gl_account: Some("612000".into()),
        vat_type: Some(1),
        vat_description: Some("VAT 21%".into()),
        payment_method: PaymentMethod::CreditCard,
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
    a.payment_method = PaymentMethod::CreditCard;
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

fn contact_xml(invoice: &PurchaseInvoice) -> String {
    let xml = invoice.to_xml();
    xml[xml.find("<Contact>").unwrap()..xml.find("</Contact>").unwrap()].to_string()
}

fn yuki_contact(code: &str, name: &str) -> Contact {
    Contact {
        id: format!("id-{name}"),
        code: code.into(),
        name: name.into(),
        ..Contact::default()
    }
}

#[test]
fn a_contact_matched_by_code_gets_only_its_code_and_bank_account() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(pdf_in(dir.path(), "factuur.pdf"));
    a.contact = None;
    a.contact_code = Some("SUP001".into());
    a.iban = Some("be68 5390 0754 7034".into());
    let mut invoice = build(&a).unwrap();
    let (mut problems, mut notes) = (Vec::new(), Vec::new());
    let yuki = [
        yuki_contact("SUP001", "Leverancier B.V."),
        yuki_contact("SUP002", "X"),
    ];
    match_supplier(&mut invoice.supplier, &yuki, &mut problems, &mut notes);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(invoice.supplier.name, "Leverancier B.V.");
    let contact = contact_xml(&invoice);
    assert!(
        contact.contains("<ContactCode>SUP001</ContactCode>"),
        "{contact}"
    );
    assert!(
        contact.contains("<BankAccount>BE68539007547034</BankAccount>"),
        "{contact}"
    );
    // Yuki would overwrite the contact with anything else.
    for absent in ["FullName", "CountryCode", "VATNumber", "ContactType"] {
        assert!(!contact.contains(absent), "{absent} in {contact}");
    }
    assert!(
        invoice
            .preview("x")
            .contains("matched by code: only the code and bank account")
    );

    // A --contact that is not Yuki's name for the code is ignored, said so.
    a.contact = Some("Someone Else".into());
    let mut invoice = build(&a).unwrap();
    match_supplier(&mut invoice.supplier, &yuki, &mut problems, &mut notes);
    assert!(
        notes[0].contains("--contact 'Someone Else' ignored"),
        "{notes:?}"
    );
    // An unknown code is refused.
    a.contact_code = Some("NOPE".into());
    let mut invoice = build(&a).unwrap();
    match_supplier(&mut invoice.supplier, &yuki, &mut problems, &mut notes);
    assert!(
        problems[0].contains("no contact has code NOPE"),
        "{problems:?}"
    );
}

#[test]
fn a_contact_matched_by_name_gets_yukis_spelling_and_no_details() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(pdf_in(dir.path(), "factuur.pdf"));
    a.contact = Some("leverancier bv".into());
    a.country = Some("BE".into());
    a.vat_number = Some("BE0123456789".into());
    let mut invoice = build(&a).unwrap();
    let (mut problems, mut notes) = (Vec::new(), Vec::new());
    let yuki = [
        yuki_contact("", "Leverancier B.V."),
        yuki_contact("", "Leveranciers"),
    ];
    match_supplier(&mut invoice.supplier, &yuki, &mut problems, &mut notes);
    assert!(problems.is_empty(), "{problems:?}");
    let contact = contact_xml(&invoice);
    assert!(
        contact.contains("<FullName>Leverancier B.V.</FullName>"),
        "{contact}"
    );
    for absent in ["ContactCode", "CountryCode", "VATNumber", "ContactType"] {
        assert!(!contact.contains(absent), "{absent} in {contact}");
    }
    // Two contacts alike, none spelled exactly so: ambiguous.
    let twins = [
        yuki_contact("", "Leverancier B.V."),
        yuki_contact("A1", "LEVERANCIER NV"),
    ];
    let mut invoice = build(&a).unwrap();
    match_supplier(&mut invoice.supplier, &twins, &mut problems, &mut notes);
    assert!(
        problems[0].contains("several contacts in Yuki match") && problems[0].contains("code A1"),
        "{problems:?}"
    );
}

#[test]
fn a_new_supplier_needs_a_country_and_is_a_company_unless_a_person() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(pdf_in(dir.path(), "factuur.pdf"));
    a.contact = Some("Nieuwe Leverancier".into());
    let mut invoice = build(&a).unwrap();
    let (mut problems, mut notes) = (Vec::new(), Vec::new());
    match_supplier(&mut invoice.supplier, &[], &mut problems, &mut notes);
    assert!(problems[0].contains("give --country"), "{problems:?}");

    a.country = Some("be".into());
    a.vat_number = Some("BE0123456789".into());
    let mut invoice = build(&a).unwrap();
    problems.clear();
    match_supplier(&mut invoice.supplier, &[], &mut problems, &mut notes);
    assert!(problems.is_empty());
    let contact = contact_xml(&invoice);
    in_order(
        &contact,
        &["FullName", "CountryCode", "VATNumber", "ContactType"],
    );
    assert!(
        contact.contains("<ContactType>Company</ContactType>"),
        "{contact}"
    );
    assert!(
        invoice
            .preview("x")
            .contains("!! NEW supplier will be created in Yuki: Nieuwe Leverancier")
    );
    a.person = true;
    let mut invoice = build(&a).unwrap();
    match_supplier(&mut invoice.supplier, &[], &mut problems, &mut notes);
    assert!(contact_xml(&invoice).contains("<ContactType>Person</ContactType>"));
}

#[test]
fn transfer_needs_a_valid_iban() {
    // clap requires --payment-method itself (see the end-to-end tests).
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(pdf_in(dir.path(), "factuur.pdf"));
    a.payment_method = PaymentMethod::Transfer;
    assert!(invalid(&a).contains("--payment-method transfer needs --iban"));
    a.iban = Some("BE68 5390 0754 7035".into());
    assert!(invalid(&a).contains("check digits are wrong"));
    a.iban = Some("be68 5390 0754 7034".into());
    let invoice = build(&a).unwrap();
    assert_eq!(invoice.supplier.iban.as_deref(), Some("BE68539007547034"));
}

#[test]
fn ibans_are_checked_by_mod_97() {
    for good in [
        "BE68539007547034",
        "NL91ABNA0417164300",
        "DE89370400440532013000",
        "GB82WEST12345698765432",
    ] {
        assert_eq!(check_iban(good), Ok(()), "{good}");
    }
    for bad in [
        "BE68539007547035",
        "NL91ABNA0417164301",
        "BE6853900754",
        "1234567890123456",
        "BE68-5390-0754-7034",
    ] {
        assert!(check_iban(bad).is_err(), "{bad}");
    }
}

#[test]
fn the_file_must_be_a_pdf_and_the_vat_within_the_total() {
    let dir = tempfile::tempdir().unwrap();
    assert!(invalid(&args(pdf_in(dir.path(), "factuur.txt"))).contains("does not end in .pdf"));
    build(&args(pdf_in(dir.path(), "FACTUUR.PDF"))).unwrap();
    let mut a = args(pdf_in(dir.path(), "factuur.pdf"));
    a.vat = Cents(13000);
    let err = invalid(&a);
    assert!(
        err.contains("--vat-amount 130.00 exceeds --total 121.00"),
        "{err}"
    );
    assert!(
        !err.contains("off by"),
        "one problem, not a negative line: {err}"
    );
}

#[test]
fn a_plain_vat_rate_is_read_from_the_description() {
    assert_eq!(plain_rate("VAT 21%"), Some(2100));
    assert_eq!(plain_rate("btw 6 %"), Some(600));
    assert_eq!(plain_rate("TVA 5,5%"), Some(550));
    assert_eq!(plain_rate("VAT 0%"), Some(0));
    for other in [
        "VAT reverse-charge (21%)",
        "Import inside the EU (services) (21%)",
        "Input tax",
        "Non-deductible",
        "VAT reverse-charge",
    ] {
        assert_eq!(plain_rate(other), None, "{other}");
    }
}

#[test]
fn the_vat_amount_must_follow_from_the_rates() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(pdf_in(dir.path(), "factuur.pdf"));
    a.gl_account = None;
    a.vat_type = None;
    a.vat_description = None;
    a.total = Cents(11861);
    a.vat = Cents(1861);
    a.lines = vec![
        parse_line("a;612000;33.33;1;VAT 21%").unwrap(),
        parse_line("b;612000;33.33;1;VAT 21%").unwrap(),
        parse_line("c;612000;33.34;2;VAT 6%").unwrap(),
    ];
    // 7.00 + 7.00 + 2.00 = 16.00 expected; 18.61 is off.
    let mut invoice = build(&a).unwrap();
    let mut problems = Vec::new();
    check_vat_amount(&mut invoice, &mut problems);
    assert!(
        problems[0].contains("at their VAT rates it is 16.00 (give or take 3 cents)"),
        "{problems:?}"
    );
    // Within a cent per line passes.
    a.total = Cents(11603);
    a.vat = Cents(1603);
    let mut invoice = build(&a).unwrap();
    problems.clear();
    check_vat_amount(&mut invoice, &mut problems);
    assert!(problems.is_empty(), "{problems:?}");
    // A code without a plain rate skips the check, said so.
    a.lines[2].vat_description = "Input tax".into();
    a.total = Cents(20000);
    a.vat = Cents(10000);
    let mut invoice = build(&a).unwrap();
    check_vat_amount(&mut invoice, &mut problems);
    assert!(problems.is_empty());
    assert!(
        invoice.notes[0].contains("VAT amount not checked: line 3's VAT code 'Input tax'"),
        "{:?}",
        invoice.notes
    );
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

fn archived(id: &str, file: &str, subject: &str, date: &str, amount: &str) -> ArchiveDocument {
    ArchiveDocument {
        subject: subject.into(),
        document_date: format!("{date}T00:00:00"),
        amount: amount.into(),
        ..doc(id, file, "Vercel Inc", "")
    }
}

#[test]
fn archive_documents_match_by_file_name_reference_or_amount_and_date() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(pdf_in(dir.path(), "2026-08-07_vercel_2E81616A-0007.pdf"));
    a.reference = "2E81616A-0007".into();
    a.date = "2026-08-07".into();
    a.total = Cents(2420);
    a.vat = Cents(420);
    let invoice = build(&a).unwrap();
    let found = |docs: &[ArchiveDocument]| archive_matches(&invoice, docs);
    // The same file, whatever amount the archive recognised.
    let same = archived(
        "f7",
        "2026-08-07_VERCEL_2E81616A-0007.PDF",
        "",
        "2026-08-07",
        "17.65",
    );
    assert!(found(&[same.clone(), same])[0].starts_with("same file name: document f7"));
    // Renamed by Yuki or the supplier.
    let renamed = archived(
        "r1",
        "Invoice-2026-08-07_vercel_2E81616A-0007.pdf",
        "",
        "2025-01-01",
        "",
    );
    assert_eq!(found(&[renamed]).len(), 1);
    // The reference in another file name, or in the subject.
    let by_ref = archived(
        "x1",
        "Vercel_2E81616A0007_receipt.pdf",
        "",
        "2025-01-01",
        "",
    );
    assert!(found(&[by_ref])[0].starts_with("reference in its file name or subject"));
    let by_subject = archived(
        "x2",
        "scan.pdf",
        "Vercel invoice 2e81616a 0007",
        "2025-01-01",
        "",
    );
    assert_eq!(found(&[by_subject]).len(), 1);
    // The same amount within three days, under any name.
    let by_amount = archived("x3", "scan.pdf", "", "2026-08-10", "24.20");
    assert!(found(&[by_amount])[0].starts_with("same amount within 3 days"));
    // Not: four days off, another amount, another invoice of the supplier.
    for other in [
        archived("n1", "scan.pdf", "", "2026-08-11", "24.20"),
        archived("n2", "scan.pdf", "", "2026-08-07", "24.21"),
        archived(
            "n3",
            "2026-07-07_vercel_2E81616A-0006.pdf",
            "Vercel 2E81616A-0006",
            "2026-07-07",
            "17.65",
        ),
    ] {
        assert!(found(std::slice::from_ref(&other)).is_empty(), "{other:?}");
    }
}

#[test]
fn file_names_compare_without_extension_case_punctuation_or_invoice_prefix() {
    assert_eq!(file_key("Invoice-2E81616A-0007.PDF"), "2e81616a0007");
    assert_eq!(file_key("Factuur 2026_12.pdf"), "202612");
    assert_eq!(file_key("invoice.pdf"), "invoice");
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
        total_succeeded: Some(u32::from(succeeded)),
        total_failed: Some(u32::from(!succeeded)),
        total_skipped: Some(0),
        invoices: vec![ImportedInvoice {
            succeeded: Some(succeeded),
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
    assert!(hint_text(refused.clone(), true).contains("support.accounting@nmbrs.com"));
    assert_eq!(hint_text(refused.clone(), false), refused);
    assert_eq!(hint_text("Unknown GL".into(), true), "Unknown GL");
    let e = hint(YukiError::Unauthorized(403), true).to_string();
    assert!(e.contains("HTTP 403") && e.contains("Client ID"), "{e}");
}
