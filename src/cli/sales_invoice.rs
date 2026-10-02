//! `yuki sales invoice`: create a sales invoice in Yuki from a TOML file or a
//! saved per-customer template, as a draft unless `--send` books and sends it.
//!
//! The invoice is read and validated, totalled in exact cents, and turned into
//! the `xmlDoc` of `ProcessSalesInvoices`, whose element order follows Yuki's
//! `SalesInvoices.xsd`. `main` shows the preview and asks for confirmation;
//! [`submit`] is the only step that contacts Yuki.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use serde::Deserialize;

use crate::client::sales::{SalesClient, SalesInvoicesImport};
use crate::config::Config;
use crate::error::YukiError;
use crate::money::{Cents, div_round, format_scaled, parse_scaled};
use crate::output::{OutputFormat, format_json, format_table, is_tty};
use crate::period::{date_from_epoch_days, epoch_days, today};

/// Namespace of the `SalesInvoices` document; Yuki rejects one without it.
pub const NAMESPACE: &str = "urn:xmlns:http://www.theyukicompany.com:salesinvoices";
const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
/// `ProductQuantity` carries up to four decimals.
const QTY_DECIMALS: u32 = 4;
/// `VATPercentage` carries up to two decimals.
const PCT_DECIMALS: u32 = 2;
/// `Notes` and `PaymentMethod` length limits from the XSD.
const NOTES_MAX: usize = 500;
const PAYMENT_METHOD_MAX: usize = 60;
/// Ten integer digits: the XSD's limit on `ProductQuantity` and `SalesPrice`
/// (totalDigits 14, fractionDigits 4) and on a line amount (12, 2).
const QTY_MAX: i64 = 10_i64.pow(10 + QTY_DECIMALS);
const PRICE_MAX: i64 = 10_i64.pow(12);
const LINE_AMOUNT_MAX: i128 = 10_i128.pow(12);
/// The largest custom PDF accepted. Yuki documents no limit; this keeps the
/// request (the PDF grows by a third as base64) to a size a SOAP call carries.
pub const PDF_MAX_BYTES: usize = 10 * 1024 * 1024;
/// The longest payment term accepted, in days.
const DUE_DAYS_MAX: i64 = 3_650;

/// How a created invoice leaves Yuki. Without one, the invoice is a draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SendMode {
    /// Book it and email it to the customer.
    Email,
    /// Book it and send it over Peppol.
    Peppol,
    /// Book it, email it and send it over Peppol.
    Both,
}

impl SendMode {
    fn email(self) -> bool {
        matches!(self, Self::Email | Self::Both)
    }

    fn peppol(self) -> bool {
        matches!(self, Self::Peppol | Self::Both)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Email => "by email",
            Self::Peppol => "over Peppol",
            Self::Both => "by email and over Peppol",
        }
    }
}

/// A quantity in ten-thousandths, as exact as `ProductQuantity` allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quantity(i64);

/// Parse a non-zero quantity with up to four decimals (`--qty`, `qty`).
pub fn parse_quantity(text: &str) -> Result<Quantity, String> {
    match parse_scaled(text, QTY_DECIMALS)? {
        0 => Err("a quantity cannot be zero".into()),
        q if q.abs() >= QTY_MAX => Err(format!("'{text}' exceeds Yuki's 10 integer digits")),
        q => Ok(Quantity(q)),
    }
}

/// Parse a unit price with up to two decimals (`--price`, `price`).
pub fn parse_price(text: &str) -> Result<Cents, String> {
    match Cents::parse_exact(text)? {
        price if price.0.abs() >= PRICE_MAX => {
            Err(format!("'{text}' exceeds Yuki's 10 integer digits"))
        }
        price => Ok(price),
    }
}

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

/// Where the invoice description comes from.
#[derive(Debug, Clone)]
pub enum Source {
    File(PathBuf),
    Template(String),
}

/// Command-line values that replace the file's.
#[derive(Debug, Default, Clone, Copy)]
pub struct Overrides<'a> {
    /// Quantity of the invoice's only line.
    pub qty: Option<Quantity>,
    /// Unit price of the invoice's only line.
    pub price: Option<Cents>,
    pub date: Option<&'a str>,
    pub subject: Option<&'a str>,
    /// A custom PDF, replacing the file's `pdf`.
    pub pdf: Option<&'a Path>,
}

