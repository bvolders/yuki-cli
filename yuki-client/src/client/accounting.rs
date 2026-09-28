use quick_xml::Reader;
use quick_xml::events::Event;

use crate::error::YukiError;

use super::soap_client::{SoapClient, SoapEnvelope};
use super::{local_name, unescape_text};

const BASE_URL: &str = "https://api.yukiworks.nl/ws/Accounting.asmx";

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
#[derive(Debug, Clone)]
pub struct GlTransactionWithContact {
    pub id: String,
    pub date: String,
    pub description: String,
    pub gl_account: String,
    pub amount: String,
    pub contact_name: String,
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
            soap: SoapClient::new(BASE_URL),
        }
    }

    /// Build over a caller-provided HTTP client, so a long-running consumer can
    /// share a single pooled client across all service clients.
    pub fn with_client(http: reqwest::Client) -> Self {
        Self {
            soap: SoapClient::with_client(BASE_URL, http),
        }
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
        reader.config_mut().trim_text(true);

        let mut balances = Vec::new();
        let mut in_account = false;
        let mut field: Option<String> = None;
        let mut current = GlAccountBalance {
            code: String::new(),
            description: String::new(),
            balance_type: String::new(),
            amount: String::new(),
        };
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
                Ok(Event::Text(ref e)) => {
                    if let Some(ref f) = field {
                        let text = unescape_text(e)
                            .map_err(|e| YukiError::Xml(e.to_string()))?
                            .trim()
                            .to_string();
                        match f.as_str() {
                            "Description" => current.description = text,
                            "Amount" => current.amount = text,
                            _ => {}
                        }
                    }
                }
                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "Description" | "Amount" => field = None,
                        "GLAccount" if in_account => {
                            balances.push(current.clone());
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

        Ok(balances)
    }

    /// Parse a GLAccountTransactions SOAP response into a list of `GlTransaction` values.
    ///
    /// Each `GLAccountTransaction` element carries an `ID` attribute and child elements
    /// `Date`, `Description`, `Amount`, and `GLAccountCode`.
    pub fn parse_gl_transactions(xml: &str) -> Result<Vec<GlTransaction>, YukiError> {
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);

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
                Ok(Event::Text(ref e)) => {
                    if let Some(ref f) = field {
                        let text = unescape_text(e)
                            .map_err(|e| YukiError::Xml(e.to_string()))?
                            .trim()
                            .to_string();
                        match f.as_str() {
                            "Date" => current.date = text,
                            "Description" => current.description = text,
                            "Amount" => current.amount = text,
                            "GLAccountCode" => current.gl_account = text,
                            _ => {}
                        }
                    }
                }
                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "Date" | "Description" | "Amount" | "GLAccountCode" => {
                            field = None;
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
                _ => {}
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
        reader.config_mut().trim_text(true);

        let mut transactions = Vec::new();
        let mut in_transaction = false;
        let mut field: Option<String> = None;
        let mut current = GlTransactionWithContact {
            id: String::new(),
            date: String::new(),
            description: String::new(),
            gl_account: String::new(),
            amount: String::new(),
            contact_name: String::new(),
        };
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "GLAccountTransaction" => {
                            in_transaction = true;
                            current = GlTransactionWithContact {
                                id: String::new(),
                                date: String::new(),
                                description: String::new(),
                                gl_account: String::new(),
                                amount: String::new(),
                                contact_name: String::new(),
                            };
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == "ID" {
                                    current.id = attr.value.into_owned();
                                }
                            }
                        }
                        "Date" | "Description" | "Amount" | "GLAccountCode" | "Contact"
                        | "ContactName"
                            if in_transaction =>
                        {
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
                            "Date" => current.date = text,
                            "Description" => current.description = text,
                            "Amount" => current.amount = text,
                            "GLAccountCode" => current.gl_account = text,
                            "Contact" | "ContactName" => current.contact_name = text,
                            _ => {}
                        }
                    }
                }
                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "Date" | "Description" | "Amount" | "GLAccountCode" | "Contact"
                        | "ContactName" => {
                            field = None;
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
                _ => {}
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
        reader.config_mut().trim_text(true);

        let mut administrations = Vec::new();
        let mut current_id = String::new();
        let mut current_name = String::new();
        let mut current_domain_id = String::new();
        let mut in_administration = false;
        let mut in_name = false;
        let mut in_domain_id = false;
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
                Ok(Event::Text(ref e)) => {
                    let text = unescape_text(e)
                        .map_err(|e| YukiError::Xml(e.to_string()))?
                        .trim()
                        .to_string();
                    if in_name {
                        current_name = text;
                    } else if in_domain_id {
                        current_domain_id = text;
                    }
                }
                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "Name" => in_name = false,
                        "DomainID" => in_domain_id = false,
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
                _ => {}
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
        reader.config_mut().trim_text(true);

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
                Ok(Event::Text(ref e)) => {
                    if let Some(ref f) = field {
                        let text = unescape_text(e)
                            .map_err(|e| YukiError::Xml(e.to_string()))?
                            .trim()
                            .to_string();
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
                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref()).to_string();
                    match local.as_str() {
                        "Contact" | "ContactName" | "Description" | "Date" | "Amount"
                        | "OriginalAmount" | "OpenAmount" => {
                            field = None;
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
                _ => {}
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
