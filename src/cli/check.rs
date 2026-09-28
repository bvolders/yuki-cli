use crate::cli::setup_domain;
use crate::client::archive::ArchiveClient;
use crate::client::soap_client::SoapClient;
use crate::client::vat::VatClient;
use crate::config::Config;
use crate::error::YukiError;
use crate::output::{OutputFormat, format_json, format_table, is_tty};
use crate::period::parse_period;

pub async fn btw(
    config: &Config,
    admin: Option<&str>,
    period: Option<&str>,
    format: Option<&str>,
    quiet: bool,
) -> Result<(), YukiError> {
    let (start, end) = resolve_period(period)?;
    let (accounting_client, target) = setup_domain(config, admin).await?;

    if !quiet {
        eprintln!("[1/3] Fetching VAT return list...");
    }
    let mut vat_client = VatClient::new().with_api_root(target.api_root);
    vat_client.authenticate(target.api_key).await?;
    let vat_returns = vat_client.vat_return_list(target.admin_id).await?;

    if !quiet {
        eprintln!("[2/3] Fetching outstanding debtor items...");
    }
    let debtors = accounting_client
        .outstanding_debtor_items_by_date(target.admin_id, &start, &end)
        .await?;

    if !quiet {
        eprintln!("[3/3] Fetching outstanding creditor items...");
    }
    let creditors = accounting_client
        .outstanding_creditor_items_by_date(target.admin_id, &start, &end)
        .await?;

    // Build report: VAT returns in period + outstanding items
    let headers = vec![
        "Type".into(),
        "Contact".into(),
        "Description".into(),
        "Date".into(),
        "Amount".into(),
        "Open".into(),
    ];
    let mut rows: Vec<Vec<String>> = Vec::new();

    for r in &vat_returns {
        if r.start_date >= start && r.end_date <= end {
            rows.push(vec![
                "VAT Return".into(),
                String::new(),
                format!("Period {} ({})", r.period, r.status),
                r.start_date.clone(),
                String::new(),
                String::new(),
            ]);
        }
    }

    for item in &debtors {
        rows.push(vec![
            "Debtor".into(),
            item.contact_name.clone(),
            item.description.clone(),
            item.date.clone(),
            item.amount.clone(),
            item.open_amount.clone(),
        ]);
    }

    for item in &creditors {
        rows.push(vec![
            "Creditor".into(),
            item.contact_name.clone(),
            item.description.clone(),
            item.date.clone(),
            item.amount.clone(),
            item.open_amount.clone(),
        ]);
    }

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}

