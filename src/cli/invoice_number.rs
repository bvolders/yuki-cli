//! Invoice numbers given by the CLI rather than Yuki, so a PDF rendered
//! beforehand can show the number Yuki will book.
//!
//! Yuki names the PDFs in the sales archive after the invoice number:
//! `Invoice 2026-19.pdf`. `--number auto` reads that folder for the invoice
//! year, adds the numbers the local ledger ([`invoice_ledger`]) still holds,
//! takes the highest `<year>-<seq>` of that year and adds one, padded as the
//! existing numbers are. Yuki's own counter does not learn about numbers
//! given this way, so once invoices are numbered here, number them all here.

use crate::cli::invoice_ledger::{InvoiceLedger, Numbers};
use crate::cli::setup_domain;
use crate::client::archive::ArchiveClient;
use crate::config::Config;
use crate::error::YukiError;
use crate::folders::folder_id;

/// `--number`: a given invoice number, or `auto`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NumberRequest {
    Auto,
    Given(String),
}

/// Parse `--number`: `auto` (any case) or a non-empty number.
pub fn parse_number_request(text: &str) -> Result<NumberRequest, String> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("auto") {
        Ok(NumberRequest::Auto)
    } else if text.is_empty() || text.chars().any(char::is_control) {
        Err("an invoice number cannot be empty or contain control characters".into())
    } else if text.chars().count() > 40 {
        Err(format!("'{text}' is longer than 40 characters"))
    } else {
        Ok(NumberRequest::Given(text.to_string()))
    }
}

/// A `<year>-<seq>` number: its year, sequence and the sequence's digit count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YearNumber {
    pub year: u32,
    pub seq: u64,
    pub digits: usize,
}

/// `text` as `<year>-<seq>`: four digits, a dash, then up to six digits.
pub fn year_number(text: &str) -> Option<YearNumber> {
    let (year, seq) = text.split_once('-')?;
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if year.len() != 4 || !all_digits(year) || seq.len() > 6 || !all_digits(seq) {
        return None;
    }
    Some(YearNumber {
        year: year.parse().ok()?,
        seq: seq.parse().ok()?,
        digits: seq.len(),
    })
}

/// The number of an invoice PDF in the sales archive: a `.pdf` named
/// `Invoice <year>-<seq>` or `Factuur <year>-<seq>` (any case), as Yuki names
/// them. Any other file, such as a timesheet or a date-stamped report, has
/// no number even when its name holds something like `2026-01`.
pub fn number_in_file_name(name: &str) -> Option<YearNumber> {
    let lower = name.trim().to_ascii_lowercase();
    let stem = lower.strip_suffix(".pdf")?;
    let rest = stem
        .strip_prefix("invoice ")
        .or_else(|| stem.strip_prefix("factuur "))?;
    year_number(rest.trim())
}

/// The invoice numbers among `files`, as `<year>-<seq>` text.
pub fn numbers_in_file_names(files: &[String]) -> Vec<String> {
    files
        .iter()
        .filter_map(|f| number_in_file_name(f))
        .map(|n| format!("{}-{:0width$}", n.year, n.seq, width = n.digits))
        .collect()
}

/// The next number for `year` after `numbers` (`<year>-<seq>` texts; others
/// are ignored): one past the highest, with its sequence zero-padded to the
/// widest padded sequence of that year (Yuki's own are not padded:
/// `2026-9`, `2026-10`).
pub fn next_number(numbers: &[String], year: u32) -> String {
    next_free(numbers, &[], year)
}

