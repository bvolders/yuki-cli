use crate::error::YukiError;

/// Parse a period string into (start_date, end_date) as "YYYY-MM-DD" strings.
///
/// Supported formats:
/// - `"YYYY"` — full calendar year
/// - `"YYYY-QN"` — calendar quarter (Q1–Q4)
/// - `"YYYY-MM"` — calendar month
pub fn parse_period(period: &str) -> Result<(String, String), YukiError> {
    let invalid = || YukiError::Config(format!("invalid period: {period}"));

    // YYYY
    if period.len() == 4 && period.chars().all(|c| c.is_ascii_digit()) {
        let year: u32 = period.parse().map_err(|_| invalid())?;
        return Ok((format!("{year:04}-01-01"), format!("{year:04}-12-31")));
    }

    // YYYY-QN
    if period.len() == 7 {
        let (year_str, rest) = period.split_at(4);
        if let Some(q) = rest.strip_prefix("-Q") {
            let year: u32 = year_str.parse().map_err(|_| invalid())?;
            let quarter: u32 = q.parse().map_err(|_| invalid())?;
            let (start_month, end_month, end_day) = match quarter {
                1 => (1u32, 3u32, 31u32),
                2 => (4, 6, 30),
                3 => (7, 9, 30),
                4 => (10, 12, 31),
                _ => return Err(invalid()),
            };
            return Ok((
                format!("{year:04}-{start_month:02}-01"),
                format!("{year:04}-{end_month:02}-{end_day:02}"),
            ));
        }
    }

    // YYYY-MM
    if period.len() == 7 {
        let (year_str, rest) = period.split_at(4);
        if let Some(month_str) = rest.strip_prefix('-') {
            let year: u32 = year_str.parse().map_err(|_| invalid())?;
            let month: u32 = month_str.parse().map_err(|_| invalid())?;
            if month == 0 || month > 12 {
                return Err(invalid());
            }
            let last_day = days_in_month(year, month);
            return Ok((
                format!("{year:04}-{month:02}-01"),
                format!("{year:04}-{month:02}-{last_day:02}"),
            ));
        }
    }

    Err(invalid())
}

/// First day of the month `months` before the month of `date` (`YYYY-MM-DD`),
/// e.g. `month_start_before("2026-02-15", 3)` is `"2025-11-01"`.
///
/// An unparseable year or month falls back to 1970 and January.
pub fn month_start_before(date: &str, months: i32) -> String {
    let year: i32 = date.get(0..4).and_then(|y| y.parse().ok()).unwrap_or(1970);
    let month: i32 = date.get(5..7).and_then(|m| m.parse().ok()).unwrap_or(1);
    let total = year * 12 + (month - 1) - months;
    format!("{:04}-{:02}-01", total / 12, total % 12 + 1)
}

/// Today's date (UTC) as `YYYY-MM-DD`.
pub fn today() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    date_from_epoch_days((secs / 86_400) as i64)
}

/// Calendar date (`YYYY-MM-DD`) of a day count since 1970-01-01.
///
/// Howard Hinnant's `civil_from_days`, valid for the proleptic Gregorian calendar.
pub fn date_from_epoch_days(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Return the number of days in the given month of the given year.
fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => unreachable!("month already validated"),
    }
}

/// Determine whether a year is a leap year.
fn is_leap_year(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}
