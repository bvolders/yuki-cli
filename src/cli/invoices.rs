use crate::cli::setup_domain;
use crate::client::accounting_info::AccountingInfoClient;
use crate::client::sales::SalesClient;
use crate::config::Config;
use crate::error::YukiError;
use crate::output::{
    ListOptions, OutputFormat, apply_pagination, format_json, format_table, is_tty, select_fields,
};

pub async fn list(
    config: &Config,
    admin: Option<&str>,
    _period: Option<&str>,
    invoice_type: Option<&str>,
    format: Option<&str>,
    opts: ListOptions<'_>,
) -> Result<(), YukiError> {
    let fmt = OutputFormat::from_flag(format, is_tty());

    match invoice_type {
        Some("purchase") | Some("creditor") => {
            let (client, target) = setup_domain(config, admin).await?;
            let items = client.outstanding_creditor_items(target.admin_id).await?;

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
        }

        // Default to sales invoices when type is "sales", "debtor", or unspecified
        _ => {
            let target = config.target(admin)?;
            let mut client = SalesClient::new().with_api_root(target.api_root);
            client.authenticate(target.api_key).await?;
            let items = client.get_sales_items().await?;

            let mut headers = vec!["ID".into(), "Description".into()];
            let mut rows: Vec<Vec<String>> = items
                .iter()
                .map(|i| vec![i.id.clone(), i.description.clone()])
                .collect();
            apply_pagination(&mut rows, &opts);
            select_fields(&mut headers, &mut rows, &opts)?;

            match fmt {
                OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
                OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
            }
        }
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

pub async fn show(
    config: &Config,
    admin: Option<&str>,
    id: &str,
    format: Option<&str>,
) -> Result<(), YukiError> {
    let target = config.target(admin)?;
    let mut client = AccountingInfoClient::new().with_api_root(target.api_root);
    client.authenticate(target.api_key).await?;
    let details = client.get_transaction_details(id).await?;

    let headers = vec![
        "ID".into(),
        "Date".into(),
        "Amount".into(),
        "Currency".into(),
        "GL Account".into(),
        "Description".into(),
    ];
    let rows: Vec<Vec<String>> = details
        .iter()
        .map(|d| {
            vec![
                d.id.clone(),
                d.date.clone(),
                d.amount.clone(),
                d.currency.clone(),
                d.gl_account_code.clone(),
                d.description.clone(),
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
