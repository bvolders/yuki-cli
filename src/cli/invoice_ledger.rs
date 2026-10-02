//! The local ledger of invoice numbers the CLI has given out:
//! `invoice-numbers.json` next to the config.
//!
//! The sales archive shows a number only once Yuki has filed the invoice's
//! PDF, which can lag. The ledger closes that gap. `prepare --out` records a
//! number as `reserved`, with the hash of the prepared invoice it was given
//! to; `create --prepared` turns that reservation, and only for that exact
//! content, into `pending` ([`InvoiceLedger::send_reserved`]) just before
//! `ProcessSalesInvoices` is called. A number `create` picks itself goes
//! straight to `pending` ([`InvoiceLedger::reserve`]). Then
//! [`commit`](InvoiceLedger::commit) or [`reject`](InvoiceLedger::reject)
//! settles it by Yuki's answer. A pending number whose outcome is unknown
//! stays taken until `sales invoice numbers --resolve` settles it; a
//! reservation stays until it is sent or released. Only a rejected number
//! may be given out again.
//!
//! Numbers belong to an administration (its `admin_id`): each administration
//! numbers its invoices on its own. Entries written before the ledger
//! recorded the administration are given to the configuration's only
//! administration when it has one (and saved so on the next write);
//! otherwise they count for none, are listed in a warning, and only
//! `numbers --resolve`/`--release` with an explicit `--admin` settles them.
//! Numbers compare as [`same_number`]: `2026-01` is `2026-1`.
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
use crate::config::Config;
use crate::error::YukiError;
use crate::ledger::{Ledger, LedgerFormat};
use crate::output::{OutputFormat, format_json, format_table, is_tty};
use crate::period::{date_from_epoch_days, epoch_days};

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
    /// The administration (`admin_id`) the number belongs to; absent in
    /// entries written before it was recorded (see the module docs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin: Option<String>,
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
    /// Whether the entry counts for administration `admin`.
    fn of(&self, admin: &str) -> bool {
        self.admin.as_deref() == Some(admin)
    }
}

/// How long a reservation may wait before `prepare`, `create` and `numbers`
/// warn about it.
pub const STALE_DAYS: i64 = 7;

/// A number about to be given out, and what it is for.
#[derive(Debug, Clone, Copy)]
pub struct Claim<'a> {
    /// The administration's `admin_id`.
    pub admin: &'a str,
    pub number: &'a str,
    pub date: &'a str,
    pub customer: &'a str,
    pub gross: &'a str,
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
    // 2: reservations, hashes and notes.
    const VERSION: u32 = 2;
    const WHAT: &'static str = "invoice number ledger";
    const START_OVER: &'static str =
        " (its numbers would be given out again until the sales archive shows them)";
    // Ledgers written before the format was versioned.
    const UNVERSIONED: bool = true;
}

impl Numbers {
    /// Give the entries without an administration to `admin`.
    fn adopt(&mut self, admin: &str) {
        for entry in self.entries.iter_mut().filter(|e| e.admin.is_none()) {
            entry.admin = Some(admin.to_string());
        }
    }

    /// Give the latest entry without an administration for `number` to
    /// `admin`, so it can be settled there; whether there was one.
    fn claim_legacy(&mut self, admin: &str, number: &str) -> bool {
        match self
            .entries
            .iter_mut()
            .rev()
            .find(|e| e.admin.is_none() && same_number(&e.number, number))
        {
            Some(entry) => {
                entry.admin = Some(admin.to_string());
                true
            }
            None => false,
        }
    }

    /// What needs attention, as warning lines: reservations of `admin`
    /// older than [`STALE_DAYS`] on `today` (epoch days), and entries that
    /// belong to no administration.
    pub fn warnings(&self, admin: &str, today: i64) -> Vec<String> {
        let mut out: Vec<String> = self
            .entries_of(admin)
            .filter(|e| e.status == Status::Reserved)
            .filter(|e| epoch_days(&e.recorded_at).is_some_and(|d| today - d > STALE_DAYS))
            .map(|e| {
                format!(
                    "{} reserved since {} for {}: book it or --release it (under continuous numbering, a number never booked is a gap)",
                    e.number,
                    &e.recorded_at[..10],
                    e.customer
                )
            })
            .collect();
        let orphans: Vec<&str> = self
            .entries
            .iter()
            .filter(|e| e.admin.is_none() && e.status != Status::Rejected)
            .map(|e| e.number.as_str())
            .collect();
        if !orphans.is_empty() {
            out.push(format!(
                "the invoice number ledger has numbers recorded without an administration ({}): they count for none; settle each with `yuki --admin <name> sales invoice numbers --resolve <number> booked|rejected` (or --release)",
                orphans.join(", ")
            ));
        }
        out
    }

