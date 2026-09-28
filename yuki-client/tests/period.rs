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