/// The lowest number for `year` above the highest in `archive` that `held`
/// (the ledger's reserved, pending and booked numbers) does not hold: a
/// number released or rejected is given out again rather than skipped, so
/// the numbering stays without gaps. Padded as [`next_number`] pads.
pub fn next_free(archive: &[String], held: &[String], year: u32) -> String {
    let of_year = |numbers: &[String]| -> Vec<YearNumber> {
        numbers
            .iter()
            .filter_map(|n| year_number(n))
            .filter(|n| n.year == year)
            .collect()
    };
    let (archived, held) = (of_year(archive), of_year(held));
    let mut seq = archived.iter().map(|n| n.seq).max().unwrap_or(0) + 1;
    while held.iter().any(|n| n.seq == seq) {
        seq += 1;
    }
    let width = archived
        .iter()
        .chain(&held)
        .filter(|n| n.digits > 1 && n.digits > n.seq.to_string().len())
        .map(|n| n.digits)
        .max()
        .unwrap_or(0);
    format!("{year}-{seq:0width$}")
}

/// Whether `a` and `b` are the same invoice number: the same year and
/// sequence for `<year>-<seq>` numbers (`2026-019` is `2026-19`), else the
/// same text.
pub fn same_number(a: &str, b: &str) -> bool {
    match (year_number(a), year_number(b)) {
        (Some(a), Some(b)) => a.year == b.year && a.seq == b.seq,
        _ => a == b,
    }
}

/// Whether `numbers` holds `number`, compared as [`same_number`].
pub fn taken(numbers: &[String], number: &str) -> bool {
    numbers.iter().any(|n| same_number(n, number))
}

/// The invoice numbers in the sales (`verkoop`) archive for `years`, each
/// year read on its own date range.
async fn archive_numbers(
    config: &Config,
    admin: Option<&str>,
    years: &[u32],
) -> Result<Vec<String>, YukiError> {
    let (accounting, target) = setup_domain(config, admin).await?;
    let session = accounting.session_id().unwrap_or_default();
    let client = ArchiveClient::new()
        .with_api_root(target.api_root)
        .with_session(session);
    let mut files = Vec::new();
    for year in years {
        let documents = client
            .documents_in_folder_strict(
                folder_id("verkoop")?,
                &format!("{year}-01-01"),
                &format!("{year}-12-31"),
            )
            .await?;
        files.extend(documents.into_iter().map(|d| d.file_name));
    }
    Ok(numbers_in_file_names(&files))
}

/// The number for an invoice dated `date`: the next one for `auto`, or the
/// given one, refused when the sales archive or the ledger already has it.
pub async fn resolve(
    config: &Config,
    admin: Option<&str>,
    request: &NumberRequest,
    date: &str,
) -> Result<String, YukiError> {
    let year: u32 = date[..4]
        .parse()
        .map_err(|_| YukiError::Config(format!("'{date}' has no year")))?;
    let mut years = vec![year];
    if let NumberRequest::Given(number) = request
        && let Some(n) = year_number(number)
        && n.year != year
    {
        years.push(n.year);
    }
    let admin_id = config.target(admin)?.admin_id;
    let archive = archive_numbers(config, admin, &years).await?;
    choose(request, year, &archive, &InvoiceLedger::peek()?, admin_id)
}

/// [`resolve`] once the archive's numbers are known.
pub fn choose(
    request: &NumberRequest,
    year: u32,
    archive: &[String],
    ledger: &Numbers,
    admin: &str,
) -> Result<String, YukiError> {
    let held: Vec<String> = ledger.taken_numbers(admin).map(str::to_string).collect();
    let number = match request {
        NumberRequest::Auto => next_free(archive, &held, year),
        NumberRequest::Given(number) => number.clone(),
    };
    if taken(archive, &number) {
        return Err(YukiError::Config(format!(
            "invoice number {number} is already in the sales archive"
        )));
    }
    if taken(&held, &number) {
        let status = ledger.holder(admin, &number).map_or("taken", |e| match e.status {
            crate::cli::invoice_ledger::Status::Pending => {
                "pending (outcome unknown: check Yuki, then `yuki sales invoice numbers --resolve`)"
            }
            crate::cli::invoice_ledger::Status::Reserved => {
                "reserved by `prepare --out` (free it with `yuki sales invoice numbers --resolve <number> --as rejected`)"
            }
            _ => "booked",
        });
        return Err(YukiError::Config(format!(
            "invoice number {number} was already given out: {status}"
        )));
    }
    Ok(number)
}

