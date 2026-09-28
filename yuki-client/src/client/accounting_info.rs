use quick_xml::Reader;
use quick_xml::events::Event;

use crate::error::YukiError;

use super::soap_client::{SoapClient, SoapEnvelope};
use super::{ElementText, Region, local_name, service_url};

const SERVICE: &str = "AccountingInfo.asmx";

/// A general ledger account from the chart of accounts.
#[derive(Debug, Clone)]
pub struct GlAccount {
    pub code: String,
    pub description: String,
    pub account_type: String,
}

/// Opening balance for a GL account.
#[derive(Debug, Clone)]
pub struct AccountStartBalance {
    pub gl_account_code: String,
    pub description: String,
    pub balance: String,
}

/// A project entry.
#[derive(Debug, Clone)]
pub struct Project {
    pub id: String,
    pub code: String,
    pub description: String,
}

/// A project balance entry.
#[derive(Debug, Clone)]
pub struct ProjectBalance {
    pub project_code: String,
    pub gl_account_code: String,
    pub amount: String,
}

/// Full details for a single transaction line.
#[derive(Debug, Clone)]
pub struct TransactionDetail {
    pub id: String,
    pub date: String,
    pub description: String,
    pub amount: String,
    pub currency: String,
    pub gl_account_code: String,
}

/// Client for the Yuki AccountingInfo SOAP service.
pub struct AccountingInfoClient {
    soap: SoapClient,
}

impl AccountingInfoClient {
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

