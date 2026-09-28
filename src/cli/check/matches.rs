//! `yuki check matches`: suggest which payments already made settle the open
//! purchase invoices.
//!
//! Yuki's API cannot link a payment to an invoice, so this only *suggests*
//! pairs; the owner confirms them in the Yuki UI. It reuses the fetches,
//! amounts, creditors ledger and filters of `check unmatched`.

use super::unmatched::{
    Cents, CreditorLedger, LOOKBACK_MONTHS, LedgerPayment, OwnTransfers, UnmatchedRules,
    UnmatchedSetup, bank_counterparty, bank_counterparty_name, entry_key, gl_entries,
    match_creditor_ledger, normalize_name, normalized_names_match,
};
use crate::cli::setup_domain;
use crate::client::accounting::{GlTransactionWithContact, OutstandingItem, TransactionType};
use crate::config::Config;
use crate::error::YukiError;
use crate::output::{OutputFormat, format_json, format_table, is_tty};
use crate::period::{date_from_epoch_days, epoch_days, month_start_before, parse_period, today};

/// Days apart within which a same-supplier, same-amount payment is `high` and
/// a nameless one `low`.
const CLOSE_DAYS: i64 = 30;
/// Days apart within which payments or invoices are added up (one payment
/// for several invoices, several payments for one).
const GROUP_DAYS: i64 = 7;
/// Days apart within which a same-supplier, same-amount payment is `medium`;
/// also how far before the oldest open invoice payments are read.
const FAR_DAYS: i64 = 90;
/// Most payments or invoices a suggestion adds up.
const MAX_GROUP: usize = 4;
/// Closest items considered for adding up, keeping the search cheap.
const GROUP_CANDIDATES: usize = 12;
/// Countries that invoice in euro. An invoice of a supplier elsewhere is
/// taken to be in another currency: Yuki books it at its own rate, the card
/// is charged at another, so its amount only matches within [`fx_tolerance`].
/// The outstanding item carries no currency, only the contact's country.
const EURO_COUNTRIES: &[&str] = &[
    "AT", "BE", "BG", "CY", "DE", "EE", "ES", "FI", "FR", "GR", "EL", "HR", "IE", "IT", "LT", "LU",
    "LV", "MT", "NL", "PT", "SI", "SK",
];

/// How far a payment may be off a foreign-currency invoice: 3% or 1.00,
/// whichever is larger.
pub(super) fn fx_tolerance(open: Cents) -> Cents {
    Cents((open.0 * 3 / 100).max(100))
}

/// How sure a suggestion is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Confidence {
    /// Same amount, but the payment names no supplier (one invoice, or
    /// several dated the payment's day).
    Low,
    /// Same supplier and amount far apart in date; several payments of the
    /// supplier adding up to one invoice; or one payment adding up invoices
    /// of several suppliers.
    Medium,
    /// Same amount and supplier, close in date; or one payment to a supplier
    /// adding up several of its invoices.
    High,
}

impl Confidence {
    fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// An open purchase invoice or credit note (an outstanding creditor item).
#[derive(Debug, Clone)]
pub(super) struct OpenInvoice {
    pub(super) contact: String,
    pub(super) date: String,
    pub(super) open: Cents,
    /// Probably not in euro (a supplier outside the euro area).
    pub(super) fx: bool,
    /// The payment method booked on the invoice when it names a card
    /// (`Creditcard`), else empty.
    pub(super) card_method: String,
}

impl OpenInvoice {
    /// A credit note: no payment settles it on its own, but a payment can
    /// settle the supplier's invoices net of it.
    pub(super) fn is_credit(&self) -> bool {
        self.open < Cents::ZERO
    }

