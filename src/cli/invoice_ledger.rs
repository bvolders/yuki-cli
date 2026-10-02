//! The local ledger of invoice numbers the CLI has given out:
//! `invoice-numbers.json` next to the config.
//!
//! The sales archive shows a number only once Yuki has filed the invoice's
//! PDF, which can lag. The ledger closes that gap: [`InvoiceLedger::reserve`]
//! records a number as `pending` just before `ProcessSalesInvoices` is
//! called, then [`commit`](InvoiceLedger::commit) or
//! [`reject`](InvoiceLedger::reject) settles it by Yuki's answer. A pending
//! number whose outcome is unknown stays taken until
//! `sales invoice numbers --resolve` settles it. Only a rejected number may
//! be given out again.
//!
//! The file is a [`Ledger`]: written atomically, and locked by an OS lock on
//! `.invoice-numbers.json.lock` only for each short read-and-write, never
//! while Yuki is called, so a crash cannot leave it locked.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;
use crate::error::YukiError;
use crate::ledger::{Ledger, LedgerFormat};
use crate::output::{OutputFormat, format_json, format_table, is_tty};
use crate::period::date_from_epoch_days;

/// What happened to a number given out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Sent, or about to be, with no answer yet: still taken.
    #[value(skip)]
    Pending,
    /// Yuki booked it.
    Booked,
    /// Yuki did not create it: the number is free again.
    Rejected,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Booked => "booked",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub number: String,
    /// Invoice date.
    pub date: String,
    pub customer: String,
    pub gross: String,
    pub status: Status,
    /// When the number was reserved, UTC.
    pub recorded_at: String,
    /// When the booking was recorded (or resolved by hand), UTC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub booked_at: Option<String>,
    /// Fields this version does not know, kept as they are.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// The ledger's entries, in memory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Numbers {
    #[serde(default)]
    entries: Vec<Entry>,
    /// Top-level fields this version does not know, kept as they are.
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

impl LedgerFormat for Numbers {
    const VERSION: u32 = 1;
    const WHAT: &'static str = "invoice number ledger";
    const START_OVER: &'static str =
        " (its numbers would be given out again until the sales archive shows them)";
    // Ledgers written before the format was versioned.
    const UNVERSIONED: bool = true;
}

impl Numbers {
    /// The entry still holding `number` (pending or booked), if any.
    pub fn holder(&self, number: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .rev()
            .find(|e| e.number == number && e.status != Status::Rejected)
    }

    /// The numbers still taken: every entry not rejected.
    pub fn taken_numbers(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter(|e| e.status != Status::Rejected)
            .map(|e| e.number.as_str())
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Record `number` as pending; refused while an entry holds it.
    pub fn reserve(
        &mut self,
        number: &str,
        date: &str,
        customer: &str,
        gross: &str,
    ) -> Result<(), YukiError> {
        if let Some(held) = self.holder(number) {
            return Err(YukiError::Config(format!(
                "invoice number {number} was already given out: {}",
                held.status.label()
            )));
        }
        self.entries.push(Entry {
            number: number.to_string(),
            date: date.to_string(),
            customer: customer.to_string(),
            gross: gross.to_string(),
            status: Status::Pending,
            recorded_at: now_utc(),
            booked_at: None,
            extra: BTreeMap::new(),
        });
        Ok(())
    }

    /// Settle the latest pending entry for `number` as `status`.
    pub(crate) fn settle(&mut self, number: &str, status: Status) -> Result<(), YukiError> {
        let entry = self
            .entries
            .iter_mut()
            .rev()
            .find(|e| e.number == number && e.status == Status::Pending)
            .ok_or_else(|| YukiError::NotFound(format!("no pending invoice number {number}")))?;
        entry.status = status;
        if status == Status::Booked {
            entry.booked_at = Some(now_utc());
        }
        Ok(())
    }
}

/// The ledger file, locked while open so two runs cannot reserve at once.
pub struct InvoiceLedger(Ledger<Numbers>);

impl InvoiceLedger {
    /// Lock and load the ledger at its default path.
    pub fn open() -> Result<Self, YukiError> {
        Self::open_at(&ledger_path())
    }

    /// Lock and load the ledger at `path`; a missing file is an empty ledger.
    pub fn open_at(path: &Path) -> Result<Self, YukiError> {
        Ledger::open(path).map(Self)
    }

    /// The numbers in the ledger at its default path, read without the lock.
    pub fn peek() -> Result<Numbers, YukiError> {
        Ledger::<Numbers>::peek(&ledger_path()).map(|l| l.state().clone())
    }

    pub fn list(&self) -> &Numbers {
        self.0.state()
    }

    /// Write-ahead: record `number` as pending before it is sent.
    pub fn reserve(
        &mut self,
        number: &str,
        date: &str,
        customer: &str,
        gross: &str,
    ) -> Result<(), YukiError> {
        self.0.state_mut().reserve(number, date, customer, gross)?;
        self.0.save()
    }

    /// Yuki booked `number`.
    pub fn commit(&mut self, number: &str) -> Result<(), YukiError> {
        self.0.state_mut().settle(number, Status::Booked)?;
        self.0.save()
    }

