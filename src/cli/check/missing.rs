//! `yuki check missing`: Yuki's "Betalingen waarvan factuur nog ontbreekt"
//! with "Toon openstaande facturen" ticked. Per supplier: the bank and card
//! payments no invoice settles yet, and that supplier's open invoices.
//!
//! One read, `OutstandingCreditorItemsByDateOutstanding` with bank
//! transactions included: the creditors account's open items as of today,
//! payments among them. It suggests no pairing; `check matches` does that.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use super::receipts::{self, Fx, Wanted, parse_fx};
use super::unmatched::Cents;
use crate::cli::setup_domain;
use crate::client::accounting::OutstandingItem;
use crate::config::Config;
use crate::error::YukiError;
use crate::output::{OutputFormat, format_json, format_table, is_tty};
use crate::period::{parse_period, today};

/// One supplier's group, as the UI shows it.
#[derive(Debug, Default)]
struct Group<'a> {
    payments: Vec<&'a OutstandingItem>,
    invoices: Vec<&'a OutstandingItem>,
}

/// Group the open items per supplier: every supplier with an unsettled
/// payment (dated in `period`, when given) whose name contains `contact`,
/// with all its open invoices and credit notes. Suppliers sort by name,
/// case-insensitively, and items by date.
fn groups<'a>(
    items: &'a [OutstandingItem],
    period: Option<&(String, String)>,
    contact: Option<&str>,
) -> Vec<(&'a str, Group<'a>)> {
    let wanted =
        |name: &str| contact.is_none_or(|c| name.to_lowercase().contains(&c.to_lowercase()));
    let in_period = |date: &str| {
        let day = date.get(0..10).unwrap_or(date);
        period.is_none_or(|(start, end)| start.as_str() <= day && day <= end.as_str())
    };
    let mut by_supplier: BTreeMap<(String, &str), Group<'a>> = BTreeMap::new();
    for item in items.iter().filter(|i| wanted(&i.contact_name)) {
        let group = by_supplier
            .entry((item.contact_name.to_lowercase(), item.contact_name.as_str()))
            .or_default();
        if !item.is_payment() {
            group.invoices.push(item);
        } else if in_period(&item.date) {
            group.payments.push(item);
        }
    }
    by_supplier
        .into_iter()
        .filter(|(_, g)| !g.payments.is_empty())
        .map(|((_, name), mut g)| {
            g.payments.sort_by(|a, b| a.date.cmp(&b.date));
            g.invoices.sort_by(|a, b| a.date.cmp(&b.date));
            (name, g)
        })
        .collect()
}

fn cents(amount: &str) -> Cents {
    Cents::parse(amount).unwrap_or_default()
}

/// Totals for one supplier, or for all.
#[derive(Debug, Default, Clone, Copy)]
struct Tally {
    payments: usize,
    paid: Cents,
    invoices: usize,
    invoiced: Cents,
    receipts: usize,
}

impl Tally {
    fn add(&mut self, other: Tally) {
        self.payments += other.payments;
        self.paid.0 += other.paid.0;
        self.invoices += other.invoices;
        self.invoiced.0 += other.invoiced.0;
        self.receipts += other.receipts;
    }
}

