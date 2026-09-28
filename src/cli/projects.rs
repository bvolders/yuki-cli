use crate::client::accounting_info::AccountingInfoClient;
use crate::config::Config;
use crate::error::YukiError;
use crate::output::{OutputFormat, format_json, format_table, is_tty};
use crate::period::parse_period;

pub async fn list(
    config: &Config,
    admin: Option<&str>,
    format: Option<&str>,
) -> Result<(), YukiError> {
    let target = config.target(admin)?;
    let mut client = AccountingInfoClient::new().with_api_root(target.api_root);
    client.authenticate(target.api_key).await?;
    let projects = client.get_projects(target.admin_id).await?;

    let headers = vec!["ID".into(), "Code".into(), "Description".into()];
    let rows: Vec<Vec<String>> = projects
        .into_iter()
        .map(|p| vec![p.id, p.code, p.description])
        .collect();

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}

pub async fn balance(
    config: &Config,
    admin: Option<&str>,
    project: &str,
    account: Option<&str>,
    period: Option<&str>,
    format: Option<&str>,
) -> Result<(), YukiError> {
    let (start, end) = resolve_period(period)?;
    let gl_code = account.unwrap_or("");
    let target = config.target(admin)?;
    let mut client = AccountingInfoClient::new().with_api_root(target.api_root);
    client.authenticate(target.api_key).await?;
    let balances = client
        .get_project_balance(target.admin_id, project, gl_code, &start, &end)
        .await?;

    let headers = vec!["Project".into(), "GL Account".into(), "Amount".into()];
    let rows: Vec<Vec<String>> = balances
        .into_iter()
        .map(|b| vec![b.project_code, b.gl_account_code, b.amount])
        .collect();

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}

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