    /// The invoice or credit note (a negative open amount) behind an
    /// outstanding item; `None` when nothing is open.
    pub(super) fn from_item(item: &OutstandingItem) -> Option<Self> {
        let open = Cents::parse(&item.open_amount).filter(|o| *o != Cents::ZERO)?;
        Some(Self {
            contact: item.contact_name.clone(),
            date: day(&item.date).to_string(),
            open,
            fx: {
                let country = item.country.trim().to_ascii_uppercase();
                !country.is_empty() && !EURO_COUNTRIES.contains(&country.as_str())
            },
            card_method: {
                let method = item.payment_method.trim();
                let lower = method.to_lowercase();
                if lower.contains("card") || lower.contains("kaart") || lower.contains("carte") {
                    method.to_string()
                } else {
                    String::new()
                }
            },
        })
    }
}

/// A payment that may settle an open invoice.
#[derive(Debug, Clone)]
pub(super) struct Payment {
    /// GL transaction id: the bank line, or the ledger line when the bank
    /// side is not on a scanned account.
    pub(super) id: String,
    pub(super) date: String,
    /// Positive amount paid.
    pub(super) amount: Cents,
    /// Bank GL account, or the creditors account for a ledger-only payment.
    pub(super) bank: String,
    /// Who the payment went to, for display.
    pub(super) counterparty: String,
    /// The supplier it names, from the creditors account or the bank line;
    /// `None` when it names nobody (a card payment or direct debit).
    pub(super) supplier: Option<String>,
    /// Booked against the supplier on the creditors account, unlinked.
    pub(super) on_ledger: bool,
}

/// One suggested pairing.
#[derive(Debug, Clone)]
pub(super) struct Suggestion {
    /// Indexes into the payments.
    pub(super) payments: Vec<usize>,
    /// Indexes of every invoice the payments settle together (this one too).
    pub(super) invoices: Vec<usize>,
    pub(super) confidence: Confidence,
    /// Largest distance in days between the invoice and a payment.
    pub(super) days: i64,
    /// Matched a foreign-currency invoice within [`fx_tolerance`], not exactly.
    pub(super) fx: bool,
}

/// Suggestions per invoice (same order), and the payments no invoice took.
#[derive(Debug)]
pub(super) struct Matches {
    pub(super) suggestions: Vec<Option<Suggestion>>,
    pub(super) unused: Vec<usize>,
}

/// The date part of an API date.
fn day(date: &str) -> &str {
    date.get(0..10).unwrap_or(date)
}

/// Days between two dates; far apart when either is no date.
fn days_apart(a: &str, b: &str) -> i64 {
    match (epoch_days(a), epoch_days(b)) {
        (Some(a), Some(b)) => (a - b).abs(),
        _ => i64::MAX,
    }
}

/// Date ranges to read.
#[derive(Debug)]
pub(super) struct Window {
    /// Payments from here: the oldest open invoice minus [`FAR_DAYS`].
    pub(super) payments_from: String,
    /// The creditors account from here, [`LOOKBACK_MONTHS`] earlier still, so
    /// that closed invoices paid inside the window are seen.
    pub(super) ledger_from: String,
    pub(super) to: String,
}

impl Window {
    pub(super) fn for_invoices(invoices: &[OpenInvoice], today: &str) -> Option<Self> {
        let oldest = invoices.iter().filter_map(|i| epoch_days(&i.date)).min()?;
        let payments_from = date_from_epoch_days(oldest - FAR_DAYS);
        Some(Self {
            ledger_from: month_start_before(&payments_from, LOOKBACK_MONTHS),
            payments_from,
            to: today.to_string(),
        })
    }
}

/// Open invoices and credit notes, optionally dated within `range`, oldest
/// first.
pub(super) fn select_invoices(
    items: &[OutstandingItem],
    range: Option<(&str, &str)>,
) -> Vec<OpenInvoice> {
    let mut invoices: Vec<OpenInvoice> = items
        .iter()
        .filter_map(OpenInvoice::from_item)
        .filter(|i| range.is_none_or(|(from, to)| (from..=to).contains(&i.date.as_str())))
        .collect();
    invoices.sort_by(|a, b| a.date.cmp(&b.date).then_with(|| a.contact.cmp(&b.contact)));
    invoices
}

/// The creditors account without the lines of the invoices still open.
///
/// [`match_creditor_ledger`] then marks a payment covered only when a *closed*
/// invoice explains it, so a payment booked against the supplier but never
/// linked to its (still open) invoice stays uncovered: a candidate.
pub(super) fn without_open_invoices(
    entries: &[GlTransactionWithContact],
    open: &[OutstandingItem],
) -> Vec<GlTransactionWithContact> {
    let mut keep = vec![true; entries.len()];
    for item in open {
        let Some(amount) = Cents::parse(&item.amount) else {
            continue;
        };
        let wanted = normalize_name(&item.contact_name);
        let found = entries.iter().enumerate().position(|(i, tx)| {
            keep[i]
                && day(&tx.date) == day(&item.date)
                && Cents::parse(&tx.amount) == Some(-amount)
                && !matches!(tx.transaction_type, Some(TransactionType::Bank))
                && (tx.contact_name == item.contact_name
                    || normalized_names_match(&normalize_name(&tx.contact_name), &wanted))
        });
        if let Some(i) = found {
            keep[i] = false;
        }
    }
    entries
        .iter()
        .zip(keep)
        .filter(|(_, keep)| *keep)
        .map(|(tx, _)| tx.clone())
        .collect()
}

/// Payments from `from` on that no closed invoice explains, and who the bank
/// paid.
///
/// A bank debit whose creditors-account line is covered paid a closed
/// invoice and is skipped; an uncovered one is named by that supplier. A
/// debit with no ledger line is skipped when it is an own transfer, matches
/// an ignore pattern, was booked to a no-document account, or goes to an
/// ignored name; otherwise its bank name (if any) is its supplier. Ledger
/// payments whose bank line is not on a scanned account (e.g. a credit
/// card) follow, with `ledger_account` as their bank.
pub(super) fn collect(
    banks: &[(String, Vec<GlTransactionWithContact>)],
    mut ledger: CreditorLedger,
    transfer_entries: &[GlTransactionWithContact],
    rules: &UnmatchedRules,
    from: &str,
    ledger_account: &str,
) -> Collected {
    let mut own_transfers = OwnTransfers::new(banks, transfer_entries);
    let mut payments = Vec::new();
    let mut bank_suppliers = Vec::new();
    for (account, txs) in banks {
        for tx in txs {
            let Some(amount) = Cents::parse(&tx.amount).filter(|a| *a < Cents::ZERO) else {
                continue;
            };
            let amount = amount.abs();
            let key = entry_key(&tx.date, amount);
            let counterparty = bank_counterparty(tx);
            let claimed = ledger.claim(&key, &counterparty);
            if day(&tx.date) < from {
                continue;
            }
            if let Some(name) = claimed
                .as_ref()
                .map(|p| p.contact.clone())
                .or_else(|| bank_counterparty_name(tx))
            {
                bank_suppliers.push(normalize_name(&name));
            }
            let (supplier, on_ledger) = match claimed {
                Some(p) if p.covered => continue,
                Some(p) => (Some(p.contact), true),
                None => {
                    if rules.skips_debit(tx, account, &key, &mut own_transfers) {
                        continue;
                    }
                    let name = bank_counterparty_name(tx);
                    if name.as_deref().is_some_and(|n| rules.skips_counterparty(n)) {
                        continue;
                    }
                    (name, false)
                }
            };
            payments.push(Payment {
                id: tx.id.clone(),
                date: day(&tx.date).to_string(),
                amount,
                bank: account.clone(),
                counterparty: supplier.clone().unwrap_or(counterparty),
                supplier,
                on_ledger,
            });
        }
    }

    // What the bank lines did not claim: paid from an account not scanned.
    let unclaimed: Vec<(String, Cents, LedgerPayment)> = ledger
        .payments
        .into_iter()
        .filter(|((date, _), _)| day(date) >= from)
        .flat_map(|((date, amount), list)| {
            list.into_iter()
                .map(move |p| (day(&date).to_string(), amount, p))
        })
        .collect();
    let mut elsewhere: Vec<String> = unclaimed
        .iter()
        .map(|(_, _, p)| normalize_name(&p.contact))
        .collect();
    let mut ledger_only: Vec<Payment> = unclaimed
        .into_iter()
        .filter(|(_, _, p)| !p.covered)
        .map(|(date, amount, p)| Payment {
            id: p.id,
            date,
            amount,
            bank: ledger_account.to_string(),
            counterparty: p.contact.clone(),
            supplier: Some(p.contact),
            on_ledger: true,
        })
        .collect();
    ledger_only.sort_by(|a, b| {
        (&a.date, a.amount, &a.counterparty, &a.id).cmp(&(
            &b.date,
            b.amount,
            &b.counterparty,
            &b.id,
        ))
    });
    payments.extend(ledger_only);
    for names in [&mut bank_suppliers, &mut elsewhere] {
        names.sort();
        names.dedup();
    }
    Collected {
        payments,
        paid: PaidVia {
            bank: bank_suppliers,
            elsewhere,
        },
    }
}

/// What [`collect`] found.
pub(super) struct Collected {
    /// Candidate payments.
    pub(super) payments: Vec<Payment>,
    /// Who was paid how in the window, candidate or not.
    pub(super) paid: PaidVia,
}

/// Suppliers (normalized) paid in the window, by where the money went out.
/// Evidence for an open invoice no payment matches.
#[derive(Debug, Default)]
pub(super) struct PaidVia {
    /// Paid from a scanned bank account.
    pub(super) bank: Vec<String>,
    /// Paid according to the creditors account, but from an account that is
    /// not scanned (in practice a credit card, settled in one monthly line).
    pub(super) elsewhere: Vec<String>,
}

/// How a payment's supplier relates to an invoice's.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Name {
    Same,
    Unknown,
    Other,
}

