use quick_xml::Reader;
use quick_xml::events::Event;
use reqwest::Client;

use crate::error::YukiError;

use super::{ElementText, local_name, service_url};

const YUKI_NS: &str = "http://www.theyukicompany.com/";
const SOAP_NS: &str = "http://schemas.xmlsoap.org/soap/envelope/";

/// Builder for SOAP XML request envelopes.
pub struct SoapEnvelope {
    operation: String,
    session_id: Option<String>,
    params: Vec<(String, String)>,
}

impl SoapEnvelope {
    pub fn new(operation: &str) -> Self {
        Self {
            operation: operation.to_string(),
            session_id: None,
            params: Vec::new(),
        }
    }

    pub fn session(mut self, session_id: &str) -> Self {
        self.session_id = Some(session_id.to_string());
        self
    }

    /// Add a text parameter. `value` is XML-escaped, so a file name such as
    /// `Tom & Jerry <x>.pdf` reaches Yuki unchanged.
    pub fn param(mut self, name: &str, value: &str) -> Self {
        self.params.push((name.to_string(), escape_xml(value)));
        self
    }

    /// Add a parameter whose content is inserted as is, for operations that
    /// take an XML document as a child element. The caller guarantees that
    /// `xml` is well-formed XML; nothing is escaped.
    pub fn param_xml(mut self, name: &str, xml: &str) -> Self {
        self.params.push((name.to_string(), xml.to_string()));
        self
    }

    pub fn build(self) -> String {
        let mut body = String::new();

        if let Some(sid) = &self.session_id {
            let sid = escape_xml(sid);
            body.push_str(&format!("      <yuki:sessionID>{sid}</yuki:sessionID>\n"));
        }

        for (name, value) in &self.params {
            body.push_str(&format!("      <yuki:{name}>{value}</yuki:{name}>\n"));
        }

        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="{SOAP_NS}"
               xmlns:yuki="{YUKI_NS}">
  <soap:Body>
    <yuki:{op}>
{body}    </yuki:{op}>
  </soap:Body>
</soap:Envelope>"#,
            op = self.operation,
        )
    }
}

