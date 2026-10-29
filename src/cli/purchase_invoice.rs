//! `yuki purchase book`: book a supplier's PDF invoice in Yuki through
//! `ProcessPurchaseInvoices`, as a proposal for the accountant unless
//! `--process` processes it.
//!
//! The invoice is checked locally (amounts in exact cents: the lines plus
//! the VAT must make the total), against Yuki's active VAT codes and chart
//! of accounts, and against everything that may already have put the PDF in
//! Yuki: the purchase ledger ([`crate::cli::purchase_ledger`]), the
//! `.yuki-sync.json` of `upload dir` governing the file, and the archive.
//! Yuki's API can delete neither a purchase invoice nor its document, so a
//! second booking is refused rather than undone. [`book`] previews, asks
//! and sends; nothing else contacts Yuki with a write.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::cli::contacts::{yes_no, yes_no_unknown};
use crate::cli::invoice_ledger::Resolution;
use crate::cli::purchase_ledger::{
    Claim, PurchaseLedger, Status as LedgerStatus, contact_key, reference_key,
};
use crate::cli::sales_invoice::{InvoiceError, Pdf};
use crate::cli::xml_doc::{XmlWriter, bool_text};
use crate::client::accounting_info::AccountingInfoClient;
use crate::client::archive::{ALL_DATES, ArchiveClient, ArchiveDocument};
use crate::client::contact::{Contact, ContactClient};
use crate::client::purchase::{PurchaseClient, PurchaseInvoicesImport};
use crate::client::vat::{VatClient, VatCode};
use crate::config::{ApiClient, Config, Target};
use crate::error::{Delivery, YukiError};
use crate::ledger::Ledger;
use crate::money::Cents;
use crate::output::{OutputFormat, format_json, format_table, human_size, is_tty};
use crate::sync::{self, Entry as SyncEntry, State as SyncState, Status as SyncStatus};

/// Namespace of the `PurchaseInvoices` document; Yuki rejects one without it.
pub const NAMESPACE: &str = "urn:xmlns:http://www.theyukicompany.com:purchaseinvoices";
/// Ten integer digits: the XSD's limit on an amount (totalDigits 12, fractionDigits 2).
const AMOUNT_MAX: i64 = 10_i64.pow(12);
/// `DocumentFileName` has at least five characters.
const FILE_NAME_MIN: usize = 5;

/// How long `--resolve` waits for a running `upload dir` to release the sync state.
const SYNC_PATIENCE: std::time::Duration = std::time::Duration::from_secs(5);

/// Where to ask for an API client when Yuki refuses the access key alone.
pub const CLIENT_HINT: &str = "If Yuki refuses purchase invoices for this access key, ask Yuki support (support.accounting@nmbrs.com) for an API Client ID and Client Secret, and set client_id and client_secret in the config (or YUKI_CLIENT_ID and YUKI_CLIENT_SECRET)";

/// `PaymentMethod`: how the invoice is (to be) paid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum PaymentMethod {
    /// ElectronicTransfer: you pay it by bank transfer (Yuki's default).
    Transfer,
    /// DirectCollection: the supplier collects it by direct debit.
    DirectDebit,
    /// Cash.
    Cash,
    /// DebitCard.
    DebitCard,
    /// CreditCard.
    #[value(alias = "card")]
    CreditCard,
    /// ReceivedElectronically.
    ReceivedElectronically,
    /// ReceivedCash.
    ReceivedCash,
    /// ToSettle.
    ToSettle,
    /// PaidByEmployee: an expense claim.
    PaidByEmployee,
}

impl PaymentMethod {
    /// The names `--payment-method` takes.
    pub const NAMES: &[&str] = &[
        "transfer",
        "direct-debit",
        "cash",
        "debit-card",
        "credit-card",
        "received-electronically",
        "received-cash",
        "to-settle",
        "paid-by-employee",
    ];

    /// The value as the XSD spells it.
    pub fn xsd(self) -> &'static str {
        match self {
            Self::Transfer => "ElectronicTransfer",
            Self::DirectDebit => "DirectCollection",
            Self::Cash => "Cash",
            Self::DebitCard => "DebitCard",
            Self::CreditCard => "CreditCard",
            Self::ReceivedElectronically => "ReceivedElectronically",
            Self::ReceivedCash => "ReceivedCash",
            Self::ToSettle => "ToSettle",
            Self::PaidByEmployee => "PaidByEmployee",
        }
    }
}

/// One invoice line as `--line` gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineArg {
    pub description: Option<String>,
    pub gl_account: String,
    /// Excluding VAT.
    pub amount: Cents,
    pub vat_type: i64,
    pub vat_description: String,
}

/// Parse `--line "description;gl_account;amount_excl_vat;vat_type;vat_description"`.
pub fn parse_line(text: &str) -> Result<LineArg, String> {
    let parts: Vec<&str> = text.split(';').map(str::trim).collect();
    let [description, gl, amount, vat_type, vat_description] = parts.as_slice() else {
        return Err(format!(
            "'{text}' is not description;gl_account;amount_excl_vat;vat_type;vat_description (5 fields separated by ';', none containing one)"
        ));
    };
    if gl.is_empty() {
        return Err(format!("'{text}': the GL account is empty"));
    }
    if vat_description.is_empty() {
        return Err(format!(
            "'{text}': the VAT description is empty (see `yuki vat codes`)"
        ));
    }
    let amount = Cents::parse_exact(amount).map_err(|e| format!("'{text}': amount: {e}"))?;
    let vat_type = vat_type
        .parse::<i64>()
        .map_err(|_| format!("'{text}': VAT type '{vat_type}' is not a number"))?;
    Ok(LineArg {
        description: (!description.is_empty()).then(|| description.to_string()),
        gl_account: gl.to_string(),
        amount,
        vat_type,
        vat_description: vat_description.to_string(),
    })
}

/// What `purchase book` was asked.
#[derive(Debug, Clone, Default)]
pub struct BookArgs {
    pub file: PathBuf,
    pub contact: String,
    pub contact_code: Option<String>,
    pub country: Option<String>,
    pub vat_number: Option<String>,
    pub iban: Option<String>,
    pub reference: String,
    pub date: String,
    pub due_date: Option<String>,
    pub total: Cents,
    pub vat: Cents,
    pub payment_method: Option<PaymentMethod>,
    pub subject: Option<String>,
    pub remarks: Option<String>,
    /// The single-line shortcut; the line amount is total − VAT.
    pub gl_account: Option<String>,
    pub vat_type: Option<i64>,
    pub vat_description: Option<String>,
    pub description: Option<String>,
    /// `--line`, instead of the shortcut.
    pub lines: Vec<LineArg>,
    /// Mark it processed instead of leaving a proposal.
    pub process: bool,
    pub dry_run: bool,
    /// The reference, repeated, for a booking without a prompt.
    pub confirm: Option<String>,
    /// Book even when the archive holds a document that looks like it.
    pub allow_archive_match: bool,
    /// Book even when the ledger holds an invoice with three of the same
    /// supplier, reference, date and total.
    pub allow_duplicate: bool,
    /// A supplier Yuki creates is a person, not a company.
    pub person: bool,
}