fn name_relation(payment: &Payment, invoice_name: &str) -> Name {
    match &payment.supplier {
        None => Name::Unknown,
        Some(s) if normalized_names_match(&normalize_name(s), invoice_name) => Name::Same,
        Some(_) => Name::Other,
    }
}

/// A possible pairing before assignment.
struct Candidate {
    invoices: Vec<usize>,
    payments: Vec<usize>,
    confidence: Confidence,
    days: i64,
    fx: bool,
}

/// Subsets of 2..=[`MAX_GROUP`] of `pool` whose amounts sum to `target`
/// exactly, each in `pool` order. Amounts may be negative (credit notes).
fn subsets(pool: &[usize], amount: &dyn Fn(usize) -> i64, target: i64) -> Vec<Vec<usize>> {
    fn walk(
        pool: &[usize],
        amount: &dyn Fn(usize) -> i64,
        left: i64,
        signed: bool,
        chosen: &mut Vec<usize>,
        out: &mut Vec<Vec<usize>>,
    ) {
        if left == 0 && chosen.len() >= 2 {
            out.push(chosen.clone());
            return;
        }
        // Overshooting is final only when nothing negative can bring it back.
        if (left <= 0 && !signed) || chosen.len() == MAX_GROUP {
            return;
        }
        for (k, &item) in pool.iter().enumerate() {
            chosen.push(item);
            walk(
                &pool[k + 1..],
                amount,
                left - amount(item),
                signed,
                chosen,
                out,
            );
            chosen.pop();
        }
    }
    let signed = pool.iter().any(|&i| amount(i) < 0);
    let mut out = Vec::new();
    walk(pool, amount, target, signed, &mut Vec::new(), &mut out);
    out
}

