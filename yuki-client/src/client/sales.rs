use quick_xml::Reader;
use quick_xml::events::Event;

use crate::error::YukiError;

use super::soap_client::{SoapClient, SoapEnvelope};
use super::{Region, local_name, service_url, unescape_text};

const SERVICE: &str = "Sales.asmx";

/// A Yuki sales item (product or service available for invoicing).
#[derive(Debug, Clone)]
pub struct SalesItem {
    pub id: String,
    pub description: String,
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

    /// Parse a GetSalesItems SOAP response into a list of `SalesItem` values.
    ///
    /// Each `SalesItem` element carries child elements `id` and `description`.
    pub fn parse_sales_items(xml: &str) -> Result<Vec<SalesItem>, YukiError> {
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);

        let mut items = Vec::new();
        let mut in_item = false;
        let mut field: Option<String> = None;
        let mut current = SalesItem {
            id: String::new(),
            description: String::new(),
        };
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
                Ok(Event::Text(ref e)) => {
                    if let Some(ref f) = field {
                        let text = unescape_text(e)
                            .map_err(|e| YukiError::Xml(e.to_string()))?
                            .trim()
                            .to_string();
                        match f.as_str() {
                            "id" => current.id = text,
                            "description" => current.description = text,
                            _ => {}
                        }
                    }
                }
                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "id" | "description" => {
                            field = None;
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
                _ => {}
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