/// List, per supplier, the payments Yuki has no invoice for and the
/// supplier's open invoices; see the module docs.
pub async fn missing(
    config: &Config,
    admin: Option<&str>,
    period: Option<&str>,
    receipts_dir: Option<&Path>,
    contact: Option<&str>,
    format: Option<&str>,
    quiet: bool,
) -> Result<(), YukiError> {
    let period = period.map(parse_period).transpose()?;
    // Read the folder first: a bad path fails before any API call.
    let local = receipts_dir.map(receipts::load).transpose()?;
    let (client, target) = setup_domain(config, admin).await?;
    let as_of = today();
    if !quiet {
        eprintln!("Fetching open creditor items and unsettled payments (as of {as_of})...");
    }
    let items = client
        .outstanding_creditor_items_with_payments(target.admin_id, &as_of)
        .await?;
    if !quiet {
        // Authenticate, SetCurrentDomain and the one read.
        eprintln!("API calls made: 3");
    }

    let groups = groups(&items, period.as_ref(), contact);
    let fx: Vec<Vec<Option<Fx>>> = groups
        .iter()
        .map(|(_, g)| {
            g.payments
                .iter()
                .map(|p| parse_fx(&p.description, cents(&p.amount)))
                .collect()
        })
        .collect();
    // Receipts are matched across all suppliers at once, so one file never
    // serves two payments.
    let wanted: Vec<Wanted<'_>> = groups
        .iter()
        .zip(&fx)
        .flat_map(|((name, g), fx)| {
            g.payments.iter().zip(fx).map(|(p, fx)| Wanted {
                supplier: name,
                description: &p.description,
                date: &p.date,
                eur: cents(&p.amount).abs(),
                fx: fx.as_ref(),
            })
        })
        .collect();
    let found = local
        .as_ref()
        .map(|l| receipts::assign(&l.files, &wanted))
        .unwrap_or_default();
    let mut found = found.into_iter();

    let fmt = OutputFormat::from_flag(format, is_tty());
    let mut headers: Vec<String> = [
        "Supplier",
        "Kind",
        "Date",
        "Description",
        "Reference",
        "Amount",
        "Open",
        "FX",
    ]
    .iter()
    .map(|h| (*h).to_string())
    .collect();
    if local.is_some() {
        headers.push("Local receipt".into());
    }
    let description = |text: &str| match fmt {
        OutputFormat::Table => text.chars().take(70).collect(),
        OutputFormat::Json => text.to_string(),
    };
    let mut rows = Vec::new();
    let mut tallies = Vec::new();
    for ((name, group), fx) in groups.iter().zip(&fx) {
        let mut tally = Tally::default();
        for (payment, fx) in group.payments.iter().zip(fx) {
            tally.payments += 1;
            tally.paid.0 += cents(&payment.open_amount).0;
            let mut row = vec![
                (*name).to_string(),
                "payment".into(),
                payment.date.clone(),
                description(&payment.description),
                String::new(),
                payment.amount.clone(),
                payment.open_amount.clone(),
                fx.as_ref().map(Fx::label).unwrap_or_default(),
            ];
            if let Some(local) = &local {
                let cell = found.next().flatten().map(|(r, fit)| {
                    tally.receipts += 1;
                    receipts::cell(&local.files[r], fit)
                });
                row.push(cell.unwrap_or_default());
            }
            rows.push(row);
        }
        for invoice in &group.invoices {
            tally.invoices += 1;
            tally.invoiced.0 += cents(&invoice.open_amount).0;
            let kind = if cents(&invoice.open_amount) < Cents::ZERO {
                "open credit note"
            } else {
                "open invoice"
            };
            let mut row = vec![
                (*name).to_string(),
                kind.into(),
                invoice.date.clone(),
                description(&invoice.description),
                invoice.reference.clone(),
                invoice.amount.clone(),
                invoice.open_amount.clone(),
                String::new(),
            ];
            if local.is_some() {
                row.push(String::new());
            }
            rows.push(row);
        }
        tallies.push((*name, tally));
    }

    let mut total = Tally::default();
    for (_, t) in &tallies {
        total.add(*t);
    }
    let mut summary_headers: Vec<String> = [
        "Supplier",
        "Payments",
        "Payments open",
        "Open invoices",
        "Invoices open",
    ]
    .iter()
    .map(|h| (*h).to_string())
    .collect();
    if local.is_some() {
        summary_headers.push("Local receipts".into());
    }
    let summary_row = |name: &str, t: &Tally| {
        let mut row = vec![
            name.to_string(),
            t.payments.to_string(),
            t.paid.to_string(),
            t.invoices.to_string(),
            t.invoiced.to_string(),
        ];
        if local.is_some() {
            row.push(t.receipts.to_string());
        }
        row
    };
    let summary: Vec<Vec<String>> = tallies.iter().map(|(n, t)| summary_row(n, t)).collect();
    let mut notes = Vec::new();
    if let Some(l) = &local {
        if !l.has_amounts {
            notes.push(format!(
                "no {} with amounts: receipts matched on vendor and date only",
                receipts::RECEIPTS_LOG
            ));
        }
        if let Some(e) = &l.state_error {
            notes.push(format!("uploads not shown: {e}"));
        } else if !l.has_sync_state {
            notes.push(format!("no {}: uploads not shown", crate::sync::STATE_FILE));
        }
    }
    let line = format!(
        "{} payments without an invoice (open {} EUR) at {} suppliers; {} open invoices (open {} EUR){}",
        total.payments,
        total.paid,
        tallies.len(),
        total.invoices,
        total.invoiced,
        if local.is_some() {
            format!("; {} with a local receipt", total.receipts)
        } else {
            String::new()
        }
    );

    match fmt {
        OutputFormat::Table => {
            println!("{}", format_table(&headers, &rows));
            if !tallies.is_empty() {
                println!("{}", format_table(&summary_headers, &summary));
            }
            println!("{line}");
            for note in &notes {
                println!("Note: {note}");
            }
        }
        OutputFormat::Json => {
            let mut out: Value = serde_json::from_str(&format_json(&headers, &rows))
                .expect("format_json writes JSON");
            let suppliers: Value = serde_json::from_str(&format_json(&summary_headers, &summary))
                .expect("format_json writes JSON");
            out["suppliers"] = suppliers["items"].clone();
            out["summary"] = json!({
                "suppliers": tallies.len(),
                "payments": total.payments,
                "payments_open": total.paid.to_string(),
                "open_invoices": total.invoices,
                "invoices_open": total.invoiced.to_string(),
                "local_receipts": local.as_ref().map(|_| total.receipts),
                "as_of": as_of,
                "notes": notes,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&out).expect("serialize output")
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(contact: &str, date: &str, open: &str, type_id: &str) -> OutstandingItem {
        OutstandingItem {
            contact_name: contact.into(),
            date: date.into(),
            amount: open.into(),
            open_amount: open.into(),
            type_id: type_id.into(),
            type_name: if type_id.is_empty() {
                "Creditcardbetaling"
            } else {
                "Aankoopfactuur"
            }
            .into(),
            ..Default::default()
        }
    }

    #[test]
    fn groups_hold_suppliers_with_a_payment_and_their_open_invoices() {
        let items = vec![
            item("Vercel Inc", "2026-09-07", "17.22", "2"),
            item("Vercel Inc", "2026-09-07", "-17.52", ""),
            item("Vercel Inc", "2026-02-08", "-17.27", ""),
            item("Ge-fibo", "2026-09-30", "446.56", "2"),
            item("amazon", "2026-08-21", "-20.87", ""),
        ];
        let all = groups(&items, None, None);
        let names: Vec<&str> = all.iter().map(|(n, _)| *n).collect();
        // Ge-fibo has only an open invoice: not in the list.
        assert_eq!(names, ["amazon", "Vercel Inc"]);
        let vercel = &all[1].1;
        assert_eq!(vercel.payments[0].date, "2026-02-08");
        assert_eq!(vercel.invoices.len(), 1);

        let q3 = ("2026-07-01".to_string(), "2026-09-30".to_string());
        let in_q3 = groups(&items, Some(&q3), Some("VERCEL"));
        assert_eq!(in_q3.len(), 1);
        assert_eq!(in_q3[0].1.payments.len(), 1);
        assert_eq!(in_q3[0].1.invoices.len(), 1);
    }
}
