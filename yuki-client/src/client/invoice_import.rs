//! The import response `ProcessSalesInvoices` and `ProcessPurchaseInvoices`
//! share, and its parser.

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::error::YukiError;

use super::soap_client::SoapClient;
use super::{ElementText, local_name};

/// The outcome of `ProcessSalesInvoices` or `ProcessPurchaseInvoices`:
/// Yuki's `SalesInvoicesImportResponse` or `PurchaseInvoicesImportResponse`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InvoicesImport {
    /// `None` when the response lacks the count or it does not parse.
    pub total_succeeded: Option<u32>,
    pub total_failed: Option<u32>,
    pub total_skipped: Option<u32>,
    pub invoices: Vec<ImportedInvoice>,
}

/// One invoice of an [`InvoicesImport`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportedInvoice {
    /// `None` when `Succeeded` is missing or neither true nor false: then
    /// nothing is known about the invoice.
    pub succeeded: Option<bool>,
    /// Booked (`Process=true` took effect), not merely saved as a draft.
    pub processed: bool,
    pub email_sent: bool,
    /// The invoice number Yuki assigned, when it assigned one.
    pub reference: String,
    pub subject: String,
    pub message: String,
}

impl InvoicesImport {
    /// Why the import did not fully succeed, or `None` when every invoice did.
    ///
    /// A failed or skipped invoice, an invoice not marked succeeded, or a
    /// response that lists no invoice at all each count as not succeeding.
    pub fn failure(&self) -> Option<String> {
        let all_succeeded = self.invoices.iter().all(|i| i.succeeded == Some(true));
        if self.total_failed == Some(0)
            && self.total_skipped == Some(0)
            && all_succeeded
            && !self.invoices.is_empty()
        {
            return None;
        }
        let count = |n: Option<u32>| n.map_or("?".to_string(), |n| n.to_string());
        let mut summary = format!(
            "Yuki did not accept every invoice: {} succeeded, {} failed, {} skipped",
            count(self.total_succeeded),
            count(self.total_failed),
            count(self.total_skipped)
        );
        if self.invoices.is_empty() {
            summary.push_str("; no invoice was reported");
        }
        let messages: Vec<&str> = self
            .invoices
            .iter()
            .filter(|i| i.succeeded != Some(true) && !i.message.is_empty())
            .map(|i| i.message.as_str())
            .collect();
        if !messages.is_empty() {
            summary.push_str(": ");
            summary.push_str(&messages.join("; "));
        }
        Some(summary)
    }

    /// Whether Yuki provably created nothing: no invoice succeeded, at least
    /// one failed, none was skipped, and every invoice listed says, in so
    /// many words, `Succeeded` false, one per failure. Anything missing,
    /// unparsed or inconsistent is not proof.
    pub fn nothing_created(&self) -> bool {
        self.total_succeeded == Some(0)
            && self.total_skipped == Some(0)
            && self.total_failed.is_some_and(|n| n >= 1)
            && self.total_failed == u32::try_from(self.invoices.len()).ok()
            && self.invoices.iter().all(|i| i.succeeded == Some(false))
    }
}

/// Parse the response of `operation` (`ProcessSalesInvoices`,
/// `ProcessPurchaseInvoices`), whose result is an import response `root`
/// (`SalesInvoicesImportResponse`, `PurchaseInvoicesImportResponse`) sent as
/// child elements or as escaped XML text; both are accepted.
pub(crate) fn parse_invoices_import(
    xml: &str,
    operation: &str,
    root: &str,
) -> Result<InvoicesImport, YukiError> {
    if let Some(fault) = SoapClient::parse_soap_fault(xml) {
        return Err(fault);
    }
    if let Some(import) = parse_import(xml, root)? {
        return Ok(import);
    }
    let text = SoapClient::parse_single_result(xml, &format!("{operation}Result"))?;
    parse_import(&text, root)
        .ok()
        .flatten()
        .ok_or_else(|| YukiError::Xml(format!("{operation} returned no {root}: {text}")))
}

/// The import response `root` in `xml`, or `None` when it has none.
fn parse_import(xml: &str, root: &str) -> Result<Option<InvoicesImport>, YukiError> {
    let strict = |text: &str| match text {
        t if t.eq_ignore_ascii_case("true") || t == "1" => Some(true),
        t if t.eq_ignore_ascii_case("false") || t == "0" => Some(false),
        _ => None,
    };
    let flag = |text: &str| strict(text) == Some(true);
    let count = |text: &str| text.parse::<u32>().ok();
    let mut reader = Reader::from_str(xml);
    let mut import: Option<InvoicesImport> = None;
    let mut invoice: Option<ImportedInvoice> = None;
    let mut field: Option<String> = None;
    let mut content = ElementText::default();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let local = local_name(e.name().as_ref()).to_string();
                match local.as_str() {
                    name if name == root => {
                        import = Some(InvoicesImport::default());
                    }
                    "Invoice" if import.is_some() => invoice = Some(ImportedInvoice::default()),
                    _ if import.is_some() => {
                        content.take();
                        field = Some(local);
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                let local = local_name(e.name().as_ref()).to_string();
                if let Some(current) = import.as_mut() {
                    if local == "Invoice" {
                        current.invoices.extend(invoice.take());
                    } else if field.as_deref() == Some(local.as_str()) {
                        field = None;
                        let text = content.take();
                        match (invoice.as_mut(), local.as_str()) {
                            (Some(i), "Succeeded") => i.succeeded = strict(&text),
                            (Some(i), "Processed") => i.processed = flag(&text),
                            (Some(i), "EmailSent") => i.email_sent = flag(&text),
                            (Some(i), "Reference") => i.reference = text,
                            (Some(i), "Subject") => i.subject = text,
                            (Some(i), "Message") => i.message = text,
                            (None, "TotalSucceeded") => current.total_succeeded = count(&text),
                            (None, "TotalFailed") => current.total_failed = count(&text),
                            (None, "TotalSkipped") => current.total_skipped = count(&text),
                            _ => {}
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(YukiError::Xml(e.to_string())),
            Ok(ref event) => content.push_if(field.is_some(), event)?,
        }
        buf.clear();
    }

    Ok(import)
}