// ---------------------------------------------------------------------------
// The file format, as written.

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InvoiceSpec {
    subject: Option<String>,
    date: Option<toml::Value>,
    due_date: Option<toml::Value>,
    due_days: Option<i64>,
    layout: Option<String>,
    currency: Option<String>,
    payment_method: Option<String>,
    remarks: Option<String>,
    notes: Option<String>,
    pdf: Option<String>,
    contact: Option<ContactSpec>,
    #[serde(default)]
    lines: Vec<LineSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContactSpec {
    code: Option<String>,
    name: Option<String>,
    country: Option<String>,
    address: Option<String>,
    address_2: Option<String>,
    zipcode: Option<String>,
    city: Option<String>,
    vat_number: Option<String>,
    email: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LineSpec {
    description: Option<String>,
    qty: Option<toml::Value>,
    price: Option<toml::Value>,
    vat_percentage: Option<toml::Value>,
    vat_type: Option<i64>,
    vat_description: Option<String>,
    gl_account: Option<String>,
    product_code: Option<String>,
}

// ---------------------------------------------------------------------------
// The validated invoice.

/// The customer: an existing Yuki contact by `code`, or the details Yuki
/// needs to match or create one by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contact {
    pub code: Option<String>,
    pub name: Option<String>,
    /// ISO 3166-1 alpha-2, upper case.
    pub country: Option<String>,
    pub address: Option<String>,
    pub address_2: Option<String>,
    pub zipcode: Option<String>,
    pub city: Option<String>,
    pub vat_number: Option<String>,
    pub email: Option<String>,
    /// `Company` or `Person`, as the XSD spells them.
    pub kind: Option<&'static str>,
}

impl Contact {
    /// One line naming the customer, for the preview and the template list.
    pub fn label(&self) -> String {
        match (&self.name, &self.code) {
            (Some(name), Some(code)) => format!("{name} (code {code})"),
            (Some(name), None) => name.clone(),
            (None, Some(code)) => format!("code {code}"),
            (None, None) => String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub description: String,
    pub qty: Quantity,
    /// Unit price excluding VAT.
    pub price: Cents,
    /// VAT percentage in hundredths: 21% is 2100.
    pub vat_percentage: i64,
    /// Yuki's VAT type, as listed under Settings > VAT rates.
    pub vat_type: i64,
    pub vat_description: Option<String>,
    pub gl_account: Option<String>,
    /// Item number of a Yuki sales item (`Product/Reference`).
    pub product_code: Option<String>,
}

impl Line {
    /// `qty × price`, rounded half away from zero to the cent.
    pub fn net(&self) -> Cents {
        // Validation bounds the amount to LINE_AMOUNT_MAX, so it fits.
        Cents(self.net_exact() as i64)
    }

    fn net_exact(&self) -> i128 {
        let exact = i128::from(self.qty.0) * i128::from(self.price.0);
        div_round(exact, 10_i128.pow(QTY_DECIMALS))
    }
}

/// VAT at one percentage: the net it applies to and the VAT on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VatRate {
    pub percentage: i64,
    pub base: Cents,
    pub vat: Cents,
}

/// A custom invoice PDF, read and checked when the invoice is loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pdf {
    /// File name sent as `DocumentFileName`.
    pub name: String,
    pub bytes: Vec<u8>,
}

impl Pdf {
    /// Read `path`, which must be a PDF of at most [`PDF_MAX_BYTES`].
    fn read(path: &Path) -> Result<Self, String> {
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

/// `2048` → `2.0 KB`, for the preview.
fn human_size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} bytes"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invoice {
    /// Where it came from, e.g. `template "acme"` or `invoice.toml`.
    pub origin: String,
    pub subject: Option<String>,
    pub date: String,
    pub due_date: Option<String>,
    pub layout: Option<String>,
    pub currency: Option<String>,
    pub payment_method: Option<String>,
    pub remarks: Option<String>,
    pub notes: Option<String>,
    /// Stored in Yuki instead of the invoice Yuki would generate.
    pub pdf: Option<Pdf>,
    pub contact: Contact,
    pub lines: Vec<Line>,
    /// `None` is a draft.
    pub send: Option<SendMode>,
}

impl Invoice {
    pub fn net(&self) -> Cents {
        self.lines.iter().map(Line::net).sum()
    }

    /// VAT per percentage, each on the summed net of its lines and rounded
    /// once, as an invoice states it. Yuki computes its own; this is the
    /// figure to approve, not a guarantee to the cent.
    pub fn vat_rates(&self) -> Vec<VatRate> {
        let mut bases: BTreeMap<i64, Cents> = BTreeMap::new();
        for line in &self.lines {
            let base = bases.entry(line.vat_percentage).or_default();
            *base = *base + line.net();
        }
        bases
            .into_iter()
            .map(|(percentage, base)| {
                let vat = div_round(
                    i128::from(base.0) * i128::from(percentage),
                    100 * 10_i128.pow(PCT_DECIMALS),
                );
                VatRate {
                    percentage,
                    base,
                    vat: Cents(vat as i64),
                }
            })
            .collect()
    }

    pub fn vat(&self) -> Cents {
        self.vat_rates().iter().map(|r| r.vat).sum()
    }

    pub fn gross(&self) -> Cents {
        self.net() + self.vat()
    }

    /// The yes/no question the confirmation prompt asks.
    pub fn question(&self) -> String {
        match self.send {
            None => "Create this draft invoice in Yuki?".into(),
            Some(mode) => format!("Book this invoice in Yuki and send it {}?", mode.label()),
        }
    }

    /// What will happen, for humans: customer, lines, totals and send mode.
    pub fn preview(&self, administration: Option<&str>) -> String {
        let mut out = format!("Sales invoice from {}\n", self.origin);
        let mut row = |label: &str, value: &str| {
            let _ = writeln!(out, "  {label:<15}{value}");
        };
        if let Some(admin) = administration {
            row("Administration", admin);
        }
        let mode = match self.send {
            None => {
                "DRAFT (Process=false): lands in \"To be sent\" in Yuki; nothing is booked or sent"
                    .to_string()
            }
            Some(mode) => format!(
                "BOOK AND SEND {} (Process=true, EmailToCustomer={}, SentToPeppol={}): books the invoice, Yuki numbers it and sends it",
                mode.label().to_uppercase(),
                mode.email(),
                mode.peppol()
            ),
        };
        row("Mode", &mode);
        row("Customer", &self.contact.label());
        let c = &self.contact;
        let details: Vec<&str> = [
            &c.address,
            &c.address_2,
            &c.zipcode,
            &c.city,
            &c.country,
            &c.vat_number,
            &c.email,
        ]
        .into_iter()
        .filter_map(|v| v.as_deref())
        .collect();
        if !details.is_empty() {
            row("", &details.join(", "));
        }
        if c.code.is_none() {
            row(
                "",
                "no contact code: Yuki matches by name and address, or creates the contact",
            );
        }
        if let Some(subject) = &self.subject {
            row("Subject", subject);
        }
        let due = self
            .due_date
            .as_deref()
            .map_or("Yuki's default term".to_string(), str::to_string);
        row("Date", &format!("{}, due {due}", self.date));
        for (label, value) in [
            ("Payment method", &self.payment_method),
            ("Layout", &self.layout),
            ("Notes", &self.notes),
            ("Remarks", &self.remarks),
        ] {
            if let Some(value) = value {
                row(label, value);
            }
        }
        if let Some(pdf) = &self.pdf {
            row(
                "PDF",
                &format!(
                    "custom PDF {} ({}) replaces Yuki's layout; amounts in the PDF must match the lines",
                    pdf.name,
                    human_size(pdf.bytes.len() as u64)
                ),
            );
        }

        let headers: Vec<String> = [
            "Description",
            "Qty",
            "Price",
            "Net",
            "VAT %",
            "VAT type",
            "GL",
        ]
        .map(String::from)
        .to_vec();
        let rows: Vec<Vec<String>> = self
            .lines
            .iter()
            .map(|l| {
                vec![
                    l.description.clone(),
                    format_scaled(l.qty.0, QTY_DECIMALS),
                    l.price.to_string(),
                    l.net().to_string(),
                    format_scaled(l.vat_percentage, PCT_DECIMALS),
                    l.vat_type.to_string(),
                    l.gl_account.clone().unwrap_or_default(),
                ]
            })
            .collect();
        let _ = writeln!(out, "{}", format_table(&headers, &rows));

        let currency = self.currency.as_deref().unwrap_or("EUR");
        let net = self.net();
        let rates = self.vat_rates();
        let gross = net + rates.iter().map(|r| r.vat).sum();
        let _ = writeln!(out, "  {:<15}{:>12} {currency}", "Net", net.to_string());
        for rate in &rates {
            let _ = writeln!(
                out,
                "  {:<15}{:>12} {currency}  ({}% on {})",
                "VAT",
                rate.vat.to_string(),
                format_scaled(rate.percentage, PCT_DECIMALS),
                rate.base
            );
        }
        let _ = writeln!(
            out,
            "  {:<15}{:>12} {currency}",
            "Gross total",
            gross.to_string()
        );
        let default = if self.currency.is_some() {
            ""
        } else {
            "; no currency given, so Yuki's default (EUR)"
        };
        let _ = write!(
            out,
            "  (VAT computed per rate; Yuki books its own figures{default})"
        );
        out
    }

    /// The `xmlDoc` of `ProcessSalesInvoices`: one `SalesInvoice` in the
    /// element order `SalesInvoices.xsd` requires, every value escaped, and
    /// empty optional elements left out (several must not be empty).
    pub fn to_xml(&self) -> String {
        self.render(true)
    }

    /// [`to_xml`](Self::to_xml) for a human: a custom PDF's base64 is
    /// replaced by a comment giving its size.
    pub fn to_display_xml(&self) -> String {
        self.render(false)
    }

    fn render(&self, embed_pdf: bool) -> String {
        let mut x = XmlWriter::default();
        let _ = writeln!(
            x.out,
            r#"<SalesInvoices xmlns="{NAMESPACE}" xmlns:xsi="{XSI}">"#
        );
        x.depth = 1;
        x.open("SalesInvoice");
        // No Reference: Yuki numbers the invoice.
        x.opt("Subject", &self.subject);
        x.opt("PaymentMethod", &self.payment_method);
        let send = self.send;
        x.leaf("Process", bool_text(send.is_some()));
        x.leaf(
            "EmailToCustomer",
            bool_text(send.is_some_and(SendMode::email)),
        );
        x.leaf(
            "SentToPeppol",
            bool_text(send.is_some_and(SendMode::peppol)),
        );
        x.opt("Layout", &self.layout);
        x.opt("Notes", &self.notes);
        x.leaf("Date", &self.date);
        x.opt("DueDate", &self.due_date);
        x.opt("Currency", &self.currency);
        x.opt("Remarks", &self.remarks);
        if let Some(pdf) = &self.pdf {
            x.leaf("DocumentFileName", &pdf.name);
            let encoded = BASE64.encode(&pdf.bytes);
            if embed_pdf {
                x.leaf("DocumentBase64", &encoded);
            } else {
                x.indent();
                let _ = writeln!(
                    x.out,
                    "<DocumentBase64><!-- {} bytes base64 ({}-byte PDF) --></DocumentBase64>",
                    encoded.len(),
                    pdf.bytes.len()
                );
            }
        }

        let c = &self.contact;
        x.open("Contact");
        x.opt("ContactCode", &c.code);
        x.opt("FullName", &c.name);
        x.opt("CountryCode", &c.country);
        x.opt("City", &c.city);
        x.opt("Zipcode", &c.zipcode);
        x.opt("AddressLine_1", &c.address);
        x.opt("AddressLine_2", &c.address_2);
        x.opt("EmailAddress", &c.email);
        x.opt("VATNumber", &c.vat_number);
        if let Some(kind) = c.kind {
            x.leaf("ContactType", kind);
        }
        x.close("Contact");

        x.open("InvoiceLines");
        for line in &self.lines {
            x.open("InvoiceLine");
            x.leaf("Description", &line.description);
            x.leaf("ProductQuantity", &format_scaled(line.qty.0, QTY_DECIMALS));
            x.open("Product");
            x.leaf("Description", &line.description);
            x.opt("Reference", &line.product_code);
            x.leaf("SalesPrice", &line.price.to_string());
            x.leaf(
                "VATPercentage",
                &format_scaled(line.vat_percentage, PCT_DECIMALS),
            );
            // Prices are always stated excluding VAT.
            x.leaf("VATIncluded", "false");
            x.leaf("VATType", &line.vat_type.to_string());
            x.opt("VATDescription", &line.vat_description);
            x.opt("GLAccountCode", &line.gl_account);
            x.close("Product");
            x.close("InvoiceLine");
        }
        x.close("InvoiceLines");
        x.close("SalesInvoice");
        x.out.push_str("</SalesInvoices>");
        x.out
    }
}

fn bool_text(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

/// Escape text for an XML element.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            ch => out.push(ch),
        }
    }
    out
}

