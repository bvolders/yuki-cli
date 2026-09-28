use quick_xml::Reader;
use quick_xml::events::Event;

use crate::error::YukiError;

use super::soap_client::{SoapClient, SoapEnvelope};
use super::{ElementText, Region, local_name, service_url};

const SERVICE: &str = "Accounting.asmx";

/// A Yuki administration (company entity).
#[derive(Debug, Clone)]
pub struct Administration {
    pub id: String,
    pub name: String,
    pub domain_id: String,
}

/// An outstanding debtor or creditor item.
#[derive(Debug, Clone)]
pub struct OutstandingItem {
    pub contact_name: String,
    pub description: String,
    pub date: String,
    pub amount: String,
    pub open_amount: String,
}

/// A general ledger transaction.
#[derive(Debug, Clone)]
pub struct GlTransaction {
    pub id: String,
    pub date: String,
    pub description: String,
    pub gl_account: String,
    pub amount: String,
}

/// A general ledger transaction with contact information.
#[derive(Debug, Clone, Default)]
pub struct GlTransactionWithContact {
    pub id: String,
    pub date: String,
    pub description: String,
    pub gl_account: String,
    pub amount: String,
    pub contact_name: String,
    /// Yuki's `TransactionType`: the kind of journal the line came from, e.g.
    /// `9` for a purchase invoice or credit note and `0`/`10` for bank lines
    /// (as observed on Yuki Belgium). Empty when the response has none.
    pub transaction_type: String,
    /// Name of the document (e.g. the invoice PDF) the line is linked to;
    /// empty for a line without one.
    pub file_name: String,
}

/// A general ledger account balance as of a date, from `GLAccountBalance`.
///
/// The operation returns every account in one response, so callers filter by
/// `code`. `balance_type` is Yuki's `B` (balance sheet) / `W` (profit & loss)
/// marker.
#[derive(Debug, Clone)]
pub struct GlAccountBalance {
    pub code: String,
    pub description: String,
    pub balance_type: String,
    pub amount: String,
}

/// Client for the Yuki Accounting SOAP service.
pub struct AccountingClient {
    soap: SoapClient,
}

impl AccountingClient {
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