/// The [`GROUP_CANDIDATES`] closest of `(days, index)`, back in index order.
fn closest(mut pool: Vec<(i64, usize)>) -> Vec<usize> {
    pool.sort_unstable();
    pool.truncate(GROUP_CANDIDATES);
    let mut kept: Vec<usize> = pool.into_iter().map(|(_, i)| i).collect();
    kept.sort_unstable();
    kept
}

/// Every candidate pairing, scored.
///
/// - one payment, one invoice of a foreign supplier, same supplier, within
///   [`fx_tolerance`] and [`CLOSE_DAYS`]: `medium` (FX);
/// - one payment, one invoice: same amount; `high` with the same supplier
///   within [`CLOSE_DAYS`], `medium` within [`FAR_DAYS`], `low` when the
///   payment names no supplier (within [`CLOSE_DAYS`]);
/// - several payments to the supplier within [`GROUP_DAYS`] adding up to one
///   invoice: `medium`;
/// - one payment adding up several invoices, each of the payment's supplier
///   within [`GROUP_DAYS`] or of any supplier dated the payment's day (a
///   marketplace order split per seller): `high` when all are the payment's
///   supplier, `medium` when some are, never when the payment names none of
///   them; a payment naming nobody adds up only same-day invoices, as `low`.
///
/// Only invoices and payments still `free` take part, so that a later round
/// draws the [`GROUP_CANDIDATES`] from what earlier rounds left.
fn candidates(
    invoices: &[OpenInvoice],
    payments: &[Payment],
    free_invoice: &[bool],
    free_payment: &[bool],
) -> Vec<Candidate> {
    let names: Vec<String> = invoices
        .iter()
        .map(|i| normalize_name(&i.contact))
        .collect();
    let days = |i: usize, p: usize| days_apart(&invoices[i].date, &payments[p].date);
    let mut found = Vec::new();

    let free_invoices = || {
        invoices
            .iter()
            .enumerate()
            .filter(|(i, _)| free_invoice[*i])
    };
    let free_payments = || {
        payments
            .iter()
            .enumerate()
            .filter(|(p, _)| free_payment[*p])
    };

    for (i, invoice) in free_invoices().filter(|(_, i)| !i.is_credit()) {
        let mut split_pool = Vec::new();
        for (p, payment) in free_payments() {
            let d = days(i, p);
            let relation = name_relation(payment, &names[i]);
            let fx_close = invoice.fx
                && payment.amount != invoice.open
                && (payment.amount.0 - invoice.open.0).abs() <= fx_tolerance(invoice.open).0;
            if fx_close && relation == Name::Same && d <= CLOSE_DAYS {
                found.push(Candidate {
                    invoices: vec![i],
                    payments: vec![p],
                    confidence: Confidence::Medium,
                    days: d,
                    fx: true,
                });
                continue;
            }
            let confidence = match (payment.amount == invoice.open, relation) {
                (true, Name::Same) if d <= CLOSE_DAYS => Some(Confidence::High),
                (true, Name::Same) if d <= FAR_DAYS => Some(Confidence::Medium),
                (true, Name::Unknown) if d <= CLOSE_DAYS => Some(Confidence::Low),
                (false, Name::Same) if payment.amount < invoice.open && d <= GROUP_DAYS => {
                    split_pool.push((d, p));
                    None
                }
                _ => None,
            };
            if let Some(confidence) = confidence {
                found.push(Candidate {
                    invoices: vec![i],
                    payments: vec![p],
                    confidence,
                    days: d,
                    fx: false,
                });
            }
        }
        let pool = closest(split_pool);
        for set in subsets(&pool, &|p| payments[p].amount.0, invoice.open.0) {
            found.push(Candidate {
                days: set.iter().map(|&p| days(i, p)).max().unwrap_or(0),
                invoices: vec![i],
                payments: set,
                confidence: Confidence::Medium,
                fx: false,
            });
        }
    }

    for (p, payment) in free_payments() {
        let pool = closest(
            free_invoices()
                .filter(|(i, invoice)| {
                    let relation = name_relation(payment, &names[*i]);
                    if invoice.is_credit() {
                        // The supplier's own credit note, netted off its invoices.
                        return relation == Name::Same && days(*i, p) <= FAR_DAYS;
                    }
                    match relation {
                        // Even one larger than the payment, net of a credit note.
                        Name::Same => days(*i, p) <= GROUP_DAYS,
                        // Another seller's part of a same-day order.
                        Name::Other | Name::Unknown => {
                            invoice.open < payment.amount && invoice.date == payment.date
                        }
                    }
                })
                .map(|(i, _)| (days(i, p), i))
                .collect(),
        );
        for set in subsets(&pool, &|i| invoices[i].open.0, payment.amount.0) {
            let same = set
                .iter()
                .filter(|&&i| name_relation(payment, &names[i]) == Name::Same)
                .count();
            let netted = set.iter().any(|&i| invoices[i].is_credit());
            let confidence = match (&payment.supplier, same) {
                // A nameless payment may be anyone's.
                (None, _) => Confidence::Low,
                // It names someone else: it pays none of them.
                (Some(_), 0) => continue,
                // Netting a credit note is a guess about how the supplier
                // settled, however well the names agree.
                (Some(_), n) if n == set.len() && !netted => Confidence::High,
                (Some(_), _) => Confidence::Medium,
            };
            found.push(Candidate {
                days: set.iter().map(|&i| days(i, p)).max().unwrap_or(0),
                invoices: set,
                payments: vec![p],
                confidence,
                fx: false,
            });
        }
    }
    found
}

