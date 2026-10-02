use super::*;

/// A full invoice: a new contact, two lines at two rates.
const FULL: &str = r#"
subject = "Hosting & support"
date = 2026-10-01
due_days = 30
layout = "Standard"
currency = "eur"
payment_method = "ElectronicTransfer"
remarks = "internal note"
notes = "Thank you"

[contact]
code = "C0042"
name = "Example Customer BV"
country = "be"
address = "Kerkstraat 1"
address_2 = "bus 2"
zipcode = "9000"
city = "Gent"
vat_number = "BE0123456789"
email = "billing@example.be"
type = "Company"

[[lines]]
description = "Managed hosting"
qty = 1
price = 100.00
vat_percentage = 21
vat_type = 1
vat_description = "BTW 21%"
gl_account = "700000"
product_code = "HOST"

[[lines]]
description = "Books"
qty = 3
price = "12.35"
vat_percentage = 6
vat_type = 2
"#;

/// The smallest valid invoice: an existing contact by code, one line.
const MINIMAL: &str = r#"
date = "2026-10-01"

[contact]
code = "C0042"

[[lines]]
description = "Consultancy"
price = 1250
vat_percentage = 21
vat_type = 1
"#;

fn invoice(text: &str, send: Option<SendMode>) -> Invoice {
    parse(text, "test.toml", &Overrides::default(), send).expect("valid invoice")
}

fn problems(text: &str, overrides: &Overrides<'_>) -> String {
    parse(text, "test.toml", overrides, None)
        .expect_err("invalid invoice")
        .to_string()
}

/// Assert each tag opens after the previous one: the XSD's `xs:sequence` order.
fn assert_in_order(xml: &str, tags: &[&str]) {
    let mut last = 0;
    for tag in tags {
        let open = format!("<{tag}>");
        let at = xml[last..]
            .find(&open)
            .unwrap_or_else(|| panic!("{open} missing or out of order in:\n{xml}"));
        last += at + open.len();
    }
}

#[test]
fn a_minimal_draft_is_exactly_this_document() {
    let xml = invoice(MINIMAL, None).to_xml();
    let expected = r#"<SalesInvoices xmlns="urn:xmlns:http://www.theyukicompany.com:salesinvoices" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <SalesInvoice>
    <Process>false</Process>
    <EmailToCustomer>false</EmailToCustomer>
    <SentToPeppol>false</SentToPeppol>
    <Date>2026-10-01</Date>
    <Contact>
      <ContactCode>C0042</ContactCode>
    </Contact>
    <InvoiceLines>
      <InvoiceLine>
        <Description>Consultancy</Description>
        <ProductQuantity>1</ProductQuantity>
        <Product>
          <Description>Consultancy</Description>
          <SalesPrice>1250.00</SalesPrice>
          <VATPercentage>21</VATPercentage>
          <VATIncluded>false</VATIncluded>
          <VATType>1</VATType>
        </Product>
      </InvoiceLine>
    </InvoiceLines>
  </SalesInvoice>
</SalesInvoices>"#;
    assert_eq!(xml, expected);
}

