use crate::cli::setup_domain;
use crate::client::accounting::{AccountingClient, GlAccountBalance};
use crate::client::accounting_info::AccountingInfoClient;
use crate::config::Config;
use crate::error::YukiError;
use crate::output::{
    ListOptions, OutputFormat, apply_pagination, format_json, format_table, is_tty, select_fields,
};
use crate::period::{parse_period, today};

pub async fn balance(
    config: &Config,
    admin: Option<&str>,
    account: Option<&str>,
    period: Option<&str>,
    format: Option<&str>,
) -> Result<(), YukiError> {
    let (_start, end) = resolve_period(period)?;
    // GLAccountBalance answers "balance on this date", so ask for the period's
    // end; a period still running has no balance yet beyond today.
    let as_of = balance_date(&end, &today());
    let (client, target) = setup_domain(config, admin).await?;
    let mut balances = client.gl_account_balances(target.admin_id, &as_of).await?;
    // The operation returns every account; narrow to one when --account is given.
    if let Some(code) = account {
        balances.retain(|b| b.code == code);
    }

    let (headers, rows) = balance_rows(&balances, &as_of);

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}

/// The date a period's balance is taken on: its last day, or today while the
/// period is still running. ISO dates compare correctly as strings.
fn balance_date(end: &str, today: &str) -> String {
    end.min(today).to_string()
}

/// Table rows for `accounts balance`, each stamped with the date it holds on.
fn balance_rows(balances: &[GlAccountBalance], as_of: &str) -> (Vec<String>, Vec<Vec<String>>) {
    let headers = vec![
        "Account".into(),
        "Description".into(),
        "Balance".into(),
        "As Of".into(),
    ];
    let rows = balances
        .iter()
        .map(|b| {
            vec![
                b.code.clone(),
                b.description.clone(),
                b.amount.clone(),
                as_of.to_string(),
            ]
        })
        .collect();
    (headers, rows)
}

pub async fn transactions(
    config: &Config,
    admin: Option<&str>,
    account: Option<&str>,
    period: Option<&str>,
    format: Option<&str>,
    opts: ListOptions<'_>,
) -> Result<(), YukiError> {
    let (start, end) = resolve_period(period)?;
    let gl_code = account.unwrap_or("");
    let (client, target) = setup_domain(config, admin).await?;
    let xml = client
        .gl_account_transactions(target.admin_id, gl_code, &start, &end)
        .await?;
    let transactions = AccountingClient::parse_gl_transactions(&xml)?;

    let mut headers = vec![
        "ID".into(),
        "Date".into(),
        "Amount".into(),
        "Description".into(),
    ];
    let mut rows: Vec<Vec<String>> = transactions
        .iter()
        .map(|t| {
            vec![
                t.id.clone(),
                t.date.clone(),
                t.amount.clone(),
                t.description.clone(),
            ]
        })
        .collect();
    apply_pagination(&mut rows, &opts);
    select_fields(&mut headers, &mut rows, &opts)?;

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}

pub async fn scheme(
    config: &Config,
    admin: Option<&str>,
    format: Option<&str>,
) -> Result<(), YukiError> {
    let target = config.target(admin)?;
    let mut client = AccountingInfoClient::new();
    client.authenticate(target.api_key).await?;
    let accounts = client.get_gl_account_scheme(target.admin_id).await?;

    let headers = vec!["Code".into(), "Description".into(), "Type".into()];
    let rows: Vec<Vec<String>> = accounts
        .into_iter()
        .map(|a| vec![a.code, a.description, a.account_type])
        .collect();

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}

pub async fn start_balance(
    config: &Config,
    admin: Option<&str>,
    year: Option<&str>,
    format: Option<&str>,
) -> Result<(), YukiError> {
    let year_str;
    let bookyear = match year {
        Some(y) => y,
        None => {
            year_str = current_year().to_string();
            &year_str
        }
    };
    let target = config.target(admin)?;
    let mut client = AccountingInfoClient::new();
    client.authenticate(target.api_key).await?;
    let balances = client
        .get_start_balance_by_gl_account(target.admin_id, bookyear)
        .await?;

    let headers = vec!["GL Account".into(), "Description".into(), "Balance".into()];
    let rows: Vec<Vec<String>> = balances
        .into_iter()
        .map(|b| vec![b.gl_account_code, b.description, b.balance])
        .collect();

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}

pub async fn revenue(
    config: &Config,
    admin: Option<&str>,
    period: Option<&str>,
    format: Option<&str>,
) -> Result<(), YukiError> {
    let (start, end) = resolve_period(period)?;
    let (client, target) = setup_domain(config, admin).await?;
    let amount = client.net_revenue(target.admin_id, &start, &end).await?;

    let headers = vec!["Period".into(), "Net Revenue".into()];
    let rows = vec![vec![format!("{start} to {end}"), amount]];

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}

/// Resolve an optional period string to (start_date, end_date).
///
/// When no period is given, defaults to the current calendar year.
pub(crate) fn resolve_period(period: Option<&str>) -> Result<(String, String), YukiError> {
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
    // Approximate: seconds since epoch divided by seconds per year
    1970 + (secs / 31_557_600) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_is_taken_at_the_period_end() {
        assert_eq!(balance_date("2025-12-31", "2026-09-28"), "2025-12-31");
    }

    #[test]
    fn balance_date_is_clamped_to_today_for_a_running_period() {
        assert_eq!(balance_date("2026-12-31", "2026-09-28"), "2026-09-28");
    }

    #[test]
    fn balance_rows_say_which_date_they_are_as_of() {
        let (headers, rows) = balance_rows(
            &[GlAccountBalance {
                code: "550003".into(),
                description: "Bank".into(),
                balance_type: "B".into(),
                amount: "100.00".into(),
            }],
            "2026-09-28",
        );
        assert_eq!(headers.last().unwrap(), "As Of");
        assert_eq!(rows[0], vec!["550003", "Bank", "100.00", "2026-09-28"]);
    }
}