    /// Yuki did not create `number`: free it again.
    pub fn reject(&mut self, number: &str) -> Result<(), YukiError> {
        self.0.state_mut().settle(number, Status::Rejected)?;
        self.0.save()
    }

    /// Settle a pending `number` by hand, after checking Yuki.
    pub fn resolve(&mut self, number: &str, status: Status) -> Result<(), YukiError> {
        match status {
            Status::Booked => self.commit(number),
            Status::Rejected => self.reject(number),
            Status::Pending => Err(YukiError::Config(
                "resolve a number as booked or rejected".into(),
            )),
        }
    }
}

/// The ledger file: `invoice-numbers.json` next to the config.
pub fn ledger_path() -> PathBuf {
    Config::default_path().parent().map_or_else(
        || PathBuf::from("invoice-numbers.json"),
        |dir| dir.join("invoice-numbers.json"),
    )
}

/// Now, UTC, as `YYYY-MM-DDTHH:MM:SSZ`.
fn now_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        date_from_epoch_days(days),
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// `sales invoice numbers`: list the ledger, after settling a pending
/// number by hand when `resolve` is given.
pub fn numbers(resolve: Option<(&str, Status)>, format: Option<&str>) -> Result<(), YukiError> {
    let numbers = match resolve {
        Some((number, status)) => {
            let mut ledger = InvoiceLedger::open()?;
            ledger.resolve(number, status)?;
            ledger.list().clone()
        }
        None => InvoiceLedger::peek()?,
    };
    let headers: Vec<String> = [
        "Number", "Date", "Customer", "Gross", "Status", "Recorded", "Booked",
    ]
    .map(String::from)
    .to_vec();
    let rows: Vec<Vec<String>> = numbers
        .entries()
        .iter()
        .map(|e| {
            vec![
                e.number.clone(),
                e.date.clone(),
                e.customer.clone(),
                e.gross.clone(),
                e.status.label().to_string(),
                e.recorded_at.clone(),
                e.booked_at.clone().unwrap_or_default(),
            ]
        })
        .collect();
    match OutputFormat::from_flag(format, is_tty()) {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_number_stays_taken_until_rejected_and_survives_a_reopen() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("invoice-numbers.json");
        {
            let mut ledger = InvoiceLedger::open_at(&path).unwrap();
            ledger
                .reserve("2026-20", "2026-10-31", "Example BV", "121.00")
                .unwrap();
            // While open, a second run cannot open it.
            let err = InvoiceLedger::open_at(&path).err().unwrap().to_string();
            assert!(err.contains("another yuki run"), "{err}");
        }
        let mut ledger = InvoiceLedger::open_at(&path).unwrap();
        assert_eq!(
            ledger.list().holder("2026-20").unwrap().status,
            Status::Pending
        );
        let err = ledger
            .reserve("2026-20", "2026-10-31", "Other", "1.00")
            .unwrap_err();
        assert!(err.to_string().contains("already given out: pending"));
        ledger.reject("2026-20").unwrap();
        assert!(ledger.list().holder("2026-20").is_none());
        // A rejected number can be given out again, and booked.
        ledger
            .reserve("2026-20", "2026-10-31", "Example BV", "121.00")
            .unwrap();
        ledger.commit("2026-20").unwrap();
        let booked = ledger.list().holder("2026-20").unwrap();
        assert_eq!(booked.status, Status::Booked);
        assert!(booked.booked_at.is_some());
        assert!(
            ledger.resolve("2026-20", Status::Rejected).is_err(),
            "nothing pending"
        );
        assert_eq!(
            ledger.list().taken_numbers().collect::<Vec<_>>(),
            ["2026-20"]
        );
        drop(ledger);
        // Only the ledger and its lock file are left, the lock released.
        let mut names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [".invoice-numbers.json.lock", "invoice-numbers.json"]
        );
        InvoiceLedger::open_at(&path).unwrap();
    }

    #[test]
    fn a_ledger_written_before_versioning_still_loads() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("invoice-numbers.json");
        std::fs::write(
            &path,
            r#"{"entries": [{"number": "2026-19", "date": "2026-09-30", "customer": "X",
                "gross": "1.00", "status": "booked", "recorded_at": "2026-09-30T10:00:00Z",
                "invoice_pdf": "kept"}]}"#,
        )
        .unwrap();
        let mut ledger = InvoiceLedger::open_at(&path).unwrap();
        assert_eq!(
            ledger.list().taken_numbers().collect::<Vec<_>>(),
            ["2026-19"]
        );
        ledger
            .reserve("2026-20", "2026-10-31", "X", "1.00")
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"version\": 1"), "{text}");
        assert!(text.contains("\"invoice_pdf\": \"kept\""), "{text}");
    }

    #[test]
    fn timestamps_are_utc_iso() {
        let now = now_utc();
        assert_eq!(now.len(), 20, "{now}");
        assert!(now.ends_with('Z') && now.as_bytes()[10] == b'T', "{now}");
    }
}
