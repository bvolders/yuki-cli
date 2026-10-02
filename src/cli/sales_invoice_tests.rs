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

/// [`MINIMAL`] with the due date a booking needs.
fn bookable() -> String {
    format!("due_days = 30\n{MINIMAL}")
}

/// `text` numbered 2026-20, as a prepared invoice is, to be booked as `send`.
fn numbered(text: &str, send: Option<SendMode>) -> Invoice {
    let mut inv = invoice(text, send);
    inv.number = Some("2026-20".into());
    inv
}

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
    let xml = invoice(&bookable(), Some(SendMode::Email)).to_xml();
    assert!(!xml.contains("<Reference>"), "{xml}");
}

#[test]
fn draft_and_send_modes_set_process_and_the_send_flags() {
    let flags = |send| {
        let xml = invoice(&bookable(), send).to_xml();
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
    let sent = invoice(&bookable(), Some(SendMode::Peppol)).preview(None);
    assert!(
        sent.contains(
            "BOOK AND SEND OVER PEPPOL (Process=true, EmailToCustomer=false, SentToPeppol=true)"
        ),
        "{sent}"
    );
    assert!(sent.contains("2026-10-01, due 2026-10-31"), "{sent}");
    assert!(
        invoice(MINIMAL, None)
            .preview(None)
            .contains("due Yuki's default term")
    );
    assert_eq!(
        invoice(&bookable(), Some(SendMode::Email)).question(),
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

fn seller() -> crate::config::Seller {
    toml::from_str(
        r#"
name = "Example Studio"
address = "Kerkstraat 1"
zipcode = "9000"
city = "Gent"
country = "BE"
phone = "0400000000"
enterprise_number = "0123.456.789"
vat_number = "BE0123.456.789"
iban = "BE00000000000000"
"#,
    )
    .unwrap()
}

/// `text` prepared as 2026-20 in administration `a1`: the file's JSON and
/// the invoice it was prepared from, to be booked as `send`.
fn prepared(text: &str, send: SendMode) -> (serde_json::Value, Invoice) {
    let mut inv = numbered(text, None);
    let json = inv.prepared_file(&seller(), "a1");
    inv.send = Some(send);
    (json, inv)
}

#[test]
fn a_prepared_file_books_exactly_what_was_prepared() {
    let (json, original) = prepared(FULL, SendMode::Email);
    // The text a reader (and the PDF) sees, plus what booking needs.
    assert_eq!(json["admin_id"], "a1");
    assert_eq!(json["firm"]["name"], "Example Studio");
    assert_eq!(json["lines"][0]["product_code"], "HOST");
    assert_eq!(json["lines"][0]["vat_description"], "BTW 21%");
    let (back, admin) = Invoice::from_prepared(&json, "p.json", SendMode::Email).unwrap();
    assert_eq!(admin, "a1");
    assert_eq!(back.to_xml(), original.to_xml());
    assert_eq!(back.gross(), original.gross());
    // Reformatting does not change the hash; an edit does.
    let pretty: serde_json::Value =
        serde_json::from_str(&serde_json::to_string_pretty(&json).unwrap()).unwrap();
    assert_eq!(content_hash(&pretty), content_hash(&json));
    let mut edited = json.clone();
    edited["lines"][0]["unit_price"] = "99.00".into();
    assert_ne!(content_hash(&edited), content_hash(&json));
}

#[test]
fn a_prepared_file_needs_a_number_an_administration_and_a_due_date() {
    let (json, _) = prepared(FULL, SendMode::Book);
    for (field, expect) in [
        ("number", "no number"),
        ("admin_id", "names no administration"),
        ("due_date", "needs a due date"),
    ] {
        let mut broken = json.clone();
        broken[field] = serde_json::Value::Null;
        let err = Invoice::from_prepared(&broken, "p.json", SendMode::Book)
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains(expect), "{field}: {err}");
    }
    let err = Invoice::from_prepared(&serde_json::json!({"a": 1}), "p.json", SendMode::Book)
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("p.json is not a prepared invoice"), "{err}");
}

#[test]
fn a_custom_pdf_is_stored_under_the_number_before_the_contact() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("doc.pdf");
    std::fs::write(&path, b"%PDF-1.7 hello").unwrap();
    let (json, _) = prepared(FULL, SendMode::Book);
    let (mut inv, _) = Invoice::from_prepared(&json, "p.json", SendMode::Book).unwrap();
    inv.attach_pdf(&path).unwrap();
    let xml = inv.to_xml();
    // Stored under the number, whatever the local file is called.
    assert!(
        xml.contains("<DocumentFileName>Invoice 2026-20.pdf</DocumentFileName>"),
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
            "custom PDF doc.pdf (14 bytes), stored as Invoice 2026-20.pdf, replaces Yuki's layout; amounts in the PDF must match the lines"
        ),
        "{preview}"
    );
}