/// An indented XML writer for the few element shapes the document needs.
#[derive(Default)]
struct XmlWriter {
    out: String,
    depth: usize,
}

impl XmlWriter {
    fn indent(&mut self) {
        self.out.push_str(&"  ".repeat(self.depth));
    }

    fn open(&mut self, name: &str) {
        self.indent();
        let _ = writeln!(self.out, "<{name}>");
        self.depth += 1;
    }

    fn close(&mut self, name: &str) {
        self.depth -= 1;
        self.indent();
        let _ = writeln!(self.out, "</{name}>");
    }

    fn leaf(&mut self, name: &str, value: &str) {
        self.indent();
        let _ = writeln!(self.out, "<{name}>{}</{name}>", escape(value));
    }

    fn opt(&mut self, name: &str, value: &Option<String>) {
        if let Some(value) = value {
            self.leaf(name, value);
        }
    }
}

// ---------------------------------------------------------------------------
// Reading and validating.

/// The directory saved templates live in: `invoices/` next to the config.
pub fn templates_dir() -> PathBuf {
    let config = Config::default_path();
    config
        .parent()
        .map_or_else(|| PathBuf::from("invoices"), |dir| dir.join("invoices"))
}

/// The file of template `name`; a name is letters, digits, `-` and `_`.
pub fn template_path(name: &str) -> Result<PathBuf, YukiError> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        return Err(YukiError::Config(format!(
            "invalid template name '{name}': use letters, digits, '-' and '_'"
        )));
    }
    Ok(templates_dir().join(format!("{name}.toml")))
}

