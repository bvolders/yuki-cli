//! The local ledger of purchase invoices `purchase book` sent to Yuki:
//! `purchase-invoices.json` next to the config.
//!
//! Yuki's API can neither find a purchase invoice it booked (the import
//! returns no ID) nor delete one, so this ledger is what stops a second
//! booking. An invoice is keyed by its supplier, its reference and the
//! sha256 of its PDF: an entry that is not `rejected` refuses another
//! booking with the same PDF, or with the same reference from the same
//! supplier. It is written `pending` just before `ProcessPurchaseInvoices`
//! is called, and Yuki's answer settles it: `booked`, or `rejected` (free
//! again) when nothing reached Yuki or Yuki provably created nothing. When
//! the outcome is unknown it stays `pending` until `purchase bookings
//! --resolve` settles it.
//!
//! Entries belong to an administration (its `admin_id`). The file is a
//! [`Ledger`], locked only for each short read-and-write, never while Yuki
//! is called.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cli::invoice_ledger::Resolution;
use crate::cli::sales_invoice::InvoiceError;
use crate::config::Config;
use crate::error::YukiError;
use crate::ledger::{Ledger, LedgerFormat};
use crate::sync::{name_key, now_utc};

/// What happened to a purchase invoice sent to Yuki.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Sent, or about to be, with no answer yet: it may be in Yuki.
    Pending,
    /// Yuki created it.
    Booked,
    /// Nothing reached Yuki, or Yuki created nothing: free to book again.
    Rejected,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Booked => "booked",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The administration (`admin_id`) it was booked in.
    pub admin: String,
    /// The supplier's name (`FullName`).
    pub contact: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_code: Option<String>,
    /// The supplier's invoice number (`Reference`).
    pub reference: String,
    /// The sha256 of the PDF.
    pub hash: String,
    /// The PDF's absolute path when it was booked.
    pub file: String,
    /// Invoice date.
    pub date: String,
    /// Total including VAT.
    pub total: String,
    pub status: Status,
    /// When it was sent, UTC.
    pub recorded_at: String,
    /// When it was settled as booked (or resolved by hand), UTC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub booked_at: Option<String>,
    /// What needs checking, or why it was rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Fields this version does not know, kept as they are.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Entry {
    /// Whether the entry still holds its invoice: anything not rejected.
    fn holds(&self) -> bool {
        self.status != Status::Rejected
    }

    /// Whether this is the same supplier: the same code when both have
    /// one, else the same name (Unicode NFC, any case).
    fn same_supplier(&self, contact: &str, code: Option<&str>) -> bool {
        match (self.contact_code.as_deref(), code) {
            (Some(a), Some(b)) => a.trim().eq_ignore_ascii_case(b.trim()),
            _ => name_key(self.contact.trim()) == name_key(contact.trim()),
        }
    }

    fn same_reference(&self, reference: &str) -> bool {
        name_key(self.reference.trim()) == name_key(reference.trim())
    }

    /// One line naming the invoice, for messages.
    pub fn label(&self) -> String {
        format!(
            "{} from {} ({}, {} EUR, {})",
            self.reference, self.contact, self.date, self.total, self.file
        )
    }
}

/// A purchase invoice about to be sent.
#[derive(Debug, Clone)]
pub struct Claim {
    pub contact: String,
    pub contact_code: Option<String>,
    pub reference: String,
    pub hash: String,
    pub file: String,
    pub date: String,
    pub total: String,
}

/// The ledger's entries, in memory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bookings {
    #[serde(default)]
    entries: Vec<Entry>,
    /// Top-level fields this version does not know, kept as they are.
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

impl LedgerFormat for Bookings {
    const VERSION: u32 = 1;
    const WHAT: &'static str = "purchase invoice ledger";
    const START_OVER: &'static str =
        " (purchase invoices booked before could then be booked a second time)";
}

/// The ledger file, seen by one administration.
pub struct PurchaseLedger {
    file: Ledger<Bookings>,
    admin: String,
}

impl PurchaseLedger {
    /// Lock and load the ledger for administration `admin`, waiting while
    /// another run holds the lock.
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

