//! `yuki check`: VAT, outstanding-item and unmatched-payment checks.

mod matches;
mod unmatched;

pub use matches::matches;
pub use unmatched::unmatched;

use crate::cli::setup_domain;
use crate::client::soap_client::SoapClient;
use crate::client::vat::VatClient;
use crate::config::Config;
use crate::error::YukiError;
use crate::output::{OutputFormat, format_json, format_table, is_tty};
use crate::period::{current_year, parse_period};

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

/// Resolve an optional period string to (start_date, end_date).
///
/// Defaults to the current calendar year when no period is given.
pub(super) fn resolve_period(period: Option<&str>) -> Result<(String, String), YukiError> {
    match period {
        Some(p) => parse_period(p),
        None => {
            let year = current_year();
            Ok((format!("{year}-01-01"), format!("{year}-12-31")))
        }
    }
}
