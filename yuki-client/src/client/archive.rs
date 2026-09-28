use quick_xml::Reader;
use quick_xml::events::Event;

use crate::error::YukiError;

use super::soap_client::{SoapClient, SoapEnvelope};
use super::{ElementText, Region, local_name, service_url};

const SERVICE: &str = "Archive.asmx";

/// A Yuki cost category.
#[derive(Debug, Clone)]
pub struct CostCategory {
    pub id: String,
    pub description: String,
}

/// A Yuki payment method.
#[derive(Debug, Clone)]
pub struct PaymentMethod {
    pub id: String,
    pub description: String,
}

/// A document returned by the Yuki archive search.
#[derive(Debug, Clone)]
pub struct ArchiveDocument {
    pub id: String,
    pub subject: String,
    pub document_date: String,
    pub amount: String,
    pub folder: String,
    pub contact_name: String,
    pub file_name: String,
    pub reference: String,
}

/// Client for the Yuki Archive SOAP service.
pub struct ArchiveClient {
    soap: SoapClient,
}

impl ArchiveClient {
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

    /// Number of records requested per `DocumentsInFolder` round trip.
    const FOLDER_PAGE_SIZE: usize = 500;

    /// Fetch one page of documents from an archive folder.
    pub async fn documents_in_folder_page(
        &self,
        folder_id: i32,
        start_date: &str,
        end_date: &str,
        number_of_records: usize,
        start_record: usize,
    ) -> Result<Vec<ArchiveDocument>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("DocumentsInFolder")
            .session(session)
            .param("folderID", &folder_id.to_string())
            .param("sortOrder", "DocumentDateDesc")
            .param("startDate", start_date)
            .param("endDate", end_date)
            .param("numberOfRecords", &number_of_records.to_string())
            .param("startRecord", &start_record.to_string())
            .build();
        let body = self.soap.call("DocumentsInFolder", envelope).await?;
        Self::parse_archive_documents(&body)
    }

    /// List every document in an archive folder.
    pub async fn documents_in_folder(
        &self,
        folder_id: i32,
        start_date: &str,
        end_date: &str,
    ) -> Result<Vec<ArchiveDocument>, YukiError> {
        self.documents_in_folder_paged(folder_id, start_date, end_date, None, None)
            .await
    }

    /// List documents in an archive folder, paging until the folder is exhausted.
    ///
    /// `offset` skips records server-side; `limit` caps the total returned. With no
    /// limit every document is fetched, so a folder larger than one page is never
    /// silently truncated.
    pub async fn documents_in_folder_paged(
        &self,
        folder_id: i32,
        start_date: &str,
        end_date: &str,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> Result<Vec<ArchiveDocument>, YukiError> {
        let mut collected: Vec<ArchiveDocument> = Vec::new();
        let mut start_record = offset.unwrap_or(0);

        while let Some(want) = next_page_size(limit, collected.len(), Self::FOLDER_PAGE_SIZE) {
            let page = self
                .documents_in_folder_page(folder_id, start_date, end_date, want, start_record)
                .await?;
            let received = page.len();
            collected.extend(page);

            // A short page means the folder is exhausted.
            if received < want {
                break;
            }
            start_record += received;
        }

        Ok(collected)
    }

    /// List all documents of a given document type.
    pub async fn documents_by_type(&self, doc_type: i32) -> Result<String, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("DocumentsByType")
            .session(session)
            .param("documentType", &doc_type.to_string())
            .build();
        self.soap.call("DocumentsByType", envelope).await
    }

    /// Search documents in the archive using a free-text query within a date range.
    ///
    /// Pass an empty string for `search_text` to retrieve all documents in the period.
    /// Returns up to 500 results sorted by document date descending.
    pub async fn search_documents(
        &self,
        search_text: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<Vec<ArchiveDocument>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("SearchDocuments")
            .session(session)
            .param("searchOption", "All")
            .param("searchText", search_text)
            .param("folderID", "-1")
            .param("tabID", "-1")
            .param("sortOrder", "DocumentDateDesc")
            .param("startDate", start_date)
            .param("endDate", end_date)
            .param("numberOfRecords", "500")
            .param("startRecord", "0")
            .build();
        let body = self.soap.call("SearchDocuments", envelope).await?;
        Self::parse_archive_documents(&body)
    }

    /// List documents of a given type that were modified since the specified date.
    pub async fn modified_documents_by_type(
        &self,
        doc_type: i32,
        modified_since: &str,
    ) -> Result<String, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("ModifiedDocumentsByType")
            .session(session)
            .param("documentType", &doc_type.to_string())
            .param("modifiedSince", modified_since)
            .build();
        self.soap.call("ModifiedDocumentsByType", envelope).await
    }

    /// Upload a document to the archive without additional metadata.
    ///
    /// Returns the document ID assigned by Yuki.
    pub async fn upload_document(
        &self,
        admin_id: &str,
        filename: &str,
        data_base64: &str,
        folder_id: i32,
    ) -> Result<String, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("UploadDocument")
            .session(session)
            .param("fileName", filename)
            .param("data", data_base64)
            .param("folder", &folder_id.to_string())
            .param("administrationID", admin_id)
            .build();
        let body = self.soap.call("UploadDocument", envelope).await?;
        SoapClient::parse_single_result(&body, "UploadDocumentResult")
    }

    /// Upload a document to the archive with invoice metadata.
    ///
    /// Returns the document ID assigned by Yuki.
    #[allow(clippy::too_many_arguments)]
    pub async fn upload_document_with_data(
        &self,
        admin_id: &str,
        filename: &str,
        data_base64: &str,
        folder_id: i32,
        currency: &str,
        amount: f64,
        cost_category: Option<&str>,
        payment_method: Option<&str>,
        project: Option<&str>,
        remarks: Option<&str>,
    ) -> Result<String, YukiError> {
        let session = self.require_session()?;
        let amount_str = format!("{amount:.2}");
        let envelope = SoapEnvelope::new("UploadDocumentWithData")
            .session(session)
            .param("fileName", filename)
            .param("data", data_base64)
            .param("folder", &folder_id.to_string())
            .param("administrationID", admin_id)
            .param("currency", currency)
            .param("amount", &amount_str)
            .param("costCategory", cost_category.unwrap_or(""))
            .param("paymentMethod", payment_method.unwrap_or("0"))
            .param("project", project.unwrap_or(""))
            .param("remarks", remarks.unwrap_or(""))
            .build();
        let body = self.soap.call("UploadDocumentWithData", envelope).await?;
        SoapClient::parse_single_result(&body, "UploadDocumentWithDataResult")
    }

    /// Retrieve all available cost categories.
    pub async fn cost_categories(&self) -> Result<Vec<CostCategory>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("CostCategories").session(session).build();
        let body = self.soap.call("CostCategories", envelope).await?;
        Self::parse_cost_categories(&body)
    }

    /// Retrieve all available payment methods.
    pub async fn payment_methods(&self) -> Result<Vec<PaymentMethod>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("PaymentMethods").session(session).build();
        let body = self.soap.call("PaymentMethods", envelope).await?;
        Self::parse_payment_methods(&body)
    }

    /// Parse a SearchDocuments or DocumentsInFolder SOAP response into a list of documents.
    ///
    /// Each `<Document ID="uuid">` element carries child elements for each field.
    /// The document ID is an XML attribute; all other fields are child text nodes.
    pub fn parse_archive_documents(xml: &str) -> Result<Vec<ArchiveDocument>, YukiError> {
        let mut reader = Reader::from_str(xml);

        let mut documents = Vec::new();
        let mut in_document = false;
        let mut field: Option<String> = None;
        let mut doc = ArchiveDocument {
            id: String::new(),
            subject: String::new(),
            document_date: String::new(),
            amount: String::new(),
            folder: String::new(),
            contact_name: String::new(),
            file_name: String::new(),
            reference: String::new(),
        };
        let mut content = ElementText::default();
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "Document" => {
                            in_document = true;
                            doc = ArchiveDocument {
                                id: String::new(),
                                subject: String::new(),
                                document_date: String::new(),
                                amount: String::new(),
                                folder: String::new(),
                                contact_name: String::new(),
                                file_name: String::new(),
                                reference: String::new(),
                            };
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == "ID" {
                                    doc.id = attr.value.into_owned();
                                }
                            }
                        }
                        "Subject" | "DocumentDate" | "Amount" | "Folder" | "ContactName"
                        | "FileName" | "Reference"
                            if in_document =>
                        {
                            field = Some(local);
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(ref e)) => {
                    let name = e.name();
                    let local = local_name(name.as_ref());
                    match local {
                        "Subject" | "DocumentDate" | "Amount" | "Folder" | "ContactName"
                        | "FileName" | "Reference" => {
                            let text = content.take();
                            if let Some(f) = field.take() {
                                match f.as_str() {
                                    "Subject" => doc.subject = text,
                                    "DocumentDate" => doc.document_date = text,
                                    "Amount" => doc.amount = text,
                                    "Folder" => doc.folder = text,
                                    "ContactName" => doc.contact_name = text,
                                    "FileName" => doc.file_name = text,
                                    "Reference" => doc.reference = text,
                                    _ => {}
                                }
                            }
                        }
                        "Document" => {
                            if !doc.id.is_empty() {
                                documents.push(doc.clone());
                            }
                            in_document = false;
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

        Ok(documents)
    }

    /// Parse a CostCategories SOAP response.
    ///
    /// Each `CostCategory` element carries an `ID` attribute and a `Description` child element:
    /// `<CostCategory ID="45100"><Description>...</Description></CostCategory>`
    pub fn parse_cost_categories(xml: &str) -> Result<Vec<CostCategory>, YukiError> {
        let mut reader = Reader::from_str(xml);

        let mut categories = Vec::new();
        let mut current_id = String::new();
        let mut current_desc = String::new();
        let mut in_category = false;
        let mut in_description = false;
        let mut content = ElementText::default();
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "CostCategory" => {
                            in_category = true;
                            current_id.clear();
                            current_desc.clear();
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == "ID" {
                                    current_id = attr.value.into_owned();
                                }
                            }
                        }
                        "Description" if in_category => {
                            in_description = true;
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(ref e)) => {
                    let name = e.name();
                    let local = local_name(name.as_ref());
                    match local {
                        "Description" if in_description => {
                            in_description = false;
                            current_desc = content.take();
                        }
                        "CostCategory" => {
                            if !current_id.is_empty() {
                                categories.push(CostCategory {
                                    id: current_id.clone(),
                                    description: current_desc.clone(),
                                });
                            }
                            in_category = false;
                        }
                        _ => {}
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(YukiError::Xml(e.to_string())),
                Ok(ref event) => content.push_if(in_description, event)?,
            }
            buf.clear();
        }

        Ok(categories)
    }

    /// Parse a PaymentMethods SOAP response.
    ///
    /// Each `PaymentMethod` element carries an `ID` attribute and a `Description` child element:
    /// `<PaymentMethod ID="4"><Description>...</Description></PaymentMethod>`
    pub fn parse_payment_methods(xml: &str) -> Result<Vec<PaymentMethod>, YukiError> {
        let mut reader = Reader::from_str(xml);

        let mut methods = Vec::new();
        let mut current_id = String::new();
        let mut current_desc = String::new();
        let mut in_method = false;
        let mut in_description = false;
        let mut content = ElementText::default();
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "PaymentMethod" => {
                            in_method = true;
                            current_id.clear();
                            current_desc.clear();
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == "ID" {
                                    current_id = attr.value.into_owned();
                                }
                            }
                        }
                        "Description" if in_method => {
                            in_description = true;
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(ref e)) => {
                    let name = e.name();
                    let local = local_name(name.as_ref());
                    match local {
                        "Description" if in_description => {
                            in_description = false;
                            current_desc = content.take();
                        }
                        "PaymentMethod" => {
                            if !current_id.is_empty() {
                                methods.push(PaymentMethod {
                                    id: current_id.clone(),
                                    description: current_desc.clone(),
                                });
                            }
                            in_method = false;
                        }
                        _ => {}
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(YukiError::Xml(e.to_string())),
                Ok(ref event) => content.push_if(in_description, event)?,
            }
            buf.clear();
        }

        Ok(methods)
    }
}