/// The supplier: matched by `code`, else by `name`, else created from the
/// details given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supplier {
    /// As given, or as Yuki has it once [`Match::Existing`].
    pub name: String,
    pub code: Option<String>,
    pub country: Option<String>,
    pub vat_number: Option<String>,
    pub iban: Option<String>,
    /// `ContactType` of a supplier Yuki creates: `Person` rather than `Company`.
    pub person: bool,
    pub matched: Match,
}

/// What Yuki's contacts say about the supplier, read by [`verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    /// Not looked up (yet).
    Unchecked,
    /// An existing contact, found by its code or by its name.
    Existing { by_code: bool },
    /// No such contact: Yuki creates it from what is sent.
    New,
}

impl Supplier {
    pub fn label(&self) -> String {
        match &self.code {
            Some(code) if self.name.is_empty() => format!("code {code}"),
            Some(code) => format!("{} (code {code})", self.name),
            None => self.name.clone(),
        }
    }

    /// Whether the contact is matched by its code: then the code alone
    /// identifies it, and Yuki overwrites it with whatever else is sent.
    fn by_code(&self) -> bool {
        self.code.is_some()
    }

    /// Whether Yuki is to create the contact.
    fn is_new(&self) -> bool {
        match self.matched {
            Match::New => true,
            Match::Existing { .. } => false,
            Match::Unchecked => self.code.is_none(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub description: Option<String>,
    pub gl_account: String,
    /// Excluding VAT.
    pub amount: Cents,
    pub vat_type: i64,
    /// As Yuki lists it once checked against the active VAT codes.
    pub vat_description: String,
}

/// A validated purchase invoice and its PDF.
#[derive(Debug, Clone)]
pub struct PurchaseInvoice {
    /// The PDF, canonical.
    pub path: PathBuf,
    pub pdf: Pdf,
    /// The PDF's sha256.
    pub hash: String,
    pub reference: String,
    pub date: String,
    pub due_date: Option<String>,
    /// Including VAT.
    pub total: Cents,
    pub vat: Cents,
    pub payment_method: Option<PaymentMethod>,
    pub subject: Option<String>,
    pub remarks: Option<String>,
    pub supplier: Supplier,
    pub lines: Vec<Line>,
    pub process: bool,
    /// What the checks against Yuki found worth saying, for the preview.
    pub notes: Vec<String>,
}

/// Trimmed text, `None` when empty; characters XML 1.0 cannot carry are a problem.
fn text(field: &str, value: Option<&str>, problems: &mut Vec<String>) -> Option<String> {
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

fn amount(field: &str, value: Cents, problems: &mut Vec<String>) {
    if value.0.abs() >= AMOUNT_MAX {
        problems.push(format!("{field} {value} exceeds Yuki's 10 integer digits"));
    }
}

/// Read the PDF and check everything that needs no API call: every problem
/// is reported at once.
pub fn build(args: &BookArgs) -> Result<PurchaseInvoice, InvoiceError> {
    let mut p = Vec::new();
    let path = std::fs::canonicalize(&args.file)
        .map_err(|e| InvoiceError::InvalidInput(format!("{}: {e}", args.file.display())))?;
    let pdf = Pdf::read(&path).map_err(InvoiceError::InvalidInput)?;
    if pdf.name.chars().count() < FILE_NAME_MIN {
        p.push(format!(
            "the file name '{}' is shorter than the {FILE_NAME_MIN} characters Yuki requires: rename it",
            pdf.name
        ));
    }
    if !pdf.name.to_lowercase().ends_with(".pdf") {
        p.push(format!(
            "'{}' does not end in .pdf: Yuki files the document by its name",
            pdf.name
        ));
    }
    let hash = sync::sha256_hex(&pdf.bytes);

    let reference = text("--reference", Some(&args.reference), &mut p).unwrap_or_else(|| {
        p.push("--reference (the supplier's invoice number) is empty".into());
        String::new()
    });
    let code = text("--contact-code", args.contact_code.as_deref(), &mut p);
    let name = text("--contact", Some(&args.contact), &mut p).unwrap_or_else(|| {
        if code.is_none() {
            p.push("--contact (the supplier's name) is empty: give it, or --contact-code".into());
        }
        String::new()
    });
    let country = text("--country", args.country.as_deref(), &mut p).map(|c| c.to_uppercase());
    if let Some(c) = &country
        && !(c.len() == 2 && c.chars().all(|ch| ch.is_ascii_alphabetic()))
    {
        p.push(format!(
            "--country '{c}' is not an ISO 3166-1 alpha-2 code such as BE"
        ));
    }
    let iban =
        text("--iban", args.iban.as_deref(), &mut p).map(|i| i.replace(' ', "").to_uppercase());
    if let Some(i) = &iban
        && let Err(why) = check_iban(i)
    {
        p.push(format!("--iban {i}: {why}"));
    }
    match args.payment_method {
        None => p.push(format!(
            "--payment-method is required: one of {}",
            PaymentMethod::NAMES.join(", ")
        )),
        Some(PaymentMethod::Transfer) if iban.is_none() => p.push(
            "--payment-method transfer needs --iban: Yuki requires the supplier's bank account to pay by transfer".into(),
        ),
        Some(_) => {}
    }
    let supplier = Supplier {
        name,
        code,
        country,
        vat_number: text("--vat-number", args.vat_number.as_deref(), &mut p),
        iban,
        person: args.person,
        matched: Match::Unchecked,
    };
    if let Some(due) = &args.due_date
        && *due < args.date
    {
        p.push(format!(
            "--due-date {due} is before the invoice date {}",
            args.date
        ));
    }
    if args.total.0 <= 0 {
        p.push(format!(
            "--total {} must be positive: credit notes are not supported",
            args.total
        ));
    }
    if args.vat.0 < 0 {
        p.push(format!("--vat-amount {} cannot be negative", args.vat));
    }
    if args.vat.0 > args.total.0 {
        p.push(format!(
            "--vat-amount {} exceeds --total {}: the total includes the VAT",
            args.vat, args.total
        ));
    }
    amount("--total", args.total, &mut p);
    amount("--vat-amount", args.vat, &mut p);

    let shortcut = [
        args.gl_account.is_some(),
        args.vat_type.is_some(),
        args.vat_description.is_some(),
        args.description.is_some(),
    ];
    let lines: Vec<Line> = if !args.lines.is_empty() {
        if shortcut.iter().any(|given| *given) {
            p.push("give the lines with --line, or one line with --gl-account, --vat-type and --vat-description; not both".into());
        }
        args.lines
            .iter()
            .map(|l| Line {
                description: l.description.clone(),
                gl_account: l.gl_account.clone(),
                amount: l.amount,
                vat_type: l.vat_type,
                vat_description: l.vat_description.clone(),
            })
            .collect()
    } else {
        let gl = text("--gl-account", args.gl_account.as_deref(), &mut p);
        let vat_description = text("--vat-description", args.vat_description.as_deref(), &mut p);
        match (gl, args.vat_type, vat_description) {
            (Some(gl_account), Some(vat_type), Some(vat_description)) => vec![Line {
                description: text("--description", args.description.as_deref(), &mut p),
                gl_account,
                amount: Cents(args.total.0 - args.vat.0),
                vat_type,
                vat_description,
            }],
            _ => {
                p.push("a line needs --gl-account, --vat-type and --vat-description (see `yuki accounts scheme` and `yuki vat codes`), or give each line with --line".into());
                Vec::new()
            }
        }
    };
    for (i, line) in lines.iter().enumerate() {
        if line.amount == Cents::ZERO {
            p.push(format!("line {} has amount 0", i + 1));
        }
        amount(&format!("line {} amount", i + 1), line.amount, &mut p);
        if let Some(d) = &line.description {
            text(&format!("line {} description", i + 1), Some(d), &mut p);
        }
    }
    let net: Cents = lines.iter().map(|l| l.amount).sum();
    if !lines.is_empty() && args.vat.0 <= args.total.0 && net + args.vat != args.total {
        p.push(format!(
            "the lines ({net} excl. VAT) plus the VAT ({}) make {}, not the total {}: off by {}",
            args.vat,
            net + args.vat,
            args.total,
            Cents(args.total.0 - net.0 - args.vat.0)
        ));
    }
    let subject = text("--subject", args.subject.as_deref(), &mut p);
    let remarks = text("--remarks", args.remarks.as_deref(), &mut p);
    if !p.is_empty() {
        return Err(InvoiceError::InvalidInput(format!(
            "cannot book {}:\n  - {}",
            args.file.display(),
            p.join("\n  - ")
        )));
    }
    Ok(PurchaseInvoice {
        path,
        pdf,
        hash,
        reference,
        date: args.date.clone(),
        due_date: args.due_date.clone(),
        total: args.total,
        vat: args.vat,
        payment_method: args.payment_method,
        subject,
        remarks,
        supplier,
        lines,
        process: args.process,
        notes: Vec::new(),
    })
}

/// Whether `iban` (upper case, no spaces) is an IBAN: country letters,
/// check digits, at most 34 characters, and the ISO 13616 mod-97 check.
pub fn check_iban(iban: &str) -> Result<(), String> {
    let b = iban.as_bytes();
    let shaped = (15..=34).contains(&b.len())
        && b[..2].iter().all(u8::is_ascii_uppercase)
        && b[2..4].iter().all(u8::is_ascii_digit)
        && b.iter().all(u8::is_ascii_alphanumeric);
    if !shaped {
        return Err(
            "not an IBAN (two letters, two check digits, up to 30 letters or digits)".into(),
        );
    }
    let rearranged = b[4..].iter().chain(&b[..4]);
    let remainder = rearranged.fold(0_u32, |acc, &c| {
        let value = if c.is_ascii_digit() {
            u32::from(c - b'0')
        } else {
            u32::from(c - b'A') + 10
        };
        if value >= 10 {
            (acc * 100 + value) % 97
        } else {
            (acc * 10 + value) % 97
        }
    });
    if remainder == 1 {
        Ok(())
    } else {
        Err("the check digits are wrong (mod-97): a typo?".into())
    }
}

/// The percentage, in hundredths, of a VAT code whose description is a
/// plain rate (`VAT 21%`, `BTW 6%`, `TVA 0%`); `None` for anything else,
/// such as reverse charge, imports or `Input tax`, where the VAT on the
/// invoice does not follow from the rate.
pub fn plain_rate(description: &str) -> Option<i64> {
    let lower = description.trim().to_lowercase();
    let rest = ["vat ", "btw ", "tva ", "mwst "]
        .iter()
        .find_map(|p| lower.strip_prefix(p))?;
    let number = rest.trim().strip_suffix('%')?.trim().replace(',', ".");
    crate::money::parse_percent(&number).ok()
}

impl PurchaseInvoice {
    pub fn net(&self) -> Cents {
        self.lines.iter().map(|l| l.amount).sum()
    }

    /// The `xmlDoc` of `ProcessPurchaseInvoices`: one `PurchaseInvoice` in
    /// the element order `PurchaseInvoices.xsd` requires, the PDF embedded.
    pub fn to_xml(&self) -> String {
        self.render(true)
    }

    /// [`to_xml`](Self::to_xml) for a human: the PDF's base64 elided.
    pub fn to_display_xml(&self) -> String {
        self.render(false)
    }

    fn render(&self, embed_pdf: bool) -> String {
        let mut x = XmlWriter::root("PurchaseInvoices", NAMESPACE);
        x.open("PurchaseInvoice");
        x.leaf("Process", bool_text(self.process));
        x.leaf("Reference", &self.reference);
        x.leaf("Date", &self.date);
        x.opt("DueDate", &self.due_date);
        x.leaf("InvoiceAmount", &self.total.to_string());
        x.leaf("InvoiceVATAmount", &self.vat.to_string());
        x.leaf("Currency", "EUR");
        x.opt("Subject", &self.subject);
        if let Some(method) = self.payment_method {
            x.leaf("PaymentMethod", method.xsd());
        }
        x.opt("Remarks", &self.remarks);
        x.leaf("DocumentFileName", &self.pdf.name);
        x.base64(
            &BASE64.encode(&self.pdf.bytes),
            self.pdf.bytes.len(),
            embed_pdf,
        );

        // Yuki overwrites a contact it matches with every element sent, so
        // an existing one gets its code (or exactly its name) and the bank
        // account, which Yuki adds rather than replaces; only a new one
        // gets the rest.
        let s = &self.supplier;
        x.open("Contact");
        x.opt("ContactCode", &s.code);
        if !s.by_code() {
            x.leaf("FullName", &s.name);
        }
        if s.is_new() {
            x.opt("CountryCode", &s.country);
            x.opt("VATNumber", &s.vat_number);
            x.leaf("ContactType", if s.person { "Person" } else { "Company" });
        }
        x.opt("BankAccount", &s.iban);
        x.close("Contact");

        x.open("InvoiceLines");
        for line in &self.lines {
            x.open("InvoiceLine");
            x.opt("Description", &line.description);
            x.leaf("GLAccountCode", &line.gl_account);
            x.leaf("LineAmount", &line.amount.to_string());
            x.leaf("LineVATType", &line.vat_type.to_string());
            x.leaf("LineVATDescription", &line.vat_description);
            x.close("InvoiceLine");
        }
        x.close("InvoiceLines");
        x.close("PurchaseInvoice");
        x.finish("PurchaseInvoices")
    }

    /// The ledger claim for this invoice.
    pub fn claim(&self) -> Claim {
        Claim {
            contact: self.supplier.name.clone(),
            contact_code: self.supplier.code.clone(),
            reference: self.reference.clone(),
            hash: self.hash.clone(),
            file: self.path.display().to_string(),
            date: self.date.clone(),
            total: self.total.to_string(),
        }
    }

    fn mode(&self) -> &'static str {
        if self.process {
            "PROCESS (Process=true): booked as processed"
        } else {
            "PROPOSAL (Process=false): the accountant reviews and processes it in Yuki"
        }
    }

    /// The yes/no question the confirmation prompt asks.
    pub fn question(&self) -> String {
        format!(
            "Book purchase invoice {} from {} ({} EUR) in Yuki? It cannot be deleted through the API",
            self.reference, self.supplier.name, self.total
        )
    }

    /// The one line `--quiet --yes` still prints.
    pub fn booking_line(&self) -> String {
        format!(
            "BOOKS PURCHASE INVOICE: {} {} {} EUR {}",
            self.reference,
            self.supplier.label(),
            self.total,
            self.pdf.name
        )
    }

    /// What will happen, for humans.
    pub fn preview(&self, administration: &str) -> String {
        let mut out = format!("Purchase invoice {}\n", self.path.display());
        let mut row = |label: &str, value: &str| {
            let _ = writeln!(out, "  {label:<15}{value}");
        };
        row("Administration", administration);
        row("Mode", self.mode());
        let s = &self.supplier;
        let bank = s
            .iban
            .as_deref()
            .map(|i| format!(" and bank account {i} (added to the contact)"))
            .unwrap_or_default();
        if s.is_new() {
            row(
                "Supplier",
                &format!("!! NEW supplier will be created in Yuki: {}", s.name),
            );
            let kind = if s.person { "person" } else { "company" };
            let details: Vec<&str> = [&s.country, &s.vat_number, &s.iban]
                .into_iter()
                .filter_map(|v| v.as_deref())
                .collect();
            row("", &format!("{kind}, {}", details.join(", ")));
        } else if s.by_code() {
            row("Supplier", &s.label());
            row(
                "",
                &format!(
                    "existing contact, matched by code: only the code{bank} is sent, so Yuki keeps its details"
                ),
            );
        } else {
            row("Supplier", &s.label());
            row(
                "",
                &format!(
                    "existing contact, matched by its name as Yuki has it: only the name{bank} is sent"
                ),
            );
        }
        row("Reference", &self.reference);
        let due = self
            .due_date
            .as_deref()
            .unwrap_or("the supplier's default term");
        row("Date", &format!("{}, due {due}", self.date));
        if let Some(method) = self.payment_method {
            row("Payment", method.xsd());
        }
        if let Some(subject) = &self.subject {
            row("Subject", subject);
        }
        if let Some(remarks) = &self.remarks {
            row("Remarks", remarks);
        }
        row(
            "PDF",
            &format!(
                "{} ({})",
                self.pdf.name,
                human_size(self.pdf.bytes.len() as u64)
            ),
        );
        let headers: Vec<String> = ["Description", "GL", "Net", "VAT type", "VAT description"]
            .map(String::from)
            .to_vec();
        let rows: Vec<Vec<String>> = self
            .lines
            .iter()
            .map(|l| {
                vec![
                    l.description.clone().unwrap_or_default(),
                    l.gl_account.clone(),
                    l.amount.to_string(),
                    l.vat_type.to_string(),
                    l.vat_description.clone(),
                ]
            })
            .collect();
        let _ = writeln!(out, "{}", format_table(&headers, &rows));
        let _ = writeln!(out, "  {:<15}{:>12} EUR", "Net", self.net().to_string());
        let _ = writeln!(out, "  {:<15}{:>12} EUR", "VAT", self.vat.to_string());
        let _ = write!(out, "  {:<15}{:>12} EUR", "Total", self.total.to_string());
        for note in &self.notes {
            let _ = write!(out, "\n  note: {note}");
        }
        out
    }
}

// ---------------------------------------------------------------------------
// What may already have put this PDF in Yuki.

/// The `upload dir` sync state governing a file: its root and the file's
/// path in it.
#[derive(Debug, Clone)]
pub struct SyncSpot {
    pub root: PathBuf,
    pub rel: String,
}

/// The sync state at or above the PDF's directory, if one governs it.
pub fn sync_spot(path: &Path) -> Result<Option<SyncSpot>, YukiError> {
    let Some(dir) = path.parent() else {
        return Ok(None);
    };
    let Some(root) = sync::find_root(dir)? else {
        return Ok(None);
    };
    Ok(sync::relative_to(&root, path).map(|rel| SyncSpot { root, rel }))
}

/// Refused when the sync state records the PDF as in Yuki, or as pending (it
/// may be). A failed upload never reached Yuki, and a skipped file was kept
/// out of it, so neither stops a booking.
fn check_sync(state: &SyncState, spot: &SyncSpot, hash: &str) -> Result<(), InvoiceError> {
    let Some(entry) = state.files.get(hash) else {
        return Ok(());
    };
    let why = match entry.status {
        SyncStatus::Uploaded | SyncStatus::AlreadyInYuki => "is already in Yuki",
        SyncStatus::Pending => "may already be in Yuki (pending: outcome unknown)",
        SyncStatus::Failed | SyncStatus::Skipped => return Ok(()),
    };
    let doc = entry
        .document_id
        .as_deref()
        .map(|d| format!(", document {d}"))
        .unwrap_or_default();
    Err(InvoiceError::InvalidInput(format!(
        "DUPLICATE: this PDF {why}: {} records it as {} ({}{doc}{}). Booking it would put it in Yuki twice, and a purchase invoice cannot be deleted through the API. Nothing was sent",
        spot.root.join(sync::STATE_FILE).display(),
        entry.status.as_str(),
        entry.path,
        entry
            .note
            .as_deref()
            .map(|n| format!("; {n}"))
            .unwrap_or_default()
    )))
}

/// Days either side of the invoice date an archive document with the same
/// amount counts as the same invoice.
const ARCHIVE_DAYS: i64 = 3;

/// The window the archive is searched in when the supplier is new, as
/// `documents exists` does: a week either side of the invoice date.
const WINDOW_DAYS: i64 = 7;

/// A file name as file names are compared: lower case, letters and digits
/// only, without the extension or a prefix Yuki or a supplier adds
/// (`Invoice-…`, `Factuur …`).
fn file_key(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let key = reference_key(&stem);
    ["invoice", "factuur", "facture", "rechnung"]
        .iter()
        .find_map(|p| key.strip_prefix(p).filter(|rest| rest.len() >= 4))
        .unwrap_or(&key)
        .to_string()
}

/// Archive documents that look like this invoice, with why: the same file
/// name (extension, case, punctuation and an `Invoice-` prefix aside; one
/// containing the other), the reference in a document's file name or
/// subject, or the same amount within [`ARCHIVE_DAYS`] of the invoice date.
/// The archive's amount is what was recognised or booked, so only the last
/// rule looks at it.
fn archive_matches(invoice: &PurchaseInvoice, docs: &[ArchiveDocument]) -> Vec<String> {
    let name = file_key(&invoice.pdf.name);
    let reference = reference_key(&invoice.reference);
    let day = crate::period::epoch_days(&invoice.date);
    let mut seen = std::collections::BTreeSet::new();
    docs.iter()
        .filter(|d| seen.insert(d.id.clone()))
        .filter_map(|d| {
            let file = file_key(&d.file_name);
            let doc_day = crate::period::epoch_days(d.document_date.get(..10).unwrap_or(""));
            let near =
                matches!((day, doc_day), (Some(a), Some(b)) if (a - b).abs() <= ARCHIVE_DAYS);
            let why = if name.len() >= 6
                && file.len() >= 6
                && (file.contains(&name) || name.contains(&file))
            {
                "same file name"
            } else if reference.len() >= 4
                && (file.contains(&reference) || reference_key(&d.subject).contains(&reference))
            {
                "reference in its file name or subject"
            } else if near && Cents::parse(&d.amount) == Some(invoice.total) {
                "same amount within 3 days"
            } else {
                return None;
            };
            Some(format!(
                "{why}: document {} '{}' ({}, {}, {} EUR, folder {})",
                d.id,
                d.file_name,
                d.contact_name,
                d.document_date.get(..10).unwrap_or(&d.document_date),
                d.amount,
                d.folder
            ))
        })
        .collect()
}

/// The archive documents to compare with: the supplier's, found by its
/// name as Yuki has it (the archive's free text matches the contact, not
/// the file name or reference), and every document within [`WINDOW_DAYS`]
/// of the invoice date, for a new supplier or one filed under another name.
async fn archive_candidates(
    archive: &ArchiveClient,
    invoice: &PurchaseInvoice,
) -> Result<Vec<ArchiveDocument>, YukiError> {
    let shift = |days: i64| {
        crate::period::epoch_days(&invoice.date)
            .map(|d| crate::period::date_from_epoch_days(d + days))
            .unwrap_or_else(|| invoice.date.clone())
    };
    let (from, to) = (shift(-WINDOW_DAYS), shift(WINDOW_DAYS));
    let by_supplier = async {
        match invoice.supplier.matched {
            Match::Existing { .. } => {
                let text = contact_key(&invoice.supplier.name);
                archive
                    .search_documents(&text, ALL_DATES.0, ALL_DATES.1)
                    .await
            }
            _ => Ok(Vec::new()),
        }
    };
    let (by_supplier, around) = tokio::join!(by_supplier, archive.search_documents("", &from, &to));
    Ok(by_supplier?.into_iter().chain(around?).collect())
}

/// The VAT code Yuki lists for `vat_type` with `description` (any case),
/// or why there is none.
fn vat_code<'a>(
    codes: &'a [VatCode],
    vat_type: i64,
    description: &str,
) -> Result<&'a VatCode, String> {
    let wanted = vat_type.to_string();
    let same_type: Vec<&VatCode> = codes.iter().filter(|c| c.code.trim() == wanted).collect();
    if let Some(code) = same_type.iter().find(|c| {
        c.description
            .trim()
            .eq_ignore_ascii_case(description.trim())
    }) {
        return Ok(code);
    }
    let quote = |v: Vec<String>| v.join(", ");
    let by_description: Vec<String> = codes
        .iter()
        .filter(|c| {
            c.description
                .trim()
                .eq_ignore_ascii_case(description.trim())
        })
        .map(|c| c.code.clone())
        .collect();
    let mut why =
        format!("VAT type {vat_type} with description '{description}' is not an active VAT code");
    if !same_type.is_empty() {
        let _ = write!(
            why,
            "; type {vat_type} is: {}",
            quote(
                same_type
                    .iter()
                    .map(|c| format!("'{}'", c.description))
                    .collect()
            )
        );
    }
    if !by_description.is_empty() {
        let _ = write!(why, "; '{description}' is type {}", quote(by_description));
    }
    why.push_str(" (see `yuki vat codes`)");
    Err(why)
}

/// Find the supplier among Yuki's `contacts` (the search for its code, or
/// its name): by code it must exist; by name it is the one contact with
/// the same [`contact_key`] (or, among several, the one spelled exactly the
/// same), and otherwise new, which needs a country.
fn match_supplier(
    s: &mut Supplier,
    contacts: &[Contact],
    problems: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    if let Some(code) = &s.code {
        let found: Vec<&Contact> = contacts
            .iter()
            .filter(|c| c.code.trim().eq_ignore_ascii_case(code.trim()))
            .collect();
        match found.as_slice() {
            [one] => {
                if !s.name.is_empty() && contact_key(&s.name) != contact_key(&one.name) {
                    notes.push(format!(
                        "--contact '{}' ignored: contact {code} is '{}' in Yuki",
                        s.name, one.name
                    ));
                }
                s.name = one.name.trim().to_string();
                s.matched = Match::Existing { by_code: true };
            }
            [] => problems.push(format!(
                "no contact has code {code} in Yuki (see `yuki contacts search <name>`)"
            )),
            _ => problems.push(format!("several contacts have code {code} in Yuki")),
        }
        return;
    }
    let key = contact_key(&s.name);
    let same: Vec<&Contact> = contacts
        .iter()
        .filter(|c| contact_key(&c.name) == key)
        .collect();
    let exact: Vec<&&Contact> = same
        .iter()
        .filter(|c| c.name.trim().eq_ignore_ascii_case(s.name.trim()))
        .collect();
    let chosen = match (same.as_slice(), exact.as_slice()) {
        ([one], _) => Some(*one),
        (_, [one]) => Some(**one),
        ([], _) => None,
        (many, _) => {
            let listed: Vec<String> = many
                .iter()
                .map(|c| match c.code.trim() {
                    "" => format!("'{}' (no code)", c.name),
                    code => format!("'{}' (code {code})", c.name),
                })
                .collect();
            problems.push(format!(
                "several contacts in Yuki match '{}': {}; pick one with --contact-code, or give its exact name",
                s.name,
                listed.join(", ")
            ));
            return;
        }
    };
    match chosen {
        Some(contact) => {
            if contact.name.trim() != s.name {
                notes.push(format!(
                    "supplier '{}' is '{}' in Yuki: that name is sent, so Yuki matches it",
                    s.name, contact.name
                ));
            }
            s.name = contact.name.trim().to_string();
            s.matched = Match::Existing { by_code: false };
        }
        None => {
            s.matched = Match::New;
            if s.country.is_none() {
                problems.push(format!(
                    "no contact named '{}' in Yuki, so Yuki would create one: give --country (and --vat-number), or --contact-code for an existing one",
                    s.name
                ));
            }
        }
    }
}

/// Check `--vat-amount` against the lines: each line's net at its VAT
/// code's rate, rounded per line, within a cent per line. A code that is
/// not a plain rate (reverse charge, imports, `Input tax`) skips the check,
/// with a note.
fn check_vat_amount(invoice: &mut PurchaseInvoice, problems: &mut Vec<String>) {
    let rates: Vec<Option<i64>> = invoice
        .lines
        .iter()
        .map(|l| plain_rate(&l.vat_description))
        .collect();
    if let Some(at) = rates.iter().position(Option::is_none) {
        invoice.notes.push(format!(
            "VAT amount not checked: line {}'s VAT code '{}' is not a plain rate",
            at + 1,
            invoice.lines[at].vat_description
        ));
        return;
    }
    let expected: Cents = invoice
        .lines
        .iter()
        .zip(&rates)
        .map(|(l, r)| l.amount.percent(r.unwrap_or_default()))
        .sum();
    let tolerance = invoice.lines.len() as i64;
    if (invoice.vat.0 - expected.0).abs() > tolerance {
        problems.push(format!(
            "--vat-amount {} does not match the lines: at their VAT rates it is {expected} (give or take {tolerance} cent{})",
            invoice.vat,
            if tolerance == 1 { "" } else { "s" }
        ));
    }
}

/// Check the invoice against Yuki, reading only: the VAT code pairs (and
/// take Yuki's spelling of each description), the GL accounts, and the
/// archive. Opens one session on the accounting service and shares it.
pub async fn verify(
    config: &Config,
    admin: Option<&str>,
    invoice: &mut PurchaseInvoice,
    allow_archive_match: bool,
) -> Result<(), InvoiceError> {
    let (accounting, archive, target) = crate::cli::setup_archive(config, admin).await?;
    let session = accounting
        .session_id()
        .map(str::to_string)
        .ok_or_else(|| YukiError::AuthFailed("no session after authenticating".into()))?;
    let vat = VatClient::new()
        .with_api_root(target.api_root)
        .with_session(&session);
    let info = AccountingInfoClient::new()
        .with_api_root(target.api_root)
        .with_session(&session);
    let contacts = ContactClient::new()
        .with_api_root(target.api_root)
        .with_session(&session);
    let (option, value) = match &invoice.supplier.code {
        Some(code) => ("Code", code.clone()),
        // Without the legal form, so `Leverancier BV` finds `Leverancier B.V.`.
        None => ("Name", contact_key(&invoice.supplier.name)),
    };
    let (codes, scheme, found) = tokio::join!(
        vat.active_vat_codes(target.admin_id),
        info.get_gl_account_scheme(target.admin_id),
        contacts.search_contacts(target.domain_id, option, &value),
    );
    let (codes, scheme) = (codes?, scheme?);
    let mut problems = Vec::new();
    match_supplier(
        &mut invoice.supplier,
        &found?,
        &mut problems,
        &mut invoice.notes,
    );
    check_vat_amount(invoice, &mut problems);
    for (i, line) in invoice.lines.iter_mut().enumerate() {
        match vat_code(&codes, line.vat_type, &line.vat_description) {
            Ok(code) => line.vat_description = code.description.trim().to_string(),
            Err(why) => problems.push(format!("line {}: {why}", i + 1)),
        }
        if !scheme.iter().any(|a| a.code.trim() == line.gl_account) {
            problems.push(format!(
                "line {}: GL account {} is not in the chart of accounts (see `yuki accounts scheme`); Yuki would book it to a default account",
                i + 1,
                line.gl_account
            ));
        }
    }
    if !problems.is_empty() {
        return Err(InvoiceError::InvalidInput(format!(
            "cannot book {}:\n  - {}",
            invoice.path.display(),
            problems.join("\n  - ")
        )));
    }
    // The supplier is known now: search the archive by it.
    let docs = archive_candidates(&archive, invoice).await?;
    let matches = archive_matches(invoice, &docs);
    if !matches.is_empty() {
        let listed = matches.join("\n  - ");
        if !allow_archive_match {
            return Err(InvoiceError::InvalidInput(format!(
                "DUPLICATE? The Yuki archive holds what looks like this invoice:\n  - {listed}\nBooking it again would put it in Yuki twice, and a purchase invoice cannot be deleted through the API. Nothing was sent. If it is a different document, book it with --allow-archive-match"
            )));
        }
        eprintln!(
            "warning: booking despite archive matches (--allow-archive-match):\n  - {listed}"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Sending.

/// The `--confirm` of a booking: required when `--yes` skips the prompt,
/// and equal to the reference whenever given.
pub fn check_confirm(
    reference: &str,
    confirm: Option<&str>,
    yes: bool,
) -> Result<(), InvoiceError> {
    match confirm {
        Some(confirm) if confirm.trim() == reference => Ok(()),
        Some(confirm) => Err(InvoiceError::ConfirmationRequired(format!(
            "--confirm {confirm} is not the reference {reference}: nothing was sent to Yuki"
        ))),
        None if yes => Err(InvoiceError::ConfirmationRequired(format!(
            "booking without a prompt needs --confirm <reference>: pass --confirm {reference} to book it; nothing was sent to Yuki"
        ))),
        None => Ok(()),
    }
}

/// Whether `message` reads like Yuki refusing the access key or the client.
fn sounds_unauthorized(message: &str) -> bool {
    let m = message.to_lowercase();
    [
        "auth",
        "client",
        "permission",
        "not allowed",
        "access",
        "rights",
        "denied",
    ]
    .iter()
    .any(|w| m.contains(w))
}

/// [`CLIENT_HINT`] appended to `message` when Yuki was asked with the access
/// key alone and the message reads like a refusal.
fn with_hint(message: String, plain: bool) -> String {
    if plain && sounds_unauthorized(&message) {
        format!("{message}. {CLIENT_HINT}")
    } else {
        message
    }
}

/// A Yuki error, with [`CLIENT_HINT`] when it is an authentication failure
/// under the access key alone.
fn hinted(e: YukiError, plain: bool) -> YukiError {
    match e {
        YukiError::AuthFailed(m) if plain => YukiError::AuthFailed(format!("{m}. {CLIENT_HINT}")),
        YukiError::Unauthorized(code) if plain => {
            YukiError::AuthFailed(format!("HTTP {code}. {CLIENT_HINT}"))
        }
        other => other,
    }
}

/// What Yuki's answer means for the invoice it was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Created as asked; with a note when it is not processed as asked.
    Done(Option<String>),
    /// Not created. `freed`: provably nothing was created.
    Rejected { message: String, freed: bool },
    /// Created under another reference than the one sent.
    ReferenceMismatch(String),
}

/// Judge Yuki's `import` of `invoice`.
pub fn verdict(import: &PurchaseInvoicesImport, invoice: &PurchaseInvoice) -> Verdict {
    if let Some(failure) = import.failure() {
        let freed = import.nothing_created();
        return Verdict::Rejected {
            message: failure,
            freed,
        };
    }
    let mut note = None;
    for booked in &import.invoices {
        if !sync::name_key(booked.reference.trim()).eq(&sync::name_key(&invoice.reference)) {
            let got = if booked.reference.trim().is_empty() {
                "no reference".to_string()
            } else {
                format!("reference {}", booked.reference)
            };
            return Verdict::ReferenceMismatch(format!(
                "REFERENCE MISMATCH: Yuki booked the purchase invoice with {got}, not {}, the reference sent. It is in Yuki: check it there, then settle the ledger with `yuki purchase bookings --resolve {} --as booked`",
                invoice.reference, invoice.reference
            ));
        }
        if invoice.process && !booked.processed {
            note = Some(format!(
                "Yuki created purchase invoice {} but did not process it: process it in Yuki",
                invoice.reference
            ));
        }
    }
    Verdict::Done(note)
}

fn print_import(import: &PurchaseInvoicesImport, invoice: &PurchaseInvoice, format: Option<&str>) {
    let headers: Vec<String> = [
        "Succeeded",
        "Processed",
        "Reference",
        "Subject",
        "File",
        "Message",
    ]
    .map(String::from)
    .to_vec();
    let rows: Vec<Vec<String>> = import
        .invoices
        .iter()
        .map(|i| {
            vec![
                yes_no_unknown(i.succeeded),
                yes_no(i.processed),
                i.reference.clone(),
                i.subject.clone(),
                invoice.pdf.name.clone(),
                i.message.clone(),
            ]
        })
        .collect();
    match OutputFormat::from_flag(format, is_tty()) {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
}

/// The sync record of a PDF being booked.
fn sync_entry(
    state: &SyncState,
    spot: &SyncSpot,
    invoice: &PurchaseInvoice,
    status: SyncStatus,
) -> SyncEntry {
    let what = format!(
        "purchase invoice {} from {}",
        invoice.reference, invoice.supplier.name
    );
    let base = SyncEntry::update(
        state,
        &invoice.hash,
        &spot.rel,
        invoice.pdf.bytes.len() as u64,
        status,
    );
    SyncEntry {
        note: Some(match status {
            SyncStatus::Pending => format!("being booked by `yuki purchase book` as {what}"),
            _ => format!("booked by `yuki purchase book` as {what}"),
        }),
        ..base
    }
}

/// Send the invoice: authenticate (as the API client when one is
/// configured), write ahead (the sync state, locked until the end, and the
/// purchase ledger, locked only to write), call `ProcessPurchaseInvoices`,
/// and settle both by the answer.
async fn submit(
    target: &Target<'_>,
    invoice: &PurchaseInvoice,
    spot: Option<&SyncSpot>,
    api_client: Option<&ApiClient>,
    allow_duplicate: bool,
    format: Option<&str>,
    quiet: bool,
) -> Result<(), InvoiceError> {
    let plain = api_client.is_none();
    let mut client = PurchaseClient::new().with_api_root(target.api_root);
    let auth = match api_client {
        Some(c) => {
            client
                .authenticate_client(&c.id, &c.secret, target.api_key)
                .await
        }
        None => client.authenticate(target.api_key).await,
    };
    auth.map_err(|e| InvoiceError::Yuki(hinted(e, plain)))?;

    // Write ahead. The sync state stays locked until the outcome is
    // recorded, so `upload dir` cannot upload the PDF meanwhile.
    let mut sync_ledger = spot.map(|s| sync::open(&s.root)).transpose()?;
    let sync_ahead = match (&mut sync_ledger, spot) {
        (Some(ledger), Some(spot)) => {
            check_sync(ledger.state(), spot, &invoice.hash)?;
            let entry = sync_entry(ledger.state(), spot, invoice, SyncStatus::Pending);
            let hash = invoice.hash.clone();
            Some(ledger.write_ahead(|s| {
                s.files.insert(hash, entry);
            })?)
        }
        _ => None,
    };
    let claim = invoice.claim();
    if let Err(e) = PurchaseLedger::open(target.admin_id)
        .map_err(InvoiceError::from)
        .and_then(|mut l| l.send(&claim, allow_duplicate))
    {
        if let (Some(ledger), Some(ahead)) = (&mut sync_ledger, sync_ahead) {
            let _ = ledger.undo(ahead);
        }
        return Err(e);
    }

    let result = client
        .process_purchase_invoices(target.admin_id, &invoice.to_xml())
        .await;
    if let Ok(import) = &result
        && !quiet
    {
        print_import(import, invoice, format);
    }
    let reference = &invoice.reference;
    let resolve = format!(
        "check Yuki, then `yuki purchase bookings --resolve {reference} --as booked` (or `--as rejected` if Yuki has nothing)"
    );
    // (purchase ledger status and note, whether the sync record becomes
    // booked (Some(true)), is undone (Some(false)) or stays pending (None),
    // and the run's result)
    let (status, note, sync_booked, outcome): (_, Option<String>, _, Result<(), InvoiceError>) =
        match result {
            Err(e) => match e.delivery() {
                Delivery::NotSent | Delivery::Refused => (
                    LedgerStatus::Rejected,
                    Some(format!("nothing reached Yuki: {e}")),
                    Some(false),
                    Err(InvoiceError::Yuki(hinted(e, plain))),
                ),
                Delivery::Unknown => {
                    let message = with_hint(e.to_string(), plain);
                    (
                        LedgerStatus::Pending,
                        Some(format!("outcome unknown: {message}")),
                        None,
                        Err(InvoiceError::OutcomeUnknown(format!(
                            "{message}: the purchase invoice may already be in Yuki. It stays pending: {resolve}"
                        ))),
                    )
                }
            },
            Ok(import) => match verdict(&import, invoice) {
                Verdict::Done(note) => {
                    if let Some(note) = &note {
                        eprintln!("warning: {note}");
                    }
                    (LedgerStatus::Booked, note, Some(true), Ok(()))
                }
                Verdict::Rejected {
                    message,
                    freed: true,
                } => (
                    LedgerStatus::Rejected,
                    Some(message.clone()),
                    Some(false),
                    Err(InvoiceError::Rejected(with_hint(message, plain))),
                ),
                Verdict::Rejected {
                    message,
                    freed: false,
                } => (
                    LedgerStatus::Pending,
                    Some(message.clone()),
                    None,
                    // Not provably nothing created: as good as no answer.
                    Err(InvoiceError::OutcomeUnknown(format!(
                        "{}. Yuki's answer does not prove nothing was created, so it stays pending: {resolve}",
                        with_hint(message, plain)
                    ))),
                ),
                Verdict::ReferenceMismatch(message) => (
                    LedgerStatus::Pending,
                    Some(message.clone()),
                    None,
                    Err(InvoiceError::ReferenceMismatch(message)),
                ),
            },
        };

    let settled = PurchaseLedger::open(target.admin_id).and_then(|mut l| match status {
        LedgerStatus::Pending => l.note(&invoice.hash, note.as_deref().unwrap_or_default()),
        to => l.settle(&invoice.hash, to, note.as_deref()),
    });
    if let Err(e) = settled {
        eprintln!(
            "warning: could not update the purchase invoice ledger: {e}; purchase invoice {reference} stays pending there: {resolve}"
        );
    }
    if let (Some(ledger), Some(spot), Some(ahead)) = (&mut sync_ledger, spot, sync_ahead) {
        let recorded = match sync_booked {
            Some(true) => {
                let entry = sync_entry(ledger.state(), spot, invoice, SyncStatus::AlreadyInYuki);
                let hash = invoice.hash.clone();
                ledger.commit(ahead, |s| {
                    s.files.insert(hash, entry);
                })
            }
            Some(false) => ledger.undo(ahead),
            // Pending stays, with why: `upload dir` never retries it.
            None => {
                let error = note.clone();
                let hash = invoice.hash.clone();
                ledger.commit(ahead, |s| {
                    if let Some(e) = s.files.get_mut(&hash) {
                        e.error = error;
                    }
                })
            }
        };
        if let Err(e) = recorded {
            eprintln!(
                "warning: could not update {}: {e}; record the PDF by hand with `yuki upload mark`",
                spot.root.join(sync::STATE_FILE).display()
            );
        }
    }
    if status == LedgerStatus::Pending {
        eprintln!("purchase invoice {reference} stays pending in the ledger: {resolve}");
    }
    outcome
}

/// `purchase book`.
pub async fn book(
    load_config: impl FnOnce() -> Result<Config, YukiError>,
    admin: Option<&str>,
    args: BookArgs,
    yes: bool,
    quiet: bool,
    format: Option<&str>,
) -> Result<(), InvoiceError> {
    let mut invoice = build(&args)?;
    let config = load_config().map_err(InvoiceError::input)?;
    let target = config.target(admin).map_err(InvoiceError::input)?;
    // Read before any API call: a half-configured client stops the run.
    let api_client = config
        .api_client(admin, |v| std::env::var(v).ok())
        .map_err(InvoiceError::input)?;

    // Everything local that may already have put the PDF in Yuki.
    PurchaseLedger::peek(target.admin_id)?.check_free(&invoice.claim(), args.allow_duplicate)?;
    let spot = sync_spot(&invoice.path)?;
    if let Some(spot) = &spot {
        check_sync(sync::peek(&spot.root)?.state(), spot, &invoice.hash)?;
    }
    // Refuse an unconfirmable booking before reading Yuki.
    if !args.dry_run {
        check_confirm(&invoice.reference, args.confirm.as_deref(), yes)?;
        if !yes && !crate::cli::can_prompt() {
            return Err(InvoiceError::ConfirmationRequired(format!(
                "purchase book writes to Yuki; pass --yes --confirm {} to confirm in non-interactive mode, or --dry-run to preview only",
                invoice.reference
            )));
        }
    }
    verify(&config, admin, &mut invoice, args.allow_archive_match).await?;

    if args.dry_run {
        if !quiet {
            eprintln!("{}\n", invoice.preview(target.config_name));
            eprintln!("Dry run: nothing was sent to Yuki (the checks only read). xmlDoc:");
        }
        println!("{}", invoice.to_display_xml());
        return Ok(());
    }
    if !(quiet && yes) {
        eprintln!("{}\n", invoice.preview(target.config_name));
    } else {
        eprintln!("{}", invoice.booking_line());
    }
    if !yes && !crate::cli::ask_yes_no(&invoice.question()) {
        return Err(InvoiceError::ConfirmationRequired(
            "not confirmed: nothing was sent to Yuki".into(),
        ));
    }
    submit(
        &target,
        &invoice,
        spot.as_ref(),
        api_client.as_ref(),
        args.allow_duplicate,
        format,
        quiet,
    )
    .await
}

/// `purchase bookings`: list the selected administration's purchase
/// invoices from the ledger, after settling a pending one by hand when
/// `resolve` is given (and its `.yuki-sync.json` record with it).
pub fn bookings(
    config: &Config,
    admin: Option<&str>,
    resolve: Option<(&str, Resolution)>,
    contact: Option<&str>,
    format: Option<&str>,
) -> Result<(), InvoiceError> {
    let admin_id = config.target(admin)?.admin_id;
    if let Some((reference, resolution)) = resolve {
        // The purchase ledger's lock is released before the sync state's
        // is waited for.
        let entry = PurchaseLedger::open(admin_id)?.resolve(reference, contact, resolution)?;
        settle_sync_by_hand(&entry.file, &entry.hash, resolution);
    }
    let ledger = PurchaseLedger::peek(admin_id)?;
    let headers: Vec<String> = [
        "Reference",
        "Contact",
        "Date",
        "Total",
        "File",
        "Status",
        "Recorded",
        "Booked",
        "Note",
    ]
    .map(String::from)
    .to_vec();
    let rows: Vec<Vec<String>> = ledger
        .entries()
        .map(|e| {
            vec![
                e.reference.clone(),
                e.contact.clone(),
                e.date.clone(),
                e.total.clone(),
                e.file.clone(),
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

/// After `--resolve`, settle the pending sync record `purchase book` wrote
/// for the PDF, if the file is still governed by a sync state: in Yuki when
/// booked, dropped when not (so `upload dir` may upload it). Problems are
/// warnings: the purchase ledger is settled either way.
fn settle_sync_by_hand(file: &str, hash: &str, resolution: Resolution) {
    let warn = |e: &dyn std::fmt::Display| {
        eprintln!(
            "warning: the upload dir sync state was not updated: {e}; use `yuki upload mark {file}`"
        );
    };
    let spot = match sync_spot(Path::new(file)) {
        Ok(Some(spot)) => spot,
        Ok(None) => return,
        Err(e) => return warn(&e),
    };
    let result = (|| -> Result<(), YukiError> {
        let mut ledger: Ledger<SyncState> = sync::open_within(&spot.root, SYNC_PATIENCE)?;
        let note = "booked by `yuki purchase book` (resolved by hand)";
        let status = ledger.state().files.get(hash).map(|e| e.status);
        match (status, resolution) {
            (Some(SyncStatus::Pending), Resolution::Booked) => {
                let entry = ledger.state_mut().files.get_mut(hash).expect("present");
                entry.status = SyncStatus::AlreadyInYuki;
                entry.error = None;
                entry.recorded_at = sync::now_utc();
                entry.note = Some(note.into());
            }
            (Some(SyncStatus::Pending), Resolution::Rejected) => {
                ledger.state_mut().files.remove(hash);
            }
            // No record (the PDF moved into a synced directory since, or the
            // write-ahead was lost): booked, it is in Yuki.
            (None | Some(SyncStatus::Failed), Resolution::Booked) => {
                let Ok(bytes) = std::fs::read(file) else {
                    return Ok(());
                };
                if sync::sha256_hex(&bytes) != hash {
                    return Ok(());
                }
                let mut entry = SyncEntry::update(
                    ledger.state(),
                    hash,
                    &spot.rel,
                    bytes.len() as u64,
                    SyncStatus::AlreadyInYuki,
                );
                entry.note = Some(note.into());
                ledger.state_mut().files.insert(hash.to_string(), entry);
            }
            _ => return Ok(()),
        }
        ledger.save()
    })();
    if let Err(e) = result {
        warn(&e);
    }
}

#[cfg(test)]
#[path = "purchase_invoice_tests.rs"]
mod tests;
