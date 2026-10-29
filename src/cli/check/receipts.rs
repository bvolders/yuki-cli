//! Local receipts for `yuki check missing`: which file in a receipts folder
//! belongs to a payment Yuki has no invoice for. Read only: the folder, its
//! `receipts-log.csv` and its `.yuki-sync.json` are never written.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::matches::days_apart;
use super::unmatched::{Cents, normalize_name, normalized_names_match};
use crate::error::YukiError;
use crate::money::{div_round, parse_scaled};
use crate::period::epoch_days;
use crate::sync::{self, Excludes, Status};

/// Days a receipt's date may lie from the payment's.
const RECEIPT_DAYS: i64 = 5;

/// Optional file with each receipt's amount, at the root of the folder.
pub(super) const RECEIPTS_LOG: &str = "receipts-log.csv";

/// Decimals a `Koers` exchange rate is read to.
const RATE_DECIMALS: u32 = 12;

/// The foreign-currency amount of a card payment, from its description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Fx {
    /// Absolute amount in the foreign currency's cents.
    amount: Cents,
    currency: String,
    /// Worked out from the `Koers` rate rather than stated as `Bedrag`.
    derived: bool,
    /// How many foreign cents the amount may be off: half a euro cent at
    /// the rate, plus one, for a derived amount; 0 for a stated one.
    slack: i64,
}

impl Fx {
    /// `20.00 USD`, with `~` when derived from the rate.
    pub(super) fn label(&self) -> String {
        let approx = if self.derived { "~" } else { "" };
        format!("{approx}{} {}", self.amount, self.currency)
    }
}

/// An amount such as `4,04` or `1.234,56` (Belgian notation), or `4.04`
/// when it has no comma.
fn belgian_amount(text: &str, decimals: u32) -> Option<i64> {
    let text = if text.contains(',') {
        text.replace('.', "").replace(',', ".")
    } else {
        text.to_string()
    };
    parse_scaled(&text, decimals).ok()
}

fn is_currency(word: &str) -> bool {
    word.len() == 3 && word.bytes().all(|b| b.is_ascii_uppercase()) && word != "EUR"
}

/// The foreign amount a card line's description gives: `Bedrag 20 USD`
/// stated, else `Koers (1 EUR = 1,133144476 USD)` applied to `eur`.
pub(super) fn parse_fx(description: &str, eur: Cents) -> Option<Fx> {
    let words: Vec<&str> = description.split_whitespace().collect();
    for w in words.windows(3) {
        if w[0] == "Bedrag"
            && is_currency(w[2])
            && let Some(cents) = belgian_amount(w[1], 2)
        {
            return Some(Fx {
                amount: Cents(cents.abs()),
                currency: w[2].to_string(),
                derived: false,
                slack: 0,
            });
        }
    }
    let rest = description.split("Koers (1 EUR = ").nth(1)?;
    let (rate, rest) = rest.split_once(' ')?;
    let currency: String = rest.chars().take_while(char::is_ascii_uppercase).collect();
    if !is_currency(&currency) {
        return None;
    }
    let rate = belgian_amount(rate, RATE_DECIMALS)?;
    let scale = 10_i128.pow(RATE_DECIMALS);
    let amount = div_round(i128::from(eur.abs().0) * i128::from(rate), scale);
    // The EUR amount is rounded to the cent, so the foreign one is known to
    // half a cent times the rate.
    let slack = div_round(i128::from(rate), 2 * scale) + 1;
    Some(Fx {
        amount: Cents(amount as i64),
        currency,
        derived: true,
        slack: slack as i64,
    })
}

/// One receipt file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Receipt {
    /// Relative to the receipts folder, `/`-separated.
    rel: String,
    /// The vendor: the file's folder name, normalized.
    vendor: String,
    /// The invoice date: from the log, else the file name's `YYYY-MM-DD` prefix.
    date: Option<String>,
    /// Amount and currency from the log; `None` when it has none.
    amount: Option<(Cents, String)>,
    /// What `.yuki-sync.json` records about the file.
    sync: Option<Status>,
}

/// One row of the log: date, amount and currency.
type LogRow = (Option<String>, Option<(Cents, String)>);

/// Split one CSV line, honouring double quotes.
fn csv_fields(line: &str) -> Vec<String> {
    let (mut fields, mut field, mut quoted) = (Vec::new(), String::new(), false);
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(std::mem::take(&mut field)),
            _ => field.push(c),
        }
    }
    fields.push(field);
    fields
}