impl Default for ArchiveClient {
    fn default() -> Self {
        Self::new()
    }
}

/// How many records to request next, or `None` when the caller's limit is satisfied.
///
/// With no limit this always asks for a full page, so a folder larger than one page
/// keeps paging instead of being silently truncated at the first response.
pub(crate) fn next_page_size(
    limit: Option<usize>,
    collected: usize,
    page_size: usize,
) -> Option<usize> {
    match limit {
        Some(l) => {
            let remaining = l.saturating_sub(collected);
            (remaining > 0).then(|| remaining.min(page_size))
        }
        None => Some(page_size),
    }
}

#[cfg(test)]
mod page_tests {
    use super::next_page_size;

    #[test]
    fn unlimited_always_requests_a_full_page() {
        // Regression: numberOfRecords was hardcoded to 100, so folders with more
        // than 100 documents were truncated with no indication.
        assert_eq!(next_page_size(None, 0, 500), Some(500));
        assert_eq!(next_page_size(None, 500, 500), Some(500));
        assert_eq!(next_page_size(None, 100_000, 500), Some(500));
    }

    #[test]
    fn limit_larger_than_page_size_pages_repeatedly() {
        assert_eq!(next_page_size(Some(1200), 0, 500), Some(500));
        assert_eq!(next_page_size(Some(1200), 500, 500), Some(500));
        assert_eq!(next_page_size(Some(1200), 1000, 500), Some(200));
        assert_eq!(next_page_size(Some(1200), 1200, 500), None);
    }

    #[test]
    fn limit_smaller_than_page_size_requests_only_what_is_needed() {
        assert_eq!(next_page_size(Some(3), 0, 500), Some(3));
        assert_eq!(next_page_size(Some(3), 3, 500), None);
    }

    #[test]
    fn zero_limit_fetches_nothing() {
        assert_eq!(next_page_size(Some(0), 0, 500), None);
    }

    #[test]
    fn overshoot_does_not_underflow() {
        assert_eq!(next_page_size(Some(5), 9, 500), None);
    }
}