/// Pair payments with open invoices, each payment to at most one invoice.
///
/// Every invoice is settled at most once too. Greedy: strongest confidence
/// first; within it an exact amount before an FX one and the fewest items
/// (an exact 1:1 before any sum), then closest in date, then invoice and
/// payment order, so the result depends on nothing but the input. Repeated
/// over what is left until a round pairs nothing more.
pub(super) fn suggest(invoices: &[OpenInvoice], payments: &[Payment]) -> Matches {
    let mut suggestions: Vec<Option<Suggestion>> = vec![None; invoices.len()];
    let mut used = vec![false; payments.len()];
    // Sums only look at the closest few items, so on a busy day a payment can
    // miss its invoices behind others; once a round has paired what it could,
    // draw the candidates again from what is left.
    loop {
        let free: Vec<bool> = suggestions.iter().map(Option::is_none).collect();
        let unused: Vec<bool> = used.iter().map(|u| !u).collect();
        let mut found = candidates(invoices, payments, &free, &unused);
        let items = |c: &Candidate| c.payments.len() + c.invoices.len();
        found.sort_by(|a, b| {
            b.confidence
                .cmp(&a.confidence)
                .then(a.fx.cmp(&b.fx))
                .then(items(a).cmp(&items(b)))
                .then(a.days.cmp(&b.days))
                .then(a.invoices.cmp(&b.invoices))
                .then(a.payments.cmp(&b.payments))
        });
        let mut paired = false;
        for c in found {
            if c.invoices.iter().any(|&i| suggestions[i].is_some())
                || c.payments.iter().any(|&p| used[p])
            {
                continue;
            }
            paired = true;
            for &p in &c.payments {
                used[p] = true;
            }
            let suggestion = Suggestion {
                payments: c.payments,
                invoices: c.invoices,
                confidence: c.confidence,
                days: c.days,
                fx: c.fx,
            };
            for &i in &suggestion.invoices {
                suggestions[i] = Some(suggestion.clone());
            }
        }
        if !paired {
            break;
        }
    }
    let unused = (0..payments.len()).filter(|&p| !used[p]).collect();
    Matches {
        suggestions,
        unused,
    }
}

