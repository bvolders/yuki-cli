use crate::client::archive::ArchiveClient;
use crate::config::Config;
use crate::error::YukiError;
use crate::folders;
use crate::output::{ListOptions, OutputFormat, format_json, format_table, is_tty, select_fields};
use crate::period::current_year;

/// Parse a document type, which Yuki identifies by a numeric ID.
fn doc_type_id(value: &str) -> Result<i32, YukiError> {
    value.trim().parse::<i32>().map_err(|_| {
        YukiError::Config(format!(
            "invalid document type: {value} (expected a numeric document type ID)"
        ))
    })
}

pub async fn list(
    config: &Config,
    admin: Option<&str>,
    folder: Option<&str>,
    doc_type: Option<&str>,
    format: Option<&str>,
    opts: ListOptions<'_>,
) -> Result<(), YukiError> {
    let target = config.target(admin)?;
    let mut client = ArchiveClient::new().with_api_root(target.api_root);
    client.authenticate(target.api_key).await?;

    // Offset and limit are pushed into the API request, so no client-side slicing.
    let docs = match (folder, doc_type) {
        (Some(f), _) => {
            let folder_id = folders::folder_id(f)?;
            client
                .documents_in_folder_paged(
                    folder_id,
                    "2000-01-01",
                    "2099-12-31",
                    opts.limit,
                    opts.offset,
                )
                .await?
        }
        (None, Some(t)) => {
            let xml = client.documents_by_type(doc_type_id(t)?).await?;
            // documents_by_type returns raw XML; wrap it for display
            let headers = vec!["Raw XML".into()];
            let rows = vec![vec![xml]];
            let fmt = OutputFormat::from_flag(format, is_tty());
            match fmt {
                OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
                OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
            }
            return Ok(());
        }
        (None, None) => {
            client
                .documents_in_folder_paged(0, "2000-01-01", "2099-12-31", opts.limit, opts.offset)
                .await?
        }
    };

    let mut headers = vec![
        "ID".into(),
        "Date".into(),
        "Amount".into(),
        "Contact".into(),
        "Subject".into(),
        "File".into(),
    ];
    let mut rows: Vec<Vec<String>> = docs
        .into_iter()
        .map(|d| {
            vec![
                d.id,
                d.document_date,
                d.amount,
                d.contact_name,
                d.subject,
                d.file_name,
            ]
        })
        .collect();
    select_fields(&mut headers, &mut rows, &opts)?;

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}