/// Extract the counterparty name from a SEPA bank transaction description.
///
/// SEPA descriptions encode counterparty info in the format:
/// `/CNTP/<iban>/<bic>/<name>/`
///
/// Falls back to the first 50 characters of the description when the field is absent.
fn parse_counterparty(description: &str) -> String {
    if let Some(cntp_start) = description.find("/CNTP/") {
        let after_cntp = &description[cntp_start + 6..];
        let parts: Vec<&str> = after_cntp.splitn(4, '/').collect();
        if parts.len() >= 3 {
            return parts[2].trim().to_string();
        }
    }
    description
        .chars()
        .take(50)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Find bank transactions on a given GL account that have no matching invoice.
///
/// Each bank debit (negative amount) is first matched by absolute amount against outstanding
/// creditor items. Any remaining unmatched debits are then checked against booked invoices in
/// the archive for the same period. Only transactions that match neither source are reported.
pub async fn unmatched(
    config: &Config,
    admin: Option<&str>,
    period: Option<&str>,
    bank_account: &str,
    format: Option<&str>,
    quiet: bool,
) -> Result<(), YukiError> {
    let (start, end) = resolve_period(period)?;

    if !quiet {
        eprintln!("[1/3] Fetching bank transactions (GL {bank_account})...");
    }
    let (accounting_client, target) = setup_domain(config, admin).await?;
    let transactions = accounting_client
        .gl_account_transactions_and_contact(target.admin_id, bank_account, &start, &end)
        .await?;

    if !quiet {
        eprintln!("[2/3] Fetching outstanding creditor items...");
    }
    let creditor_items = accounting_client
        .outstanding_creditor_items(target.admin_id)
        .await?;

    if !quiet {
        eprintln!("[3/3] Fetching booked invoices from archive...");
    }
    let mut archive_client = ArchiveClient::new().with_api_root(target.api_root);
    archive_client.authenticate(target.api_key).await?;
    let archive_docs = archive_client.search_documents("", &start, &end).await?;

    if !quiet {
        eprintln!("API calls made: 4");
    }

    // Build a pool of creditor open amounts for single-pass matching.
    // Key: canonical amount string (absolute value), value: remaining count.
    let mut creditor_pool: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for item in &creditor_items {
        let key = item.open_amount.trim().to_string();
        *creditor_pool.entry(key).or_insert(0) += 1;
    }

    // Build a pool of archive document amounts for matching booked invoices.
    // Only positive amounts (purchase invoices) are relevant.
    let mut archive_pool: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for doc in &archive_docs {
        if let Ok(amt) = doc.amount.trim().parse::<f64>()
            && amt > 0.0
        {
            let key = format!("{amt:.2}");
            *archive_pool.entry(key).or_insert(0) += 1;
        }
    }

    // Build a set of normalized contact names from the archive for fallback name matching.
    // This catches batched or split charges where the amount differs but the supplier is known.
    let archive_names: std::collections::HashSet<String> = archive_docs
        .iter()
        .filter(|d| !d.contact_name.is_empty())
        .map(|d| normalize_name(&d.contact_name))
        .filter(|n| !n.is_empty())
        .collect();

    let mut unmatched_rows: Vec<Vec<String>> = Vec::new();

    for tx in &transactions {
        let amount: f64 = tx.amount.trim().parse().unwrap_or(0.0);
        if amount >= 0.0 {
            // Only debits (payments out) are relevant.
            continue;
        }
        // Represent the absolute value as a rounded-cent string for matching.
        let abs_amount = format!("{:.2}", amount.abs());

        // Check outstanding creditor items first.
        if let Some(count) = creditor_pool.get_mut(&abs_amount)
            && *count > 0
        {
            *count -= 1;
            continue;
        }

        // Check booked invoices in the archive by amount.
        if let Some(count) = archive_pool.get_mut(&abs_amount)
            && *count > 0
        {
            *count -= 1;
            continue;
        }

        // Use API-provided contact name when available, fall back to SEPA parsing.
        let counterparty = if tx.contact_name.is_empty() {
            parse_counterparty(&tx.description)
        } else {
            tx.contact_name.clone()
        };

        // Skip counterparties matching the configured ignore list.
        let cp_lower = counterparty.to_lowercase();
        if config
            .unmatched_ignore
            .iter()
            .any(|pat| cp_lower.contains(&pat.to_lowercase()))
        {
            continue;
        }

        // Fallback: check if the counterparty name is known in the archive.
        // Handles batched or split charges where the amount may differ.
        if archive_names.iter().any(|n| names_match(&counterparty, n)) {
            continue;
        }

        unmatched_rows.push(vec![
            tx.id.clone(),
            tx.date.clone(),
            format!("-{abs_amount}"),
            counterparty,
            tx.description.chars().take(80).collect::<String>(),
        ]);
    }

    let headers = vec![
        "ID".into(),
        "Date".into(),
        "Amount".into(),
        "Counterparty".into(),
        "Description".into(),
    ];

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &unmatched_rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &unmatched_rows)),
    }
    Ok(())
}