/// Why a suggestion was made, in words.
fn reason(s: &Suggestion, invoices: &[OpenInvoice], payments: &[Payment]) -> String {
    let apart = match s.days {
        0 => "same day".to_string(),
        1 => "1 day apart".to_string(),
        d => format!("{d} days apart"),
    };
    let first = &payments[s.payments[0]];
    let booked = if s.payments.iter().all(|&p| payments[p].on_ledger) {
        ", booked to the supplier but not linked"
    } else {
        ""
    };
    let within = if s.days <= 1 {
        apart.clone()
    } else {
        format!("within {} days", s.days)
    };
    let credits = s
        .invoices
        .iter()
        .filter(|&&i| invoices[i].is_credit())
        .count();
    if credits > 0 {
        let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        return format!(
            "one payment settles {} net of {} of the supplier, {within}{booked}",
            plural(s.invoices.len() - credits, "invoice"),
            plural(credits, "credit note"),
        );
    }
    if s.invoices.len() > 1 {
        let (payment, suppliers) = match s.confidence {
            Confidence::High => ("payment", "of the supplier"),
            Confidence::Medium => ("payment", "of several suppliers"),
            Confidence::Low => ("nameless payment", "dated that day"),
        };
        return format!(
            "one {payment} adds up {} invoices {suppliers}, {within}{booked}",
            s.invoices.len()
        );
    }
    if s.fx {
        return format!(
            "FX: foreign-currency invoice, amount within 3% (min 1.00), same supplier, {apart}{booked}"
        );
    }
    match (s.payments.len(), first.supplier.is_some()) {
        (1, true) => format!("same amount and supplier, {apart}{booked}"),
        (1, false) => format!("same amount, payment names no supplier, {apart}"),
        (n, _) => format!("{n} payments to the supplier add up to it, {within}{booked}"),
    }
}