/// The log's rows by `filename` (as [`sync::name_key`]: NFC, lowercase),
/// when it has that column. `amount` (dot or Belgian comma decimals) and
/// `currency` (EUR when absent) and `invoice_date` are read when present.
fn read_log(text: &str) -> HashMap<String, LogRow> {
    let mut lines = text.lines();
    let header: Vec<String> = lines
        .next()
        .map(csv_fields)
        .unwrap_or_default()
        .into_iter()
        .map(|h| h.trim().to_lowercase())
        .collect();
    let col = |name: &str| header.iter().position(|h| h == name);
    let Some(file_col) = col("filename") else {
        return HashMap::new();
    };
    let (amount_col, currency_col, date_col) =
        (col("amount"), col("currency"), col("invoice_date"));
    let mut rows = HashMap::new();
    for line in lines.filter(|l| !l.trim().is_empty()) {
        let fields = csv_fields(line);
        let get = |i: Option<usize>| {
            i.and_then(|i| fields.get(i))
                .map(|f| f.trim())
                .filter(|f| !f.is_empty())
        };
        let Some(file) = get(Some(file_col)) else {
            continue;
        };
        let amount = get(amount_col)
            .and_then(|a| belgian_amount(a, 2))
            .map(Cents)
            .map(|a| {
                let currency = get(currency_col).unwrap_or("EUR").to_uppercase();
                (a.abs(), currency)
            });
        let date = get(date_col)
            .filter(|d| epoch_days(d).is_some())
            .map(str::to_string);
        rows.insert(sync::name_key(file), (date, amount));
    }
    rows
}

/// The `YYYY-MM-DD` a file name starts with.
fn name_date(name: &str) -> Option<String> {
    let date = name.get(0..10)?;
    epoch_days(date).map(|_| date.to_string())
}

/// What [`load`] found.
#[derive(Debug, Default)]
pub(super) struct Receipts {
    pub(super) files: Vec<Receipt>,
    /// Whether a `receipts-log.csv` gave amounts; without them a receipt
    /// matches on vendor and date only.
    pub(super) has_amounts: bool,
    /// Whether a `.yuki-sync.json` was read.
    pub(super) has_sync_state: bool,
    /// Why the `.yuki-sync.json` found could not be read.
    pub(super) state_error: Option<String>,
}

/// Read the receipts under `dir`: the files `upload dir` would take (so not
/// `_to_delete` or dot files), with amounts from `receipts-log.csv` and the
/// state `.yuki-sync.json` records, at `dir` or the synced folder above it.
pub(super) fn load(dir: &Path) -> Result<Receipts, YukiError> {
    let scan = sync::scan(dir, &Excludes::new(&[])?)?;
    let log = match fs::read_to_string(dir.join(RECEIPTS_LOG)) {
        Ok(text) => read_log(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(e) => {
            return Err(YukiError::Config(format!(
                "{}: {e}",
                dir.join(RECEIPTS_LOG).display()
            )));
        }
    };
    // An unreadable state only hides the upload marks; this command never
    // writes it, so it is no reason to stop.
    let (state, state_error) = match sync::find_root(dir)?.map(|root| sync::peek(&root)) {
        Some(Ok(state)) => (Some(state), None),
        Some(Err(e)) => (None, Some(e.to_string())),
        None => (None, None),
    };
    let files = scan
        .files
        .into_iter()
        .map(|found| {
            let (folder, name) = found.rel.rsplit_once('/').unwrap_or(("", &found.rel));
            let folder = folder.rsplit('/').next().unwrap_or(folder);
            // A file at the top takes its vendor from `date_vendor_ref` instead.
            let vendor = if folder.is_empty() {
                name.split('_').nth(1).unwrap_or(name)
            } else {
                folder
            };
            let (log_date, amount) = log
                .get(&sync::name_key(&found.rel))
                .cloned()
                .unwrap_or_default();
            Receipt {
                vendor: normalize_name(vendor),
                date: log_date.or_else(|| name_date(name)),
                amount,
                sync: state
                    .as_ref()
                    .and_then(|s| s.state().files.get(&found.hash))
                    .map(|e| e.status),
                rel: found.rel,
            }
        })
        .collect::<Vec<_>>();
    Ok(Receipts {
        has_amounts: files.iter().any(|r| r.amount.is_some()),
        has_sync_state: state.is_some(),
        state_error,
        files,
    })
}

/// A payment to find a receipt for.
#[derive(Debug, Clone)]
pub(super) struct Wanted<'a> {
    pub(super) supplier: &'a str,
    pub(super) description: &'a str,
    pub(super) date: &'a str,
    /// Absolute amount in EUR.
    pub(super) eur: Cents,
    pub(super) fx: Option<&'a Fx>,
}