    /// Refused when an entry still holds the claimed PDF, or the claimed
    /// reference from the same supplier.
    pub fn check_free(&self, claim: &Claim) -> Result<(), InvoiceError> {
        let held = self.entries().filter(|e| e.holds());
        for entry in held {
            let why = if entry.hash == claim.hash {
                "this PDF"
            } else if entry.same_reference(&claim.reference)
                && entry.same_supplier(&claim.contact, claim.contact_code.as_deref())
            {
                "this supplier's reference"
            } else {
                continue;
            };
            let state = match entry.status {
                Status::Pending => format!(
                    "is pending (sent, outcome unknown): check Yuki, then `yuki purchase bookings --resolve {} --as booked` (or `--as rejected` if Yuki has nothing)",
                    entry.reference
                ),
                _ => format!("was booked {}", entry.booked_at.as_deref().unwrap_or("")),
            };
            return Err(InvoiceError::InvalidInput(format!(
                "DUPLICATE: {why} was sent to Yuki before: purchase invoice {} {state}. Nothing was sent; a purchase invoice cannot be deleted through the API",
                entry.label()
            )));
        }
        Ok(())
    }

    /// Write-ahead: record the claim as pending, refused like
    /// [`check_free`](Self::check_free).
    pub fn send(&mut self, claim: &Claim) -> Result<(), InvoiceError> {
        self.check_free(claim)?;
        self.file.state_mut().entries.push(Entry {
            admin: self.admin.clone(),
            contact: claim.contact.clone(),
            contact_code: claim.contact_code.clone(),
            reference: claim.reference.clone(),
            hash: claim.hash.clone(),
            file: claim.file.clone(),
            date: claim.date.clone(),
            total: claim.total.clone(),
            status: Status::Pending,
            recorded_at: now_utc(),
            booked_at: None,
            note: None,
            extra: BTreeMap::new(),
        });
        Ok(self.file.save()?)
    }

    /// Settle the pending entry for the PDF `hash` as `to`, with `note`.
    pub fn settle(&mut self, hash: &str, to: Status, note: Option<&str>) -> Result<(), YukiError> {
        let admin = &self.admin;
        let entry = self
            .file
            .state_mut()
            .entries
            .iter_mut()
            .rev()
            .find(|e| e.admin == *admin && e.status == Status::Pending && e.hash == hash)
            .ok_or_else(|| {
                YukiError::NotFound("no pending purchase invoice for this PDF".into())
            })?;
        entry.status = to;
        if to == Status::Booked {
            entry.booked_at = Some(now_utc());
        }
        if let Some(note) = note {
            entry.note = Some(note.to_string());
        }
        self.file.save()
    }

    /// Note on the pending entry for `hash`, leaving it pending.
    pub fn note(&mut self, hash: &str, note: &str) -> Result<(), YukiError> {
        let admin = &self.admin;
        if let Some(entry) = self
            .file
            .state_mut()
            .entries
            .iter_mut()
            .rev()
            .find(|e| e.admin == *admin && e.status == Status::Pending && e.hash == hash)
        {
            entry.note = Some(note.to_string());
        }
        self.file.save()
    }

    /// Settle the pending entry with `reference` by hand, after checking
    /// Yuki; `contact` picks one when several suppliers have it pending.
    /// Returns the entry as settled.
    pub fn resolve(
        &mut self,
        reference: &str,
        contact: Option<&str>,
        resolution: Resolution,
    ) -> Result<Entry, InvoiceError> {
        let pending: Vec<&Entry> = self
            .entries()
            .filter(|e| e.status == Status::Pending && e.same_reference(reference))
            .filter(|e| contact.is_none_or(|c| e.same_supplier(c, None)))
            .collect();
        let hash = match pending.as_slice() {
            [] => {
                return Err(InvoiceError::Yuki(YukiError::NotFound(format!(
                    "no pending purchase invoice {reference}{}",
                    contact.map(|c| format!(" from {c}")).unwrap_or_default()
                ))));
            }
            [one] => one.hash.clone(),
            many => {
                let suppliers: Vec<&str> = many.iter().map(|e| e.contact.as_str()).collect();
                return Err(InvoiceError::InvalidInput(format!(
                    "{} pending purchase invoices have reference {reference} (from {}): add --contact <name>",
                    many.len(),
                    suppliers.join(", ")
                )));
            }
        };
        let (to, note) = match resolution {
            Resolution::Booked => (Status::Booked, "resolved by hand as booked"),
            Resolution::Rejected => (Status::Rejected, "resolved by hand as not in Yuki"),
        };
        self.settle(&hash, to, Some(note))?;
        let settled = self
            .entries()
            .filter(|e| e.hash == hash)
            .last()
            .cloned()
            .expect("the entry just settled");
        Ok(settled)
    }
}

