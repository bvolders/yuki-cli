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
//! The API is deliberately narrow (open, reserve, commit, reject, list,
//! resolve) so its storage can later move to a shared write-ahead ledger.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::YukiError;
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
}

/// The ledger's entries, in memory.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Ledger {
    #[serde(default)]
    entries: Vec<Entry>,
}

impl Ledger {
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
pub struct InvoiceLedger {
    path: PathBuf,
    ledger: Ledger,
    _lock: Lock,
}

impl InvoiceLedger {
    /// Lock and load the ledger at its default path.
    pub fn open() -> Result<Self, YukiError> {
        Self::open_at(&ledger_path())
    }

    /// Lock and load the ledger at `path`; a missing file is an empty ledger.
    pub fn open_at(path: &Path) -> Result<Self, YukiError> {
        let lock = Lock::acquire(path)?;
        let ledger = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| YukiError::Config(format!("{}: {e}", path.display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ledger::default(),
            Err(e) => return Err(YukiError::Config(format!("{}: {e}", path.display()))),
        };
        Ok(Self {
            path: path.to_path_buf(),
            ledger,
            _lock: lock,
        })
    }

    pub fn list(&self) -> &Ledger {
        &self.ledger
    }

    /// Write-ahead: record `number` as pending before it is sent.
    pub fn reserve(
        &mut self,
        number: &str,
        date: &str,
        customer: &str,
        gross: &str,
    ) -> Result<(), YukiError> {
        self.ledger.reserve(number, date, customer, gross)?;
        self.save()
    }

    /// Yuki booked `number`.
    pub fn commit(&mut self, number: &str) -> Result<(), YukiError> {
        self.ledger.settle(number, Status::Booked)?;
        self.save()
    }

    /// Yuki did not create `number`: free it again.
    pub fn reject(&mut self, number: &str) -> Result<(), YukiError> {
        self.ledger.settle(number, Status::Rejected)?;
        self.save()
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

    /// Write atomically: a temporary file in the same directory, flushed to
    /// disk, renamed over the ledger, then the directory synced, so a crash
    /// leaves the old ledger or the new one, never half of one.
    fn save(&self) -> Result<(), YukiError> {
        let path = &self.path;
        let fail = |e: std::io::Error| YukiError::Config(format!("{}: {e}", path.display()));
        let dir = path.parent().unwrap_or(Path::new("."));
        let text = serde_json::to_string_pretty(&self.ledger).expect("serialize ledger");
        let tmp = sibling(path, &format!("tmp-{}", std::process::id()));
        let mut file = std::fs::File::create(&tmp).map_err(fail)?;
        file.write_all(text.as_bytes()).map_err(fail)?;
        file.sync_all().map_err(fail)?;
        drop(file);
        std::fs::rename(&tmp, path).map_err(fail)?;
        if let Ok(dir) = std::fs::File::open(dir) {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

/// `.<file name>.<suffix>` next to `path`.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .map_or("ledger".into(), |n| n.to_string_lossy());
    path.with_file_name(format!(".{name}.{suffix}"))
}

/// An exclusive lock file next to the ledger, removed when dropped.
struct Lock(PathBuf);

impl Lock {
    fn acquire(path: &Path) -> Result<Self, YukiError> {
        let lock = sibling(path, "lock");
        if let Some(dir) = lock.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| YukiError::Config(format!("{}: {e}", dir.display())))?;
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(_) => Ok(Self(lock)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(YukiError::Config(format!(
                    "{} is locked by another yuki run; delete {} if none is running",
                    path.display(),
                    lock.display()
                )))
            }
            Err(e) => Err(YukiError::Config(format!("{}: {e}", lock.display()))),
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
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
    let mut ledger = InvoiceLedger::open()?;
    if let Some((number, status)) = resolve {
        ledger.resolve(number, status)?;
    }
    let headers: Vec<String> = [
        "Number", "Date", "Customer", "Gross", "Status", "Recorded", "Booked",
    ]
    .map(String::from)
    .to_vec();
    let rows: Vec<Vec<String>> = ledger
        .list()
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
            assert!(err.contains("locked by another yuki run"), "{err}");
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
        // Only the ledger is left: no temporary file, no lock.
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["invoice-numbers.json"]);
    }

    #[test]
    fn timestamps_are_utc_iso() {
        let now = now_utc();
        assert_eq!(now.len(), 20, "{now}");
        assert!(now.ends_with('Z') && now.as_bytes()[10] == b'T', "{now}");
    }
}