#[test]
fn a_pdf_that_is_missing_not_a_pdf_or_too_large_is_rejected() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("doc.pdf");
    std::fs::write(&path, b"PK\x03\x04 a zip").unwrap();
    assert!(
        Pdf::read(&path)
            .unwrap_err()
            .contains("is not a PDF (no %PDF- header)")
    );
    let missing = dir.path().join("missing.pdf");
    assert!(Pdf::read(&missing).unwrap_err().contains("missing.pdf"));
    let mut big = b"%PDF-1.7".to_vec();
    big.resize(PDF_MAX_BYTES + 1, b' ');
    std::fs::write(&path, &big).unwrap();
    assert!(
        Pdf::read(&path)
            .unwrap_err()
            .contains("over the 3.0 MB limit")
    );
}

#[test]
fn book_books_without_sending_and_the_number_is_the_reference() {
    let inv = numbered(&bookable(), Some(SendMode::Book));
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
    let inv = numbered(FULL, None);
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

#[test]
fn an_invoice_file_takes_no_pdf() {
    // A PDF is bound to the prepared invoice it was rendered from.
    let text = format!("pdf = \"rendered.pdf\"\n{MINIMAL}");
    let err = parse(&text, "t", &Overrides::default(), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("unknown field `pdf`"), "{err}");
}
#[test]
fn a_line_remark_goes_under_the_line_and_into_prepare() {
    let text = MINIMAL.replace(
        "vat_type = 1\n",
        "vat_type = 1\nunit = \"u\"\nremarks = \"waarvan overdracht auteursrecht op ontwikkelde software van 25% of €312.50\"\n",
    );
    let inv = invoice(&text, None);
    let xml = inv.to_xml();
    assert_in_order(
        &xml,
        &[
            "InvoiceLine",
            "Description",
            "Remarks",
            "ProductQuantity",
            "Product",
        ],
    );
    assert!(xml.contains("<Remarks>waarvan overdracht auteursrecht op ontwikkelde software van 25% of €312.50</Remarks>"));
    // The unit is for the PDF only.
    assert!(!xml.contains(">u<"), "{xml}");
    let json = inv.prepared();
    assert_eq!(json["lines"][0]["unit"], "u");
    assert!(
        json["lines"][0]["remarks"]
            .as_str()
            .unwrap()
            .starts_with("waarvan")
    );
    assert!(inv.preview(None).contains("line 1 remarks: waarvan"));
}

#[test]
fn per_line_rounding_that_differs_is_flagged() {
    // Three lines of 0.05 at 21%: per rate 0.15 × 21% = 0.03; per line 3 × 0.01.
    let lines = "[[lines]]\ndescription = \"x\"\nprice = 0.05\nvat_percentage = 21\nvat_type = 1\n";
    let text = format!("date = 2026-10-01\n[contact]\ncode = \"C\"\n{lines}{lines}{lines}");
    let inv = invoice(&text, None);
    assert_eq!(inv.vat(), Cents(3));
    assert_eq!(inv.vat_per_line(), Cents(3));
    // 0.07 × 21% = 0.0147 per line (0.01 each, 0.03), per rate 0.21 × 21% = 0.0441 (0.04).
    let lines = lines.replace("0.05", "0.07");
    let text = format!("date = 2026-10-01\n[contact]\ncode = \"C\"\n{lines}{lines}{lines}");
    let inv = invoice(&text, None);
    assert_eq!((inv.vat(), inv.vat_per_line()), (Cents(4), Cents(3)));
    let preview = inv.preview(None);
    assert!(
        preview
            .contains("!! VAT rounded per line would be 0.03 EUR, not 0.04: Yuki may book either"),
        "{preview}"
    );
    assert_eq!(inv.prepared()["totals"]["vat_rounded_per_line"], "0.03");
    assert!(invoice(MINIMAL, None).prepared()["totals"]["vat_rounded_per_line"].is_null());
}

#[test]
fn a_booking_announces_itself_in_one_line() {
    let inv = numbered(&bookable(), Some(SendMode::Book));
    assert_eq!(
        inv.booking_line().unwrap(),
        "BOOKS IMMEDIATELY: 2026-20 code C0042 1512.50 EUR"
    );
    assert!(invoice(MINIMAL, None).booking_line().is_none());
}

#[test]
fn placeholders_fill_in_the_month_and_a_share_of_the_line() {
    let text = r#"
subject = "Consultancy {month} {year} ({month_num})"
date = 2026-03-31

[contact]
code = "C0042"

[[lines]]
description = "Development {month}"
qty = 172.5
price = 100
vat_percentage = 21
vat_type = 1
remarks = "waarvan overdracht auteursrecht op ontwikkelde software van 25% of €{pct_of_net:25}"
"#;
    let invoice = invoice(text, None);
    assert_eq!(
        invoice.subject.as_deref(),
        Some("Consultancy maart 2026 (03)")
    );
    assert_eq!(invoice.lines[0].description, "Development maart");
    assert_eq!(
        invoice.lines[0].remarks.as_deref(),
        Some("waarvan overdracht auteursrecht op ontwikkelde software van 25% of €4.312,50")
    );
    // The filled text is what prepare shows and create sends.
    assert!(invoice.to_xml().contains("€4.312,50"));
    assert_eq!(invoice.prepared()["subject"], "Consultancy maart 2026 (03)");
}

#[test]
fn a_share_of_the_net_rounds_half_away_from_zero() {
    let net = Some(Cents(1_001)); // 10.01
    assert_eq!(fill("{pct_of_net:25}", "2026-10-31", net).unwrap(), "2,50"); // 2.5025
    assert_eq!(fill("{pct_of_net:50}", "2026-10-31", net).unwrap(), "5,01"); // 5.005
    assert_eq!(
        fill("{pct_of_net:12.5}", "2026-10-31", Some(Cents(100_000))).unwrap(),
        "125,00"
    );
    assert_eq!(
        fill("{pct_of_net:100}", "2026-10-31", Some(Cents(123_456_789))).unwrap(),
        "1.234.567,89"
    );
}

#[test]
fn unknown_or_misplaced_placeholders_are_errors() {
    for (text, expect) in [
        ("{maand}", "unknown placeholder {maand}"),
        ("{month", "without"),
        ("{pct_of_net:x}", "0 to 100"),
        ("{pct_of_net:101}", "0 to 100"),
    ] {
        let err = fill(text, "2026-10-31", Some(Cents(100))).unwrap_err();
        assert!(err.contains(expect), "{text}: {err}");
    }
    let err = fill("{pct_of_net:25}", "2026-10-31", None).unwrap_err();
    assert!(err.contains("only works in a line"), "{err}");
    let week = MINIMAL.replace(
        "description = \"Consultancy\"",
        "description = \"Consultancy {week}\"",
    );
    let err = problems(&week, &Overrides::default());
    assert!(
        err.contains("lines[1].description: unknown placeholder {week}"),
        "{err}"
    );
    let err = problems(
        &format!("subject = \"{{pct_of_net:25}}\"\n{MINIMAL}"),
        &Overrides::default(),
    );
    assert!(
        err.contains("subject: {pct_of_net:…} only works in a line"),
        "{err}"
    );
}

#[test]
fn a_booking_needs_a_due_date() {
    for send in [SendMode::Book, SendMode::Email, SendMode::Peppol] {
        let err = parse(MINIMAL, "t", &Overrides::default(), Some(send))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("a booked invoice needs a due date: give due_days or due_date"),
            "{err}"
        );
    }
    // A draft may leave it to Yuki; due_date works as well as due_days.
    assert!(parse(MINIMAL, "t", &Overrides::default(), None).is_ok());
    let dated = format!("due_date = 2026-11-01\n{MINIMAL}");
    assert!(parse(&dated, "t", &Overrides::default(), Some(SendMode::Book)).is_ok());
}