    /// Retrieve full details for a single transaction by ID.
    pub async fn get_transaction_details(
        &self,
        transaction_id: &str,
    ) -> Result<Vec<TransactionDetail>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("GetTransactionDetails")
            .session(session)
            .param("transactionId", transaction_id)
            .build();
        let body = self.soap.call("GetTransactionDetails", envelope).await?;
        Self::parse_transaction_details(&body)
    }

    /// Parse a GetTransactionDetails SOAP response into a list of `TransactionDetail` values.
    ///
    /// Each `TransactionInfo` element carries child elements `id`, `transactionDate`,
    /// `description`, `transactionAmount`, `currency`, and `glAccountCode`.
    pub fn parse_transaction_details(xml: &str) -> Result<Vec<TransactionDetail>, YukiError> {
        let mut reader = Reader::from_str(xml);

        let mut details = Vec::new();
        let mut in_info = false;
        let mut field: Option<String> = None;
        let mut current = TransactionDetail {
            id: String::new(),
            date: String::new(),
            description: String::new(),
            amount: String::new(),
            currency: String::new(),
            gl_account_code: String::new(),
        };
        let mut content = ElementText::default();
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "TransactionInfo" => {
                            in_info = true;
                            current = TransactionDetail {
                                id: String::new(),
                                date: String::new(),
                                description: String::new(),
                                amount: String::new(),
                                currency: String::new(),
                                gl_account_code: String::new(),
                            };
                        }
                        "id" | "transactionDate" | "description" | "transactionAmount"
                        | "currency" | "glAccountCode"
                            if in_info =>
                        {
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
                        "id" | "transactionDate" | "description" | "transactionAmount"
                        | "currency" | "glAccountCode" => {
                            let text = content.take();
                            if let Some(f) = field.take() {
                                match f.as_str() {
                                    "id" => current.id = text,
                                    "transactionDate" => current.date = text,
                                    "description" => current.description = text,
                                    "transactionAmount" => current.amount = text,
                                    "currency" => current.currency = text,
                                    "glAccountCode" => current.gl_account_code = text,
                                    _ => {}
                                }
                            }
                        }
                        "TransactionInfo" if in_info => {
                            details.push(current.clone());
                            in_info = false;
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

        Ok(details)
    }

    /// Retrieve the full GL account scheme (chart of accounts).
    pub async fn get_gl_account_scheme(
        &self,
        administration_id: &str,
    ) -> Result<Vec<GlAccount>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("GetGLAccountScheme")
            .session(session)
            .param("administrationID", administration_id)
            .build();
        let body = self.soap.call("GetGLAccountScheme", envelope).await?;
        Self::parse_gl_accounts(&body)
    }

    /// Retrieve the document linked to a transaction.
    pub async fn get_transaction_document(
        &self,
        administration_id: &str,
        transaction_id: &str,
    ) -> Result<String, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("GetTransactionDocument")
            .session(session)
            .param("administrationID", administration_id)
            .param("transactionID", transaction_id)
            .build();
        self.soap.call("GetTransactionDocument", envelope).await
    }

    /// Retrieve the period date table for a given fiscal year.
    pub async fn get_period_date_table(&self, year: &str) -> Result<String, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("GetPeriodDateTable")
            .session(session)
            .param("year", year)
            .build();
        self.soap.call("GetPeriodDateTable", envelope).await
    }

    /// Retrieve opening balances per GL account for a book year.
    pub async fn get_start_balance_by_gl_account(
        &self,
        administration_id: &str,
        bookyear: &str,
    ) -> Result<Vec<AccountStartBalance>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("GetStartBalanceByGlAccount")
            .session(session)
            .param("administrationID", administration_id)
            .param("bookyear", bookyear)
            .param("financialMode", "0")
            .build();
        let body = self
            .soap
            .call("GetStartBalanceByGlAccount", envelope)
            .await?;
        Self::parse_start_balances(&body)
    }

    /// Retrieve all projects.
    pub async fn get_projects(&self, administration_id: &str) -> Result<Vec<Project>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("GetProjectsAndID")
            .session(session)
            .param("administrationID", administration_id)
            .param("searchOption", "")
            .param("searchValue", "")
            .build();
        let body = self.soap.call("GetProjectsAndID", envelope).await?;
        Self::parse_projects(&body)
    }

    /// Retrieve project balance for a specific project and GL account over a date range.
    pub async fn get_project_balance(
        &self,
        administration_id: &str,
        project_code: &str,
        gl_account_code: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<Vec<ProjectBalance>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("GetProjectBalance")
            .session(session)
            .param("administrationID", administration_id)
            .param("GLAccountCode", gl_account_code)
            .param("projectCode", project_code)
            .param("StartDate", start_date)
            .param("EndDate", end_date)
            .build();
        let body = self.soap.call("GetProjectBalance", envelope).await?;
        Self::parse_project_balances(&body)
    }

    /// Parse a GetGLAccountScheme response into a list of `GlAccount` values.
    ///
    /// Each `GlAccount` element carries child elements for code, description, and type.
    pub fn parse_gl_accounts(xml: &str) -> Result<Vec<GlAccount>, YukiError> {
        let mut reader = Reader::from_str(xml);

        let mut accounts = Vec::new();
        let mut in_account = false;
        let mut field: Option<String> = None;
        let mut current = GlAccount {
            code: String::new(),
            description: String::new(),
            account_type: String::new(),
        };
        let mut content = ElementText::default();
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "GlAccount" | "GLAccount" => {
                            in_account = true;
                            current = GlAccount {
                                code: String::new(),
                                description: String::new(),
                                account_type: String::new(),
                            };
                        }
                        "Code" | "code" | "Description" | "description" | "descripton" | "Type"
                        | "type"
                            if in_account =>
                        {
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
                        "Code" | "code" | "Description" | "description" | "descripton" | "Type"
                        | "type" => {
                            let text = content.take();
                            if let Some(f) = field.take() {
                                match f.as_str() {
                                    "Code" | "code" => current.code = text,
                                    "Description" | "description" | "descripton" => {
                                        current.description = text;
                                    }
                                    "Type" | "type" => current.account_type = text,
                                    _ => {}
                                }
                            }
                        }
                        "GlAccount" | "GLAccount" if in_account => {
                            accounts.push(current.clone());
                            in_account = false;
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

        Ok(accounts)
    }

    pub fn parse_start_balances(xml: &str) -> Result<Vec<AccountStartBalance>, YukiError> {
        let mut reader = Reader::from_str(xml);

        let mut balances = Vec::new();
        let mut in_item = false;
        let mut field: Option<String> = None;
        let mut current = AccountStartBalance {
            gl_account_code: String::new(),
            description: String::new(),
            balance: String::new(),
        };
        let mut content = ElementText::default();
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "AccountStartBalance" => {
                            in_item = true;
                            current = AccountStartBalance {
                                gl_account_code: String::new(),
                                description: String::new(),
                                balance: String::new(),
                            };
                        }
                        "GLAccountCode" | "glAccountCode" | "accountID" | "Description"
                        | "description" | "accountDescription" | "Balance" | "balance"
                        | "StartBalance" | "startBalance"
                            if in_item =>
                        {
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
                        "GLAccountCode" | "glAccountCode" | "accountID" | "Description"
                        | "description" | "accountDescription" | "Balance" | "balance"
                        | "StartBalance" | "startBalance" => {
                            let text = content.take();
                            if let Some(f) = field.take() {
                                match f.as_str() {
                                    "GLAccountCode" | "glAccountCode" | "accountID" => {
                                        current.gl_account_code = text;
                                    }
                                    "Description" | "description" | "accountDescription" => {
                                        current.description = text;
                                    }
                                    "Balance" | "balance" | "StartBalance" | "startBalance" => {
                                        current.balance = text;
                                    }
                                    _ => {}
                                }
                            }
                        }
                        "AccountStartBalance" if in_item => {
                            balances.push(current.clone());
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

        Ok(balances)
    }

    fn parse_projects(xml: &str) -> Result<Vec<Project>, YukiError> {
        let mut reader = Reader::from_str(xml);

        let mut projects = Vec::new();
        let mut in_item = false;
        let mut field: Option<String> = None;
        let mut current = Project {
            id: String::new(),
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
                        "Project" => {
                            in_item = true;
                            current = Project {
                                id: String::new(),
                                code: String::new(),
                                description: String::new(),
                            };
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == "ID" {
                                    current.id = attr.value.into_owned();
                                }
                            }
                        }
                        "Code" | "code" | "Description" | "description" if in_item => {
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
                        "Code" | "code" | "Description" | "description" => {
                            let text = content.take();
                            if let Some(f) = field.take() {
                                match f.as_str() {
                                    "Code" | "code" => current.code = text,
                                    "Description" | "description" => current.description = text,
                                    _ => {}
                                }
                            }
                        }
                        "Project" if in_item => {
                            projects.push(current.clone());
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

        Ok(projects)
    }

    fn parse_project_balances(xml: &str) -> Result<Vec<ProjectBalance>, YukiError> {
        let mut reader = Reader::from_str(xml);

        let mut balances = Vec::new();
        let mut in_item = false;
        let mut field: Option<String> = None;
        let mut current = ProjectBalance {
            project_code: String::new(),
            gl_account_code: String::new(),
            amount: String::new(),
        };
        let mut content = ElementText::default();
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "ProjectBalance" => {
                            in_item = true;
                            current = ProjectBalance {
                                project_code: String::new(),
                                gl_account_code: String::new(),
                                amount: String::new(),
                            };
                        }
                        "ProjectCode" | "projectCode" | "GLAccountCode" | "glAccountCode"
                        | "Amount" | "amount"
                            if in_item =>
                        {
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
                        "ProjectCode" | "projectCode" | "GLAccountCode" | "glAccountCode"
                        | "Amount" | "amount" => {
                            let text = content.take();
                            if let Some(f) = field.take() {
                                match f.as_str() {
                                    "ProjectCode" | "projectCode" => current.project_code = text,
                                    "GLAccountCode" | "glAccountCode" => {
                                        current.gl_account_code = text
                                    }
                                    "Amount" | "amount" => current.amount = text,
                                    _ => {}
                                }
                            }
                        }
                        "ProjectBalance" if in_item => {
                            balances.push(current.clone());
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

        Ok(balances)
    }
}

impl Default for AccountingInfoClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod entity_tests {
    use super::AccountingInfoClient;

    #[test]
    fn projects_keep_entities() {
        let xml = r#"<Projects><Project ID="p-1"><Code>P&amp;1</Code><Description> Smith &amp; Jones fee &lt;x&gt; caf&#233; </Description></Project></Projects>"#;
        let projects = AccountingInfoClient::parse_projects(xml).unwrap();
        assert_eq!(projects[0].id, "p-1");
        assert_eq!(projects[0].code, "P&1");
        assert_eq!(projects[0].description, "Smith & Jones fee <x> café");
    }

    #[test]
    fn project_balances_keep_entities() {
        let xml = r#"<Balances><ProjectBalance><ProjectCode>R&amp;D</ProjectCode><GLAccountCode>&#52;5100</GLAccountCode><Amount>1.00</Amount></ProjectBalance></Balances>"#;
        let balances = AccountingInfoClient::parse_project_balances(xml).unwrap();
        assert_eq!(balances[0].project_code, "R&D");
        assert_eq!(balances[0].gl_account_code, "45100");
        assert_eq!(balances[0].amount, "1.00");
    }
}