pub async fn search(
    config: &Config,
    admin: Option<&str>,
    query: &str,
    format: Option<&str>,
) -> Result<(), YukiError> {
    let target = config.target(admin)?;
    let mut client = ArchiveClient::new().with_api_root(target.api_root);
    client.authenticate(target.api_key).await?;

    // Use current year as default search range when no date is specified.
    let year = current_year();
    let start = format!("{year}-01-01");
    let end = format!("{year}-12-31");

    let docs = client.search_documents(query, &start, &end).await?;

    let headers = vec![
        "ID".into(),
        "Date".into(),
        "Amount".into(),
        "Contact".into(),
        "Subject".into(),
        "File".into(),
    ];
    let rows: Vec<Vec<String>> = docs
        .into_iter()
        .map(|d| {
            vec![
                d.id,
                d.document_date,
                d.amount,
                d.contact_name,
                d.subject,
                d.file_name,
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

/// Check if an invoice exists in the archive by amount and date (±7 days).
///
/// Searches within a month around the given date, then filters by amount (±0.01)
/// and date proximity (±7 days). Returns matching documents or exit code 3 if none found.
pub async fn exists(
    config: &Config,
    admin: Option<&str>,
    amount: f64,
    date: &str,
    contact: Option<&str>,
    format: Option<&str>,
) -> Result<(), YukiError> {
    let (search_start, search_end, filter_start, filter_end) = date_range(date)?;

    let target = config.target(admin)?;
    let mut client = ArchiveClient::new().with_api_root(target.api_root);
    client.authenticate(target.api_key).await?;

    let search_text = contact.unwrap_or("");
    let docs = client
        .search_documents(search_text, &search_start, &search_end)
        .await?;

    let matched: Vec<_> = docs
        .into_iter()
        .filter(|d| {
            let amount_matches = d
                .amount
                .trim()
                .parse::<f64>()
                .map(|a| (a - amount).abs() <= 0.01)
                .unwrap_or(false);
            let date_matches = date_in_range(&d.document_date, &filter_start, &filter_end);
            amount_matches && date_matches
        })
        .collect();

    let headers = vec![
        "ID".into(),
        "Date".into(),
        "Amount".into(),
        "Contact".into(),
        "Subject".into(),
        "File".into(),
    ];
    let rows: Vec<Vec<String>> = matched
        .iter()
        .map(|d| {
            vec![
                d.id.clone(),
                d.document_date.clone(),
                d.amount.clone(),
                d.contact_name.clone(),
                d.subject.clone(),
                d.file_name.clone(),
            ]
        })
        .collect();

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }

    if rows.is_empty() {
        return Err(YukiError::NotFound(
            "no matching document found".to_string(),
        ));
    }

    Ok(())
}

/// Build a search window and date filter from a date string.
///
/// Accepts YYYY-MM-DD (±7 day match), YYYY-MM (full month), or YYYY-QN / YYYY (via parse_period).
/// Returns (search_start, search_end, filter_start, filter_end).
fn date_range(date: &str) -> Result<(String, String, String, String), YukiError> {
    let parts: Vec<&str> = date.split('-').collect();
    match parts.len() {
        3 => {
            // YYYY-MM-DD: search ±1 month, filter ±7 days
            let y: i32 = parts[0].parse().unwrap_or(0);
            let m: i32 = parts[1].parse().unwrap_or(0);
            let search_start = format!("{y:04}-{:02}-01", (m - 1).max(1));
            let search_end = format!("{y:04}-{:02}-28", (m + 1).min(12));
            let filter_start = shift_days(date, -7);
            let filter_end = shift_days(date, 7);
            Ok((search_start, search_end, filter_start, filter_end))
        }
        _ => {
            // YYYY-MM, YYYY-QN, YYYY: use parse_period for both search and filter
            let (start, end) = crate::period::parse_period(date)?;
            Ok((start.clone(), end.clone(), start, end))
        }
    }
}

/// Rough date shift by days on a YYYY-MM-DD string.
fn shift_days(date: &str, days: i32) -> String {
    let to_days = |s: &str| -> i32 {
        let p: Vec<&str> = s.split('-').collect();
        if p.len() < 3 {
            return 0;
        }
        let y: i32 = p[0].parse().unwrap_or(0);
        let m: i32 = p[1].parse().unwrap_or(0);
        let d: i32 = p[2].parse().unwrap_or(0);
        y * 365 + m * 30 + d
    };
    let from_days = |total: i32| -> String {
        let y = total / 365;
        let rem = total % 365;
        let m = (rem / 30).clamp(1, 12);
        let d = (rem % 30).clamp(1, 28);
        format!("{y:04}-{m:02}-{d:02}")
    };
    from_days(to_days(date) + days)
}

/// Check if a document date falls within a filter range.
fn date_in_range(doc_date: &str, filter_start: &str, filter_end: &str) -> bool {
    let normalized = doc_date.split('T').next().unwrap_or(doc_date);
    normalized >= filter_start && normalized <= filter_end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_range_exact_date() {
        let (ss, se, fs, fe) = date_range("2025-03-15").unwrap();
        // Search window: ±1 month
        assert_eq!(ss, "2025-02-01");
        assert_eq!(se, "2025-04-28");
        // Filter: ±7 days
        assert!(fs.as_str() <= "2025-03-08");
        assert!(fe.as_str() >= "2025-03-22");
    }

    #[test]
    fn date_range_month_period() {
        let (ss, se, fs, fe) = date_range("2025-03").unwrap();
        assert_eq!(ss, "2025-03-01");
        assert_eq!(se, "2025-03-31");
        assert_eq!(fs, ss);
        assert_eq!(fe, se);
    }

    #[test]
    fn date_range_quarter_period() {
        let (ss, se, fs, fe) = date_range("2025-Q1").unwrap();
        assert_eq!(ss, "2025-01-01");
        assert_eq!(se, "2025-03-31");
        assert_eq!(fs, ss);
        assert_eq!(fe, se);
    }

    #[test]
    fn date_range_year_period() {
        let (ss, se, fs, fe) = date_range("2025").unwrap();
        assert_eq!(ss, "2025-01-01");
        assert_eq!(se, "2025-12-31");
        assert_eq!(fs, ss);
        assert_eq!(fe, se);
    }

    #[test]
    fn date_in_range_within() {
        assert!(date_in_range("2025-03-15", "2025-03-01", "2025-03-31"));
    }

    #[test]
    fn date_in_range_strips_time() {
        assert!(date_in_range(
            "2025-03-15T10:30:00",
            "2025-03-01",
            "2025-03-31"
        ));
    }

    #[test]
    fn date_in_range_outside() {
        assert!(!date_in_range("2025-04-01", "2025-03-01", "2025-03-31"));
    }

    #[test]
    fn date_in_range_boundaries() {
        assert!(date_in_range("2025-03-01", "2025-03-01", "2025-03-31"));
        assert!(date_in_range("2025-03-31", "2025-03-01", "2025-03-31"));
    }

    #[test]
    fn shift_days_forward() {
        let result = shift_days("2025-03-15", 7);
        assert!(result.as_str() > "2025-03-15");
    }

    #[test]
    fn shift_days_backward() {
        let result = shift_days("2025-03-15", -7);
        assert!(result.as_str() < "2025-03-15");
    }
}