    /// The entry of `admin` still holding `number` (not rejected), if any.
    pub fn holder(&self, admin: &str, number: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .rev()
            .find(|e| e.of(admin) && e.status != Status::Rejected && same_number(&e.number, number))
    }

    /// The numbers `admin` still holds: every entry not rejected.
    pub fn taken_numbers<'a>(&'a self, admin: &'a str) -> impl Iterator<Item = &'a str> {
        self.entries
            .iter()
            .filter(move |e| e.of(admin) && e.status != Status::Rejected)
            .map(|e| e.number.as_str())
    }

    /// The entries of `admin`, oldest first.
    pub fn entries_of<'a>(&'a self, admin: &'a str) -> impl Iterator<Item = &'a Entry> {
        self.entries.iter().filter(move |e| e.of(admin))
    }

    /// Record the claim as pending; refused while an entry holds the number.
    pub fn reserve(&mut self, claim: &Claim<'_>) -> Result<(), YukiError> {
        self.claim(claim, Status::Pending, None)
    }

    /// Record the claim as reserved for the prepared invoice whose content
    /// hash is `hash`; refused while an entry holds the number.
    pub fn reserve_prepared(&mut self, claim: &Claim<'_>, hash: &str) -> Result<(), YukiError> {
        self.claim(claim, Status::Reserved, Some(hash))
    }

    fn claim(
        &mut self,
        claim: &Claim<'_>,
        status: Status,
        hash: Option<&str>,
    ) -> Result<(), YukiError> {
        if let Some(held) = self.holder(claim.admin, claim.number) {
            return Err(YukiError::Config(format!(
                "invoice number {} was already given out: {}",
                claim.number,
                held.status.label()
            )));
        }
        self.entries.push(Entry {
            number: claim.number.to_string(),
            admin: Some(claim.admin.to_string()),
            date: claim.date.to_string(),
            customer: claim.customer.to_string(),
            gross: claim.gross.to_string(),
            status,
            recorded_at: now_utc(),
            booked_at: None,
            hash: hash.map(str::to_string),
            note: None,
            extra: BTreeMap::new(),
        });
        Ok(())
    }

    /// The latest entry of `admin` for `number` in one of `states`.
    fn latest(&mut self, admin: &str, number: &str, states: &[Status]) -> Option<&mut Entry> {
        self.entries
            .iter_mut()
            .rev()
            .find(|e| e.of(admin) && states.contains(&e.status) && same_number(&e.number, number))
    }

    /// The reservation of `admin` for `number`, checked against the content
    /// hash of the prepared invoice about to be sent.
    pub fn check_reserved(&self, admin: &str, number: &str, hash: &str) -> Result<(), YukiError> {
        let held = self.holder(admin, number).ok_or_else(|| {
            YukiError::Config(format!(
                "invoice number {number} is not reserved in this administration: prepare it again with `sales invoice prepare --out`"
            ))
        })?;
        match held.status {
            Status::Reserved if held.hash.as_deref() == Some(hash) => Ok(()),
            Status::Reserved => Err(YukiError::Config(format!(
                "the prepared invoice {number} is not the one prepare reserved the number for: it changed since, so its PDF may not match; prepare it again"
            ))),
            other => Err(YukiError::Config(format!(
                "invoice number {number} is {} already, not reserved: it was sent before",
                other.label()
            ))),
        }
    }