/// The Belgian structured payment reference (OGM/VCS) for an invoice number,
/// `+++DDD/DDDD/DDDCC+++`.
///
/// The ten base digits are the year and the sequence padded to six digits
/// for a `<year>-<seq>` number (`2026-20` → `2026000020`), else every digit
/// of the number, left-padded with zeros. The check digits `CC` are the base
/// modulo 97, with 97 for a remainder of 0.
pub fn structured_reference(number: &str) -> Result<String, String> {
    let base: u64 = match year_number(number) {
        Some(n) => u64::from(n.year) * 1_000_000 + n.seq,
        None => {
            let digits: String = number.chars().filter(char::is_ascii_digit).collect();
            if digits.is_empty() || digits.len() > 10 {
                return Err(format!(
                    "'{number}' needs 1 to 10 digits for a structured reference"
                ));
            }
            digits
                .parse()
                .map_err(|_| format!("'{number}' is not a number"))?
        }
    };
    let check = match base % 97 {
        0 => 97,
        rest => rest,
    };
    let d = format!("{base:010}{check:02}");
    Ok(format!("+++{}/{}/{}+++", &d[..3], &d[3..7], &d[7..]))
}

/// The Dutch month names, January first.
pub const MONTHS: [&str; 12] = [
    "januari",
    "februari",
    "maart",
    "april",
    "mei",
    "juni",
    "juli",
    "augustus",
    "september",
    "oktober",
    "november",
    "december",
];

