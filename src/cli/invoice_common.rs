//! What sales and purchase invoices share: the error a command stops
//! with, the PDF, date and text checks, and how a failed import call or a
//! failed import is judged.

use std::path::Path;

use crate::client::invoice_import::InvoicesImport;
use crate::error::{Delivery, YukiError};
use crate::output::human_size;
use crate::period::epoch_days;

/// The largest custom PDF accepted. Yuki documents no limit, but ASP.NET's
/// default `maxRequestLength` is 4 MB and base64 adds a third, so 3 MB of
/// PDF (4 MB encoded) is about what one request can carry.
pub const PDF_MAX_BYTES: usize = 3 * 1024 * 1024;

/// Accept an ISO calendar date (`--date`, `date`, `due_date`).
pub fn parse_date(text: &str) -> Result<String, String> {
    let text = text.trim();
    let shaped = text.len() == 10
        && text.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 => b == b'-',
            _ => b.is_ascii_digit(),
        });
    if shaped && epoch_days(text).is_some() {
        Ok(text.to_string())
    } else {
        Err(format!("'{text}' is not a date (YYYY-MM-DD)"))
    }
}

/// A custom invoice PDF, read and checked when the invoice is loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pdf {
    /// The local file name, for the preview; Yuki stores it as `Invoice <number>.pdf`.
    pub name: String,
    pub bytes: Vec<u8>,
}

impl Pdf {
    /// Read `path`, which must be a PDF of at most [`PDF_MAX_BYTES`].
    pub fn read(path: &Path) -> Result<Self, String> {
        let shown = path.display();
        let size = std::fs::metadata(path)
            .map_err(|e| format!("{shown}: {e}"))?
            .len();
        if size > PDF_MAX_BYTES as u64 {
            return Err(format!(
                "{shown} is {}, over the {} limit",
                human_size(size),
                human_size(PDF_MAX_BYTES as u64)
            ));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{shown}: {e}"))?;
        if !bytes.starts_with(b"%PDF-") {
            return Err(format!("{shown} is not a PDF (no %PDF- header)"));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(Self { name, bytes })
    }
}

/// Trimmed text, `None` when empty; characters XML 1.0 cannot carry are a
/// problem of `field`, pushed to `problems`.
pub fn xml_text(field: &str, value: Option<&str>, problems: &mut Vec<String>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    if value.chars().any(|c| {
        (c.is_control() && !matches!(c, '\t' | '\n' | '\r')) || matches!(c, '\u{FFFE}' | '\u{FFFF}')
    }) {
        problems.push(format!("{field} contains a character XML cannot carry"));
    }
    Some(value.to_string())
}

/// A failed import call: [`InvoiceError::Yuki`] when nothing reached Yuki
/// or Yuki refused it unprocessed, else [`InvoiceError::OutcomeUnknown`],
/// saying `what_now`.
pub fn classify(e: YukiError, what_now: &str) -> InvoiceError {
    match e.delivery() {
        Delivery::Unknown => InvoiceError::OutcomeUnknown(format!("{e}: {what_now}")),
        Delivery::NotSent | Delivery::Refused => InvoiceError::Yuki(e),
    }
}

/// Why Yuki did not accept every invoice of `import`, and whether its
/// answer proves it created nothing (so the invoice is free again); `None`
/// when it accepted them all.
pub fn rejection(import: &InvoicesImport) -> Option<(String, bool)> {
    import.failure().map(|m| (m, import.nothing_created()))
}

/// Why an invoice command stopped, raised where it is found, with the
/// error kind the CLI reports.
#[derive(Debug)]
pub enum InvoiceError {
    /// From Yuki or the configuration; for an import call, nothing reached Yuki
    /// or Yuki refused it unprocessed ([`Delivery::NotSent`],
    /// [`Delivery::Refused`]). Its kind is the [`YukiError`]'s.
    Yuki(YukiError),
    /// The invoice, its file, the flags or the number are not acceptable;
    /// nothing was sent (`invalid_input`).
    InvalidInput(String),
    /// Not confirmed, or not confirmable without a prompt; nothing was sent.
    ConfirmationRequired(String),
    /// Yuki answered, but did not create or book it as asked.
    Rejected(String),
    /// Booked, but not emailed as asked.
    SendIncomplete(String),
    /// Booked under another reference than the number sent.
    ReferenceMismatch(String),
    /// The request may have been processed, without an answer saying how
    /// ([`Delivery::Unknown`]): a timeout, a dropped connection, a fault.
    OutcomeUnknown(String),
}

impl InvoiceError {
    /// The error kind; `error` for [`Yuki`](Self::Yuki), whose own kind the
    /// CLI reports instead.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Yuki(_) => "error",
            Self::InvalidInput(_) => "invalid_input",
            Self::ConfirmationRequired(_) => "confirmation_required",
            Self::Rejected(_) => "invoice_rejected",
            Self::SendIncomplete(_) => "send_incomplete",
            Self::ReferenceMismatch(_) => "reference_mismatch",
            Self::OutcomeUnknown(_) => "outcome_unknown",
        }
    }

    /// A configuration error is a problem with the input; anything else
    /// stays a Yuki error.
    pub fn input(e: YukiError) -> Self {
        match e {
            YukiError::Config(message) => Self::InvalidInput(message),
            other => Self::Yuki(other),
        }
    }
}

impl std::fmt::Display for InvoiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Yuki(e) => write!(f, "{e}"),
            Self::InvalidInput(m)
            | Self::ConfirmationRequired(m)
            | Self::Rejected(m)
            | Self::SendIncomplete(m)
            | Self::ReferenceMismatch(m)
            | Self::OutcomeUnknown(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for InvoiceError {}

impl From<YukiError> for InvoiceError {
    fn from(e: YukiError) -> Self {
        Self::Yuki(e)
    }
}
