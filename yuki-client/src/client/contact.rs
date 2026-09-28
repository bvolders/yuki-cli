use quick_xml::Reader;
use quick_xml::events::Event;

use crate::error::YukiError;

use super::soap_client::{SoapClient, SoapEnvelope};
use super::{Region, local_name, service_url, unescape_text};

const SERVICE: &str = "Contact.asmx";

/// A Yuki contact (customer or supplier).
#[derive(Debug, Clone)]
pub struct Contact {
    pub id: String,
    pub name: String,
    pub contact_type: String,
    pub country: String,
    pub is_supplier: bool,
    pub is_customer: bool,
}

/// Client for the Yuki Contact SOAP service.
pub struct ContactClient {
    soap: SoapClient,
}

impl ContactClient {
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

    /// Search for contacts matching a query string.
    pub async fn search_contacts(&self, query: &str) -> Result<Vec<Contact>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("SearchContacts")
            .session(session)
            .param("searchQuery", query)
            .build();
        let body = self.soap.call("SearchContacts", envelope).await?;
        parse_contacts(&body)
    }

    /// Fetch one page of suppliers and customers. Pages are 1-based.
    pub async fn get_suppliers_and_customers_page(
        &self,
        contact_type: &str,
        page_number: u32,
    ) -> Result<Vec<Contact>, YukiError> {
        let session = self.require_session()?;
        let envelope = suppliers_envelope(session, contact_type, page_number);
        let body = self.soap.call("GetSuppliersAndCustomers", envelope).await?;
        parse_contacts(&body)
    }

    /// Retrieve every supplier and customer of the given type, following pagination.
    ///
    /// The API returns a fixed-size page; without `pageNumber` only the first page is
    /// ever returned, which silently truncates larger address books.
    pub async fn get_suppliers_and_customers(
        &self,
        contact_type: &str,
    ) -> Result<Vec<Contact>, YukiError> {
        let mut collected: Vec<Contact> = Vec::new();
        let mut page = 1;
        loop {
            let batch = self
                .get_suppliers_and_customers_page(contact_type, page)
                .await?;
            if batch.is_empty() {
                break;
            }
            let received = batch.len();
            collected.extend(batch);
            // A short page means the last page was reached.
            if received < CONTACT_PAGE_SIZE {
                break;
            }
            page += 1;
        }
        Ok(collected)
    }
}

/// Records returned per `GetSuppliersAndCustomers` page, fixed by the API.
const CONTACT_PAGE_SIZE: usize = 100;

/// Parse a SearchContacts or GetSuppliersAndCustomers SOAP response into a list of contacts.
///
/// Each `<Contact ID="uuid">` element carries child elements for each field.
/// The contact ID is an XML attribute; all other fields are child text nodes.
pub fn parse_contacts(xml: &str) -> Result<Vec<Contact>, YukiError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut contacts = Vec::new();
    let mut in_contact = false;
    let mut current_field = String::new();
    let mut contact = Contact {
        id: String::new(),
        name: String::new(),
        contact_type: String::new(),
        country: String::new(),
        is_supplier: false,
        is_customer: false,
    };
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let local = local_name(e.name().as_ref()).to_string();
                match local.as_str() {
                    "Contact" => {
                        in_contact = true;
                        contact = Contact {
                            id: String::new(),
                            name: String::new(),
                            contact_type: String::new(),
                            country: String::new(),
                            is_supplier: false,
                            is_customer: false,
                        };
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == "ID" {
                                contact.id = attr.value.into_owned();
                            }
                        }
                    }
                    "Type" | "Name" | "Country" | "IsSupplier" | "IsCustomer" if in_contact => {
                        current_field = local;
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) if in_contact && !current_field.is_empty() => {
                let text = unescape_text(e)
                    .map_err(|e| YukiError::Xml(e.to_string()))?
                    .trim()
                    .to_string();
                match current_field.as_str() {
                    "Type" => contact.contact_type = text,
                    "Name" => contact.name = text,
                    "Country" => contact.country = text,
                    "IsSupplier" => contact.is_supplier = text.eq_ignore_ascii_case("true"),
                    "IsCustomer" => contact.is_customer = text.eq_ignore_ascii_case("true"),
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                match local {
                    "Type" | "Name" | "Country" | "IsSupplier" | "IsCustomer" => {
                        current_field.clear();
                    }
                    "Contact" => {
                        if !contact.id.is_empty() {
                            contacts.push(contact.clone());
                        }
                        in_contact = false;
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

    Ok(contacts)
}

impl Default for ContactClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the `GetSuppliersAndCustomers` envelope for a single page.
///
/// Every element the schema declares is sent. Omitting `pageNumber` pins the request
/// to the first page; omitting `contactType` sends an empty enum value and the whole
/// request is rejected.
pub(crate) fn suppliers_envelope(session: &str, contact_type: &str, page_number: u32) -> String {
    SoapEnvelope::new("GetSuppliersAndCustomers")
        .session(session)
        .param("searchOption", "All")
        .param("searchValue", "")
        .param("sortOrder", "Name")
        .param("active", "Both")
        .param("pageNumber", &page_number.to_string())
        .param("contactType", contact_type)
        .build()
}

#[cfg(test)]
mod envelope_tests {
    use super::suppliers_envelope;

    #[test]
    fn sends_the_requested_page_number() {
        // Regression: pageNumber was never sent, so only the first 100 contacts
        // were ever returned and larger address books were silently truncated.
        let xml = suppliers_envelope("sess", "Supplier", 3);
        assert!(xml.contains("pageNumber"), "{xml}");
        assert!(
            xml.contains(">3<"),
            "page number must reach the request: {xml}"
        );
    }

    #[test]
    fn sends_a_non_empty_contact_type() {
        // Regression: an empty ContactType is not a member of Yuki's enum and the
        // API rejects the entire request with a schema validation fault.
        let xml = suppliers_envelope("sess", "Both", 1);
        assert!(xml.contains("contactType"), "{xml}");
        assert!(
            !xml.contains("<yuki:contactType></yuki:contactType>"),
            "{xml}"
        );
        assert!(!xml.contains("<yuki:contactType/>"), "{xml}");
    }
}
