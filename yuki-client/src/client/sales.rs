use quick_xml::Reader;
use quick_xml::events::Event;

use crate::error::YukiError;

use super::soap_client::{SoapClient, SoapEnvelope, payload_timeout};
use super::{ElementText, Region, local_name, service_url};

const SERVICE: &str = "Sales.asmx";

/// A Yuki sales item (product or service available for invoicing).
#[derive(Debug, Clone)]
pub struct SalesItem {
    pub id: String,
    pub description: String,
}

/// The outcome of `ProcessSalesInvoices`: Yuki's `SalesInvoicesImportResponse`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SalesInvoicesImport {
    pub total_succeeded: u32,
    pub total_failed: u32,
    pub total_skipped: u32,
    pub invoices: Vec<ImportedInvoice>,
}

/// One invoice of a [`SalesInvoicesImport`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportedInvoice {
    pub succeeded: bool,
    /// Booked (`Process=true` took effect), not merely saved as a draft.
    pub processed: bool,
    pub email_sent: bool,
    /// The invoice number Yuki assigned, when it assigned one.
    pub reference: String,
    pub subject: String,
    pub message: String,
}

impl SalesInvoicesImport {
    /// Why the import did not fully succeed, or `None` when every invoice did.
    ///
    /// A failed or skipped invoice, an invoice not marked succeeded, or a
    /// response that lists no invoice at all each count as not succeeding.
    pub fn failure(&self) -> Option<String> {
        let all_succeeded = self.invoices.iter().all(|i| i.succeeded);
        if self.total_failed == 0
            && self.total_skipped == 0
            && all_succeeded
            && !self.invoices.is_empty()
        {
            return None;
        }
        let mut summary = format!(
            "Yuki did not accept every invoice: {} succeeded, {} failed, {} skipped",
            self.total_succeeded, self.total_failed, self.total_skipped
        );
        if self.invoices.is_empty() {
            summary.push_str("; no invoice was reported");
        }
        let messages: Vec<&str> = self
            .invoices
            .iter()
            .filter(|i| !i.succeeded && !i.message.is_empty())
            .map(|i| i.message.as_str())
            .collect();
        if !messages.is_empty() {
            summary.push_str(": ");
            summary.push_str(&messages.join("; "));
        }
        Some(summary)
    }
}

/// Client for the Yuki Sales SOAP service.
pub struct SalesClient {
    soap: SoapClient,
}

impl SalesClient {
    pub fn new() -> Self {
        Self {
            soap: SoapClient::new(&service_url(Region::default().api_root(), SERVICE)),
        }
    }

    /// Build over a caller-provided HTTP client, so a long-running consumer can
    /// share a single pooled client across all service clients.
    pub fn with_client(http: reqwest::Client) -> Self {
        Self {
            soap: SoapClient::with_client(
                &service_url(Region::default().api_root(), SERVICE),
                http,
            ),
        }
    }

    /// Target another Yuki deployment, e.g. `Region::Be.api_root()`, or any root
    /// such as a local mock. The service path is appended to `api_root`.
    #[must_use]
    pub fn with_api_root(mut self, api_root: &str) -> Self {
        self.soap.retarget(api_root, SERVICE);
        self
    }

    /// The endpoint this client posts to.
    pub fn base_url(&self) -> &str {
        self.soap.base_url()
    }

    fn require_session(&self) -> Result<&str, YukiError> {
        self.soap.session_id().ok_or_else(|| {
            YukiError::AuthFailed("not authenticated — call authenticate() first".to_string())
        })
    }

    /// Authenticate with the Yuki API and store the session ID.
    pub async fn authenticate(&mut self, api_key: &str) -> Result<String, YukiError> {
        self.soap.authenticate(api_key).await
    }