/// Table rows, one per open invoice. One without a candidate says what is
/// known about how it is paid: `card` when the invoice's payment method is a
/// card or its supplier was paid from an account not scanned ([`PaidVia`]),
/// both leaving no bank line to match; else `none` when the supplier was paid
/// from a scanned bank account (so the payment would show: probably unpaid);
/// else `unseen`: no payment to it in the window at all, which is a new
/// supplier as often as a nameless card payment or direct debit.
pub(super) fn rows_for(
    invoices: &[OpenInvoice],
    payments: &[Payment],
    matches: &Matches,
    paid: &PaidVia,
) -> (Vec<String>, Vec<Vec<String>>) {
    let headers = [
        "Supplier",
        "Invoice Date",
        "Open",
        "Confidence",
        "Payment Date",
        "Paid",
        "Bank",
        "Payment ID",
        "Counterparty",
        "Reason",
    ]
    .iter()
    .map(|h| (*h).to_string())
    .collect();
    let join = |s: &Suggestion, f: &dyn Fn(&Payment) -> String| {
        s.payments
            .iter()
            .map(|&p| f(&payments[p]))
            .collect::<Vec<_>>()
            .join("; ")
    };
    let rows: Vec<Vec<String>> = invoices
        .iter()
        .zip(&matches.suggestions)
        .map(|(invoice, s)| {
            let mut row = vec![
                invoice.contact.clone(),
                invoice.date.clone(),
                invoice.open.to_string(),
            ];
            match s {
                Some(s) => {
                    let mut names = s
                        .payments
                        .iter()
                        .map(|&p| payments[p].counterparty.clone())
                        .collect::<Vec<_>>();
                    names.dedup();
                    row.extend([
                        s.confidence.label().to_string(),
                        join(s, &|p| p.date.clone()),
                        join(s, &|p| p.amount.to_string()),
                        join(s, &|p| p.bank.clone()),
                        join(s, &|p| p.id.clone()),
                        names.join("; "),
                        reason(s, invoices, payments),
                    ]);
                }
                None => {
                    let name = normalize_name(&invoice.contact);
                    let among = |names: &[String]| names.iter().any(|n| normalized_names_match(n, &name));
                    let (status, why) = if invoice.is_credit() {
                        ("credit", "open credit note: nothing to pay".to_string())
                    } else if !invoice.card_method.is_empty() {
                        (
                            "card",
                            format!(
                                "invoice payment method {}: no bank line to match",
                                invoice.card_method
                            ),
                        )
                    } else if among(&paid.elsewhere) {
                        (
                            "card",
                            "paid from an account not scanned before (card?): no bank line to match"
                                .to_string(),
                        )
                    } else if among(&paid.bank) {
                        ("none", "no candidate payment: probably unpaid".to_string())
                    } else {
                        (
                            "unseen",
                            "no payment to this supplier seen: new supplier, or a nameless card payment or direct debit"
                                .to_string(),
                        )
                    };
                    row.push(status.to_string());
                    row.extend(std::iter::repeat_n(String::new(), 5));
                    row.push(why);
                }
            }
            row
        })
        .collect();
    (headers, rows)
}

/// Append the payments booked to a supplier that no open invoice took.
pub(super) fn unallocated_rows(
    payments: &[Payment],
    matches: &Matches,
    rows: &mut Vec<Vec<String>>,
) {
    for &p in &matches.unused {
        let payment = &payments[p];
        let Some(supplier) = payment.supplier.as_ref().filter(|_| payment.on_ledger) else {
            continue;
        };
        rows.push(vec![
            supplier.clone(),
            String::new(),
            String::new(),
            "unallocated".into(),
            payment.date.clone(),
            payment.amount.to_string(),
            payment.bank.clone(),
            payment.id.clone(),
            payment.counterparty.clone(),
            "paid to the supplier, no open invoice matches".into(),
        ]);
    }
}