/// The ledger file: `purchase-invoices.json` next to the config.
pub fn ledger_path() -> PathBuf {
    Config::dir().join("purchase-invoices.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(reference: &str, hash: &str) -> Claim {
        Claim {
            contact: "Leverancier BV".into(),
            contact_code: None,
            reference: reference.into(),
            hash: hash.into(),
            file: "/tmp/f.pdf".into(),
            date: "2026-10-20".into(),
            total: "121.00".into(),
        }
    }

    struct Dir(tempfile::TempDir);

    impl Dir {
        fn new() -> Self {
            Self(tempfile::TempDir::new().unwrap())
        }

        fn open(&self, admin: &str) -> PurchaseLedger {
            PurchaseLedger::open_at(&self.0.path().join("purchase-invoices.json"), admin).unwrap()
        }
    }

    #[test]
    fn the_same_pdf_or_reference_is_refused_until_rejected() {
        let dir = Dir::new();
        let mut ledger = dir.open("a1");
        ledger.send(&claim("F-1", "h1")).unwrap();
        // The same PDF under another reference, and the same reference
        // (any case) with another PDF, are both refused while pending.
        let err = ledger.send(&claim("F-2", "h1")).unwrap_err().to_string();
        assert!(
            err.contains("DUPLICATE: this PDF") && err.contains("pending"),
            "{err}"
        );
        let err = ledger.send(&claim("f-1", "h2")).unwrap_err().to_string();
        assert!(err.contains("this supplier's reference"), "{err}");
        // Another supplier may use the same reference.
        let other = Claim {
            contact: "Andere NV".into(),
            ..claim("F-1", "h3")
        };
        ledger.send(&other).unwrap();

        ledger
            .settle("h1", Status::Rejected, Some("not sent"))
            .unwrap();
        ledger.send(&claim("F-1", "h1")).unwrap();
        ledger.settle("h1", Status::Booked, None).unwrap();
        let err = ledger.send(&claim("F-1", "h1")).unwrap_err().to_string();
        assert!(err.contains("was booked"), "{err}");
        drop(ledger);
        // Another administration has its own invoices.
        dir.open("a2").send(&claim("F-1", "h1")).unwrap();
    }

    #[test]
    fn a_contact_code_identifies_the_supplier() {
        let dir = Dir::new();
        let mut ledger = dir.open("a1");
        let coded = |name: &str, code: &str, hash: &str| Claim {
            contact: name.into(),
            contact_code: Some(code.into()),
            ..claim("F-1", hash)
        };
        ledger.send(&coded("Leverancier BV", "SUP1", "h1")).unwrap();
        assert!(ledger.send(&coded("Leverancier", "sup1", "h2")).is_err());
        ledger.send(&coded("Leverancier BV", "SUP2", "h3")).unwrap();
    }

    #[test]
    fn a_pending_invoice_is_resolved_by_reference() {
        let dir = Dir::new();
        let mut ledger = dir.open("a1");
        ledger.send(&claim("F-1", "h1")).unwrap();
        let other = Claim {
            contact: "Andere NV".into(),
            ..claim("F-1", "h2")
        };
        ledger.send(&other).unwrap();
        let err = ledger
            .resolve("F-1", None, Resolution::Booked)
            .unwrap_err()
            .to_string();
        assert!(err.contains("add --contact"), "{err}");
        let settled = ledger
            .resolve("f-1", Some("andere nv"), Resolution::Rejected)
            .unwrap();
        assert_eq!(
            (settled.hash.as_str(), settled.status),
            ("h2", Status::Rejected)
        );
        let settled = ledger.resolve("F-1", None, Resolution::Booked).unwrap();
        assert_eq!(settled.status, Status::Booked);
        assert!(settled.booked_at.is_some());
        assert!(ledger.resolve("F-1", None, Resolution::Booked).is_err());
    }
}