/// How a receipt matched its payment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Fit {
    /// Vendor, amount and date.
    Amount,
    /// Vendor and date; the amounts cannot be compared ([`same_amount`]).
    VendorDate,
}

/// Whether the receipt's amount is the payment's, in EUR or in the
/// description's foreign currency; `None` when they cannot be compared: the
/// receipt has no amount, or is in a currency the description does not state
/// (a foreign charge paid through PayPal shows only its EUR amount).
fn same_amount(receipt: &Receipt, wanted: &Wanted<'_>) -> Option<bool> {
    let (amount, currency) = receipt.amount.as_ref()?;
    if currency == "EUR" {
        return Some(*amount == wanted.eur);
    }
    let fx = wanted.fx.filter(|fx| fx.currency == *currency)?;
    Some((fx.amount.0 - amount.0).abs() <= fx.slack)
}

/// A payment's candidate receipts, closest date first.
type Edges = Vec<Vec<(Fit, i64, usize)>>;

/// Kuhn's augmenting path: give payment `w` a receipt of at most fit
/// `limit[w]`, moving earlier payments to another of theirs if need be.
fn augment(
    w: usize,
    edges: &Edges,
    limit: &[Fit],
    seen: &mut [bool],
    owner: &mut [Option<usize>],
    out: &mut [Option<(usize, Fit)>],
) -> bool {
    for &(fit, _, r) in &edges[w] {
        if fit > limit[w] || seen[r] {
            continue;
        }
        seen[r] = true;
        if owner[r].is_none_or(|other| augment(other, edges, limit, seen, owner, out)) {
            owner[r] = Some(w);
            out[w] = Some((r, fit));
            return true;
        }
    }
    false
}