/// Suggest which payments already made settle the open purchase invoices.
///
/// Reads the open purchase invoices first, then, from the oldest one's date
/// minus [`FAR_DAYS`] to today, the bank accounts, the creditors account
/// (reaching [`LOOKBACK_MONTHS`] further back) and the transfer accounts.
/// `period` only selects which open invoices are considered: a payment made
/// after the period can still settle an invoice from it.
pub async fn matches(
    config: &Config,
    admin: Option<&str>,
    period: Option<&str>,
    bank_accounts: &[String],
    unallocated: bool,
    format: Option<&str>,
    quiet: bool,
) -> Result<(), YukiError> {
    let range = period.map(parse_period).transpose()?;
    let (accounting, target) = setup_domain(config, admin).await?;
    let setup = UnmatchedSetup::resolve(config, target.config_name, bank_accounts);
    let admin_id = target.admin_id;

    if !quiet {
        eprintln!("Fetching outstanding creditor items...");
    }
    let items = accounting.outstanding_creditor_items(admin_id).await?;
    let invoices = select_invoices(
        &items,
        range.as_ref().map(|(s, e)| (s.as_str(), e.as_str())),
    );
    let mut calls = 2 + 1;

    let (collected, window) = match Window::for_invoices(&invoices, &today()) {
        None => (
            Collected {
                payments: Vec::new(),
                paid: PaidVia::default(),
            },
            None,
        ),
        Some(window) => {
            if !quiet {
                for account in &setup.bank_accounts {
                    eprintln!(
                        "Fetching bank transactions (GL {account}, {} to {})...",
                        window.payments_from, window.to
                    );
                }
                for account in &setup.creditor_accounts {
                    eprintln!(
                        "Fetching supplier ledger (GL {account}, from {})...",
                        window.ledger_from
                    );
                }
                for account in &setup.transfer_accounts {
                    eprintln!("Fetching internal transfers (GL {account})...");
                }
            }
            let (bank_entries, creditor_entries, transfer_entries) = tokio::try_join!(
                gl_entries(
                    &accounting,
                    admin_id,
                    &setup.bank_accounts,
                    &window.payments_from,
                    &window.to
                ),
                gl_entries(
                    &accounting,
                    admin_id,
                    &setup.creditor_accounts,
                    &window.ledger_from,
                    &window.to
                ),
                gl_entries(
                    &accounting,
                    admin_id,
                    &setup.transfer_accounts,
                    &window.payments_from,
                    &window.to
                ),
            )?;
            calls += bank_entries.len() + creditor_entries.len() + transfer_entries.len();
            let banks: Vec<(String, Vec<GlTransactionWithContact>)> = setup
                .bank_accounts
                .iter()
                .cloned()
                .zip(bank_entries)
                .collect();
            let ledger = match_creditor_ledger(
                &without_open_invoices(&creditor_entries.concat(), &items),
                &window.payments_from,
            );
            let ledger_account = setup.creditor_accounts.join(",");
            let collected = collect(
                &banks,
                ledger,
                &transfer_entries.concat(),
                &setup.rules,
                &window.payments_from,
                &ledger_account,
            );
            (collected, Some(window))
        }
    };

    let payments = collected.payments;
    let result = suggest(&invoices, &payments);
    let (headers, mut rows) = rows_for(&invoices, &payments, &result, &collected.paid);
    if !quiet {
        eprintln!("API calls made: {calls}");
        let count = |label: &str| rows.iter().filter(|r| r[3] == label).count();
        let window = window.map_or(String::new(), |w| {
            format!(" (payments {} to {})", w.payments_from, w.to)
        });
        eprintln!(
            "{} open items{window}: {} high, {} medium, {} low; no candidate: {} card, {} probably unpaid, {} supplier never seen paid, {} credit notes",
            invoices.len(),
            count("high"),
            count("medium"),
            count("low"),
            count("card"),
            count("none"),
            count("unseen"),
            count("credit"),
        );
    }
    if unallocated {
        unallocated_rows(&payments, &result, &mut rows);
    }
    let fmt = OutputFormat::from_flag(format, is_tty());
    if matches!(fmt, OutputFormat::Table) {
        let reason = headers.len() - 1;
        for row in &mut rows {
            row[0] = row[0].chars().take(30).collect();
            row[reason] = row[reason].chars().take(70).collect();
        }
    }
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}

#[cfg(test)]
#[path = "matches_tests.rs"]
mod tests;