#[test]
fn a_vat_mention_is_passed_through_and_missed_at_zero_percent() {
    let zero = MINIMAL.replace("vat_percentage = 21", "vat_percentage = 0");
    let inv = invoice(&zero, None);
    assert!(inv.lacks_vat_mention());
    assert!(
        inv.preview(None)
            .contains("0% VAT and there is no vat_mention")
    );
    let mentioned = format!("vat_mention = \"Btw verlegd\"\n{zero}");
    let inv = invoice(&mentioned, None);
    assert!(!inv.lacks_vat_mention());
    assert!(!inv.preview(None).contains("no vat_mention"));
    assert_eq!(inv.prepared()["vat_mention"], "Btw verlegd");
    // Not part of what Yuki receives.
    assert!(!inv.to_xml().contains("verlegd"));
    assert!(!invoice(MINIMAL, None).lacks_vat_mention());
}

#[test]
fn the_seller_is_the_prepared_firm() {
    let seller: crate::config::Seller = toml::from_str(
        r#"
name = "Studio Maak"
address = "Meidoornlaan 13"
zipcode = "2920"
city = "Kalmthout"
country = "BE"
phone = "0491370721"
enterprise_number = "0748.926.706"
vat_number = "BE0748.926.706"
iban = "BE35733070723437"
"#,
    )
    .unwrap();
    let json = invoice(MINIMAL, None).prepared_for(Some(&seller));
    assert_eq!(json["firm"]["name"], "Studio Maak");
    assert_eq!(json["firm"]["enterprise_number"], "0748.926.706");
    assert_eq!(json["firm"]["iban"], "BE35733070723437");
    for unset in ["bic", "legal_form", "rpr"] {
        assert!(json["firm"][unset].is_null(), "{unset}");
    }
    assert!(invoice(MINIMAL, None).prepared_for(None)["firm"].is_null());
}