/// Read, override and validate the invoice from `source`.
pub fn load(
    source: &Source,
    overrides: &Overrides<'_>,
    send: Option<SendMode>,
) -> Result<Invoice, YukiError> {
    let (path, origin) = match source {
        Source::File(path) => (path.clone(), path.display().to_string()),
        Source::Template(name) => (template_path(name)?, format!("template \"{name}\"")),
    };
    let text = std::fs::read_to_string(&path).map_err(|e| {
        let hint = match source {
            Source::Template(_) if e.kind() == std::io::ErrorKind::NotFound => {
                " (see `yuki sales invoice templates`)"
            }
            _ => "",
        };
        YukiError::Config(format!("{}: {e}{hint}", path.display()))
    })?;
    let base = path.parent().unwrap_or(Path::new("."));
    parse_at(&text, &origin, base, overrides, send)
}

/// Parse and validate invoice TOML; `origin` names it in errors, and a
/// relative `pdf` is read from the working directory.
pub fn parse(
    text: &str,
    origin: &str,
    overrides: &Overrides<'_>,
    send: Option<SendMode>,
) -> Result<Invoice, YukiError> {
    parse_at(text, origin, Path::new("."), overrides, send)
}

/// [`parse`], with a relative `pdf` resolved against `base`, the directory
/// of the invoice file.
fn parse_at(
    text: &str,
    origin: &str,
    base: &Path,
    overrides: &Overrides<'_>,
    send: Option<SendMode>,
) -> Result<Invoice, YukiError> {
    let spec: InvoiceSpec = toml::from_str(text)
        .map_err(|e| YukiError::Config(format!("invalid invoice {origin}: {e}")))?;
    validate(spec, origin, base, overrides, send).map_err(|problems| {
        YukiError::Config(format!(
            "invalid invoice {origin}:\n  - {}",
            problems.join("\n  - ")
        ))
    })
}

