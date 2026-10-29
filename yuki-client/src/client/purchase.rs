//! `Purchase.asmx`: import purchase invoices (`ProcessPurchaseInvoices`).

use crate::error::YukiError;

use super::invoice_import::{InvoicesImport, parse_invoices_import};
use super::soap_client::{SoapClient, SoapEnvelope, payload_timeout};
use super::{Region, service_url};

const SERVICE: &str = "Purchase.asmx";

/// The outcome of `ProcessPurchaseInvoices`: Yuki's
/// `PurchaseInvoicesImportResponse`, the same shape as the sales import
/// (`email_sent` is never set). It returns no ID for the invoice created.
pub type PurchaseInvoicesImport = InvoicesImport;

/// Client for the Yuki Purchase SOAP service.
pub struct PurchaseClient {
    soap: SoapClient,
}

impl Default for PurchaseClient {
    fn default() -> Self {
        Self::new()
    }
}

impl PurchaseClient {
    pub fn new() -> Self {
        Self {
            soap: SoapClient::new(&service_url(Region::default().api_root(), SERVICE)),
        }
    }

    /// Build over a caller-provided HTTP client.
    pub fn with_client(http: reqwest::Client) -> Self {
        Self {
            soap: SoapClient::with_client(
                &service_url(Region::default().api_root(), SERVICE),
                http,
            ),
        }
    }

    /// Target another Yuki deployment or a local mock; the service path is
    /// appended to `api_root`.
    #[must_use]
    pub fn with_api_root(mut self, api_root: &str) -> Self {
        self.soap.retarget(api_root, SERVICE);
        self
    }

    /// The endpoint this client posts to.
    pub fn base_url(&self) -> &str {
        self.soap.base_url()
    }

    /// Reuse a session opened by another service client instead of
    /// authenticating again.
    #[must_use]
    pub fn with_session(mut self, session_id: &str) -> Self {
        self.soap = self.soap.with_session(session_id);
        self
    }

    fn require_session(&self) -> Result<&str, YukiError> {
        self.soap.session_id().ok_or_else(|| {
            YukiError::AuthFailed("not authenticated — call authenticate() first".to_string())
        })
    }

    /// Authenticate with the access key alone (`Authenticate`).
    pub async fn authenticate(&mut self, api_key: &str) -> Result<String, YukiError> {
        self.soap.authenticate(api_key).await
    }

    /// Authenticate as a registered API client (`AuthenticateClient`).
    pub async fn authenticate_client(
        &mut self,
        client_id: &str,
        client_secret: &str,
        api_key: &str,
    ) -> Result<String, YukiError> {
        self.soap
            .authenticate_client(client_id, client_secret, api_key)
            .await
    }

    /// Import purchase invoices: `xml_doc` is a `<PurchaseInvoices>` document
    /// in the `urn:xmlns:http://www.theyukicompany.com:purchaseinvoices`
    /// namespace, each invoice carrying its PDF.
    ///
    /// This writes to the administration: a purchase invoice (and its
    /// document in the archive) that the API cannot delete.
    pub async fn process_purchase_invoices(
        &self,
        administration_id: &str,
        xml_doc: &str,
    ) -> Result<PurchaseInvoicesImport, YukiError> {
        let session = self.require_session()?;
        let envelope =
            Self::process_purchase_invoices_envelope(session, administration_id, xml_doc);
        let timeout = Some(payload_timeout(xml_doc));
        let body = self
            .soap
            .call_with_timeout("ProcessPurchaseInvoices", envelope, timeout)
            .await?;
        Self::parse_process_purchase_invoices(&body)
    }

    /// The `ProcessPurchaseInvoices` envelope: `sessionId` and
    /// `administrationId` with a lowercase `d`, `disableAutoCorrect` as the
    /// string `false` (Yuki may correct rounding differences), and `xmlDoc`
    /// as child elements (`s:any`).
    pub fn process_purchase_invoices_envelope(
        session_id: &str,
        administration_id: &str,
        xml_doc: &str,
    ) -> String {
        SoapEnvelope::new("ProcessPurchaseInvoices")
            .param("sessionId", session_id)
            .param("administrationId", administration_id)
            .param("disableAutoCorrect", "false")
            .param_xml("xmlDoc", xml_doc)
            .build()
    }

    /// Parse a `ProcessPurchaseInvoices` SOAP response; the
    /// `PurchaseInvoicesImportResponse` may arrive as child elements or as
    /// escaped XML text.
    pub fn parse_process_purchase_invoices(xml: &str) -> Result<PurchaseInvoicesImport, YukiError> {
        parse_invoices_import(
            xml,
            "ProcessPurchaseInvoices",
            "PurchaseInvoicesImportResponse",
        )
    }
}