#[test]
fn elements_follow_the_xsd_sequence_order() {
    let xml = invoice(FULL, Some(SendMode::Both)).to_xml();
    assert!(xml.starts_with(&format!(r#"<SalesInvoices xmlns="{NAMESPACE}" xmlns:xsi="#)));
    assert_in_order(
        &xml,
        &[
            "SalesInvoice",
            "Subject",
            "PaymentMethod",
            "Process",
            "EmailToCustomer",
            "SentToPeppol",
            "Layout",
            "Notes",
            "Date",
            "DueDate",
            "Currency",
            "Remarks",
            "Contact",
            "InvoiceLines",
        ],
    );
    assert_in_order(
        &xml,
        &[
            "ContactCode",
            "FullName",
            "CountryCode",
            "City",
            "Zipcode",
            "AddressLine_1",
            "AddressLine_2",
            "EmailAddress",
            "VATNumber",
            "ContactType",
        ],
    );
    assert_in_order(
        &xml,
        &[
            "InvoiceLine",
            "Description",
            "ProductQuantity",
            "Product",
            "Description",
            "Reference",
            "SalesPrice",
            "VATPercentage",
            "VATIncluded",
            "VATType",
            "VATDescription",
            "GLAccountCode",
        ],
    );
    // Normalised values, dot decimals.
    for fragment in [
        "<Currency>EUR</Currency>",
        "<CountryCode>BE</CountryCode>",
        "<ContactType>Company</ContactType>",
        "<DueDate>2026-10-31</DueDate>",
        "<ProductQuantity>3</ProductQuantity>",
        "<SalesPrice>12.35</SalesPrice>",
        "<VATPercentage>6</VATPercentage>",
        "<Reference>HOST</Reference>",
    ] {
        assert!(xml.contains(fragment), "{fragment} missing:\n{xml}");
    }
}

#[test]
fn the_invoice_carries_no_reference_so_yuki_numbers_it() {
    let xml = invoice(MINIMAL, Some(SendMode::Email)).to_xml();
    assert!(!xml.contains("<Reference>"), "{xml}");
}

#[test]
fn draft_and_send_modes_set_process_and_the_send_flags() {
    let flags = |send| {
        let xml = invoice(MINIMAL, send).to_xml();
        ["Process", "EmailToCustomer", "SentToPeppol"]
            .map(|tag| xml.contains(&format!("<{tag}>true</{tag}>")))
    };
    assert_eq!(flags(None), [false, false, false]);
    assert_eq!(flags(Some(SendMode::Email)), [true, true, false]);
    assert_eq!(flags(Some(SendMode::Peppol)), [true, false, true]);
    assert_eq!(flags(Some(SendMode::Both)), [true, true, true]);
}

#[test]
fn every_user_value_is_escaped() {
    let text = r#"
subject = "Fish & <Chips> \"quoted\" 'single'"
date = 2026-10-01
notes = "a < b"
[contact]
name = "Smith & Jones <Ltd>"
country = "BE"
[[lines]]
description = "R&D </Description><Evil>"
price = 1
vat_percentage = 21
vat_type = 1
gl_account = "70&0"
"#;
    let xml = invoice(text, None).to_xml();
    assert!(xml.contains(
        "<Subject>Fish &amp; &lt;Chips&gt; &quot;quoted&quot; &apos;single&apos;</Subject>"
    ));
    assert!(xml.contains("<FullName>Smith &amp; Jones &lt;Ltd&gt;</FullName>"));
    assert!(xml.contains("<Description>R&amp;D &lt;/Description&gt;&lt;Evil&gt;</Description>"));
    assert!(xml.contains("<GLAccountCode>70&amp;0</GLAccountCode>"));
    assert!(xml.contains("<Notes>a &lt; b</Notes>"));
    assert!(!xml.contains("<Evil>"), "{xml}");
}

#[test]
fn totals_are_exact_and_vat_is_rounded_per_rate() {
    let inv = invoice(FULL, None);
    // 100.00 at 21%, 3 × 12.35 = 37.05 at 6%.
    assert_eq!(inv.net(), Cents(13_705));
    assert_eq!(
        inv.vat_rates(),
        vec![
            VatRate {
                percentage: 600,
                base: Cents(3_705),
                vat: Cents(222), // 2.223
            },
            VatRate {
                percentage: 2_100,
                base: Cents(10_000),
                vat: Cents(2_100),
            },
        ]
    );
    assert_eq!(inv.gross(), Cents(13_705 + 222 + 2_100));

    // 2.5 × 85.10 = 212.75; with 100.00, 21% of 312.75 = 65.6775 → 65.68.
    let text = MINIMAL.replace("price = 1250", "price = 100").replace(
        "vat_type = 1\n",
        "vat_type = 1\n\n[[lines]]\ndescription = \"Support\"\nqty = 2.5\nprice = 85.10\nvat_percentage = 21\nvat_type = 1\n",
    );
    let inv = invoice(&text, None);
    assert_eq!(inv.lines[1].net(), Cents(21_275));
    assert_eq!(inv.vat(), Cents(6_568));
    assert_eq!(inv.gross(), Cents(37_843));
}

#[test]
fn command_line_overrides_replace_the_file_values() {
    let overrides = Overrides {
        qty: Some(parse_quantity("7.5").unwrap()),
        price: Some(parse_price("80").unwrap()),
        date: Some("2026-11-01"),
        subject: Some("November"),
        pdf: None,
        number: None,
    };
    let text = format!("subject = \"October\"\ndue_days = 14\n{MINIMAL}");
    let inv = parse(&text, "t", &overrides, None).unwrap();
    assert_eq!(inv.subject.as_deref(), Some("November"));
    assert_eq!(inv.date, "2026-11-01");
    // due_days counts from the overridden date.
    assert_eq!(inv.due_date.as_deref(), Some("2026-11-15"));
    assert_eq!(inv.lines[0].net(), Cents(60_000));
}

#[test]
fn a_missing_date_defaults_to_today() {
    let inv = invoice(&MINIMAL.replace("date = \"2026-10-01\"", ""), None);
    assert_eq!(inv.date, today());
}

#[test]
fn a_new_contact_without_a_country_is_rejected() {
    let text = MINIMAL.replace("code = \"C0042\"", "name = \"New Customer\"");
    let err = problems(&text, &Overrides::default());
    assert!(
        err.contains("contact.country is required for a contact without a code"),
        "{err}"
    );
}

#[test]
fn every_problem_is_reported_at_once() {
    let text = r#"
due_date = "2026-09-01"
due_days = 3
currency = "euro"
[contact]
country = "Belgium"
type = "robot"
[[lines]]
qty = 0
price = "1.005"
vat_percentage = 121
[[lines]]
description = "ok"
price = 1
vat_percentage = "x"
vat_type = 1
"#;
    let err = problems(text, &Overrides::default());
    for expected in [
        "give due_date or due_days, not both",
        "currency 'EURO' is not an ISO 4217 code",
        "contact.country 'BELGIUM' is not an ISO 3166-1 alpha-2 code",
        "contact needs `code` (an existing Yuki contact) or `name` and `country`",
        "contact.type 'robot' must be \"company\" or \"person\"",
        "lines[1].description is required",
        "lines[1].qty: a quantity cannot be zero",
        "lines[1].price: '1.005' has more than 2 decimals",
        "lines[1].vat_percentage must be between 0 and 100",
        "lines[1].vat_type is required",
        "lines[2].vat_percentage: 'x' is not a decimal number",
    ] {
        assert!(err.contains(expected), "missing {expected:?} in:\n{err}");
    }
}

#[test]
fn structural_problems_are_reported() {
    let err = problems("subject = \"x\"\n", &Overrides::default());
    assert!(err.contains("[contact] is missing"), "{err}");
    assert!(err.contains("an invoice needs at least one line"), "{err}");

    let err = problems(
        &MINIMAL.replace("vat_type", "vat_typ"),
        &Overrides::default(),
    );
    assert!(err.contains("unknown field `vat_typ`"), "{err}");

    let err = problems(
        &MINIMAL.replace("date = \"2026-10-01\"", "date = \"2026-02-30\""),
        &Overrides::default(),
    );
    assert!(err.contains("date: '2026-02-30' is not a date"), "{err}");

    let err = problems(
        &MINIMAL.replace(
            "date = \"2026-10-01\"",
            "date = \"2026-10-01\"\ndue_date = 2026-09-30",
        ),
        &Overrides::default(),
    );
    assert!(err.contains("before the invoice date"), "{err}");
}

#[test]
fn qty_and_price_overrides_need_a_single_line() {
    let text = FULL;
    let overrides = Overrides {
        qty: Some(parse_quantity("2").unwrap()),
        ..Default::default()
    };
    let err = problems(text, &overrides);
    assert!(
        err.contains("--qty applies to a single-line invoice; this one has 2 lines"),
        "{err}"
    );
}

#[test]
fn control_characters_cannot_reach_the_xml() {
    let err = problems(
        &MINIMAL.replace("Consultancy", "Bell\\u0007"),
        &Overrides::default(),
    );
    assert!(
        err.contains("lines[1].description contains a character XML cannot carry"),
        "{err}"
    );
}

#[test]
fn command_line_values_are_parsed_exactly() {
    assert_eq!(parse_quantity("7.5"), Ok(Quantity(75_000)));
    assert!(parse_quantity("0").is_err());
    assert!(parse_quantity("1.00001").is_err());
    assert_eq!(parse_price("1250.5"), Ok(Cents(125_050)));
    assert!(parse_price("12,50").is_err());
    assert_eq!(parse_date("2026-10-01").as_deref(), Ok("2026-10-01"));
    assert!(parse_date("2026-10-01T00:00").is_err());
    assert!(parse_date("01/10/2026").is_err());
}

#[test]
fn template_names_cannot_leave_the_template_directory() {
    assert!(
        template_path("acme-hosting_2")
            .unwrap()
            .ends_with("invoices/acme-hosting_2.toml")
    );
    for bad in ["", "../config", "a/b", "a.b", "x y"] {
        assert!(template_path(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn the_preview_states_customer_lines_totals_and_mode() {
    let draft = invoice(FULL, None).preview(Some("example"));
    for expected in [
        "Sales invoice from test.toml",
        "Administration example",
        "DRAFT (Process=false): lands in \"To be sent\"",
        "Example Customer BV (code C0042)",
        "Kerkstraat 1, bus 2, 9000, Gent, BE, BE0123456789, billing@example.be",
        "Managed hosting",
        "Gross total",
        "160.27 EUR",
        "(6% on 37.05)",
        "2026-10-01, due 2026-10-31",
    ] {
        assert!(
            draft.contains(expected),
            "missing {expected:?} in:\n{draft}"
        );
    }
    let sent = invoice(MINIMAL, Some(SendMode::Peppol)).preview(None);
    assert!(
        sent.contains(
            "BOOK AND SEND OVER PEPPOL (Process=true, EmailToCustomer=false, SentToPeppol=true)"
        ),
        "{sent}"
    );
    assert!(sent.contains("due Yuki's default term"), "{sent}");
    assert_eq!(
        invoice(MINIMAL, Some(SendMode::Email)).question(),
        "Book this invoice in Yuki and send it by email?"
    );
}

#[test]
fn only_an_explicit_yes_confirms() {
    for (answer, confirmed) in [
        ("y\n", true),
        ("YES\n", true),
        (" yes \n", true),
        ("\n", false),
        ("n\n", false),
        ("yep\n", false),
        ("", false),
    ] {
        let mut prompt = Vec::new();
        let got = ask("Create?", &mut answer.as_bytes(), &mut prompt).unwrap();
        assert_eq!(got, confirmed, "{answer:?}");
        assert_eq!(String::from_utf8(prompt).unwrap(), "Create? [y/N] ");
    }
}

#[test]
fn malformed_dates_and_terms_are_rejected() {
    for bad in ["2026-1-012", "+026-01-01", "2026-+2-01", "2026/10/01"] {
        assert!(parse_date(bad).is_err(), "{bad:?}");
    }
    let err = problems(
        &MINIMAL.replace(
            "date = \"2026-10-01\"",
            "date = \"2026-10-01\"\ndue_days = 9223372036854775807",
        ),
        &Overrides::default(),
    );
    assert!(err.contains("due_days must be between 0 and 3650"), "{err}");
}

#[test]
fn amounts_beyond_the_xsd_digits_are_rejected() {
    assert!(parse_quantity("10000000000").is_err());
    assert!(parse_quantity("9999999999.9999").is_ok());
    assert!(parse_price("10000000000").is_err());
    let err = problems(
        &MINIMAL.replace("price = 1250", "price = 9999999999.99\nqty = 1000000000"),
        &Overrides::default(),
    );
    assert!(
        err.contains("exceeds Yuki's 10 integer digits for a line amount"),
        "{err}"
    );
}

#[test]
fn zero_prices_and_non_positive_totals_are_rejected() {
    let err = problems(
        &MINIMAL.replace("price = 1250", "price = 0"),
        &Overrides::default(),
    );
    assert!(err.contains("price cannot be 0"), "{err}");
    let err = problems(
        &MINIMAL.replace("price = 1250", "price = -5"),
        &Overrides::default(),
    );
    assert!(err.contains("the invoice total is -5.00"), "{err}");
}

#[test]
fn xml_non_characters_and_an_empty_subject_override_are_rejected() {
    let err = problems(
        &MINIMAL.replace("Consultancy", "A\\uFFFEB"),
        &Overrides::default(),
    );
    assert!(err.contains("a character XML cannot carry"), "{err}");
    let overrides = Overrides {
        subject: Some(" "),
        ..Default::default()
    };
    assert!(problems(MINIMAL, &overrides).contains("--subject cannot be empty"));
}

#[test]
fn emailing_a_new_contact_needs_its_address() {
    let text = MINIMAL.replace("code = \"C0042\"", "name = \"New\"\ncountry = \"BE\"");
    let err = parse(&text, "t", &Overrides::default(), Some(SendMode::Email))
        .unwrap_err()
        .to_string();
    assert!(err.contains("--send email needs contact.email"), "{err}");
    assert!(parse(&text, "t", &Overrides::default(), None).is_ok());
}

/// Load `file` booked without sending and numbered 2026-20, as a PDF needs.
fn load_booked(file: PathBuf, overrides: Overrides<'_>) -> Result<Invoice, YukiError> {
    let number = NumberRequest::Given("2026-20".into());
    let overrides = Overrides {
        number: Some(&number),
        ..overrides
    };
    load(&Source::File(file), &overrides, Some(SendMode::Book))
}

/// Write `invoice.toml` (the FULL invoice) naming `pdf = "doc.pdf"`, and
/// `doc.pdf` with `bytes`.
fn invoice_with_pdf(bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("doc.pdf"), bytes).unwrap();
    let file = dir.path().join("invoice.toml");
    std::fs::write(&file, format!("pdf = \"doc.pdf\"\n{FULL}")).unwrap();
    (dir, file)
}

#[test]
fn a_custom_pdf_is_read_relative_to_the_file_and_embedded_before_the_contact() {
    let (_dir, file) = invoice_with_pdf(b"%PDF-1.7 hello");
    let inv = load_booked(file, Overrides::default()).unwrap();
    let xml = inv.to_xml();
    assert!(
        xml.contains("<DocumentFileName>doc.pdf</DocumentFileName>"),
        "{xml}"
    );
    let base64 = BASE64.encode(b"%PDF-1.7 hello");
    assert!(
        xml.contains(&format!("<DocumentBase64>{base64}</DocumentBase64>")),
        "{xml}"
    );
    assert_in_order(
        &xml,
        &[
            "Currency",
            "Remarks",
            "DocumentFileName",
            "DocumentBase64",
            "Contact",
        ],
    );
    // The lines stay: Yuki books from them.
    assert!(xml.contains("<InvoiceLines>"));

    let display = inv.to_display_xml();
    assert!(!display.contains(&base64), "{display}");
    assert!(
        display.contains("<DocumentBase64><!-- 20 bytes base64 (14-byte PDF) --></DocumentBase64>"),
        "{display}"
    );
    let preview = inv.preview(None);
    assert!(
        preview.contains(
            "custom PDF doc.pdf (14 bytes) replaces Yuki's layout; amounts in the PDF must match the lines"
        ),
        "{preview}"
    );
}

#[test]
fn the_pdf_flag_replaces_the_file_value() {
    let (dir, file) = invoice_with_pdf(b"%PDF-1.4");
    let other = dir.path().join("other.pdf");
    std::fs::write(&other, b"%PDF-1.4 other").unwrap();
    let overrides = Overrides {
        pdf: Some(&other),
        ..Default::default()
    };
    let inv = load_booked(file, overrides).unwrap();
    assert_eq!(inv.pdf.unwrap().name, "other.pdf");
}

#[test]
fn a_pdf_that_is_missing_not_a_pdf_or_too_large_is_rejected() {
    let (_dir, file) = invoice_with_pdf(b"PK\x03\x04 a zip");
    let err = load_booked(file, Overrides::default())
        .unwrap_err()
        .to_string();
    assert!(err.contains("is not a PDF (no %PDF- header)"), "{err}");

    let (dir, file) = invoice_with_pdf(b"%PDF-1.7");
    std::fs::remove_file(dir.path().join("doc.pdf")).unwrap();
    let err = load_booked(file, Overrides::default())
        .unwrap_err()
        .to_string();
    assert!(err.contains("pdf: ") && err.contains("doc.pdf"), "{err}");

    let mut big = b"%PDF-1.7".to_vec();
    big.resize(PDF_MAX_BYTES + 1, b' ');
    let (_dir, file) = invoice_with_pdf(&big);
    let err = load_booked(file, Overrides::default())
        .unwrap_err()
        .to_string();
    assert!(err.contains("over the 3.0 MB limit"), "{err}");
}

#[test]
fn a_pdf_name_without_the_extension_gets_one() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("factuur-2026-10");
    std::fs::write(&path, b"%PDF-1.7").unwrap();
    assert_eq!(Pdf::read(&path).unwrap().name, "factuur-2026-10.pdf");
    let upper = dir.path().join("INVOICE.PDF");
    std::fs::write(&upper, b"%PDF-1.7").unwrap();
    assert_eq!(Pdf::read(&upper).unwrap().name, "INVOICE.PDF");
}

#[test]
fn a_template_cannot_carry_a_pdf_but_an_invoice_file_can() {
    let text = format!("pdf = \"doc.pdf\"\n{MINIMAL}");
    let err = parse_at(
        &text,
        "template",
        Path::new("."),
        false,
        &Overrides::default(),
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("a template can't carry a PDF; pass --pdf per invoice"),
        "{err}"
    );
}

#[test]
fn a_pdf_needs_a_booked_and_numbered_invoice() {
    let (dir, file) = invoice_with_pdf(b"%PDF-1.7");
    let err = load(&Source::File(file), &Overrides::default(), None)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(
            "Yuki only accepts a custom PDF on a booked invoice: add --send email|peppol|both (or --book)"
        ),
        "{err}"
    );
    assert!(err.contains("a custom PDF needs --number"), "{err}");
    drop(dir);
}

#[test]
fn book_books_without_sending_and_the_number_is_the_reference() {
    let number = NumberRequest::Given("2026-20".into());
    let overrides = Overrides {
        number: Some(&number),
        ..Default::default()
    };
    let inv = parse(MINIMAL, "t", &overrides, Some(SendMode::Book)).unwrap();
    let xml = inv.to_xml();
    for fragment in [
        "<Process>true</Process>",
        "<EmailToCustomer>false</EmailToCustomer>",
        "<SentToPeppol>false</SentToPeppol>",
    ] {
        assert!(xml.contains(fragment), "{fragment}: {xml}");
    }
    assert_in_order(&xml, &["SalesInvoice", "Reference", "Process"]);
    assert!(xml.contains("<Reference>2026-20</Reference>"));
    let preview = inv.preview(None);
    assert!(preview.contains("BOOK WITHOUT SENDING"), "{preview}");
    assert!(
        preview.contains(
            "BOOKS IMMEDIATELY and fixes the invoice number: there is no draft to review in Yuki"
        ),
        "{preview}"
    );
    assert!(preview.contains("Number         2026-20"), "{preview}");
    // A draft says it has no number yet, and has no warning.
    let draft = invoice(MINIMAL, None).preview(None);
    assert!(!draft.contains("BOOKS IMMEDIATELY"), "{draft}");
    assert!(draft.contains("none yet"), "{draft}");
}

#[test]
fn prepared_json_carries_the_figures_create_sends() {
    let number = NumberRequest::Given("2026-20".into());
    let overrides = Overrides {
        number: Some(&number),
        ..Default::default()
    };
    let inv = parse(FULL, "t", &overrides, None).unwrap();
    let json = inv.prepared();
    assert_eq!(json["number"], "2026-20");
    assert_eq!(json["date"]["iso"], "2026-10-01");
    assert_eq!(json["date"]["text"], "1 oktober 2026");
    assert_eq!(json["due_date"]["text"], "31 oktober 2026");
    assert_eq!(json["customer"]["vat_number"], "BE0123456789");
    assert_eq!(json["customer"]["city"], "Gent");
    assert_eq!(json["lines"][1]["qty"], "3");
    assert_eq!(json["lines"][1]["unit_price"], "12.35");
    assert_eq!(json["lines"][1]["net"], "37.05");
    assert_eq!(json["totals"]["net"], inv.net().to_string());
    assert_eq!(json["totals"]["vat"], inv.vat().to_string());
    assert_eq!(json["totals"]["gross"], inv.gross().to_string());
    assert_eq!(json["totals"]["by_rate"][0]["vat_percentage"], "6");
    assert_eq!(json["totals"]["by_rate"][0]["vat"], "2.22");
    assert_eq!(json["payment_reference"], "+++202/6000/02014+++");
    // Without a number there is no reference either.
    let unnumbered = invoice(FULL, None).prepared();
    assert!(unnumbered["number"].is_null() && unnumbered["payment_reference"].is_null());
}