/// Check if a specific invoice reference is still outstanding.
pub async fn outstanding(
    config: &Config,
    admin: Option<&str>,
    reference: &str,
    format: Option<&str>,
) -> Result<(), YukiError> {
    let (client, target) = setup_domain(config, admin).await?;
    let xml = client
        .check_outstanding_item_admin(target.admin_id, reference)
        .await?;
    let result =
        SoapClient::parse_single_result(&xml, "CheckOutstandingItemAdminResult").unwrap_or(xml);

    let headers = vec!["Reference".into(), "Result".into()];
    let rows = vec![vec![reference.to_string(), result]];

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}

/// Normalize a company name for fuzzy matching.
///
/// Lowercases the name, removes "via ..." suffixes, strips common legal suffixes
/// and punctuation, and trims whitespace. This allows loose matching between bank
/// counterparty names and Yuki contact names despite formatting differences.
fn normalize_name(name: &str) -> String {
    let lower = name.to_lowercase();
    // Remove "via ..." suffix (e.g. "Vimexx via Mollie" -> "vimexx")
    let base = lower.split(" via ").next().unwrap_or(&lower);
    base.replace("b.v.", "")
        .replace("bv", "")
        .replace("gmbh", "")
        .replace("inc", "")
        .replace("ltd", "")
        .replace("s.a.", "")
        .replace(['.', ','], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Check whether two company names refer to the same entity.
///
/// Returns true if one normalized name contains the other, allowing for
/// abbreviations or partial matches.
fn names_match(bank_name: &str, archive_name: &str) -> bool {
    let a = normalize_name(bank_name);
    let b = normalize_name(archive_name);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a.contains(b.as_str()) || b.contains(a.as_str())
}

/// Resolve an optional period string to (start_date, end_date).
///
/// Defaults to the current calendar year when no period is given.
fn resolve_period(period: Option<&str>) -> Result<(String, String), YukiError> {
    match period {
        Some(p) => parse_period(p),
        None => {
            let year = current_year();
            Ok((format!("{year}-01-01"), format!("{year}-12-31")))
        }
    }
}

fn current_year() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    1970 + (secs / 31_557_600) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_counterparty_extracts_cntp_name() {
        let desc = "/TRTP/SEPA/CNTP/NL01ABNA0001234567/ABNANL2A/Vimexx B.V./REMI/Hosting";
        assert_eq!(parse_counterparty(desc), "Vimexx B.V.");
    }

    #[test]
    fn parse_counterparty_falls_back_to_description() {
        assert_eq!(
            parse_counterparty("ING bankkosten maart"),
            "ING bankkosten maart"
        );
    }

    #[test]
    fn parse_counterparty_truncates_long() {
        let desc = "A".repeat(100);
        assert_eq!(parse_counterparty(&desc).len(), 50);
    }

    #[test]
    fn normalize_name_strips_legal_suffixes() {
        assert_eq!(normalize_name("Vimexx B.V."), "vimexx");
        assert_eq!(normalize_name("Hetzner GmbH"), "hetzner");
        assert_eq!(normalize_name("Amazon Inc"), "amazon");
    }

    #[test]
    fn normalize_name_removes_via_suffix() {
        assert_eq!(normalize_name("Vimexx via Mollie"), "vimexx");
    }

    #[test]
    fn normalize_name_handles_empty() {
        assert_eq!(normalize_name(""), "");
    }

    #[test]
    fn names_match_bidirectional_substring() {
        assert!(names_match("Hetzner Online GmbH", "Hetzner"));
        assert!(names_match("Hetzner", "Hetzner Online GmbH"));
    }

    #[test]
    fn names_match_case_insensitive() {
        assert!(names_match("HETZNER", "hetzner online"));
    }

    #[test]
    fn names_match_strips_legal_suffixes() {
        assert!(names_match("Vimexx B.V.", "Vimexx via Mollie"));
    }

    #[test]
    fn names_match_rejects_empty() {
        assert!(!names_match("", "Hetzner"));
        assert!(!names_match("Hetzner", ""));
    }

    #[test]
    fn names_match_rejects_unrelated() {
        assert!(!names_match("Hetzner", "Amazon"));
    }
}