fn import_of(reference: &str, processed: bool, email_sent: bool) -> SalesInvoicesImport {
    SalesInvoicesImport {
        total_succeeded: 1,
        total_failed: 0,
        total_skipped: 0,
        invoices: vec![crate::client::sales::ImportedInvoice {
            succeeded: true,
            processed,
            email_sent,
            reference: reference.into(),
            subject: "Consultancy".into(),
            message: String::new(),
        }],
    }
}

#[test]
fn the_reference_yuki_booked_must_be_the_number_sent() {
    let inv = numbered(&bookable(), Some(SendMode::Book));
    assert_eq!(
        verdict(&import_of("2026-20", true, false), &inv),
        Verdict::Done
    );
    // Padding aside, it is the same number.
    assert_eq!(
        verdict(&import_of("2026-020", true, false), &inv),
        Verdict::Done
    );
    for (reference, shown) in [("2026-21", "reference 2026-21"), ("", "no reference")] {
        match verdict(&import_of(reference, true, false), &inv) {
            Verdict::ReferenceMismatch(m) => {
                assert!(m.contains(&format!("with {shown}, not 2026-20")), "{m}");
                assert!(
                    m.contains("Email: not requested. Peppol: not requested."),
                    "{m}"
                );
                assert!(m.contains("--resolve 2026-20 --as booked"), "{m}");
                assert!(m.contains("--resolve 2026-20 --as rejected"), "{m}");
            }
            other => panic!("{reference}: {other:?}"),
        }
    }
    // Yuki numbers an invoice sent without one: nothing to compare.
    let yuki_numbered = invoice(&bookable(), Some(SendMode::Book));
    assert_eq!(
        verdict(&import_of("2026-21", true, false), &yuki_numbered),
        Verdict::Done
    );
}

#[test]
fn a_booking_without_the_prompt_names_its_number() {
    assert!(check_confirm(Some("2026-20"), Some("2026-20"), true).is_ok());
    assert!(
        check_confirm(Some("2026-20"), None, false).is_ok(),
        "the prompt asks"
    );
    let err = check_confirm(Some("2026-20"), None, true)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("needs --confirm <number>: pass --confirm 2026-20"),
        "{err}"
    );
    let err = check_confirm(Some("2026-20"), Some("2026-21"), false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("is not the invoice number 2026-20"), "{err}");
    assert!(check_confirm(None, Some("2026-20"), true).is_err());
    assert!(check_confirm(None, None, true).is_err());
    // The prompt itself names the number.
    let inv = numbered(&bookable(), Some(SendMode::Email));
    assert_eq!(
        inv.question(),
        "Book invoice 2026-20 in Yuki and send it by email?"
    );
}

#[test]
fn doubled_braces_are_literal() {
    let date = "2026-10-31";
    assert_eq!(
        fill("{{month}} is {month}", date, None).unwrap(),
        "{month} is oktober"
    );
    assert_eq!(fill("a }} b {{", date, None).unwrap(), "a } b {");
    assert_eq!(fill("{{{year}}}", date, None).unwrap(), "{2026}");
    let err = fill("a } b", date, None).unwrap_err();
    assert!(
        err.contains("'}' without '{'") && err.contains("}}"),
        "{err}"
    );
}

#[test]
fn notes_remarks_and_the_vat_mention_take_placeholders_too() {
    let text = format!(
        "notes = \"Prestaties {{month}} {{year}}\"\nremarks = \"run {{month_num}}\"\nvat_mention = \"Btw verlegd ({{year}})\"\n{MINIMAL}"
    );
    let inv = invoice(&text, None);
    assert_eq!(inv.notes.as_deref(), Some("Prestaties oktober 2026"));
    assert_eq!(inv.remarks.as_deref(), Some("run 10"));
    assert_eq!(inv.vat_mention.as_deref(), Some("Btw verlegd (2026)"));
    let err = problems(
        &format!("notes = \"{{pct_of_net:25}}\"\n{MINIMAL}"),
        &Overrides::default(),
    );
    assert!(
        err.contains("notes: {pct_of_net:…} only works in a line"),
        "{err}"
    );
}

#[test]
fn a_prepared_invoice_without_a_currency_sends_none() {
    let (json, original) = prepared(&bookable(), SendMode::Book);
    assert!(json["currency"].is_null());
    let (back, _) = Invoice::from_prepared(&json, "p.json", SendMode::Book).unwrap();
    assert!(!back.to_xml().contains("<Currency>"), "{}", back.to_xml());
    assert_eq!(back.to_xml(), original.to_xml());
}