    /// List all administrations accessible with the current session.
    pub async fn administrations(&self) -> Result<Vec<Administration>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("Administrations")
            .session(session)
            .build();
        let body = self.soap.call("Administrations", envelope).await?;
        Self::parse_administrations(&body)
    }

    /// Set the active domain (administration) for subsequent calls.
    pub async fn set_current_domain(&mut self, domain_id: &str) -> Result<(), YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("SetCurrentDomain")
            .session(session)
            .param("domainID", domain_id)
            .build();
        self.soap.call("SetCurrentDomain", envelope).await?;
        Ok(())
    }

    /// Retrieve balances for all GL accounts as of a given date.
    ///
    /// Yuki's `GLAccountBalance` operation returns the full chart of accounts
    /// (balance sheet and profit & loss) in one response and ignores any
    /// account-code filter, so this returns every account; callers select the
    /// ones they need by [`GlAccountBalance::code`].
    pub async fn gl_account_balances(
        &self,
        administration_id: &str,
        transaction_date: &str,
    ) -> Result<Vec<GlAccountBalance>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("GLAccountBalance")
            .session(session)
            .param("administrationID", administration_id)
            .param("transactionDate", transaction_date)
            .build();
        let body = self.soap.call("GLAccountBalance", envelope).await?;
        Self::parse_gl_account_balances(&body)
    }

    /// Retrieve transactions for a GL account over a date range.
    pub async fn gl_account_transactions(
        &self,
        administration_id: &str,
        gl_account_code: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<String, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("GLAccountTransactions")
            .session(session)
            .param("administrationID", administration_id)
            .param("GLAccountCode", gl_account_code)
            .param("StartDate", start_date)
            .param("EndDate", end_date)
            .build();
        self.soap.call("GLAccountTransactions", envelope).await
    }

    /// Retrieve outstanding debtor items.
    pub async fn outstanding_debtor_items(
        &self,
        administration_id: &str,
    ) -> Result<Vec<OutstandingItem>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("OutstandingDebtorItems")
            .session(session)
            .param("administrationID", administration_id)
            .build();
        let body = self.soap.call("OutstandingDebtorItems", envelope).await?;
        Self::parse_outstanding_items(&body, "OutstandingDebtorItemsResult")
    }

    /// Retrieve outstanding debtor items filtered by date range.
    pub async fn outstanding_debtor_items_by_date(
        &self,
        administration_id: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<Vec<OutstandingItem>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("OutstandingDebtorItemsByDate")
            .session(session)
            .param("administrationID", administration_id)
            .param("startDate", start_date)
            .param("endDate", end_date)
            .build();
        let body = self
            .soap
            .call("OutstandingDebtorItemsByDate", envelope)
            .await?;
        Self::parse_outstanding_items(&body, "OutstandingDebtorItemsByDateResult")
    }

    /// Retrieve outstanding creditor items.
    pub async fn outstanding_creditor_items(
        &self,
        administration_id: &str,
    ) -> Result<Vec<OutstandingItem>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("OutstandingCreditorItems")
            .session(session)
            .param("administrationID", administration_id)
            .build();
        let body = self.soap.call("OutstandingCreditorItems", envelope).await?;
        Self::parse_outstanding_items(&body, "OutstandingCreditorItemsResult")
    }

    /// Retrieve outstanding creditor items filtered by date range.
    pub async fn outstanding_creditor_items_by_date(
        &self,
        administration_id: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<Vec<OutstandingItem>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("OutstandingCreditorItemsByDate")
            .session(session)
            .param("administrationID", administration_id)
            .param("startDate", start_date)
            .param("endDate", end_date)
            .build();
        let body = self
            .soap
            .call("OutstandingCreditorItemsByDate", envelope)
            .await?;
        Self::parse_outstanding_items(&body, "OutstandingCreditorItemsByDateResult")
    }

    /// Retrieve transactions for a GL account with contact info over a date range.
    pub async fn gl_account_transactions_and_contact(
        &self,
        administration_id: &str,
        gl_account_code: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<Vec<GlTransactionWithContact>, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("GLAccountTransactionsAndContact")
            .session(session)
            .param("administrationID", administration_id)
            .param("GLAccountCode", gl_account_code)
            .param("StartDate", start_date)
            .param("EndDate", end_date)
            .build();
        let body = self
            .soap
            .call("GLAccountTransactionsAndContact", envelope)
            .await?;
        Self::parse_gl_transactions_with_contact(&body)
    }

    /// Retrieve net revenue for a date range.
    pub async fn net_revenue(
        &self,
        administration_id: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<String, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("NetRevenue")
            .session(session)
            .param("administrationID", administration_id)
            .param("StartDate", start_date)
            .param("EndDate", end_date)
            .build();
        let body = self.soap.call("NetRevenue", envelope).await?;
        SoapClient::parse_single_result(&body, "NetRevenueResult")
    }

    /// Check if a specific reference is still outstanding in an administration.
    pub async fn check_outstanding_item_admin(
        &self,
        administration_id: &str,
        reference: &str,
    ) -> Result<String, YukiError> {
        let session = self.require_session()?;
        let envelope = SoapEnvelope::new("CheckOutstandingItemAdmin")
            .session(session)
            .param("administrationID", administration_id)
            .param("Reference", reference)
            .build();
        self.soap.call("CheckOutstandingItemAdmin", envelope).await
    }

    /// Parse a GLAccountBalance SOAP response into per-account balances.
    ///
    /// Each repeating `GLAccount` element carries `Code` and `BalanceType`
    /// attributes and child elements `Description` and `Amount`:
    /// `<GLAccount Code="20200" BalanceType="B"><Description>RC Ruben Jongejan</Description><Amount>3472.31</Amount></GLAccount>`
    pub fn parse_gl_account_balances(xml: &str) -> Result<Vec<GlAccountBalance>, YukiError> {
        if let Some(err) = SoapClient::parse_soap_fault(xml) {
            return Err(err);
        }
        let mut reader = Reader::from_str(xml);

        let mut balances = Vec::new();
        let mut in_account = false;
        let mut field: Option<String> = None;
        let mut current = GlAccountBalance {
            code: String::new(),
            description: String::new(),
            balance_type: String::new(),
            amount: String::new(),
        };
        let mut content = ElementText::default();
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "GLAccount" => {
                            in_account = true;
                            current = GlAccountBalance {
                                code: String::new(),
                                description: String::new(),
                                balance_type: String::new(),
                                amount: String::new(),
                            };
                            for attr in e.attributes().flatten() {
                                match attr.key.as_ref() {
                                    "Code" => {
                                        current.code = attr.value.into_owned();
                                    }
                                    "BalanceType" => {
                                        current.balance_type = attr.value.into_owned();
                                    }
                                    _ => {}
                                }
                            }
                        }
                        "Description" | "Amount" if in_account => {
                            field = Some(local);
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "Description" | "Amount" => {
                            let text = content.take();
                            if let Some(f) = field.take() {
                                match f.as_str() {
                                    "Description" => current.description = text,
                                    "Amount" => current.amount = text,
                                    _ => {}
                                }
                            }
                        }
                        "GLAccount" if in_account => {
                            balances.push(current.clone());
                            in_account = false;
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

        Ok(balances)
    }

    /// Parse a GLAccountTransactions SOAP response into a list of `GlTransaction` values.
    ///
    /// Each `GLAccountTransaction` element carries an `ID` attribute and child elements
    /// `Date`, `Description`, `Amount`, and `GLAccountCode`.
    pub fn parse_gl_transactions(xml: &str) -> Result<Vec<GlTransaction>, YukiError> {
        let mut reader = Reader::from_str(xml);

        let mut transactions = Vec::new();
        let mut in_transaction = false;
        let mut field: Option<String> = None;
        let mut current = GlTransaction {
            id: String::new(),
            date: String::new(),
            description: String::new(),
            gl_account: String::new(),
            amount: String::new(),
        };
        let mut content = ElementText::default();
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "GLAccountTransaction" => {
                            in_transaction = true;
                            current = GlTransaction {
                                id: String::new(),
                                date: String::new(),
                                description: String::new(),
                                gl_account: String::new(),
                                amount: String::new(),
                            };
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == "ID" {
                                    current.id = attr.value.into_owned();
                                }
                            }
                        }
                        "Date" | "Description" | "Amount" | "GLAccountCode" if in_transaction => {
                            field = Some(local);
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "Date" | "Description" | "Amount" | "GLAccountCode" => {
                            let text = content.take();
                            if let Some(f) = field.take() {
                                match f.as_str() {
                                    "Date" => current.date = text,
                                    "Description" => current.description = text,
                                    "Amount" => current.amount = text,
                                    "GLAccountCode" => current.gl_account = text,
                                    _ => {}
                                }
                            }
                        }
                        "GLAccountTransaction" if in_transaction => {
                            transactions.push(current.clone());
                            in_transaction = false;
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

        Ok(transactions)
    }

    /// Parse a GLAccountTransactionsAndContact SOAP response.
    ///
    /// Like `parse_gl_transactions` but also captures `Contact`/`ContactName`.
    pub fn parse_gl_transactions_with_contact(
        xml: &str,
    ) -> Result<Vec<GlTransactionWithContact>, YukiError> {
        let mut reader = Reader::from_str(xml);

        let mut transactions = Vec::new();
        let mut in_transaction = false;
        let mut field: Option<String> = None;
        let mut current = GlTransactionWithContact::default();
        let mut content = ElementText::default();
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "GLAccountTransaction" => {
                            in_transaction = true;
                            current = GlTransactionWithContact::default();
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == "ID" {
                                    current.id = attr.value.into_owned();
                                }
                            }
                        }
                        "Date" | "Description" | "Amount" | "GLAccountCode" | "Contact"
                        | "ContactName" | "TransactionType" | "FileName"
                            if in_transaction =>
                        {
                            field = Some(local);
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "Date" | "Description" | "Amount" | "GLAccountCode" | "Contact"
                        | "ContactName" | "TransactionType" | "FileName" => {
                            let text = content.take();
                            if let Some(f) = field.take() {
                                match f.as_str() {
                                    "Date" => current.date = text,
                                    "Description" => current.description = text,
                                    "Amount" => current.amount = text,
                                    "GLAccountCode" => current.gl_account = text,
                                    "Contact" | "ContactName" => current.contact_name = text,
                                    "TransactionType" => current.transaction_type = text,
                                    "FileName" => current.file_name = text,
                                    _ => {}
                                }
                            }
                        }
                        "GLAccountTransaction" if in_transaction => {
                            transactions.push(current.clone());
                            in_transaction = false;
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

        Ok(transactions)
    }

    /// Parse an Administrations SOAP response into a list of `Administration` values.
    ///
    /// The Yuki API returns Administration elements with the ID as an XML attribute
    /// and Name as a child element:
    /// `<Administration ID="uuid"><Name>Company</Name>...</Administration>`
    pub fn parse_administrations(xml: &str) -> Result<Vec<Administration>, YukiError> {
        let mut reader = Reader::from_str(xml);

        let mut administrations = Vec::new();
        let mut current_id = String::new();
        let mut current_name = String::new();
        let mut current_domain_id = String::new();
        let mut in_administration = false;
        let mut in_name = false;
        let mut in_domain_id = false;
        let mut content = ElementText::default();
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "Administration" => {
                            in_administration = true;
                            current_id.clear();
                            current_name.clear();
                            current_domain_id.clear();
                            // ID is an attribute on the Administration element
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == "ID" {
                                    current_id = attr.value.into_owned();
                                }
                            }
                        }
                        "Name" if in_administration => in_name = true,
                        "DomainID" if in_administration => in_domain_id = true,
                        _ => {}
                    }
                }
                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "Name" if in_name => {
                            in_name = false;
                            current_name = content.take();
                        }
                        "DomainID" if in_domain_id => {
                            in_domain_id = false;
                            current_domain_id = content.take();
                        }
                        "Administration" => {
                            if !current_id.is_empty() {
                                administrations.push(Administration {
                                    id: current_id.clone(),
                                    name: current_name.clone(),
                                    domain_id: current_domain_id.clone(),
                                });
                            }
                            in_administration = false;
                        }
                        _ => {}
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(YukiError::Xml(e.to_string())),
                Ok(ref event) => content.push_if(in_name || in_domain_id, event)?,
            }
            buf.clear();
        }

        Ok(administrations)
    }

    /// Parse an outstanding items SOAP response into a list of `OutstandingItem` values.
    ///
    /// The `result_tag` identifies the wrapper element in the response
    /// (e.g. `"OutstandingDebtorItemsResult"`).
    pub fn parse_outstanding_items(
        xml: &str,
        result_tag: &str,
    ) -> Result<Vec<OutstandingItem>, YukiError> {
        let mut reader = Reader::from_str(xml);

        let mut items = Vec::new();
        let mut in_result = false;
        let mut in_item = false;
        let mut field: Option<String> = None;
        let mut current = OutstandingItem {
            contact_name: String::new(),
            description: String::new(),
            date: String::new(),
            amount: String::new(),
            open_amount: String::new(),
        };
        let mut content = ElementText::default();
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        tag if tag == result_tag => in_result = true,
                        "Item" if in_result => {
                            in_item = true;
                            current = OutstandingItem {
                                contact_name: String::new(),
                                description: String::new(),
                                date: String::new(),
                                amount: String::new(),
                                open_amount: String::new(),
                            };
                        }
                        "Contact" | "ContactName" | "Description" | "Date" | "Amount"
                        | "OriginalAmount" | "OpenAmount"
                            if in_item =>
                        {
                            field = Some(local);
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "Contact" | "ContactName" | "Description" | "Date" | "Amount"
                        | "OriginalAmount" | "OpenAmount" => {
                            let text = content.take();
                            if let Some(f) = field.take() {
                                match f.as_str() {
                                    "Contact" | "ContactName" => current.contact_name = text,
                                    "Description" => current.description = text,
                                    "Date" => current.date = text,
                                    "Amount" | "OriginalAmount" => current.amount = text,
                                    "OpenAmount" => current.open_amount = text,
                                    _ => {}
                                }
                            }
                        }
                        "Item" if in_item => {
                            items.push(current.clone());
                            in_item = false;
                        }
                        tag if tag == result_tag => {
                            in_result = false;
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

impl Default for AccountingClient {
    fn default() -> Self {
        Self::new()
    }
}