    /// Settle the latest pending entry of `admin` for `number` as `status`.
    pub(crate) fn settle(
        &mut self,
        admin: &str,
        number: &str,
        status: Status,
    ) -> Result<(), YukiError> {
        let entry = self
            .latest(admin, number, &[Status::Pending])
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
    /// Lock and load the ledger at its default path, for `config`: a
    /// configuration with one administration takes the entries without one.
    pub fn open(config: &Config) -> Result<Self, YukiError> {
        let mut ledger = Self::open_at(&ledger_path())?;
        if let Some(admin) = sole_admin(config) {
            ledger.0.state_mut().adopt(admin);
        }
        Ok(ledger)
    }

    /// Lock and load the ledger at `path`, waiting while another run holds
    /// the lock; a missing file is an empty ledger.
    pub fn open_at(path: &Path) -> Result<Self, YukiError> {
        Ledger::open_wait(path).map(Self)
    }

    /// The numbers in the ledger at its default path, read without the lock,
    /// for `config` as [`open`](Self::open) reads them.
    pub fn peek(config: &Config) -> Result<Numbers, YukiError> {
        let mut numbers = Ledger::<Numbers>::peek(&ledger_path())?.state().clone();
        if let Some(admin) = sole_admin(config) {
            numbers.adopt(admin);
        }
        Ok(numbers)
    }

    pub fn list(&self) -> &Numbers {
        self.0.state()
    }

    /// Write-ahead: record the claimed number as pending before it is sent.
    pub fn reserve(&mut self, claim: &Claim<'_>) -> Result<(), YukiError> {
        self.0.state_mut().reserve(claim)?;
        self.0.save()
    }

    /// Reserve the claimed number for the prepared invoice with content
    /// hash `hash` (`prepare --out`).
    pub fn reserve_prepared(&mut self, claim: &Claim<'_>, hash: &str) -> Result<(), YukiError> {
        self.0.state_mut().reserve_prepared(claim, hash)?;
        self.0.save()
    }

    /// Write-ahead for a prepared invoice: its reservation, checked against
    /// `hash`, becomes pending before it is sent.
    pub fn send_reserved(
        &mut self,
        admin: &str,
        number: &str,
        hash: &str,
    ) -> Result<(), YukiError> {
        let numbers = self.0.state_mut();
        numbers.check_reserved(admin, number, hash)?;
        if let Some(entry) = numbers.latest(admin, number, &[Status::Reserved]) {
            entry.status = Status::Pending;
        }
        self.0.save()
    }

    /// Undo [`send_reserved`](Self::send_reserved) when nothing reached Yuki:
    /// the number is reserved again for the same content, to retry.
    pub fn unsend(&mut self, admin: &str, number: &str) -> Result<(), YukiError> {
        let entry = self
            .0
            .state_mut()
            .latest(admin, number, &[Status::Pending])
            .ok_or_else(|| YukiError::NotFound(format!("no pending invoice number {number}")))?;
        entry.status = Status::Reserved;
        self.0.save()
    }

    /// Release a reservation that will not be sent: the number is free again.
    pub fn release(&mut self, admin: &str, number: &str) -> Result<(), YukiError> {
        let entry = self
            .0
            .state_mut()
            .latest(admin, number, &[Status::Reserved])
            .ok_or_else(|| YukiError::NotFound(format!("no reserved invoice number {number}")))?;
        entry.status = Status::Rejected;
        self.0.save()
    }

    /// Yuki booked `number`.
    pub fn commit(&mut self, admin: &str, number: &str) -> Result<(), YukiError> {
        self.0.state_mut().settle(admin, number, Status::Booked)?;
        self.0.save()
    }

    /// Yuki did not create `number`: free it again.
    pub fn reject(&mut self, admin: &str, number: &str) -> Result<(), YukiError> {
        self.0.state_mut().settle(admin, number, Status::Rejected)?;
        self.0.save()
    }

    /// Settle a pending `number` by hand, after checking Yuki; `rejected`
    /// also releases a reservation.
    pub fn resolve(&mut self, admin: &str, number: &str, status: Status) -> Result<(), YukiError> {
        match status {
            Status::Booked => self.commit(admin, number),
            Status::Rejected => {
                let reserved =
                    self.list().holder(admin, number).map(|e| e.status) == Some(Status::Reserved);
                if reserved {
                    self.release(admin, number)
                } else {
                    self.reject(admin, number)
                }
            }
            Status::Pending | Status::Reserved => Err(YukiError::Config(
                "resolve a number as booked or rejected".into(),
            )),
        }
    }

    /// Note on the latest pending entry of `admin` for `number`.
    pub fn note(&mut self, admin: &str, number: &str, note: &str) -> Result<(), YukiError> {
        if let Some(entry) = self.0.state_mut().latest(admin, number, &[Status::Pending]) {
            entry.note = Some(note.to_string());
        }
        self.0.save()
    }
}

/// The `admin_id` of the configuration's only administration, if it has
/// exactly one.
fn sole_admin(config: &Config) -> Option<&str> {
    match config
        .administrations
        .values()
        .collect::<Vec<_>>()
        .as_slice()
    {
        [only] => Some(only.admin_id.as_str()),
        _ => None,
    }
}

/// Print [`Numbers::warnings`] for `admin` to stderr.
pub fn warn(numbers: &Numbers, admin: &str) {
    let today = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
        .div_euclid(86_400);
    for warning in numbers.warnings(admin, today) {
        eprintln!("warning: {warning}");
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

/// What `sales invoice numbers` changes before listing.
#[derive(Debug, Clone, Copy)]
pub enum Settle<'a> {
    /// `--resolve <number> booked|rejected`.
    Resolve(&'a str, Status),
    /// `--release <number>`: a reservation only.
    Release(&'a str),
}

/// `sales invoice numbers`: list the numbers of administration `admin`,
/// after settling one by hand when `settle` is given. With `explicit` (an
/// `--admin` on the command line) an entry without an administration may be
/// settled, and is then recorded as `admin`'s.
pub fn numbers(
    config: &Config,
    admin: &str,
    explicit: bool,
    settle: Option<Settle<'_>>,
    format: Option<&str>,
) -> Result<(), YukiError> {
    let numbers = match settle {
        Some(settle) => {
            let mut ledger = InvoiceLedger::open(config)?;
            let (Settle::Resolve(number, _) | Settle::Release(number)) = settle;
            let own = ledger
                .list()
                .entries_of(admin)
                .any(|e| same_number(&e.number, number));
            if explicit && !own {
                ledger.0.state_mut().claim_legacy(admin, number);
            }
            match settle {
                Settle::Resolve(number, status) => ledger.resolve(admin, number, status)?,
                Settle::Release(number) => ledger.release(admin, number)?,
            }
            ledger.list().clone()
        }
        None => InvoiceLedger::peek(config)?,
    };
    warn(&numbers, admin);
    let headers: Vec<String> = [
        "Number", "Date", "Customer", "Gross", "Status", "Recorded", "Booked", "Note",
    ]
    .map(String::from)
    .to_vec();
    let rows: Vec<Vec<String>> = numbers
        .entries_of(admin)
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

    fn claim<'a>(admin: &'a str, number: &'a str) -> Claim<'a> {
        Claim {
            admin,
            number,
            date: "2026-10-31",
            customer: "Example BV",
            gross: "121.00",
        }
    }

    #[test]
    fn a_number_stays_taken_until_rejected_and_survives_a_reopen() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("invoice-numbers.json");
        InvoiceLedger::open_at(&path)
            .unwrap()
            .reserve(&claim("a1", "2026-20"))
            .unwrap();
        let mut ledger = InvoiceLedger::open_at(&path).unwrap();
        assert_eq!(
            ledger.list().holder("a1", "2026-20").unwrap().status,
            Status::Pending
        );
        let err = ledger.reserve(&claim("a1", "2026-20")).unwrap_err();
        assert!(err.to_string().contains("already given out: pending"));
        ledger.reject("a1", "2026-20").unwrap();
        assert!(ledger.list().holder("a1", "2026-20").is_none());
        // A rejected number can be given out again, and booked.
        ledger.reserve(&claim("a1", "2026-20")).unwrap();
        ledger.commit("a1", "2026-20").unwrap();
        let booked = ledger.list().holder("a1", "2026-20").unwrap();
        assert_eq!(booked.status, Status::Booked);
        assert!(booked.booked_at.is_some());
        assert!(
            ledger.resolve("a1", "2026-20", Status::Rejected).is_err(),
            "nothing pending"
        );
        assert_eq!(
            ledger.list().taken_numbers("a1").collect::<Vec<_>>(),
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
    }

    #[test]
    fn numbers_belong_to_an_administration_and_compare_by_value() {
        let mut numbers = Numbers::default();
        numbers.reserve(&claim("a1", "2026-01")).unwrap();
        // 2026-1 is 2026-01, in the same administration only.
        let err = numbers.reserve(&claim("a1", "2026-1")).unwrap_err();
        assert!(err.to_string().contains("already given out"), "{err}");
        assert!(numbers.holder("a1", "2026-001").is_some());
        numbers.reserve(&claim("a2", "2026-1")).unwrap();
        assert_eq!(numbers.taken_numbers("a2").collect::<Vec<_>>(), ["2026-1"]);
        numbers.settle("a2", "2026-01", Status::Booked).unwrap();
        assert_eq!(
            numbers.holder("a1", "2026-1").unwrap().status,
            Status::Pending
        );
        assert_eq!(
            numbers.holder("a2", "2026-1").unwrap().status,
            Status::Booked
        );
    }

    #[test]
    fn a_second_opener_waits_for_the_lock() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("invoice-numbers.json");
        let held = InvoiceLedger::open_at(&path).unwrap();
        let start = std::time::Instant::now();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            drop(held);
        });
        InvoiceLedger::open_at(&path).unwrap();
        assert!(start.elapsed() >= std::time::Duration::from_millis(150));
        release.join().unwrap();
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
        // An entry without an administration counts for none, with a warning.
        assert_eq!(ledger.list().taken_numbers("any").count(), 0);
        let warnings = ledger.list().warnings("any", 0);
        assert!(
            warnings[0].contains("without an administration (2026-19)"),
            "{warnings:?}"
        );
        ledger.reserve(&claim("a1", "2026-20")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"version\": 2"), "{text}");
        assert!(text.contains("\"invoice_pdf\": \"kept\""), "{text}");
    }