/// The receipt for each payment, or `None`: same vendor, date within
/// [`RECEIPT_DAYS`], and the same amount when the two can be compared. The
/// vendor is the payment's supplier; only when no receipt is that supplier's
/// is it looked for in the description (a card line booked to a catch-all
/// contact). Each receipt goes to one payment: as many payments as possible
/// get an amount match, then as many of the rest as possible a date-only
/// one, preferring the closest date.
pub(super) fn assign(receipts: &[Receipt], wanted: &[Wanted<'_>]) -> Vec<Option<(usize, Fit)>> {
    let edges: Edges = wanted
        .iter()
        .map(|payment| {
            let supplier = normalize_name(payment.supplier);
            let vendor = if receipts
                .iter()
                .any(|r| normalized_names_match(&r.vendor, &supplier))
            {
                supplier
            } else {
                normalize_name(payment.description)
            };
            let mut list: Vec<(Fit, i64, usize)> = receipts
                .iter()
                .enumerate()
                .filter_map(|(r, receipt)| {
                    let days = days_apart(receipt.date.as_deref()?, payment.date);
                    if days > RECEIPT_DAYS || !normalized_names_match(&receipt.vendor, &vendor) {
                        return None;
                    }
                    let fit = match same_amount(receipt, payment) {
                        Some(true) => Fit::Amount,
                        Some(false) => return None,
                        None => Fit::VendorDate,
                    };
                    Some((fit, days, r))
                })
                .collect();
            list.sort_by_key(|&(fit, days, r)| (days, fit, r));
            list
        })
        .collect();
    let mut owner = vec![None; receipts.len()];
    let mut out = vec![None; wanted.len()];
    // Amount matches first; a payment that has one keeps an amount match
    // when the second pass moves it.
    let mut limit = vec![Fit::Amount; wanted.len()];
    // Payments with the closest candidate go first, so a tie between them
    // goes to the closer one; a later payment still gets a receipt by moving
    // an earlier one to another of its candidates.
    let mut order: Vec<usize> = (0..wanted.len()).collect();
    order.sort_by_key(|&w| edges[w].first().map_or(i64::MAX, |e| e.1));
    for pass in [Fit::Amount, Fit::VendorDate] {
        for &w in &order {
            if out[w].is_none() {
                limit[w] = pass;
                let mut seen = vec![false; receipts.len()];
                augment(w, &edges, &limit, &mut seen, &mut owner, &mut out);
            }
        }
    }
    out
}

/// The `Local receipt` cell: the path, and what the sync state or the
/// match says about it.
pub(super) fn cell(receipt: &Receipt, fit: Fit) -> String {
    let mut notes = Vec::new();
    match receipt.sync {
        Some(Status::Uploaded) => notes.push("uploaded, awaiting Yuki"),
        Some(Status::AlreadyInYuki) => notes.push("already in Yuki"),
        Some(Status::Pending) => notes.push("upload outcome unknown"),
        Some(Status::Skipped) => notes.push("skipped"),
        Some(Status::Failed) | None => {}
    }
    if fit == Fit::VendorDate {
        notes.push("vendor and date only");
    }
    if notes.is_empty() {
        receipt.rel.clone()
    } else {
        format!("{} ({})", receipt.rel, notes.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(rel: &str, vendor: &str, date: &str, amount: Option<(&str, &str)>) -> Receipt {
        Receipt {
            rel: rel.into(),
            vendor: normalize_name(vendor),
            date: Some(date.into()),
            amount: amount.map(|(a, c)| (Cents::parse_exact(a).unwrap(), c.into())),
            sync: None,
        }
    }

    #[test]
    fn fx_is_read_from_bedrag_or_worked_out_from_koers() {
        let eur = Cents::parse_exact("17.52").unwrap();
        let fx = parse_fx(
            "VERCEL INC. Bedrag 20 USD COVINA C Koers (1 EUR = 1,141552511 USD)",
            eur,
        )
        .unwrap();
        assert_eq!(fx.label(), "20.00 USD");
        let fx = parse_fx(
            "NEON.TECH Bedrag 4,04 USD WILMINGTON D Koers (1 EUR = 1,138028169 USD)",
            Cents(355),
        )
        .unwrap();
        assert_eq!(fx.label(), "4.04 USD");
        let fx = parse_fx(
            "VERCEL INC. COVINA C Koers (1 EUR = 1,133144476 USD)",
            Cents(-1765),
        )
        .unwrap();
        assert_eq!(fx.label(), "~20.00 USD");
        assert_eq!(
            parse_fx("Tesla Belgium BVBA AARTSELAAR B", Cents(999)),
            None
        );
    }

    #[test]
    fn the_log_gives_amounts_by_file_name() {
        let log = read_log(
            "invoice_date,vendor,invoice_number,amount,currency,document_type,filename,collected_on\n\
             2026-08-07,Vercel,2E81616A-0007,20.00,USD,invoice,2026/vercel/a.pdf,2026-09-01\n\
             2026-08-21,\"Google, Inc\",x,25.99,EUR,invoice,2026/google/b.pdf,2026-09-01\n",
        );
        assert_eq!(
            log["2026/vercel/a.pdf"],
            (Some("2026-08-07".into()), Some((Cents(2000), "USD".into())))
        );
        assert_eq!(
            log["2026/google/b.pdf"].1,
            Some((Cents(2599), "EUR".into()))
        );
        assert!(read_log("vendor,amount\nx,1.00\n").is_empty());
    }

    #[test]
    fn a_receipt_matches_on_vendor_amount_and_date() {
        let receipts = vec![
            receipt(
                "vercel/0006.pdf",
                "vercel",
                "2026-07-07",
                Some(("20.00", "USD")),
            ),
            receipt(
                "vercel/0007.pdf",
                "vercel",
                "2026-08-07",
                Some(("20.00", "USD")),
            ),
            receipt("bol-com/1.pdf", "bol-com", "2026-08-21", None),
            receipt(
                "google/1.pdf",
                "google",
                "2026-08-20",
                Some(("25.99", "EUR")),
            ),
        ];
        let derived = parse_fx(
            "VERCEL INC. COVINA C Koers (1 EUR = 1,133144476 USD)",
            Cents(1765),
        );
        let wanted = [
            Wanted {
                supplier: "Vercel Inc",
                description: "VERCEL INC. COVINA C",
                date: "2026-08-07",
                eur: Cents(1765),
                fx: derived.as_ref(),
            },
            // No FX in the description: a USD receipt matches on vendor and
            // date only.
            Wanted {
                supplier: "Vercel Inc",
                description: "Vercrel inc",
                date: "2026-07-08",
                eur: Cents(1779),
                fx: None,
            },
            Wanted {
                supplier: "Bol.com",
                description: "",
                date: "2026-08-23",
                eur: Cents(3800),
                fx: None,
            },
            // Named only in the description, booked to a catch-all contact.
            Wanted {
                supplier: "Diverse leveranciers",
                description: "PAYPAL GOOGLE YOUTUBE 35314369001 0",
                date: "2026-08-21",
                eur: Cents(2599),
                fx: None,
            },
        ];
        let got = assign(&receipts, &wanted);
        assert_eq!(got[0], Some((1, Fit::Amount)));
        assert_eq!(got[1], Some((0, Fit::VendorDate)));
        assert_eq!(got[2], Some((2, Fit::VendorDate)));
        assert_eq!(got[3], Some((3, Fit::Amount)));
        assert_eq!(
            cell(&receipts[2], Fit::VendorDate),
            "bol-com/1.pdf (vendor and date only)"
        );
    }

    #[test]
    fn each_receipt_goes_to_one_payment() {
        let receipts = vec![receipt(
            "tesla/a.pdf",
            "tesla",
            "2026-09-01",
            Some(("9.99", "EUR")),
        )];
        let pay = |date| Wanted {
            supplier: "Tesla Belgium",
            description: "",
            date,
            eur: Cents(999),
            fx: None,
        };
        let got = assign(&receipts, &[pay("2026-09-04"), pay("2026-09-01")]);
        assert_eq!(got, vec![None, Some((0, Fit::Amount))]);
    }

    #[test]
    fn receipts_go_to_as_many_payments_as_possible() {
        let receipts = vec![
            receipt("x/r1.pdf", "acme", "2026-09-04", Some(("10.00", "EUR"))),
            receipt("x/r2.pdf", "acme", "2026-09-08", Some(("10.00", "EUR"))),
        ];
        let pay = |date| Wanted {
            supplier: "Acme",
            description: "",
            date,
            eur: Cents(1000),
            fx: None,
        };
        // Closest first would give r1 to B and leave A without one.
        let got = assign(&receipts, &[pay("2026-09-01"), pay("2026-09-05")]);
        assert_eq!(got, vec![Some((0, Fit::Amount)), Some((1, Fit::Amount))]);
    }

    #[test]
    fn a_description_names_the_vendor_only_when_no_receipt_is_the_suppliers() {
        let receipts = vec![receipt("google/a.pdf", "google", "2026-08-21", None)];
        let pay = |supplier| Wanted {
            supplier,
            description: "PAYPAL GOOGLE YOUTUBE",
            date: "2026-08-21",
            eur: Cents(2599),
            fx: None,
        };
        assert_eq!(
            assign(&receipts, &[pay("Diverse leveranciers")])[0].map(|m| m.0),
            Some(0)
        );
        let both = [
            receipt("google/a.pdf", "google", "2026-08-21", None),
            receipt("paypal/b.pdf", "paypal", "2026-01-01", None),
        ];
        // Paypal has receipts of its own, none in the window: not Google's.
        assert_eq!(assign(&both, &[pay("Paypal")]), vec![None]);
    }

    #[test]
    fn a_derived_amount_allows_for_the_rate() {
        let fx = parse_fx("SHOP TOKYO Koers (1 EUR = 159,98 JPY)", Cents(1875)).unwrap();
        assert_eq!(fx.label(), "~2999.63 JPY");
        assert_eq!(fx.slack, 81);
        let receipts = vec![receipt(
            "shop/a.pdf",
            "shop",
            "2026-09-01",
            Some(("3000", "JPY")),
        )];
        let wanted = Wanted {
            supplier: "Shop",
            description: "",
            date: "2026-09-01",
            eur: Cents(1875),
            fx: Some(&fx),
        };
        assert_eq!(assign(&receipts, &[wanted])[0], Some((0, Fit::Amount)));
        assert_eq!(
            parse_fx("X Bedrag 4.04 USD", Cents(355)).unwrap().label(),
            "4.04 USD"
        );
    }

    #[test]
    fn the_log_is_keyed_like_file_names_on_disk() {
        let log = read_log("filename,amount\n2026/caf\u{e9}/a.pdf,\"20,00\"\n");
        // macOS hands out NFD names.
        assert_eq!(
            log[&sync::name_key("2026/cafe\u{301}/a.pdf")].1,
            Some((Cents(2000), "EUR".into()))
        );
    }
}
