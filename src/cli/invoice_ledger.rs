//! The local ledger of invoice numbers the CLI has given out:
//! `invoice-numbers.json` next to the config.
//!
//! The sales archive shows a number only once Yuki has filed the invoice's
//! PDF, which can lag. The ledger closes that gap. `prepare --out` records a
//! number as `reserved` ([`InvoiceLedger::reserve`]), with the hash of the
//! prepared invoice it was given to; `create --prepared` turns that
//! reservation, and only for that exact content, into `pending`
//! ([`InvoiceLedger::send_reserved`]) just before `ProcessSalesInvoices` is
//! called. Yuki's answer then settles it: `booked`, `rejected` (free again),
//! back to `reserved` when nothing reached Yuki, or still `pending` when the
//! outcome is unknown, until `sales invoice numbers --resolve` settles it.
//! A reservation stays until it is sent or released. Only a rejected number
//! may be given out again.
//!
//! Numbers belong to an administration (its `admin_id`), and an
//! [`InvoiceLedger`] is opened for one: each administration numbers its
//! invoices on its own. Numbers compare as [`same_number`]: `2026-01` is
//! `2026-1`.
//!
//! The file is a [`Ledger`]: written atomically, and locked by an OS lock on
//! `.invoice-numbers.json.lock` only for each short read-and-write, never
//! while Yuki is called, so a crash cannot leave it locked. A second run
//! waits for that lock rather than failing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cli::invoice_number::same_number;
use crate::cli::sales_invoice::InvoiceError;
use crate::config::Config;
use crate::error::YukiError;
use crate::ledger::{Ledger, LedgerFormat};
use crate::output::{OutputFormat, format_json, format_table, is_tty};
use crate::period::{epoch_days, today};
use crate::sync::now_utc;

/// What `numbers --resolve <number> --as …` settles a number as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Resolution {
    /// Yuki booked it: the number is used.
    Booked,
    /// Yuki did not create it, or a reservation will not be sent: free.
    Rejected,
}

/// What happened to a number given out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Given to a prepared invoice (`prepare --out`), not sent yet.
    Reserved,
    /// Sent, or about to be, with no answer yet: still taken.
    Pending,
    /// Yuki booked it.
    Booked,
    /// Yuki did not create it: the number is free again.
    Rejected,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Pending => "pending",
            Self::Booked => "booked",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub number: String,
    /// The administration (`admin_id`) the number belongs to.
    pub admin: String,
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
    /// The content hash of the prepared invoice a reservation is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// What needs checking, e.g. a reference Yuki booked differently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Fields this version does not know, kept as they are.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Entry {
    /// Whether the entry still holds its number: anything not rejected.
    fn holds(&self) -> bool {
        self.status != Status::Rejected
    }
}

/// The refusal for a number an entry still holds, saying what to do: shared
/// by reserving and by `--number` (see `invoice_number::choose`).
pub fn given_out(entry: &Entry) -> InvoiceError {
    let status = match entry.status {
        Status::Pending => {
            "pending (outcome unknown: check Yuki, then `yuki sales invoice numbers --resolve`)"
        }
        Status::Reserved => {
            "reserved by `prepare --out` (free it with `yuki sales invoice numbers --resolve <number> --as rejected`)"
        }
        other => other.label(),
    };
    InvoiceError::InvalidInput(format!(
        "invoice number {} was already given out: {status}",
        entry.number
    ))
}

/// How long a reservation may wait before `prepare`, `create` and `numbers`
/// warn about it.
pub const STALE_DAYS: i64 = 7;

/// A number about to be given out, and the invoice it is for.
#[derive(Debug, Clone)]
pub struct Claim {
    pub number: String,
    pub date: String,
    pub customer: String,
    pub gross: String,
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
}

/// The ledger file, seen by one administration: locked for a short
/// read-and-write ([`open`](Self::open)), or only read ([`peek`](Self::peek)).
pub struct InvoiceLedger {
    file: Ledger<Numbers>,
    admin: String,
}

impl InvoiceLedger {
    /// Lock and load the ledger, for administration `admin` (its
    /// `admin_id`), waiting while another run holds the lock.
    pub fn open(admin: &str) -> Result<Self, YukiError> {
        Self::open_at(&ledger_path(), admin)
    }

    /// [`open`](Self::open) the ledger at `path`; a missing file is empty.
    pub fn open_at(path: &Path, admin: &str) -> Result<Self, YukiError> {
        Ok(Self {
            file: Ledger::open_wait(path)?,
            admin: admin.to_string(),
        })
    }

    /// Load the ledger for `admin` without the lock, to read only.
    pub fn peek(admin: &str) -> Result<Self, YukiError> {
        Ok(Self {
            file: Ledger::peek(&ledger_path())?,
            admin: admin.to_string(),
        })
    }