    #[test]
    fn a_reservation_is_sent_only_for_its_content_and_can_be_released() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("invoice-numbers.json");
        let mut ledger = InvoiceLedger::open_at(&path).unwrap();
        ledger
            .reserve_prepared(&claim("a1", "2026-20"), "hash-a")
            .unwrap();
        // Reserved numbers are taken, like pending ones.
        assert!(ledger.reserve(&claim("a1", "2026-20")).is_err());
        let err = ledger.send_reserved("a1", "2026-20", "hash-b").unwrap_err();
        assert!(err.to_string().contains("changed since"), "{err}");
        assert!(ledger.send_reserved("a2", "2026-20", "hash-a").is_err());
        ledger.send_reserved("a1", "2026-20", "hash-a").unwrap();
        assert_eq!(
            ledger.list().holder("a1", "2026-20").unwrap().status,
            Status::Pending
        );
        let err = ledger.send_reserved("a1", "2026-20", "hash-a").unwrap_err();
        assert!(err.to_string().contains("pending already"), "{err}");
        assert!(
            ledger.release("a1", "2026-20").is_err(),
            "pending, not reserved"
        );
        ledger.commit("a1", "2026-20").unwrap();

        // Released (or resolved as rejected), a reservation frees its number.
        ledger
            .reserve_prepared(&claim("a1", "2026-21"), "h")
            .unwrap();
        ledger
            .reserve_prepared(&claim("a1", "2026-22"), "h")
            .unwrap();
        ledger.release("a1", "2026-21").unwrap();
        ledger.resolve("a1", "2026-22", Status::Rejected).unwrap();
        assert!(ledger.list().holder("a1", "2026-21").is_none());
        assert!(ledger.list().holder("a1", "2026-22").is_none());
    }

    #[test]
    fn legacy_entries_go_to_a_sole_administration_or_to_an_explicit_settle() {
        let legacy = || {
            let mut numbers = Numbers::default();
            numbers.reserve(&claim("x", "2026-19")).unwrap();
            numbers.entries[0].admin = None;
            numbers
        };
        let mut sole = legacy();
        sole.adopt("a1");
        assert_eq!(
            sole.holder("a1", "2026-19").unwrap().admin.as_deref(),
            Some("a1")
        );
        assert!(sole.warnings("a1", 0).is_empty());

        let mut many = legacy();
        assert!(many.holder("a1", "2026-19").is_none());
        assert!(many.settle("a1", "2026-19", Status::Booked).is_err());
        assert!(!many.claim_legacy("a1", "2026-20"));
        assert!(many.claim_legacy("a1", "2026-019"));
        many.settle("a1", "2026-19", Status::Booked).unwrap();
        assert!(many.warnings("a1", 0).is_empty());
    }

    #[test]
    fn old_reservations_are_warned_about() {
        let mut numbers = Numbers::default();
        numbers
            .reserve_prepared(&claim("a1", "2026-20"), "h")
            .unwrap();
        numbers.entries[0].recorded_at = "2026-10-01T09:00:00Z".into();
        numbers.entries[0].customer = "Buuurt".into();
        let day = epoch_days("2026-10-01").unwrap();
        assert!(numbers.warnings("a1", day + STALE_DAYS).is_empty());
        let warnings = numbers.warnings("a1", day + STALE_DAYS + 1);
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].starts_with(
                "2026-20 reserved since 2026-10-01 for Buuurt: book it or --release it"
            ),
            "{warnings:?}"
        );
        assert!(numbers.warnings("a2", day + 30).is_empty());
    }

    #[test]
    fn timestamps_are_utc_iso() {
        let now = now_utc();
        assert_eq!(now.len(), 20, "{now}");
        assert!(now.ends_with('Z') && now.as_bytes()[10] == b'T', "{now}");
    }
}