/// Escape the five XML special characters for use in element text.
fn escape_xml(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// How long one SOAP request may take before it is abandoned.
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// HTTP transport client for the Yuki SOAP API.
pub struct SoapClient {
    http: Client,
    base_url: String,
    pub(super) session_id: Option<String>,
}

impl SoapClient {
    /// Create a transport with its own default HTTP client. Convenient for
    /// short-lived consumers such as the CLI, where each invocation is fresh.
    /// Requests time out after [`REQUEST_TIMEOUT`].
    pub fn new(base_url: &str) -> Self {
        let http = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_else(|_| Client::new());
        Self::with_client(base_url, http)
    }

    /// Create a transport over a caller-provided HTTP client. Long-running
    /// consumers (e.g. a server) should pass a single shared, pooled client
    /// rather than constructing one per request.
    pub fn with_client(base_url: &str, http: Client) -> Self {
        Self {
            http,
            base_url: base_url.to_string(),
            session_id: None,
        }
    }

    pub fn with_session(mut self, session_id: &str) -> Self {
        self.session_id = Some(session_id.to_string());
        self
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Point this transport at `service` (e.g. `Accounting.asmx`) under
    /// `api_root`, such as `Region::Be.api_root()` or a local mock.
    pub fn retarget(&mut self, api_root: &str, service: &str) {
        self.base_url = service_url(api_root, service);
    }

    /// The endpoint this transport posts to.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Build the SOAPAction header value for a given operation.
    pub fn soap_action(_service: &str, operation: &str) -> String {
        format!("{YUKI_NS}{operation}")
    }

    /// POST a SOAP envelope and return the raw response body.
    pub async fn call(&self, operation: &str, envelope: String) -> Result<String, YukiError> {
        let action = Self::soap_action("", operation);
        let response = self
            .http
            .post(&self.base_url)
            .header("Content-Type", "text/xml; charset=utf-8")
            .header("SOAPAction", format!("\"{action}\""))
            .body(envelope)
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        // Set YUKI_DEBUG_XML to dump raw SOAP responses to stderr for diagnosis.
        if std::env::var_os("YUKI_DEBUG_XML").is_some() {
            eprintln!(
                "=== YUKI_DEBUG_XML {operation} HTTP {} ===\n{body}\n=== end {operation} ===",
                status.as_u16()
            );
        }

        if status == 401 || status == 403 {
            return Err(YukiError::AuthFailed(format!("HTTP {status}")));
        }
        if status == 429 {
            return Err(YukiError::RateLimited);
        }
        if !status.is_success() {
            // SOAP faults are returned as HTTP 500 — try to parse them
            if let Some(fault) = Self::parse_soap_fault(&body) {
                return Err(fault);
            }
            return Err(YukiError::Http {
                status: status.as_u16(),
                body,
            });
        }

        Ok(body)
    }

    /// Authenticate with an API key, storing the returned session ID.
    pub async fn authenticate(&mut self, api_key: &str) -> Result<String, YukiError> {
        let envelope = SoapEnvelope::new("Authenticate")
            .param("accessKey", api_key)
            .build();

        let body = self.call("Authenticate", envelope).await?;
        let session = Self::parse_single_result(&body, "AuthenticateResult")?;
        self.session_id = Some(session.clone());
        Ok(session)
    }

    /// Extract the text content of a single named element from a SOAP response.
    ///
    /// Returns an error if a SOAP fault is found or the element is missing.
    pub fn parse_single_result(xml: &str, result_tag: &str) -> Result<String, YukiError> {
        // Check for fault first.
        if let Some(err) = Self::parse_soap_fault(xml) {
            return Err(err);
        }

        let mut reader = Reader::from_str(xml);

        let mut inside_target = false;
        let mut content = ElementText::default();
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let name = e.name();
                    let local = local_name(name.as_ref());
                    if local == result_tag {
                        inside_target = true;
                    }
                }
                Ok(Event::End(ref e)) => {
                    let name = e.name();
                    let local = local_name(name.as_ref());
                    if local == result_tag {
                        inside_target = false;
                        let text = content.take();
                        if !text.is_empty() {
                            return Ok(text);
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(YukiError::Xml(e.to_string())),
                Ok(ref event) => content.push_if(inside_target, event)?,
            }
            buf.clear();
        }

        Err(YukiError::Xml(format!(
            "element '{result_tag}' not found in response"
        )))
    }

    /// Detect a SOAP fault in the response and return it as a `YukiError`.
    ///
    /// Returns `None` if no fault is present.
    pub fn parse_soap_fault(xml: &str) -> Option<YukiError> {
        let mut reader = Reader::from_str(xml);

        let mut in_fault = false;
        let mut in_faultcode = false;
        let mut in_faultstring = false;
        let mut faultcode = String::new();
        let mut faultstring = String::new();
        let mut content = ElementText::default();
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let name = e.name();
                    let local = local_name(name.as_ref());
                    match local {
                        "Fault" => in_fault = true,
                        "faultcode" if in_fault => in_faultcode = true,
                        "faultstring" if in_fault => in_faultstring = true,
                        _ => {}
                    }
                }
                Ok(Event::End(ref e)) => {
                    let name = e.name();
                    let local = local_name(name.as_ref());
                    match local {
                        "faultcode" if in_faultcode => {
                            in_faultcode = false;
                            faultcode = content.take();
                        }
                        "faultstring" if in_faultstring => {
                            in_faultstring = false;
                            faultstring = content.take();
                        }
                        "Fault" => {
                            if !faultstring.is_empty() {
                                let msg_lower = faultstring.to_lowercase();
                                if msg_lower.contains("invalid")
                                    && (msg_lower.contains("key")
                                        || msg_lower.contains("session")
                                        || msg_lower.contains("auth"))
                                {
                                    return Some(YukiError::AuthFailed(faultstring));
                                }
                                return Some(YukiError::SoapFault {
                                    code: faultcode,
                                    message: faultstring,
                                });
                            }
                            return None;
                        }
                        _ => {}
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                Ok(ref event) => content.push_lossy_if(in_faultcode || in_faultstring, event),
            }
            buf.clear();
        }

        None
    }
}