    /// This administration's entries, oldest first.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.file
            .state()
            .entries
            .iter()
            .filter(move |e| e.admin == self.admin)
    }

    /// The entry still holding `number`, if any.
    pub fn holder(&self, number: &str) -> Option<&Entry> {
        self.entries()
            .filter(|e| e.holds() && same_number(&e.number, number))
            .last()
    }

    /// The numbers still held: every entry not rejected.
    pub fn taken_numbers(&self) -> impl Iterator<Item = &str> {
        self.entries()
            .filter(|e| e.holds())
            .map(|e| e.number.as_str())
    }

    /// Reservations older than [`STALE_DAYS`] on `today` (epoch days), as
    /// warning lines.
    pub fn warnings(&self, today: i64) -> Vec<String> {
        self.entries()
            .filter(|e| e.status == Status::Reserved)
            .filter(|e| epoch_days(&e.recorded_at).is_some_and(|d| today - d > STALE_DAYS))
            .map(|e| {
                format!(
                    "{n} reserved since {} for {}: book it or release it (--resolve {n} --as rejected); under continuous numbering, a number never booked is a gap",
                    &e.recorded_at[..10],
                    e.customer,
                    n = e.number,
                )
            })
            .collect()
    }

    /// Print [`warnings`](Self::warnings) for today to stderr.
    pub fn warn(&self) {
        let today = epoch_days(&today()).unwrap_or_default();
        for warning in self.warnings(today) {
            eprintln!("warning: {warning}");
        }
    }

    /// The position of the reservation of `number`, checked against the
    /// content hash of the prepared invoice about to be sent.
    fn find_reserved(&self, number: &str, hash: &str) -> Result<usize, InvoiceError> {
        let entries = &self.file.state().entries;
        let at = entries
            .iter()
            .rposition(|e| e.admin == self.admin && e.holds() && same_number(&e.number, number))
            .ok_or_else(|| {
                InvoiceError::InvalidInput(format!(
                    "invoice number {number} is not reserved in this administration: prepare it again with `sales invoice prepare --out`"
                ))
            })?;
        let held = &entries[at];
        match held.status {
            Status::Reserved if held.hash.as_deref() == Some(hash) => Ok(at),
            Status::Reserved => Err(InvoiceError::InvalidInput(format!(
                "the prepared invoice {number} is not the one prepare reserved the number for: it changed since, so its PDF may not match; prepare it again"
            ))),
            other => Err(InvoiceError::InvalidInput(format!(
                "invoice number {number} is {} already, not reserved: it was sent before",
                other.label()
            ))),
        }
    }

    /// Whether `number` is reserved for the prepared invoice with content
    /// hash `hash`.
    pub fn check_reserved(&self, number: &str, hash: &str) -> Result<(), InvoiceError> {
        self.find_reserved(number, hash).map(|_| ())
    }

    /// Reserve the claimed number for the prepared invoice with content
    /// hash `hash` (`prepare --out`); refused while an entry holds it.
    pub fn reserve(&mut self, claim: &Claim, hash: &str) -> Result<(), InvoiceError> {
        if let Some(held) = self.holder(&claim.number) {
            return Err(given_out(held));
        }
        self.file.state_mut().entries.push(Entry {
            number: claim.number.clone(),
            admin: self.admin.clone(),
            date: claim.date.clone(),
            customer: claim.customer.clone(),
            gross: claim.gross.clone(),
            status: Status::Reserved,
            recorded_at: now_utc(),
            booked_at: None,
            hash: Some(hash.to_string()),
            note: None,
            extra: BTreeMap::new(),
        });
        Ok(self.file.save()?)
    }

    /// Write-ahead for a prepared invoice: the reservation
    /// [`check_reserved`](Self::check_reserved) finds becomes pending before
    /// the invoice is sent.
    pub fn send_reserved(&mut self, number: &str, hash: &str) -> Result<(), InvoiceError> {
        let at = self.find_reserved(number, hash)?;
        self.file.state_mut().entries[at].status = Status::Pending;
        Ok(self.file.save()?)
    }

    /// Move the latest entry for `number` from `from` to `to` and save;
    /// refused when there is none in `from`.
    fn transition(&mut self, number: &str, from: Status, to: Status) -> Result<(), InvoiceError> {
        let admin = &self.admin;
        let entry = self
            .file
            .state_mut()
            .entries
            .iter_mut()
            .rev()
            .find(|e| e.admin == *admin && e.status == from && same_number(&e.number, number))
            .ok_or_else(|| {
                YukiError::NotFound(format!("no {} invoice number {number}", from.label()))
            })?;
        entry.status = to;
        if to == Status::Booked {
            entry.booked_at = Some(now_utc());
        }
        Ok(self.file.save()?)
    }

    /// Nothing reached Yuki: the number is reserved again for the same
    /// content, to retry.
    pub fn unsend(&mut self, number: &str) -> Result<(), InvoiceError> {
        self.transition(number, Status::Pending, Status::Reserved)
    }

    /// Release a reservation that will not be sent: the number is free again.
    pub fn release(&mut self, number: &str) -> Result<(), InvoiceError> {
        self.transition(number, Status::Reserved, Status::Rejected)
    }

    /// Yuki booked `number`.
    pub fn commit(&mut self, number: &str) -> Result<(), InvoiceError> {
        self.transition(number, Status::Pending, Status::Booked)
    }

    /// Yuki did not create `number`: free it again.
    pub fn reject(&mut self, number: &str) -> Result<(), InvoiceError> {
        self.transition(number, Status::Pending, Status::Rejected)
    }

    /// Settle `number` by hand, after checking Yuki: a pending number as
    /// booked or rejected; a reservation, never sent, only as rejected.
    pub fn resolve(&mut self, number: &str, resolution: Resolution) -> Result<(), InvoiceError> {
        let reserved = self.holder(number).map(|e| e.status) == Some(Status::Reserved);
        match (resolution, reserved) {
            (Resolution::Rejected, true) => self.release(number),
            (Resolution::Rejected, false) => self.reject(number),
            (Resolution::Booked, true) => Err(InvoiceError::InvalidInput(format!(
                "invoice number {number} is reserved, never sent: it can only be released (--as rejected)"
            ))),
            (Resolution::Booked, false) => self.commit(number),
        }
    }

    /// Note on the latest pending entry for `number`.
    pub fn note(&mut self, number: &str, note: &str) -> Result<(), InvoiceError> {
        let admin = &self.admin;
        if let Some(entry) = self.file.state_mut().entries.iter_mut().rev().find(|e| {
            e.admin == *admin && e.status == Status::Pending && same_number(&e.number, number)
        }) {
            entry.note = Some(note.to_string());
        }
        Ok(self.file.save()?)
    }
}

