use crate::cli::accounts::resolve_period;
use crate::cli::setup_domain;
use crate::client::accounting_info::{AccountingInfoClient, TransactionDetail};
use crate::config::Config;
use crate::error::YukiError;
use crate::output::{
    ListOptions, OutputFormat, apply_pagination, format_json, format_table, is_tty, select_fields,
};
use crate::period::parse_period;

/// Which side of the ledger `invoices list` reports on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvoiceSide {
    /// Outstanding sales invoices (debtor items).
    Debtor,
    /// Outstanding purchase invoices (creditor items).
    Creditor,
}

/// Map the `--invoice-type` flag to a ledger side. Sales is the default.
fn invoice_side(invoice_type: Option<&str>) -> Result<InvoiceSide, YukiError> {
    let Some(raw) = invoice_type.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(InvoiceSide::Debtor);
    };
    match raw.to_ascii_lowercase().as_str() {
        "sales" | "debtor" => Ok(InvoiceSide::Debtor),
        "purchase" | "creditor" => Ok(InvoiceSide::Creditor),
        _ => Err(YukiError::Config(format!(
            "invalid invoice type: {raw}. Use sales (debtor) or purchase (creditor)."
        ))),
    }
}

pub async fn list(
    config: &Config,
    admin: Option<&str>,
    period: Option<&str>,
    invoice_type: Option<&str>,
    format: Option<&str>,
    opts: ListOptions<'_>,
) -> Result<(), YukiError> {
    let fmt = OutputFormat::from_flag(format, is_tty());
    let side = invoice_side(invoice_type)?;
    let range = period.map(parse_period).transpose()?;

    let (client, target) = setup_domain(config, admin).await?;
    let admin_id = target.admin_id;
    let items = match (side, range) {
        (InvoiceSide::Debtor, None) => client.outstanding_debtor_items(admin_id).await?,
        (InvoiceSide::Debtor, Some((start, end))) => {
            client
                .outstanding_debtor_items_by_date(admin_id, &start, &end)
                .await?
        }
        (InvoiceSide::Creditor, None) => client.outstanding_creditor_items(admin_id).await?,
        (InvoiceSide::Creditor, Some((start, end))) => {
            client
                .outstanding_creditor_items_by_date(admin_id, &start, &end)
                .await?
        }
    };

    let mut headers = vec![
        "Contact".into(),
        "Description".into(),
        "Date".into(),
        "Amount".into(),
        "Open".into(),
    ];
    let mut rows: Vec<Vec<String>> = items
        .iter()
        .map(|i| {
            vec![
                i.contact_name.clone(),
                i.description.clone(),
                i.date.clone(),
                i.amount.clone(),
                i.open_amount.clone(),
            ]
        })
        .collect();
    apply_pagination(&mut rows, &opts);
    select_fields(&mut headers, &mut rows, &opts)?;

    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }

    Ok(())
}

pub async fn document(
    config: &Config,
    admin: Option<&str>,
    id: &str,
    format: Option<&str>,
) -> Result<(), YukiError> {
    let target = config.target(admin)?;
    let mut client = AccountingInfoClient::new().with_api_root(target.api_root);
    client.authenticate(target.api_key).await?;
    let xml = client.get_transaction_document(target.admin_id, id).await?;

    let result = crate::client::soap_client::SoapClient::parse_single_result(
        &xml,
        "GetTransactionDocumentResult",
    )
    .unwrap_or(xml);

    let headers = vec!["Transaction".into(), "Document".into()];
    let rows = vec![vec![id.to_string(), result]];

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}

/// Keep only the line with `id`, or report which scope was searched.
fn only_transaction(
    details: Vec<TransactionDetail>,
    id: &str,
    account: &str,
) -> Result<Vec<TransactionDetail>, YukiError> {
    let found: Vec<TransactionDetail> = details.into_iter().filter(|d| d.id == id).collect();
    if found.is_empty() {
        return Err(YukiError::NotFound(format!(
            "transaction {id} on GL account {account} in the given period; \
             check --account and --period"
        )));
    }
    Ok(found)
}

/// Show one transaction.
///
/// Yuki's `GetTransactionDetails` cannot look a transaction up by ID: it returns
/// every line on one GL account in a date range. So this fetches that scope in a
/// single API call and filters client-side; the response grows with the
/// account's volume in the period, so a narrow `--period` is cheaper.
pub async fn show(
    config: &Config,
    admin: Option<&str>,
    id: &str,
    account: &str,
    period: Option<&str>,
    format: Option<&str>,
) -> Result<(), YukiError> {
    let (start, end) = resolve_period(period)?;
    let target = config.target(admin)?;
    let mut client = AccountingInfoClient::new().with_api_root(target.api_root);
    client.authenticate(target.api_key).await?;
    let details = client
        .get_transaction_details(target.admin_id, account, &start, &end)
        .await?;
    let details = only_transaction(details, id, account)?;

    let headers = vec![
        "ID".into(),
        "Date".into(),
        "Amount".into(),
        "Currency".into(),
        "GL Account".into(),
        "Contact".into(),
        "Description".into(),
    ];
    let rows: Vec<Vec<String>> = details
        .into_iter()
        .map(|d| {
            vec![
                d.id,
                d.date,
                d.amount,
                d.currency,
                d.gl_account_code,
                d.contact_name,
                d.description,
            ]
        })
        .collect();

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sales_and_default_list_outstanding_debtor_items() {
        assert_eq!(invoice_side(None).unwrap(), InvoiceSide::Debtor);
        assert_eq!(invoice_side(Some("sales")).unwrap(), InvoiceSide::Debtor);
        assert_eq!(invoice_side(Some("Debtor")).unwrap(), InvoiceSide::Debtor);
    }

    #[test]
    fn purchase_lists_outstanding_creditor_items() {
        assert_eq!(
            invoice_side(Some("purchase")).unwrap(),
            InvoiceSide::Creditor
        );
        assert_eq!(
            invoice_side(Some("CREDITOR")).unwrap(),
            InvoiceSide::Creditor
        );
    }

    fn detail(id: &str) -> TransactionDetail {
        TransactionDetail {
            id: id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn show_returns_only_the_requested_transaction() {
        let details = vec![detail("tx-1"), detail("tx-2"), detail("tx-3")];
        let found = only_transaction(details, "tx-2", "400000").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "tx-2");
    }

    #[test]
    fn show_reports_not_found_with_the_searched_scope() {
        let err = only_transaction(vec![detail("tx-1")], "tx-9", "400000").unwrap_err();
        assert!(matches!(err, YukiError::NotFound(_)), "{err}");
        assert!(err.to_string().contains("400000"), "{err}");
    }

    #[test]
    fn unknown_invoice_type_is_rejected() {
        let err = invoice_side(Some("items")).unwrap_err();
        assert!(err.to_string().contains("items"), "{err}");
    }
}