    /// Retrieve all sales items.
    pub async fn get_sales_items(&self) -> Result<Vec<SalesItem>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("GetSalesItems").session(session).build();
        let body = self.soap.call("GetSalesItems", envelope).await?;
        Self::parse_sales_items(&body)
    }

    /// Import sales invoices: `xml_doc` is a `<SalesInvoices>` document in the
    /// `urn:xmlns:http://www.theyukicompany.com:salesinvoices` namespace.
    ///
    /// This writes to the administration: a draft or, with `Process=true`, a
    /// booked and possibly sent invoice.
    pub async fn process_sales_invoices(
        &self,
        administration_id: &str,
        xml_doc: &str,
    ) -> Result<SalesInvoicesImport, YukiError> {
        let session = self.require_session()?;
        let envelope = Self::process_sales_invoices_envelope(session, administration_id, xml_doc);
        // The document may carry a PDF: allow for its size, like an upload.
        let timeout = Some(payload_timeout(xml_doc));
        let body = self
            .soap
            .call_with_timeout("ProcessSalesInvoices", envelope, timeout)
            .await?;
        Self::parse_process_sales_invoices(&body)
    }

    /// The `ProcessSalesInvoices` envelope. Its parameters are `sessionId` and
    /// `administrationId` with a lowercase `d`, unlike most operations, and
    /// `xmlDoc` is `s:any`, so the document goes in as child elements.
    pub fn process_sales_invoices_envelope(
        session_id: &str,
        administration_id: &str,
        xml_doc: &str,
    ) -> String {
        SoapEnvelope::new("ProcessSalesInvoices")
            .param("sessionId", session_id)
            .param("administrationId", administration_id)
            .param_xml("xmlDoc", xml_doc)
            .build()
    }

    /// Parse a `ProcessSalesInvoices` SOAP response.
    ///
    /// The result is `s:any`: the `SalesInvoicesImportResponse` may arrive as
    /// child elements or as escaped XML text inside `ProcessSalesInvoicesResult`.
    /// Both are accepted.
    pub fn parse_process_sales_invoices(xml: &str) -> Result<SalesInvoicesImport, YukiError> {
        if let Some(fault) = SoapClient::parse_soap_fault(xml) {
            return Err(fault);
        }
        if let Some(import) = Self::parse_import(xml)? {
            return Ok(import);
        }
        let text = SoapClient::parse_single_result(xml, "ProcessSalesInvoicesResult")?;
        Self::parse_import(&text).ok().flatten().ok_or_else(|| {
            YukiError::Xml(format!(
                "ProcessSalesInvoices returned no SalesInvoicesImportResponse: {text}"
            ))
        })
    }

    /// The `SalesInvoicesImportResponse` in `xml`, or `None` when it has none.
    fn parse_import(xml: &str) -> Result<Option<SalesInvoicesImport>, YukiError> {
        let flag = |text: &str| text.eq_ignore_ascii_case("true") || text == "1";
        let count = |text: &str| text.parse::<u32>().unwrap_or(0);
        let mut reader = Reader::from_str(xml);
        let mut import: Option<SalesInvoicesImport> = None;
        let mut invoice: Option<ImportedInvoice> = None;
        let mut field: Option<String> = None;
        let mut content = ElementText::default();
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "SalesInvoicesImportResponse" => {
                            import = Some(SalesInvoicesImport::default());
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
                                (Some(i), "Succeeded") => i.succeeded = flag(&text),
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

    /// Parse a GetSalesItems SOAP response into a list of `SalesItem` values.
    ///
    /// Each `SalesItem` element carries child elements `id` and `description`.
    pub fn parse_sales_items(xml: &str) -> Result<Vec<SalesItem>, YukiError> {
        let mut reader = Reader::from_str(xml);

        let mut items = Vec::new();
        let mut in_item = false;
        let mut field: Option<String> = None;
        let mut current = SalesItem {
            id: String::new(),
            description: String::new(),
        };
        let mut content = ElementText::default();
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "SalesItem" => {
                            in_item = true;
                            current = SalesItem {
                                id: String::new(),
                                description: String::new(),
                            };
                        }
                        "id" | "description" if in_item => {
                            field = Some(local);
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "id" | "description" => {
                            let text = content.take();
                            if let Some(f) = field.take() {
                                match f.as_str() {
                                    "id" => current.id = text,
                                    "description" => current.description = text,
                                    _ => {}
                                }
                            }
                        }
                        "SalesItem" if in_item => {
                            items.push(current.clone());
                            in_item = false;
                        }
                        _ => {}
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(YukiError::Xml(e.to_string())),
                Ok(ref event) => content.push_if(field.is_some(), event)?,
            }
            buf.clear();
        }

        Ok(items)
    }
}

impl Default for SalesClient {
    fn default() -> Self {
        Self::new()
    }
}