/// The ledger file: `invoice-numbers.json` next to the config.
pub fn ledger_path() -> PathBuf {
    Config::default_path().parent().map_or_else(
        || PathBuf::from("invoice-numbers.json"),
        |dir| dir.join("invoice-numbers.json"),
    )
}

/// `sales invoice numbers`: list the selected administration's numbers,
/// after settling one by hand when `resolve` is given.
pub fn numbers(
    config: &Config,
    admin: Option<&str>,
    resolve: Option<(&str, Resolution)>,
    format: Option<&str>,
) -> Result<(), InvoiceError> {
    let admin = config.target(admin)?.admin_id;
    let ledger = match resolve {
        Some((number, resolution)) => {
            let mut ledger = InvoiceLedger::open(admin)?;
            ledger.resolve(number, resolution)?;
            ledger
        }
        None => InvoiceLedger::peek(admin)?,
    };
    ledger.warn();
    let headers: Vec<String> = [
        "Number", "Date", "Customer", "Gross", "Status", "Recorded", "Booked", "Note",
    ]
    .map(String::from)
    .to_vec();
    let rows: Vec<Vec<String>> = ledger
        .entries()
        .map(|e| {
            vec![
                e.number.clone(),
                e.date.clone(),
                e.customer.clone(),
                e.gross.clone(),
                e.status.label().to_string(),
                e.recorded_at.clone(),
                e.booked_at.clone().unwrap_or_default(),
                e.note.clone().unwrap_or_default(),
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

    fn claim(number: &str) -> Claim {
        Claim {
            number: number.into(),
            date: "2026-10-31".into(),
            customer: "Example BV".into(),
            gross: "121.00".into(),
        }
    }

    struct Dir(tempfile::TempDir);

    impl Dir {
        fn new() -> Self {
            Self(tempfile::TempDir::new().unwrap())
        }

        fn open(&self, admin: &str) -> InvoiceLedger {
            InvoiceLedger::open_at(&self.0.path().join("invoice-numbers.json"), admin).unwrap()
        }
    }

    /// Reserve `number` and send it: pending.
    fn pend(ledger: &mut InvoiceLedger, number: &str) -> Result<(), InvoiceError> {
        ledger.reserve(&claim(number), "h")?;
        ledger.send_reserved(number, "h")
    }

    #[test]
    fn a_number_stays_taken_until_rejected_and_survives_a_reopen() {
        let dir = Dir::new();
        pend(&mut dir.open("a1"), "2026-20").unwrap();
        let mut ledger = dir.open("a1");
        assert_eq!(ledger.holder("2026-20").unwrap().status, Status::Pending);
        let err = pend(&mut ledger, "2026-20").unwrap_err();
        assert!(err.to_string().contains("already given out: pending"));
        ledger.reject("2026-20").unwrap();
        assert!(ledger.holder("2026-20").is_none());
        // A rejected number can be given out again, and booked.
        pend(&mut ledger, "2026-20").unwrap();
        ledger.commit("2026-20").unwrap();
        let booked = ledger.holder("2026-20").unwrap();
        assert_eq!(booked.status, Status::Booked);
        assert!(booked.booked_at.is_some());
        assert!(
            ledger.resolve("2026-20", Resolution::Rejected).is_err(),
            "nothing pending"
        );
        assert_eq!(ledger.taken_numbers().collect::<Vec<_>>(), ["2026-20"]);
    }

    #[test]
    fn numbers_belong_to_an_administration_and_compare_by_value() {
        let dir = Dir::new();
        let mut a1 = dir.open("a1");
        a1.reserve(&claim("2026-01"), "h").unwrap();
        // 2026-1 is 2026-01, in the same administration only.
        let err = a1.reserve(&claim("2026-1"), "h").unwrap_err();
        assert!(err.to_string().contains("already given out"), "{err}");
        assert!(a1.holder("2026-001").is_some());
        drop(a1);
        let mut a2 = dir.open("a2");
        a2.reserve(&claim("2026-1"), "h").unwrap();
        assert_eq!(a2.taken_numbers().collect::<Vec<_>>(), ["2026-1"]);
        drop(a2);
        assert!(dir.open("a3").holder("2026-1").is_none());
    }

    #[test]
    fn a_second_opener_waits_for_the_lock() {
        let dir = Dir::new();
        let held = dir.open("a1");
        let start = std::time::Instant::now();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            drop(held);
        });
        dir.open("a1");
        assert!(start.elapsed() >= std::time::Duration::from_millis(150));
        release.join().unwrap();
    }

    #[test]
    fn a_reservation_is_sent_only_for_its_content_and_can_be_released() {
        let dir = Dir::new();
        let mut ledger = dir.open("a1");
        ledger.reserve(&claim("2026-20"), "hash-a").unwrap();
        // Reserved numbers are taken, like pending ones.
        assert!(ledger.reserve(&claim("2026-20"), "x").is_err());
        let err = ledger.send_reserved("2026-20", "hash-b").unwrap_err();
        assert!(err.to_string().contains("changed since"), "{err}");
        assert!(
            dir.open_other("a2")
                .send_reserved("2026-20", "hash-a")
                .is_err()
        );
        ledger.send_reserved("2026-20", "hash-a").unwrap();
        assert_eq!(ledger.holder("2026-20").unwrap().status, Status::Pending);
        let err = ledger.send_reserved("2026-20", "hash-a").unwrap_err();
        assert!(err.to_string().contains("pending already"), "{err}");
        assert!(ledger.release("2026-20").is_err(), "pending, not reserved");
        // Nothing reached Yuki: reserved again, for the same content.
        ledger.unsend("2026-20").unwrap();
        ledger.send_reserved("2026-20", "hash-a").unwrap();
        ledger.commit("2026-20").unwrap();

        // Resolved as rejected, a reservation frees its number; it can
        // never be booked.
        ledger.reserve(&claim("2026-21"), "h").unwrap();
        assert!(
            ledger.resolve("2026-21", Resolution::Booked).is_err(),
            "never sent"
        );
        ledger.resolve("2026-21", Resolution::Rejected).unwrap();
        assert!(ledger.holder("2026-21").is_none());
    }

    impl Dir {
        /// A second administration's view, without waiting for the lock
        /// the test holds.
        fn open_other(&self, admin: &str) -> InvoiceLedger {
            InvoiceLedger {
                file: Ledger::peek(&self.0.path().join("invoice-numbers.json")).unwrap(),
                admin: admin.into(),
            }
        }
    }

    #[test]
    fn old_reservations_are_warned_about() {
        let dir = Dir::new();
        let mut ledger = dir.open("a1");
        ledger
            .reserve(
                &Claim {
                    customer: "Buuurt".into(),
                    ..claim("2026-20")
                },
                "h",
            )
            .unwrap();
        ledger.file.state_mut().entries[0].recorded_at = "2026-10-01T09:00:00Z".into();
        let day = epoch_days("2026-10-01").unwrap();
        assert!(ledger.warnings(day + STALE_DAYS).is_empty());
        let warnings = ledger.warnings(day + STALE_DAYS + 1);
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].starts_with(
                "2026-20 reserved since 2026-10-01 for Buuurt: book it or release it (--resolve 2026-20 --as rejected)"
            ),
            "{warnings:?}"
        );
        assert!(dir.open_other("a2").warnings(day + 30).is_empty());
    }
}