/// Collects every problem, so one run reports them all.
#[derive(Default)]
struct Problems(Vec<String>);

impl Problems {
    fn push(&mut self, problem: impl Into<String>) {
        self.0.push(problem.into());
    }

    /// Trimmed text, `None` when empty; characters XML 1.0 cannot carry are a problem.
    fn text(&mut self, field: &str, value: Option<String>) -> Option<String> {
        let value = value?.trim().to_string();
        if value.is_empty() {
            return None;
        }
        if value.chars().any(|c| {
            (c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
                || matches!(c, '\u{FFFE}' | '\u{FFFF}')
        }) {
            self.push(format!("{field} contains a character XML cannot carry"));
        }
        Some(value)
    }

    /// Decimal text of a TOML number or string.
    fn number(&mut self, field: &str, value: Option<toml::Value>) -> Option<String> {
        match value? {
            toml::Value::Integer(i) => Some(i.to_string()),
            // Display is the shortest text that reads back as the same f64, so
            // `0.1` stays "0.1" rather than its binary expansion.
            toml::Value::Float(f) if f.is_finite() => Some(f.to_string()),
            toml::Value::String(s) => Some(s),
            other => {
                self.push(format!(
                    "{field} must be a number, not {}",
                    other.type_str()
                ));
                None
            }
        }
    }

    /// A date given as a TOML date (`2026-10-01`) or a string.
    fn date(&mut self, field: &str, value: Option<toml::Value>) -> Option<String> {
        let text = match value? {
            toml::Value::String(s) => s,
            toml::Value::Datetime(d) => d.to_string(),
            other => {
                self.push(format!("{field} must be a date, not {}", other.type_str()));
                return None;
            }
        };
        parse_date(&text)
            .map_err(|e| self.push(format!("{field}: {e}")))
            .ok()
    }
}

fn validate(
    spec: InvoiceSpec,
    origin: &str,
    base: &Path,
    overrides: &Overrides<'_>,
    send: Option<SendMode>,
) -> Result<Invoice, Vec<String>> {
    let mut p = Problems::default();

    let subject = match overrides.subject {
        Some(subject) => {
            let subject = p.text("--subject", Some(subject.to_string()));
            if subject.is_none() {
                p.push("--subject cannot be empty");
            }
            subject
        }
        None => p.text("subject", spec.subject),
    };
    let date = match overrides.date {
        Some(date) => parse_date(date)
            .map_err(|e| p.push(format!("--date: {e}")))
            .ok(),
        None => p.date("date", spec.date),
    }
    .unwrap_or_else(today);
    let due_date = match (p.date("due_date", spec.due_date), spec.due_days) {
        (Some(_), Some(_)) => {
            p.push("give due_date or due_days, not both");
            None
        }
        (Some(due), None) => {
            if due < date {
                p.push(format!("due_date {due} is before the invoice date {date}"));
            }
            Some(due)
        }
        (None, Some(days)) if !(0..=DUE_DAYS_MAX).contains(&days) => {
            p.push(format!("due_days must be between 0 and {DUE_DAYS_MAX}"));
            None
        }
        (None, Some(days)) => epoch_days(&date).map(|d| date_from_epoch_days(d + days)),
        (None, None) => None,
    };
    let layout = p.text("layout", spec.layout);
    let currency = p.text("currency", spec.currency).map(|c| c.to_uppercase());
    if let Some(c) = &currency
        && !(c.len() == 3 && c.chars().all(|ch| ch.is_ascii_alphabetic()))
    {
        p.push(format!(
            "currency '{c}' is not an ISO 4217 code such as EUR"
        ));
    }
    let payment_method = p.text("payment_method", spec.payment_method);
    if payment_method
        .as_ref()
        .is_some_and(|m| m.chars().count() > PAYMENT_METHOD_MAX)
    {
        p.push(format!(
            "payment_method is longer than {PAYMENT_METHOD_MAX} characters"
        ));
    }
    let remarks = p.text("remarks", spec.remarks);
    let notes = p.text("notes", spec.notes);
    if notes
        .as_ref()
        .is_some_and(|n| n.chars().count() > NOTES_MAX)
    {
        p.push(format!("notes is longer than {NOTES_MAX} characters"));
    }

    let contact = match spec.contact {
        None => {
            p.push("[contact] is missing: give `code` for an existing Yuki contact, or `name` and `country`");
            Contact::default()
        }
        Some(spec) => validate_contact(spec, &mut p),
    };

    if spec.lines.is_empty() {
        p.push("no [[lines]]: an invoice needs at least one line");
    }
    let single = spec.lines.len() == 1;
    for (flag, given) in [
        ("--qty", overrides.qty.is_some()),
        ("--price", overrides.price.is_some()),
    ] {
        if given && !single {
            p.push(format!(
                "{flag} applies to a single-line invoice; this one has {} lines",
                spec.lines.len()
            ));
        }
    }
    let lines: Vec<Line> = spec
        .lines
        .into_iter()
        .enumerate()
        .filter_map(|(i, line)| validate_line(line, i + 1, overrides, &mut p))
        .collect();

    let net: i128 = lines.iter().map(|l| i128::from(l.net().0)).sum();
    if !lines.is_empty() && net <= 0 {
        p.push(format!(
            "the invoice total is {}: credit notes and zero invoices are not supported",
            Cents(net as i64)
        ));
    }
    if send.is_some_and(SendMode::email) && contact.code.is_none() && contact.email.is_none() {
        p.push("--send email needs contact.email for a contact without a code");
    }

    let pdf_path = match overrides.pdf {
        Some(path) => Some(path.to_path_buf()),
        None => p.text("pdf", spec.pdf).map(|pdf| base.join(pdf)),
    };
    let pdf = pdf_path.and_then(|path| {
        Pdf::read(&path)
            .map_err(|e| p.push(format!("pdf: {e}")))
            .ok()
    });

    if !p.0.is_empty() {
        return Err(p.0);
    }
    Ok(Invoice {
        origin: origin.to_string(),
        subject,
        date,
        due_date,
        layout,
        currency,
        payment_method,
        remarks,
        notes,
        pdf,
        contact,
        lines,
        send,
    })
}

fn validate_contact(spec: ContactSpec, p: &mut Problems) -> Contact {
    let code = p.text("contact.code", spec.code);
    let name = p.text("contact.name", spec.name);
    let country = p
        .text("contact.country", spec.country)
        .map(|c| c.to_uppercase());
    if let Some(c) = &country
        && !(c.len() == 2 && c.chars().all(|ch| ch.is_ascii_alphabetic()))
    {
        p.push(format!(
            "contact.country '{c}' is not an ISO 3166-1 alpha-2 code such as BE"
        ));
    }
    if code.is_none() {
        match (&name, &country) {
            (None, _) => p.push(
                "contact needs `code` (an existing Yuki contact) or `name` and `country` (a new one)",
            ),
            (Some(_), None) => p.push(
                "contact.country is required for a contact without a code (ISO 3166-1 alpha-2, e.g. BE)",
            ),
            _ => {}
        }
    }
    let kind = match p.text("contact.type", spec.kind) {
        None => None,
        Some(kind) => match kind.to_ascii_lowercase().as_str() {
            "company" => Some("Company"),
            "person" => Some("Person"),
            _ => {
                p.push(format!(
                    "contact.type '{kind}' must be \"company\" or \"person\""
                ));
                None
            }
        },
    };
    Contact {
        code,
        name,
        country,
        address: p.text("contact.address", spec.address),
        address_2: p.text("contact.address_2", spec.address_2),
        zipcode: p.text("contact.zipcode", spec.zipcode),
        city: p.text("contact.city", spec.city),
        vat_number: p.text("contact.vat_number", spec.vat_number),
        email: p.text("contact.email", spec.email),
        kind,
    }
}

fn validate_line(
    spec: LineSpec,
    n: usize,
    overrides: &Overrides<'_>,
    p: &mut Problems,
) -> Option<Line> {
    let field = |name: &str| format!("lines[{n}].{name}");
    let description = p.text(&field("description"), spec.description);
    if description.is_none() {
        p.push(format!("{} is required", field("description")));
    }
    let qty = match overrides.qty {
        Some(qty) => Some(qty),
        None => match p.number(&field("qty"), spec.qty) {
            None => Some(Quantity(10_i64.pow(QTY_DECIMALS))),
            Some(text) => parse_quantity(&text)
                .map_err(|e| p.push(format!("{}: {e}", field("qty"))))
                .ok(),
        },
    };
    let price = match overrides.price {
        Some(price) => Some(price),
        None => match p.number(&field("price"), spec.price) {
            None => {
                p.push(format!(
                    "{} is required (unit price excluding VAT)",
                    field("price")
                ));
                None
            }
            Some(text) => parse_price(&text)
                .map_err(|e| p.push(format!("{}: {e}", field("price"))))
                .ok(),
        },
    };
    let vat_percentage = match p.number(&field("vat_percentage"), spec.vat_percentage) {
        None => {
            p.push(format!("{} is required, e.g. 21", field("vat_percentage")));
            None
        }
        Some(text) => match parse_scaled(&text, PCT_DECIMALS) {
            Ok(pct) if (0..=100 * 10_i64.pow(PCT_DECIMALS)).contains(&pct) => Some(pct),
            Ok(_) => {
                p.push(format!(
                    "{} must be between 0 and 100",
                    field("vat_percentage")
                ));
                None
            }
            Err(e) => {
                p.push(format!("{}: {e}", field("vat_percentage")));
                None
            }
        },
    };
    if spec.vat_type.is_none() {
        p.push(format!(
            "{} is required: Yuki's VAT type number (Settings > VAT rates)",
            field("vat_type")
        ));
    }
    let vat_description = p.text(&field("vat_description"), spec.vat_description);
    let gl_account = p.text(&field("gl_account"), spec.gl_account);
    let product_code = p.text(&field("product_code"), spec.product_code);
    let line = Line {
        description: description?,
        qty: qty?,
        price: price?,
        vat_percentage: vat_percentage?,
        vat_type: spec.vat_type?,
        vat_description,
        gl_account,
        product_code,
    };
    if line.price == Cents::ZERO {
        p.push(format!(
            "lines[{n}].price cannot be 0: Yuki would bill the catalogue price instead"
        ));
        return None;
    }
    if line.net_exact().abs() >= LINE_AMOUNT_MAX {
        p.push(format!(
            "lines[{n}]: qty × price exceeds Yuki's 10 integer digits for a line amount"
        ));
        return None;
    }
    Some(line)
}

// ---------------------------------------------------------------------------
// Confirmation and the API call.

/// Whether a confirmation prompt can be asked and answered.
pub fn can_prompt() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Ask `question` on stderr and read the answer from stdin.
pub fn confirm(question: &str) -> Result<bool, YukiError> {
    ask(
        question,
        &mut std::io::stdin().lock(),
        &mut std::io::stderr(),
    )
}

/// Ask `question`; only `y` or `yes` confirms, so an empty line or end of
/// input declines.
fn ask(
    question: &str,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<bool, YukiError> {
    let _ = write!(output, "{question} [y/N] ");
    let _ = output.flush();
    let mut answer = String::new();
    input
        .read_line(&mut answer)
        .map_err(|e| YukiError::Config(format!("cannot read the answer: {e}")))?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// Send the invoice to Yuki and print Yuki's answer per invoice.
///
/// Returns the import so the caller can fail the run when an invoice was
/// not accepted; the rows are printed either way.
pub async fn submit(
    config: &Config,
    admin: Option<&str>,
    invoice: &Invoice,
    format: Option<&str>,
    quiet: bool,
) -> Result<SalesInvoicesImport, YukiError> {
    let target = config.target(admin)?;
    let mut client = SalesClient::new().with_api_root(target.api_root);
    client.authenticate(target.api_key).await?;
    let import = client
        .process_sales_invoices(target.admin_id, &invoice.to_xml())
        .await?;

    if !quiet {
        print_import(&import, format);
    }
    Ok(import)
}

fn print_import(import: &SalesInvoicesImport, format: Option<&str>) {
    let yes_no = |b: bool| if b { "Yes" } else { "No" }.to_string();
    let headers: Vec<String> = [
        "Succeeded",
        "Processed",
        "Email Sent",
        "Reference",
        "Subject",
        "Message",
    ]
    .map(String::from)
    .to_vec();
    let rows: Vec<Vec<String>> = import
        .invoices
        .iter()
        .map(|i| {
            vec![
                yes_no(i.succeeded),
                yes_no(i.processed),
                yes_no(i.email_sent),
                i.reference.clone(),
                i.subject.clone(),
                i.message.clone(),
            ]
        })
        .collect();
    match OutputFormat::from_flag(format, is_tty()) {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
}

/// Why Yuki did not do what `--send` asked of an invoice it accepted: not
/// booked, or (when emailing) not emailed. Peppol delivery is not reported
/// back, so it cannot be checked.
pub fn unsent(import: &SalesInvoicesImport, send: Option<SendMode>) -> Option<String> {
    let mode = send?;
    let problems: Vec<String> = import
        .invoices
        .iter()
        .filter(|i| i.succeeded)
        .filter_map(|invoice| {
            let name = if invoice.reference.is_empty() {
                &invoice.subject
            } else {
                &invoice.reference
            };
            if !invoice.processed {
                Some(format!("Yuki saved invoice {name} but did not book it"))
            } else if mode.email() && !invoice.email_sent {
                Some(format!("Yuki booked invoice {name} but did not email it"))
            } else {
                None
            }
        })
        .collect();
    (!problems.is_empty()).then(|| problems.join("; "))
}

/// List the saved templates, each validated as `create` would read it.
pub fn templates(format: Option<&str>) -> Result<(), YukiError> {
    let dir = templates_dir();
    let mut names: Vec<String> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "toml"))
            .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(YukiError::Config(format!("{}: {e}", dir.display()))),
    };
    names.sort();

    let headers: Vec<String> = [
        "Name", "Customer", "Subject", "Lines", "Net", "Path", "Status",
    ]
    .map(String::from)
    .to_vec();
    let rows: Vec<Vec<String>> = names.iter().map(|name| template_row(name)).collect();
    let format = OutputFormat::from_flag(format, is_tty());
    if rows.is_empty() && matches!(format, OutputFormat::Table) {
        eprintln!("No invoice templates in {}", dir.display());
    }
    match format {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}

fn template_row(name: &str) -> Vec<String> {
    let path = template_path(name)
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    match load(
        &Source::Template(name.to_string()),
        &Overrides::default(),
        None,
    ) {
        Ok(invoice) => vec![
            name.to_string(),
            invoice.contact.label(),
            invoice.subject.clone().unwrap_or_default(),
            invoice.lines.len().to_string(),
            invoice.net().to_string(),
            path,
            "ok".to_string(),
        ],
        Err(e) => {
            let reason = e.to_string().replace('\n', " ");
            vec![
                name.to_string(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                path,
                format!("invalid: {reason}"),
            ]
        }
    }
}

#[cfg(test)]
#[path = "sales_invoice_tests.rs"]
mod tests;
