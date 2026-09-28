// Tests for `check matches`. Names, amounts and references are made up.

use super::*;
use crate::client::accounting::TransactionType;

const CODA_CARD: &str = "Kaarten : Betaling met debetkaart binnen eurozone | Netto \
    bedrag: 12,340 : | Debet ATM/POS - Gemaskeerde PAN of kaartnummer: 0000000000000000";
const CODA_TRANSFER_TO_PERSON: &str = "Binnenlandse overschrijvingen - SEPA credit transfers : \
    Enkelvoudige overschrijving | Netto bedrag: 165,000 : Overschrijving | JANSSENS-PEETERS";

fn tx(
    id: &str,
    date: &str,
    amount: &str,
    description: &str,
    contact: &str,
) -> GlTransactionWithContact {
    GlTransactionWithContact {
        id: id.into(),
        date: date.into(),
        description: description.into(),
        amount: amount.into(),
        contact_name: contact.into(),
        ..Default::default()
    }
}

/// A creditors-account line: "9" purchase document, "0" bank line.
fn ledger_line(
    kind: &str,
    id: &str,
    date: &str,
    amount: &str,
    contact: &str,
) -> GlTransactionWithContact {
    let mut t = tx(id, date, amount, "", contact);
    t.transaction_type = TransactionType::from_code(kind);
    t
}

fn item(contact: &str, date: &str, open: &str) -> OutstandingItem {
    OutstandingItem {
        contact_name: contact.into(),
        description: format!("Factuur van {contact}"),
        date: date.into(),
        amount: open.into(),
        open_amount: open.into(),
        country: "BE".into(),
        ..Default::default()
    }
}

fn foreign_invoice(contact: &str, date: &str, open: &str) -> OpenInvoice {
    let mut item = item(contact, date, open);
    item.country = "US".into();
    OpenInvoice::from_item(&item).unwrap()
}

fn invoice(contact: &str, date: &str, open: &str) -> OpenInvoice {
    OpenInvoice::from_item(&item(contact, date, open)).unwrap()
}

fn payment(id: &str, date: &str, amount: &str, supplier: Option<&str>) -> Payment {
    Payment {
        id: id.into(),
        date: date.into(),
        amount: Cents::parse(amount).unwrap(),
        bank: "550000".into(),
        counterparty: supplier.unwrap_or("Kaarten").into(),
        supplier: supplier.map(str::to_string),
        on_ledger: supplier.is_some(),
    }
}

fn confidence_of(result: &Matches, invoice: usize) -> Option<Confidence> {
    result.suggestions[invoice].as_ref().map(|s| s.confidence)
}

#[test]
fn exact_amount_supplier_and_close_date_is_high() {
    let invoices = vec![invoice("Supplier A", "2026-04-24", "1240.50")];
    let payments = vec![payment(
        "p1",
        "2026-03-30",
        "1240.50",
        Some("Supplier A NV"),
    )];
    let result = suggest(&invoices, &payments);
    let s = result.suggestions[0].as_ref().unwrap();
    assert_eq!(s.confidence, Confidence::High);
    assert_eq!(s.payments, vec![0]);
    assert_eq!(s.days, 25);
}

#[test]
fn exact_amount_and_supplier_far_apart_is_medium() {
    let invoices = vec![invoice("Supplier A", "2026-06-30", "50.00")];
    let payments = vec![payment("p1", "2026-04-20", "50.00", Some("Supplier A"))];
    assert_eq!(
        confidence_of(&suggest(&invoices, &payments), 0),
        Some(Confidence::Medium)
    );
}

#[test]
fn split_payments_of_the_supplier_summing_to_the_invoice_are_medium() {
    let invoices = vec![invoice("Supplier B", "2026-08-29", "58.85")];
    let payments = vec![
        payment("p1", "2026-08-29", "21.30", Some("Supplier B")),
        payment("p2", "2026-08-30", "37.55", Some("Supplier B")),
        payment("p3", "2026-08-30", "5.00", Some("Supplier B")),
    ];
    let result = suggest(&invoices, &payments);
    let s = result.suggestions[0].as_ref().unwrap();
    assert_eq!(s.confidence, Confidence::Medium);
    assert_eq!(s.payments, vec![0, 1]);
    assert_eq!(result.unused, vec![2]);
}

