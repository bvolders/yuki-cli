//! Invoice numbers given by the CLI rather than Yuki, so a PDF rendered
//! beforehand can show the number Yuki will book.
//!
//! Yuki names the PDFs in the sales archive after the invoice number:
//! `Invoice 2026-19.pdf`. `--number auto` reads that folder, takes the
//! highest `<year>-<seq>` of the invoice date's year and adds one, padded as
//! the existing numbers are. Yuki's own counter does not learn about numbers
//! given this way, so once invoices are numbered here, number them all here.

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

/// The `<year>-<seq>` a file name ends with before its extension, as in
/// `Invoice 2026-19.pdf`. The number must follow a non-digit, so a report
/// named `..._2026-01-01_2026-01-31.pdf` (a date) does not count.
pub fn number_in_file_name(name: &str) -> Option<YearNumber> {
    let stem = match name.rsplit_once('.') {
        Some((stem, _)) => stem,
        None => name,
    };
    let start = stem
        .char_indices()
        .rev()
        .find(|(_, c)| !(c.is_ascii_digit() || *c == '-'))
        .map_or(0, |(i, c)| i + c.len_utf8());
    let tail = &stem[start..];
    // In `2026-01-31` the tail would be the whole date: not one number.
    if tail.matches('-').count() != 1 {
        return None;
    }
    year_number(tail)
}

/// The next number for `year` after the numbers in `files`: one past the
/// highest, with its sequence zero-padded to the widest padded sequence of
/// that year (Yuki's own are not padded: `2026-9`, `2026-10`).
pub fn next_number(files: &[String], year: u32) -> String {
    let numbers: Vec<YearNumber> = files
        .iter()
        .filter_map(|f| number_in_file_name(f))
        .filter(|n| n.year == year)
        .collect();
    let seq = numbers.iter().map(|n| n.seq).max().unwrap_or(0) + 1;
    let width = numbers
        .iter()
        .filter(|n| n.digits > 1 && n.digits > n.seq.to_string().len())
        .map(|n| n.digits)
        .max()
        .unwrap_or(0);
    format!("{year}-{seq:0width$}")
}

/// Whether a file in `files` already carries `number`: the same year and
/// sequence for a `<year>-<seq>` number (`2026-019` is `2026-19`), else a
/// file stem ending in `number` after a non-alphanumeric character.
pub fn taken(files: &[String], number: &str) -> bool {
    if let Some(wanted) = year_number(number) {
        return files
            .iter()
            .filter_map(|f| number_in_file_name(f))
            .any(|n| n.year == wanted.year && n.seq == wanted.seq);
    }
    files.iter().any(|f| {
        let stem = f.rsplit_once('.').map_or(f.as_str(), |(stem, _)| stem);
        stem.strip_suffix(number)
            .is_some_and(|before| before.chars().last().is_none_or(|c| !c.is_alphanumeric()))
    })
}

/// The file names in the sales (`verkoop`) archive folder.
async fn sales_file_names(config: &Config, admin: Option<&str>) -> Result<Vec<String>, YukiError> {
    let target = config.target(admin)?;
    let mut client = ArchiveClient::new().with_api_root(target.api_root);
    client.authenticate(target.api_key).await?;
    let documents = client
        .documents_in_folder(folder_id("verkoop")?, "2000-01-01", "2099-12-31")
        .await?;
    Ok(documents.into_iter().map(|d| d.file_name).collect())
}

/// The number for an invoice dated `date`, checked against the sales
/// archive: the next one for `auto`, or the given one if no file has it.
pub async fn resolve(
    config: &Config,
    admin: Option<&str>,
    request: &NumberRequest,
    date: &str,
) -> Result<String, YukiError> {
    let files = sales_file_names(config, admin).await?;
    let number = match request {
        NumberRequest::Auto => {
            let year = date[..4]
                .parse()
                .map_err(|_| YukiError::Config(format!("'{date}' has no year")))?;
            next_number(&files, year)
        }
        NumberRequest::Given(number) => number.clone(),
    };
    if taken(&files, &number) {
        return Err(YukiError::Config(format!(
            "invoice number {number} is already in the sales archive"
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

/// An ISO date as Dutch text: `2026-09-30` → `30 september 2026`.
pub fn dutch_date(iso: &str) -> String {
    const MONTHS: [&str; 12] = [
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
            "Toggl_Track_summary_report_2026-01-01_2026-01-31 (1).pdf",
        ]
        .map(String::from)
        .to_vec()
    }

    #[test]
    fn numbers_are_read_from_invoice_file_names_only() {
        let found: Vec<Option<YearNumber>> =
            archive().iter().map(|f| number_in_file_name(f)).collect();
        assert_eq!(
            found[0],
            Some(YearNumber {
                year: 2026,
                seq: 19,
                digits: 2
            })
        );
        assert!(found[4..].iter().all(Option::is_none), "{found:?}");
    }

    #[test]
    fn the_next_number_follows_the_highest_of_the_year() {
        assert_eq!(next_number(&archive(), 2026), "2026-20");
        assert_eq!(next_number(&archive(), 2025), "2025-25");
        assert_eq!(next_number(&archive(), 2027), "2027-1");
        let padded = ["F 2026-007.pdf", "F 2026-012.pdf"].map(String::from);
        assert_eq!(next_number(&padded, 2026), "2026-013");
    }

    #[test]
    fn a_number_in_the_archive_is_taken() {
        let files = archive();
        assert!(taken(&files, "2026-19"));
        assert!(taken(&files, "2026-019"));
        assert!(!taken(&files, "2026-20"));
        assert!(!taken(&files, "2026-1"));
        let other = ["Factuur INV-7.pdf"].map(String::from);
        assert!(taken(&other, "INV-7"));
        assert!(!taken(&other, "NV-7"));
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
