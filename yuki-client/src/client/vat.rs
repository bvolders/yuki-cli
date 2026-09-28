use quick_xml::Reader;
use quick_xml::events::Event;

use crate::error::YukiError;

use super::soap_client::{SoapClient, SoapEnvelope};
use super::{ElementText, Region, local_name, service_url};

const SERVICE: &str = "Vat.asmx";

/// A VAT return record for a specific period.
#[derive(Debug, Clone)]
pub struct VatReturn {
    pub period: String,
    pub status: String,
    pub start_date: String,
    pub end_date: String,
}

/// An active VAT code definition.
#[derive(Debug, Clone)]
pub struct VatCode {
    pub code: String,
    pub description: String,
}

/// Client for the Yuki VAT SOAP service.
pub struct VatClient {
    soap: SoapClient,
}

impl VatClient {
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
        self.soap.base_url = service_url(api_root, SERVICE);
        self
    }

    /// The endpoint this client posts to.
    pub fn base_url(&self) -> &str {
        &self.soap.base_url
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

    /// Retrieve the list of VAT returns for an administration.
    pub async fn vat_return_list(
        &self,
        administration_id: &str,
    ) -> Result<Vec<VatReturn>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("VATReturnList")
            .session(session)
            .param("administrationID", administration_id)
            .build();
        let body = self.soap.call("VATReturnList", envelope).await?;
        Self::parse_vat_returns(&body)
    }

    /// Retrieve all active VAT codes for an administration.
    pub async fn active_vat_codes(
        &self,
        administration_id: &str,
    ) -> Result<Vec<VatCode>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("ActiveVATCodesList")
            .session(session)
            .param("administrationID", administration_id)
            .build();
        let body = self.soap.call("ActiveVATCodesList", envelope).await?;
        Self::parse_vat_codes(&body)
    }

    /// Parse a VatReturnList SOAP response into a list of `VatReturn` values.
    pub fn parse_vat_returns(xml: &str) -> Result<Vec<VatReturn>, YukiError> {
        let mut reader = Reader::from_str(xml);

        let mut returns = Vec::new();
        let mut in_item = false;
        let mut field: Option<String> = None;
        let mut current = VatReturn {
            period: String::new(),
            status: String::new(),
            start_date: String::new(),
            end_date: String::new(),
        };
        let mut content = ElementText::default();
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "VATReturnInfo" => {
                            in_item = true;
                            current = VatReturn {
                                period: String::new(),
                                status: String::new(),
                                start_date: String::new(),
                                end_date: String::new(),
                            };
                        }
                        "startDate" | "endDate" | "status" if in_item => {
                            field = Some(local);
                        }
                        _ => {}
                    }
                }
                Ok(ref event @ (Event::Text(_) | Event::GeneralRef(_) | Event::CData(_)))
                    if field.is_some() =>
                {
                    content.push(event)?;
                }
                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "startDate" | "endDate" | "status" => {
                            let text = content.take();
                            if let Some(f) = field.take() {
                                match f.as_str() {
                                    "startDate" => current.start_date = text,
                                    "endDate" => current.end_date = text,
                                    "status" => current.status = text,
                                    _ => {}
                                }
                            }
                        }
                        "VATReturnInfo" if in_item => {
                            // Derive period from start/end dates
                            current.period = format!(
                                "{} - {}",
                                current
                                    .start_date
                                    .split('T')
                                    .next()
                                    .unwrap_or(&current.start_date),
                                current
                                    .end_date
                                    .split('T')
                                    .next()
                                    .unwrap_or(&current.end_date),
                            );
                            returns.push(current.clone());
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

        Ok(returns)
    }

    /// Parse an ActiveVatCodes SOAP response into a list of `VatCode` values.
    pub fn parse_vat_codes(xml: &str) -> Result<Vec<VatCode>, YukiError> {
        let mut reader = Reader::from_str(xml);

        let mut codes = Vec::new();
        let mut in_item = false;
        let mut field: Option<String> = None;
        let mut current = VatCode {
            code: String::new(),
            description: String::new(),
        };
        let mut content = ElementText::default();
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "VATCode" => {
                            in_item = true;
                            current = VatCode {
                                code: String::new(),
                                description: String::new(),
                            };
                        }
                        "type" | "description" if in_item => {
                            field = Some(local);
                        }
                        _ => {}
                    }
                }
                Ok(ref event @ (Event::Text(_) | Event::GeneralRef(_) | Event::CData(_)))
                    if field.is_some() =>
                {
                    content.push(event)?;
                }
                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "type" | "description" => {
                            let text = content.take();
                            if let Some(f) = field.take() {
                                match f.as_str() {
                                    "type" => current.code = text,
                                    "description" => current.description = text,
                                    _ => {}
                                }
                            }
                        }
                        "VATCode" if in_item => {
                            codes.push(current.clone());
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

        Ok(codes)
    }
}

impl Default for VatClient {
    fn default() -> Self {
        Self::new()
    }
}