#[test]
fn anonymous_payment_of_the_same_amount_is_low() {
    let invoices = vec![invoice("Supplier C", "2026-08-07", "58.20")];
    let payments = vec![payment("p1", "2026-08-08", "58.20", None)];
    assert_eq!(
        confidence_of(&suggest(&invoices, &payments), 0),
        Some(Confidence::Low)
    );
}

#[test]
fn a_payment_naming_another_party_is_no_candidate() {
    let invoices = vec![invoice("Supplier D", "2026-06-26", "165.00")];
    let payments = vec![payment(
        "p1",
        "2026-07-08",
        "165.00",
        Some("Janssens-Peeters"),
    )];
    let result = suggest(&invoices, &payments);
    assert_eq!(confidence_of(&result, 0), None);
    assert_eq!(result.unused, vec![0]);
}

#[test]
fn one_payment_settles_at_most_one_invoice() {
    // Two identical monthly invoices, one payment: the closer invoice gets it.
    let invoices = vec![
        invoice("Supplier E", "2026-07-02", "26.40"),
        invoice("Supplier E", "2026-08-02", "26.40"),
    ];
    let payments = vec![payment("p1", "2026-08-03", "26.40", Some("Supplier E"))];
    let result = suggest(&invoices, &payments);
    assert_eq!(confidence_of(&result, 0), None);
    assert_eq!(confidence_of(&result, 1), Some(Confidence::High));
}

#[test]
fn higher_confidence_wins_a_contested_payment() {
    // p1 pays invoice 0 exactly (high); p1 + p2 would also add up to invoice 1
    // (a medium split). The exact match keeps p1, so invoice 1 gets nothing.
    let invoices = vec![
        invoice("Supplier F", "2026-05-10", "40.00"),
        invoice("Supplier F", "2026-05-01", "55.00"),
    ];
    let payments = vec![
        payment("p1", "2026-05-02", "40.00", Some("Supplier F")),
        payment("p2", "2026-05-02", "15.00", Some("Supplier F")),
    ];
    let result = suggest(&invoices, &payments);
    assert_eq!(confidence_of(&result, 0), Some(Confidence::High));
    assert_eq!(confidence_of(&result, 1), None);
    assert_eq!(result.unused, vec![1]);
}

#[test]
fn an_invoice_without_any_payment_has_no_suggestion() {
    let invoices = vec![invoice("Supplier H", "2026-04-09", "15.80")];
    let result = suggest(&invoices, &[]);
    assert_eq!(confidence_of(&result, 0), None);
}

#[test]
fn credit_notes_are_kept_as_open_credits() {
    let credit = OpenInvoice::from_item(&item("Supplier I", "2025-11-10", "-187.00")).unwrap();
    assert!(credit.is_credit());
    assert!(OpenInvoice::from_item(&item("Supplier I", "2025-11-10", "0.00")).is_none());
}

#[test]
fn window_reaches_back_from_the_oldest_open_invoice() {
    let invoices = vec![
        invoice("Supplier A", "2026-08-03", "10.00"),
        invoice("Supplier B", "2025-11-10", "20.00"),
    ];
    let window = Window::for_invoices(&invoices, "2026-09-28").unwrap();
    assert_eq!(window.payments_from, "2025-08-12");
    assert_eq!(window.ledger_from, "2025-05-01");
    assert_eq!(window.to, "2026-09-28");
    assert!(Window::for_invoices(&[], "2026-09-28").is_none());
}

#[test]
fn select_invoices_filters_by_period_and_keeps_credit_notes() {
    let items = vec![
        item("Supplier A", "2025-11-10", "20.00"),
        item("Supplier B", "2026-07-15", "-5.00"),
        item("Supplier C", "2026-07-20", "7.00"),
        item("Supplier D", "2026-07-21", "0.00"),
    ];
    let all = select_invoices(&items, None);
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].date, "2025-11-10");
    let q3 = select_invoices(&items, Some(("2026-07-01", "2026-09-30")));
    assert_eq!(q3.len(), 2);
    assert!(q3[0].is_credit());
    assert_eq!(q3[1].contact, "Supplier C");
}

