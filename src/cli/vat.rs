use crate::client::vat::VatClient;
use crate::config::Config;
use crate::error::YukiError;
use crate::output::{OutputFormat, format_json, format_table, is_tty};

pub async fn returns(
    config: &Config,
    admin: Option<&str>,
    year: Option<&str>,
    format: Option<&str>,
) -> Result<(), YukiError> {
    let target = config.target(admin)?;
    let mut client = VatClient::new().with_api_root(target.api_root);
    client.authenticate(target.api_key).await?;
    let all_returns = client.vat_return_list(target.admin_id).await?;

    let filtered: Vec<_> = match year {
        Some(y) => all_returns
            .into_iter()
            .filter(|r| r.period.starts_with(y))
            .collect(),
        None => all_returns,
    };

    let headers = vec![
        "Period".into(),
        "Status".into(),
        "Start".into(),
        "End".into(),
    ];
    let rows: Vec<Vec<String>> = filtered
        .iter()
        .map(|r| {
            vec![
                r.period.clone(),
                r.status.clone(),
                r.start_date.clone(),
                r.end_date.clone(),
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

pub async fn codes(
    config: &Config,
    admin: Option<&str>,
    format: Option<&str>,
) -> Result<(), YukiError> {
    let target = config.target(admin)?;
    let mut client = VatClient::new().with_api_root(target.api_root);
    client.authenticate(target.api_key).await?;
    let vat_codes = client.active_vat_codes(target.admin_id).await?;

    let headers = vec!["Code".into(), "Description".into()];
    let rows: Vec<Vec<String>> = vat_codes
        .iter()
        .map(|c| vec![c.code.clone(), c.description.clone()])
        .collect();

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}
