use yuki_client::period::{month_start_before, parse_period};

#[test]
fn parses_quarter_q1() {
    let (start, end) = parse_period("2025-Q1").unwrap();
    assert_eq!(start, "2025-01-01");
    assert_eq!(end, "2025-03-31");
}

#[test]
fn parses_quarter_q2() {
    let (start, end) = parse_period("2025-Q2").unwrap();
    assert_eq!(start, "2025-04-01");
    assert_eq!(end, "2025-06-30");
}

#[test]
fn parses_quarter_q3() {
    let (start, end) = parse_period("2025-Q3").unwrap();
    assert_eq!(start, "2025-07-01");
    assert_eq!(end, "2025-09-30");
}

#[test]
fn parses_quarter_q4() {
    let (start, end) = parse_period("2025-Q4").unwrap();
    assert_eq!(start, "2025-10-01");
    assert_eq!(end, "2025-12-31");
}

#[test]
fn parses_year_only() {
    let (start, end) = parse_period("2025").unwrap();
    assert_eq!(start, "2025-01-01");
    assert_eq!(end, "2025-12-31");
}

#[test]
fn parses_month_january() {
    let (start, end) = parse_period("2025-01").unwrap();
    assert_eq!(start, "2025-01-01");
    assert_eq!(end, "2025-01-31");
}

#[test]
fn parses_month_march() {
    let (start, end) = parse_period("2025-03").unwrap();
    assert_eq!(start, "2025-03-01");
    assert_eq!(end, "2025-03-31");
}

#[test]
fn parses_month_april() {
    let (start, end) = parse_period("2025-04").unwrap();
    assert_eq!(start, "2025-04-01");
    assert_eq!(end, "2025-04-30");
}

#[test]
fn parses_month_february_non_leap() {
    let (start, end) = parse_period("2025-02").unwrap();
    assert_eq!(start, "2025-02-01");
    assert_eq!(end, "2025-02-28");
}

#[test]
fn parses_month_february_leap() {
    let (start, end) = parse_period("2024-02").unwrap();
    assert_eq!(start, "2024-02-01");
    assert_eq!(end, "2024-02-29");
}

#[test]
fn invalid_string_returns_error() {
    assert!(parse_period("abc").is_err());
}

#[test]
fn invalid_quarter_q5_returns_error() {
    assert!(parse_period("2025-Q5").is_err());
}

#[test]
fn invalid_month_13_returns_error() {
    assert!(parse_period("2025-13").is_err());
}

#[test]
fn invalid_month_zero_returns_error() {
    assert!(parse_period("2025-00").is_err());
}

#[test]
fn month_start_before_crosses_year_boundaries() {
    assert_eq!(month_start_before("2026-07-01", 3), "2026-04-01");
    assert_eq!(month_start_before("2026-02-15", 3), "2025-11-01");
    assert_eq!(month_start_before("2026-01-01", 3), "2025-10-01");
}

#[test]
fn epoch_days_convert_to_calendar_dates() {
    use yuki_client::period::date_from_epoch_days;
    assert_eq!(date_from_epoch_days(0), "1970-01-01");
    assert_eq!(date_from_epoch_days(19_782), "2024-02-29");
    assert_eq!(date_from_epoch_days(20_724), "2026-09-28");
}

#[test]
fn today_is_an_iso_date() {
    let today = yuki_client::period::today();
    assert_eq!(today.len(), 10);
    assert!(today.as_str() >= "2026-01-01");
}

#[test]
fn epoch_days_parse_iso_dates_and_round_trip() {
    use yuki_client::period::{date_from_epoch_days, epoch_days};
    assert_eq!(epoch_days("1970-01-01"), Some(0));
    assert_eq!(epoch_days("2026-09-28"), Some(20_724));
    assert_eq!(epoch_days("2026-09-28T00:00:00"), Some(20_724));
    assert_eq!(
        epoch_days("2025-11-06")
            .map(|d| d - 90)
            .map(date_from_epoch_days),
        Some("2025-08-08".into())
    );
    assert_eq!(epoch_days("not a date"), None);
    assert_eq!(epoch_days("2026-13-01"), None);
}

#[test]
fn local_date_follows_the_utc_offset_across_midnight() {
    use yuki_client::period::date_at;
    // 2026-09-27 22:30 UTC is already 2026-09-28 in Brussels (UTC+2).
    let secs = 20_723 * 86_400 + 22 * 3_600 + 30 * 60;
    assert_eq!(date_at(secs, 0), "2026-09-27");
    assert_eq!(date_at(secs, 2 * 3_600), "2026-09-28");
    // West of Greenwich the day starts later.
    assert_eq!(date_at(20_724 * 86_400 + 3_600, -5 * 3_600), "2026-09-27");
}

#[cfg(unix)]
#[test]
fn today_is_the_local_calendar_date() {
    let out = std::process::Command::new("date")
        .arg("+%Y-%m-%d")
        .output()
        .unwrap();
    let local = String::from_utf8(out.stdout).unwrap();
    assert_eq!(yuki_client::period::today(), local.trim());
}