#[test]
fn open_invoices_do_not_cover_ledger_payments() {
    // A payment booked against the supplier but not linked to its invoice:
    // the invoice stays open, so it must not count as what the payment paid.
    let ledger = vec![
        ledger_line("9", "i1", "2026-03-01", "-8.45", "Supplier T"),
        ledger_line("0", "p1", "2026-03-01", "8.45", "Supplier T"),
        ledger_line("9", "i2", "2026-07-01", "-8.45", "Supplier T"),
        ledger_line("0", "p2", "2026-06-12", "8.45", "Supplier T"),
    ];
    let open = vec![item("Supplier T", "2026-07-01", "8.45")];
    let closed_only = without_open_invoices(&ledger, &open);
    assert_eq!(closed_only.len(), 3);
    let mut ledger = match_creditor_ledger(&closed_only, "2026-04-01");
    let p = ledger
        .claim(&entry_key("2026-06-12", Cents(845)), "")
        .unwrap();
    assert!(!p.covered, "the June payment paid no closed invoice");
    assert_eq!(p.id, "p2");
}

#[test]
fn collect_payments_uses_ledger_suppliers_and_skips_what_is_explained() {
    let banks = vec![(
        "550000".to_string(),
        vec![
            // Paid a closed invoice: covered on the ledger, not a candidate.
            tx("b1", "2026-06-01", "-8.45", CODA_CARD, ""),
            // Booked against the supplier, unlinked: candidate named by the ledger.
            tx("b2", "2026-06-12", "-8.45", CODA_CARD, ""),
            // Anonymous card payment booked straight to a cost account.
            tx("b3", "2026-06-15", "-58.20", CODA_CARD, "612000"),
            // Bank costs (class 65) never carry an invoice.
            tx("b4", "2026-06-15", "-6.35", CODA_CARD, "657000"),
            // Own transfer to the transfer account.
            tx("b5", "2026-06-16", "-500.00", CODA_TRANSFER_TO_PERSON, ""),
            // A named transfer to a person, not on the ledger.
            tx("b6", "2026-06-20", "-165.00", CODA_TRANSFER_TO_PERSON, ""),
            // Before the window: ignored.
            tx("b7", "2026-01-10", "-1.00", CODA_CARD, ""),
            // A credit: ignored.
            tx("b8", "2026-06-21", "10.00", CODA_CARD, ""),
        ],
    )];
    let ledger_entries = vec![
        ledger_line("9", "i1", "2026-05-31", "-8.45", "Supplier T"),
        ledger_line("0", "l1", "2026-06-01", "8.45", "Supplier T"),
        ledger_line("0", "l2", "2026-06-12", "8.45", "Supplier T"),
        // Paid by card from an account that is not scanned.
        ledger_line("0", "l3", "2026-06-25", "30.00", "Supplier U"),
    ];
    let transfers = vec![tx("t1", "2026-06-16", "500.00", "", "")];
    let rules = UnmatchedRules {
        no_document_accounts: vec!["65".into()],
        ..Default::default()
    };
    let ledger = match_creditor_ledger(&ledger_entries, "2026-03-01");
    let payments = collect(&banks, ledger, &transfers, &rules, "2026-03-01", "440000").payments;
    let got: Vec<(&str, &str, Option<&str>, bool)> = payments
        .iter()
        .map(|p| {
            (
                p.id.as_str(),
                p.bank.as_str(),
                p.supplier.as_deref(),
                p.on_ledger,
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            ("b2", "550000", Some("Supplier T"), true),
            ("b3", "550000", None, false),
            ("b6", "550000", Some("JANSSENS-PEETERS"), false),
            ("l3", "440000", Some("Supplier U"), true),
        ]
    );
}

#[test]
fn rows_list_every_invoice_and_optionally_unallocated_payments() {
    let invoices = vec![
        invoice("Supplier A", "2026-04-24", "1240.50"),
        invoice("Supplier H", "2026-04-09", "15.80"),
    ];
    let payments = vec![
        payment("p1", "2026-03-30", "1240.50", Some("Supplier A")),
        payment("p2", "2026-05-02", "12.00", Some("Supplier Z")),
    ];
    let result = suggest(&invoices, &payments);
    let paid = PaidVia {
        bank: vec![normalize_name("Supplier H")],
        elsewhere: Vec::new(),
    };
    let (headers, rows) = rows_for(&invoices, &payments, &result, &paid);
    assert_eq!(headers[0], "Supplier");
    assert_eq!(rows.len(), 2);
    let conf = headers.iter().position(|h| h == "Confidence").unwrap();
    assert_eq!(rows[0][conf], "high");
    assert_eq!(rows[1][conf], "none");
    let (_, with_unallocated) = rows_with(&invoices, &payments, &result);
    assert_eq!(with_unallocated.len(), 3);
    assert_eq!(with_unallocated[2][conf], "unallocated");
    assert_eq!(with_unallocated[2][0], "Supplier Z");
}

fn rows_with(
    invoices: &[OpenInvoice],
    payments: &[Payment],
    result: &Matches,
) -> (Vec<String>, Vec<Vec<String>>) {
    let mut rows = rows_for(invoices, payments, result, &PaidVia::default());
    unallocated_rows(payments, result, &mut rows.1);
    rows
}

/// A marketplace order paid in one card payment but invoiced per seller:
/// 69.60 = 21.30 + 24.15 + 24.15 (all the marketplace itself, two identical
/// amounts), and 74.15 = 55.40 (a third-party seller, invoiced the payment's
/// day) + 18.75 (the marketplace).
fn marketplace() -> (Vec<OpenInvoice>, Vec<Payment>) {
    let invoices = vec![
        invoice("Marketplace NV", "2026-08-29", "21.30"),
        invoice("Seller X BV", "2026-08-29", "55.40"),
        invoice("Marketplace NV", "2026-08-30", "18.75"),
        invoice("Marketplace NV", "2026-08-30", "24.15"),
        invoice("Marketplace NV", "2026-08-30", "24.15"),
    ];
    let payments = vec![
        payment("b1", "2026-08-29", "74.15", Some("Marketplace.com")),
        payment("b2", "2026-08-29", "69.60", Some("Marketplace.com")),
    ];
    (invoices, payments)
}

#[test]
fn one_payment_adds_up_several_invoices_of_its_supplier_as_high() {
    let (invoices, payments) = marketplace();
    let result = suggest(&invoices, &payments);
    for i in [0, 3, 4] {
        let s = result.suggestions[i].as_ref().unwrap();
        assert_eq!(s.confidence, Confidence::High, "invoice {i}");
        assert_eq!(s.payments, vec![1]);
        assert_eq!(s.invoices, vec![0, 3, 4]);
        assert_eq!(s.days, 1);
    }
}

#[test]
fn one_payment_adding_up_invoices_of_mixed_suppliers_is_medium() {
    let (invoices, payments) = marketplace();
    let result = suggest(&invoices, &payments);
    for i in [1, 2] {
        let s = result.suggestions[i].as_ref().unwrap();
        assert_eq!(s.confidence, Confidence::Medium, "invoice {i}");
        assert_eq!(s.payments, vec![0]);
        assert_eq!(s.invoices, vec![1, 2]);
    }
    assert!(result.unused.is_empty());
    let (headers, rows) = rows_for(&invoices, &payments, &result, &PaidVia::default());
    let reason = headers.iter().position(|h| h == "Reason").unwrap();
    assert!(rows[1][reason].starts_with("one payment adds up 2 invoices of several suppliers"));
    assert!(rows[0][reason].starts_with("one payment adds up 3 invoices of the supplier"));
}

#[test]
fn invoices_are_only_added_up_close_to_the_payment() {
    // Same supplier, but 10 days from the payment and on another day: no group.
    let invoices = vec![
        invoice("Marketplace NV", "2026-08-19", "10.00"),
        invoice("Marketplace NV", "2026-08-19", "5.00"),
    ];
    let payments = vec![payment("b1", "2026-08-29", "15.00", Some("Marketplace"))];
    let result = suggest(&invoices, &payments);
    assert!(result.suggestions.iter().all(Option::is_none));
}

#[test]
fn a_foreign_invoice_matches_a_supplier_payment_within_the_fx_tolerance_as_medium() {
    // Booked at 16.40, charged at 16.80: 2.4% off.
    let invoices = vec![foreign_invoice("Hosting Inc", "2026-06-07", "16.40")];
    let payments = vec![payment("p1", "2026-06-07", "16.80", Some("Hosting Inc"))];
    let result = suggest(&invoices, &payments);
    let s = result.suggestions[0].as_ref().unwrap();
    assert_eq!(s.confidence, Confidence::Medium);
    assert!(s.fx);
    let (headers, rows) = rows_for(&invoices, &payments, &result, &PaidVia::default());
    let reason = headers.iter().position(|h| h == "Reason").unwrap();
    assert!(rows[0][reason].starts_with("FX:"), "{}", rows[0][reason]);
}

#[test]
fn the_fx_tolerance_is_three_percent_or_one_euro_whichever_is_larger() {
    assert_eq!(fx_tolerance(Cents(1640)), Cents(100));
    assert_eq!(fx_tolerance(Cents(100_000)), Cents(3000));
    let invoices = vec![foreign_invoice("Hosting Inc", "2026-06-07", "16.40")];
    let too_far = vec![payment("p1", "2026-06-07", "17.41", Some("Hosting Inc"))];
    assert_eq!(confidence_of(&suggest(&invoices, &too_far), 0), None);
    // A nameless payment near a foreign amount is too weak to suggest.
    let nameless = vec![payment("p1", "2026-06-07", "16.80", None)];
    assert_eq!(confidence_of(&suggest(&invoices, &nameless), 0), None);
}

#[test]
fn euro_invoices_stay_exact() {
    let invoices = vec![invoice("Supplier A", "2026-06-07", "16.40")];
    let payments = vec![payment("p1", "2026-06-07", "16.80", Some("Supplier A"))];
    assert_eq!(confidence_of(&suggest(&invoices, &payments), 0), None);
    assert!(!invoices[0].fx);
    let mut unknown = item("Supplier A", "2026-06-07", "16.40");
    unknown.country = String::new();
    assert!(!OpenInvoice::from_item(&unknown).unwrap().fx);
}

#[test]
fn card_needs_evidence_and_a_supplier_never_seen_is_no_bank_line_seen() {
    let invoices = vec![
        invoice("Card Supplier", "2026-09-07", "17.40"),
        invoice("Bank Supplier", "2026-09-07", "50.00"),
        invoice("New Supplier", "2026-09-07", "12.10"),
    ];
    let result = suggest(&invoices, &[]);
    let paid = PaidVia {
        bank: vec![normalize_name("Bank Supplier BV")],
        elsewhere: vec![normalize_name("Card Supplier")],
    };
    let (headers, rows) = rows_for(&invoices, &[], &result, &paid);
    let conf = headers.iter().position(|h| h == "Confidence").unwrap();
    let reason = headers.iter().position(|h| h == "Reason").unwrap();
    assert_eq!(rows[0][conf], "card");
    assert_eq!(
        rows[0][reason],
        "paid from an account not scanned before (card?): no bank line to match"
    );
    assert_eq!(rows[1][conf], "none");
    assert_eq!(rows[1][reason], "no candidate payment: probably unpaid");
    // Neither paid from the bank nor elsewhere in the window: a first invoice,
    // or paid by a direct debit or card whose bank line names nobody. Not
    // card, and not "probably unpaid" either.
    assert_eq!(rows[2][conf], "unseen");
    assert_eq!(
        rows[2][reason],
        "no payment to this supplier seen: new supplier, or a nameless card payment or direct debit"
    );
}

#[test]
fn collect_records_suppliers_paid_from_an_account_not_scanned() {
    // Supplier U's payments reach the creditors account but no scanned bank
    // line carries them: paid from elsewhere (a credit card). Covered or not,
    // both count; one before the window does not.
    let ledger_entries = vec![
        ledger_line("9", "i1", "2026-05-01", "-20.00", "Supplier U"),
        ledger_line("0", "l1", "2026-05-02", "20.00", "Supplier U"),
        ledger_line("0", "l2", "2026-06-25", "30.00", "Supplier U"),
        ledger_line("0", "l3", "2026-01-05", "30.00", "Supplier W"),
        ledger_line("9", "i2", "2026-05-31", "-9.40", "Supplier T"),
        ledger_line("0", "l4", "2026-06-01", "9.40", "Supplier T"),
    ];
    let banks = vec![(
        "550000".to_string(),
        vec![tx("b1", "2026-06-01", "-9.40", CODA_CARD, "")],
    )];
    let ledger = match_creditor_ledger(&ledger_entries, "2026-03-01");
    let collected = collect(
        &banks,
        ledger,
        &[],
        &UnmatchedRules::default(),
        "2026-03-01",
        "440000",
    );
    assert_eq!(collected.paid.bank, vec!["supplier t".to_string()]);
    assert_eq!(collected.paid.elsewhere, vec!["supplier u".to_string()]);
}

#[test]
fn collect_payments_records_every_supplier_paid_from_the_bank() {
    // Even a payment a closed invoice explains shows the supplier is paid
    // from the bank.
    let banks = vec![(
        "550000".to_string(),
        vec![tx("b1", "2026-06-01", "-8.45", CODA_CARD, "")],
    )];
    let ledger_entries = vec![
        ledger_line("9", "i1", "2026-05-31", "-8.45", "Supplier T"),
        ledger_line("0", "l1", "2026-06-01", "8.45", "Supplier T"),
    ];
    let ledger = match_creditor_ledger(&ledger_entries, "2026-03-01");
    let collected = collect(
        &banks,
        ledger,
        &[],
        &UnmatchedRules::default(),
        "2026-03-01",
        "440000",
    );
    assert!(collected.payments.is_empty());
    assert_eq!(collected.paid.bank, vec!["supplier t".to_string()]);
    assert!(collected.paid.elsewhere.is_empty());
}

#[test]
fn a_supplier_name_inside_another_word_is_not_the_same_supplier() {
    // "ING" is a substring of "Bookings Online", not the same supplier: the
    // same amount makes it no candidate, certainly not `high`.
    let invoices = vec![invoice("Bookings Online", "2026-06-10", "31.40")];
    let payments = vec![payment("p1", "2026-06-11", "31.40", Some("ING"))];
    assert_eq!(confidence_of(&suggest(&invoices, &payments), 0), None);
}

#[test]
fn a_payment_naming_another_supplier_never_adds_up_same_day_invoices() {
    let invoices = vec![
        invoice("Seller X BV", "2026-05-04", "10.10"),
        invoice("Seller Y BV", "2026-05-04", "20.20"),
    ];
    let payments = vec![payment(
        "p1",
        "2026-05-04",
        "30.30",
        Some("Janssens-Peeters"),
    )];
    let result = suggest(&invoices, &payments);
    assert!(result.suggestions.iter().all(Option::is_none));
    assert_eq!(result.unused, vec![0]);
}

#[test]
fn a_nameless_payment_adding_up_same_day_invoices_is_low() {
    let invoices = vec![
        invoice("Seller X BV", "2026-05-04", "10.10"),
        invoice("Seller Y BV", "2026-05-04", "20.20"),
    ];
    let payments = vec![payment("p1", "2026-05-04", "30.30", None)];
    let result = suggest(&invoices, &payments);
    for i in [0, 1] {
        let s = result.suggestions[i].as_ref().unwrap();
        assert_eq!(s.confidence, Confidence::Low, "invoice {i}");
        assert_eq!(s.invoices, vec![0, 1]);
    }
    let (headers, rows) = rows_for(&invoices, &payments, &result, &PaidVia::default());
    let reason = headers.iter().position(|h| h == "Reason").unwrap();
    assert!(
        rows[0][reason].starts_with("one nameless payment adds up 2 invoices"),
        "{}",
        rows[0][reason]
    );
}

#[test]
fn an_exact_one_to_one_match_beats_a_closer_group_of_equal_confidence() {
    // Both are `high`: p1 pays invoice 0 exactly 20 days on, or adds up
    // invoices 1 and 2 of the same day. The exact 1:1 is the stronger story.
    let invoices = vec![
        invoice("Supplier F", "2026-05-01", "40.40"),
        invoice("Supplier F", "2026-05-21", "10.10"),
        invoice("Supplier F", "2026-05-21", "30.30"),
    ];
    let payments = vec![payment("p1", "2026-05-21", "40.40", Some("Supplier F"))];
    let result = suggest(&invoices, &payments);
    let s = result.suggestions[0].as_ref().unwrap();
    assert_eq!(s.confidence, Confidence::High);
    assert_eq!((s.invoices.clone(), s.days), (vec![0], 20));
    assert!(result.suggestions[1].is_none() && result.suggestions[2].is_none());
}

#[test]
fn among_equal_confidence_one_to_one_matches_the_closest_date_wins() {
    let invoices = vec![
        invoice("Supplier F", "2026-05-01", "40.40"),
        invoice("Supplier F", "2026-05-15", "40.40"),
    ];
    let payments = vec![payment("p1", "2026-05-20", "40.40", Some("Supplier F"))];
    let result = suggest(&invoices, &payments);
    assert!(result.suggestions[0].is_none());
    assert_eq!(result.suggestions[1].as_ref().unwrap().days, 5);
}

#[test]
fn an_invoice_booked_as_paid_by_card_is_card_even_for_a_new_supplier() {
    let mut paid_by_card = item("New Supplier", "2026-09-07", "12.10");
    paid_by_card.payment_method = "Creditcard".into();
    let invoices = vec![OpenInvoice::from_item(&paid_by_card).unwrap()];
    let result = suggest(&invoices, &[]);
    let (headers, rows) = rows_for(&invoices, &[], &result, &PaidVia::default());
    let conf = headers.iter().position(|h| h == "Confidence").unwrap();
    let reason = headers.iter().position(|h| h == "Reason").unwrap();
    assert_eq!(rows[0][conf], "card");
    assert_eq!(
        rows[0][reason],
        "invoice payment method Creditcard: no bank line to match"
    );
}

#[test]
fn a_busy_day_still_reaches_invoices_beyond_the_first_candidates() {
    // Fourteen invoices of one supplier on one day. p1 adds up the first two;
    // p2 the last two, which are not among the first GROUP_CANDIDATES. Once
    // p1 took its invoices, p2's candidates are drawn again from what is left.
    let mut invoices: Vec<OpenInvoice> = (0..14)
        .map(|k| invoice("Supplier M", "2026-06-03", &format!("5.{k:02}")))
        .collect();
    invoices[0] = invoice("Supplier M", "2026-06-03", "1.10");
    invoices[1] = invoice("Supplier M", "2026-06-03", "2.20");
    invoices[12] = invoice("Supplier M", "2026-06-03", "3.40");
    invoices[13] = invoice("Supplier M", "2026-06-03", "4.30");
    let payments = vec![
        payment("p1", "2026-06-03", "3.30", Some("Supplier M")),
        payment("p2", "2026-06-03", "7.70", Some("Supplier M")),
    ];
    const { assert!(GROUP_CANDIDATES < 13) };
    let result = suggest(&invoices, &payments);
    let p2 = result.suggestions[13]
        .as_ref()
        .expect("p2 settles 12 and 13");
    assert_eq!(p2.payments, vec![1]);
    assert_eq!(p2.invoices, vec![12, 13]);
}

#[test]
fn a_payment_can_settle_an_invoice_net_of_a_credit_note_of_the_supplier() {
    // 480.00 invoiced, 195.50 credited earlier, 284.50 paid.
    let invoices = vec![
        invoice("Supplier K", "2026-05-12", "-195.50"),
        invoice("Supplier K", "2026-06-01", "480.00"),
        invoice("Supplier L", "2026-06-01", "-10.00"),
    ];
    let payments = vec![payment("p1", "2026-06-03", "284.50", Some("Supplier K"))];
    let result = suggest(&invoices, &payments);
    for i in [0, 1] {
        let s = result.suggestions[i].as_ref().unwrap();
        assert_eq!(s.confidence, Confidence::Medium, "item {i}");
        assert_eq!(s.invoices, vec![0, 1]);
        assert_eq!(s.payments, vec![0]);
    }
    let (headers, rows) = rows_for(&invoices, &payments, &result, &PaidVia::default());
    let conf = headers.iter().position(|h| h == "Confidence").unwrap();
    let reason = headers.iter().position(|h| h == "Reason").unwrap();
    assert!(
        rows[1][reason]
            .starts_with("one payment settles 1 invoice net of 1 credit note of the supplier"),
        "{}",
        rows[1][reason]
    );
    // Another supplier's credit note is not netted, and stays a credit.
    assert_eq!(rows[2][conf], "credit");
    assert_eq!(rows[2][reason], "open credit note: nothing to pay");
}

#[test]
fn a_credit_note_is_never_paid_on_its_own() {
    // A tiny foreign credit note is not "within the FX tolerance" of a payment.
    let mut credit = item("Hosting Inc", "2026-06-07", "-0.40");
    credit.country = "US".into();
    let invoices = vec![OpenInvoice::from_item(&credit).unwrap()];
    let payments = vec![payment("p1", "2026-06-07", "0.50", Some("Hosting Inc"))];
    assert_eq!(confidence_of(&suggest(&invoices, &payments), 0), None);
}