/// An ISO date as Dutch text: `2026-09-30` → `30 september 2026`.
pub fn dutch_date(iso: &str) -> String {
    let parts: Vec<u32> = iso.split('-').filter_map(|p| p.parse().ok()).collect();
    match parts.as_slice() {
        [year, month @ 1..=12, day] => {
            format!("{day} {} {year}", MONTHS[*month as usize - 1])
        }
        _ => iso.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shapes of the real sales archive: Yuki's invoices, unpadded, plus
    /// timesheets and date-stamped reports that are not invoice numbers.
    fn archive() -> Vec<String> {
        [
            "Invoice 2026-19.pdf",
            "Invoice 2026-9.pdf",
            "Invoice 2026-18.pdf",
            "Invoice 2025-24.pdf",
            "uren januari 2026.xlsx",
            "_bertv timesheet 2025 (1).xlsx",
            "Toggl_Track_summary_report_2026-01-01_2026-01-31.pdf",
            "Toggl 2026-40.pdf",
            "Invoice 2026-41.xlsx",
            "Kopie Invoice 2026-42.pdf",
            "Invoice 2026-43 (1).pdf",
        ]
        .map(String::from)
        .to_vec()
    }

    #[test]
    fn only_invoice_pdfs_carry_a_number() {
        assert_eq!(
            numbers_in_file_names(&archive()),
            ["2026-19", "2026-9", "2026-18", "2025-24"]
        );
        assert!(number_in_file_name("FACTUUR 2026-007.PDF").is_some());
        assert!(number_in_file_name("invoice 2026-19.pdf").is_some());
    }

    #[test]
    fn the_next_number_follows_the_highest_of_the_year() {
        let numbers = numbers_in_file_names(&archive());
        assert_eq!(next_number(&numbers, 2026), "2026-20");
        assert_eq!(next_number(&numbers, 2025), "2025-25");
        assert_eq!(next_number(&numbers, 2027), "2027-1");
        let padded = ["2026-007", "2026-012"].map(String::from);
        assert_eq!(next_number(&padded, 2026), "2026-013");
    }

    #[test]
    fn auto_fills_the_lowest_gap_above_the_archive() {
        let archive = ["2026-19".to_string()];
        let held = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(next_free(&archive, &held(&[]), 2026), "2026-20");
        // 20 and 22 held, 21 released: 21 is given out again.
        assert_eq!(
            next_free(&archive, &held(&["2026-20", "2026-22"]), 2026),
            "2026-21"
        );
        assert_eq!(
            next_free(&archive, &held(&["2026-20", "2026-21", "2026-22"]), 2026),
            "2026-23"
        );
        // Numbers below the archive's highest are never reused.
        assert_eq!(next_free(&archive, &held(&["2026-5"]), 2026), "2026-20");
        assert_eq!(next_free(&archive, &held(&["2025-20"]), 2026), "2026-20");
    }

    #[test]
    fn a_number_in_the_archive_is_taken() {
        let numbers = numbers_in_file_names(&archive());
        assert!(taken(&numbers, "2026-19"));
        assert!(taken(&numbers, "2026-019"));
        assert!(!taken(&numbers, "2026-20"));
        assert!(!taken(&numbers, "2026-40"), "not an invoice file");
        assert!(taken(&["INV-7".to_string()], "INV-7"));
    }

    #[test]
    fn the_ledger_holds_numbers_the_archive_does_not_show_yet() {
        let archive = numbers_in_file_names(&archive());
        let mut ledger = Numbers::default();
        ledger
            .reserve_prepared(
                &crate::cli::invoice_ledger::Claim {
                    admin: "a1",
                    number: "2026-20",
                    date: "2026-10-31",
                    customer: "Example BV",
                    gross: "121.00",
                },
                "h",
            )
            .unwrap();
        // auto skips the pending number; giving it is refused.
        assert_eq!(
            choose(&NumberRequest::Auto, 2026, &archive, &ledger, "a1").unwrap(),
            "2026-21"
        );
        let err = choose(
            &NumberRequest::Given("2026-20".into()),
            2026,
            &archive,
            &ledger,
            "a1",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("already given out: reserved"), "{err}");
        let err = choose(
            &NumberRequest::Given("2026-19".into()),
            2026,
            &archive,
            &ledger,
            "a1",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("already in the sales archive"), "{err}");
        // A rejected number is free again.
        {
            use crate::cli::invoice_ledger::Status;
            ledger
                .transition("a1", "2026-20", Status::Reserved, Status::Rejected)
                .unwrap();
        }
        assert_eq!(
            choose(&NumberRequest::Auto, 2026, &archive, &ledger, "a1").unwrap(),
            "2026-20"
        );
        // Another administration's numbers do not count.
        ledger
            .reserve_prepared(
                &crate::cli::invoice_ledger::Claim {
                    admin: "a2",
                    number: "2026-20",
                    date: "2026-10-31",
                    customer: "Other BV",
                    gross: "1.00",
                },
                "h",
            )
            .unwrap();
        assert_eq!(
            choose(&NumberRequest::Auto, 2026, &archive, &ledger, "a1").unwrap(),
            "2026-20"
        );
        assert_eq!(
            choose(&NumberRequest::Auto, 2026, &archive, &ledger, "a2").unwrap(),
            "2026-21"
        );
    }

    #[test]
    fn the_structured_reference_has_mod_97_check_digits() {
        // 2026000020 mod 97 = 2026000020 - 97 * 20886598 = 14.
        assert_eq!(
            structured_reference("2026-20").unwrap(),
            "+++202/6000/02014+++"
        );
        // A base divisible by 97 takes 97.
        assert_eq!(structured_reference("97").unwrap(), "+++000/0000/09797+++");
        assert!(structured_reference("INV").is_err());
    }

    #[test]
    fn dates_read_in_dutch() {
        assert_eq!(dutch_date("2026-09-30"), "30 september 2026");
        assert_eq!(dutch_date("2026-03-01"), "1 maart 2026");
    }

    #[test]
    fn the_number_flag_takes_auto_or_a_number() {
        assert_eq!(parse_number_request("AUTO"), Ok(NumberRequest::Auto));
        assert_eq!(
            parse_number_request(" 2026-20 "),
            Ok(NumberRequest::Given("2026-20".into()))
        );
        assert!(parse_number_request(" ").is_err());
    }
}
