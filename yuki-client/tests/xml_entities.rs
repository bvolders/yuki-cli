//! Regression tests: text containing entity or character references must be
//! parsed whole. Since quick-xml 0.38 such text arrives as several events
//! (`Text("Smith ")`, `GeneralRef("amp")`, `Text(" Jones fee")`), and parsers
//! that assigned per text event kept only the last fragment.

use yuki_client::client::SoapClient;
use yuki_client::client::accounting::AccountingClient;
use yuki_client::client::accounting_info::AccountingInfoClient;
use yuki_client::client::archive::ArchiveClient;
use yuki_client::client::contact::parse_contacts;
use yuki_client::client::sales::SalesClient;
use yuki_client::client::vat::VatClient;
use yuki_client::error::YukiError;

/// Escaped text with a predefined entity, `&lt;`/`&gt;`, a decimal and a hex
/// character reference, and surrounding whitespace that must be trimmed.
const RAW: &str = "  Smith &amp; Jones fee &lt;x&gt; caf&#233; &#x2713;  ";
const EXPECTED: &str = "Smith & Jones fee <x> café ✓";

fn soap(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    {body}
  </soap:Body>
</soap:Envelope>"#
    )
}

#[test]
fn gl_account_balances_keep_entities() {
    let xml = soap(&format!(
        r#"<GLAccountBalanceResponse xmlns="http://www.theyukicompany.com/"><GLAccountBalanceResult><GLAccountBalance xmlns="">
          <GLAccount Code="20200" BalanceType="B"><Description>{RAW}</Description><Amount>1.00</Amount></GLAccount>
        </GLAccountBalance></GLAccountBalanceResult></GLAccountBalanceResponse>"#
    ));
    let balances = AccountingClient::parse_gl_account_balances(&xml).unwrap();
    assert_eq!(balances[0].description, EXPECTED);
    assert_eq!(balances[0].amount, "1.00");
}

#[test]
fn gl_transactions_keep_entities() {
    let xml = soap(&format!(
        r#"<GLAccountTransactionsResponse xmlns="http://www.theyukicompany.com/"><GLAccountTransactionsResult><GLAccountTransactions xmlns="">
          <GLAccountTransaction ID="tx-001">
            <Date>2025-03-15</Date>
            <Description>{RAW}</Description>
            <Amount>500.00</Amount>
            <GLAccountCode>11001</GLAccountCode>
          </GLAccountTransaction>
        </GLAccountTransactions></GLAccountTransactionsResult></GLAccountTransactionsResponse>"#
    ));
    let txs = AccountingClient::parse_gl_transactions(&xml).unwrap();
    assert_eq!(txs[0].description, EXPECTED);
    assert_eq!(txs[0].date, "2025-03-15");
    assert_eq!(txs[0].gl_account, "11001");
}

#[test]
fn gl_transactions_with_contact_keep_entities() {
    let xml = soap(&format!(
        r#"<GLAccountTransactionsAndContactResponse xmlns="http://www.theyukicompany.com/"><GLAccountTransactionsAndContactResult><GLAccountTransactions xmlns="">
          <GLAccountTransaction ID="tx-001">
            <Date>2025-03-15</Date>
            <Description>{RAW}</Description>
            <Amount>-7.28</Amount>
            <GLAccountCode>11001</GLAccountCode>
            <ContactName>Smith &amp; Jones</ContactName>
          </GLAccountTransaction>
        </GLAccountTransactions></GLAccountTransactionsAndContactResult></GLAccountTransactionsAndContactResponse>"#
    ));
    let txs = AccountingClient::parse_gl_transactions_with_contact(&xml).unwrap();
    assert_eq!(txs[0].description, EXPECTED);
    assert_eq!(txs[0].contact_name, "Smith & Jones");
}

#[test]
fn administrations_keep_entities() {
    let xml = soap(&format!(
        r#"<AdministrationsResponse xmlns="http://www.theyukicompany.com/"><AdministrationsResult><Administrations xmlns="">
          <Administration ID="admin-001">
            <Name>{RAW}</Name>
            <DomainID>domain-001</DomainID>
          </Administration>
        </Administrations></AdministrationsResult></AdministrationsResponse>"#
    ));
    let admins = AccountingClient::parse_administrations(&xml).unwrap();
    assert_eq!(admins[0].name, EXPECTED);
    assert_eq!(admins[0].domain_id, "domain-001");
}

#[test]
fn outstanding_items_keep_entities() {
    let xml = soap(&format!(
        r#"<OutstandingDebtorItemsResponse xmlns="http://www.theyukicompany.com/"><OutstandingDebtorItemsResult>
          <Item>
            <ContactName>Smith &amp; Jones</ContactName>
            <Description>{RAW}</Description>
            <Date>2025-03-01</Date>
            <Amount>1000.00</Amount>
            <OpenAmount>500.00</OpenAmount>
          </Item>
        </OutstandingDebtorItemsResult></OutstandingDebtorItemsResponse>"#
    ));
    let items =
        AccountingClient::parse_outstanding_items(&xml, "OutstandingDebtorItemsResult").unwrap();
    assert_eq!(items[0].contact_name, "Smith & Jones");
    assert_eq!(items[0].description, EXPECTED);
    assert_eq!(items[0].open_amount, "500.00");
}

#[test]
fn transaction_details_keep_entities() {
    let xml = soap(&format!(
        r#"<GetTransactionDetailsResponse xmlns="http://www.theyukicompany.com/"><GetTransactionDetailsResult><TransactionDetails xmlns="">
          <TransactionInfo>
            <id>detail-001</id>
            <transactionDate>2025-03-15</transactionDate>
            <description>{RAW}</description>
            <transactionAmount>7.28</transactionAmount>
            <currency>EUR</currency>
            <glAccountCode>45100</glAccountCode>
          </TransactionInfo>
        </TransactionDetails></GetTransactionDetailsResult></GetTransactionDetailsResponse>"#
    ));
    let details = AccountingInfoClient::parse_transaction_details(&xml).unwrap();
    assert_eq!(details[0].description, EXPECTED);
    assert_eq!(details[0].currency, "EUR");
}

#[test]
fn gl_accounts_keep_entities() {
    let xml = soap(&format!(
        r#"<GetGLAccountSchemeResponse xmlns="http://www.theyukicompany.com/"><GetGLAccountSchemeResult>
          <GlAccount><code>01000</code><type>1</type><descripton>{RAW}</descripton></GlAccount>
        </GetGLAccountSchemeResult></GetGLAccountSchemeResponse>"#
    ));
    let accounts = AccountingInfoClient::parse_gl_accounts(&xml).unwrap();
    assert_eq!(accounts[0].description, EXPECTED);
    assert_eq!(accounts[0].code, "01000");
}

#[test]
fn start_balances_keep_entities() {
    let xml = soap(&format!(
        r#"<GetStartBalanceByGlAccountResponse xmlns="http://www.theyukicompany.com/"><GetStartBalanceByGlAccountResult>
          <AccountStartBalance><accountID>02300</accountID><startBalance>1216.53</startBalance><accountDescription>{RAW}</accountDescription></AccountStartBalance>
        </GetStartBalanceByGlAccountResult></GetStartBalanceByGlAccountResponse>"#
    ));
    let balances = AccountingInfoClient::parse_start_balances(&xml).unwrap();
    assert_eq!(balances[0].description, EXPECTED);
    assert_eq!(balances[0].balance, "1216.53");
}

#[test]
fn archive_documents_keep_entities() {
    let xml = soap(&format!(
        r#"<SearchDocumentsResponse xmlns="http://www.theyukicompany.com/"><SearchDocumentsResult><Documents xmlns="">
          <Document ID="doc-001">
            <Subject>{RAW}</Subject>
            <DocumentDate>2025-03-01</DocumentDate>
            <ContactName>Smith &amp; Jones</ContactName>
            <FileName>a&amp;b.pdf</FileName>
          </Document>
        </Documents></SearchDocumentsResult></SearchDocumentsResponse>"#
    ));
    let docs = ArchiveClient::parse_archive_documents(&xml).unwrap();
    assert_eq!(docs[0].subject, EXPECTED);
    assert_eq!(docs[0].contact_name, "Smith & Jones");
    assert_eq!(docs[0].file_name, "a&b.pdf");
}

#[test]
fn cost_categories_keep_entities() {
    let xml = soap(&format!(
        r#"<CostCategoriesResponse xmlns="http://www.theyukicompany.com/"><CostCategoriesResult><CostCategories xmlns="">
          <CostCategory ID="45100"><Description>{RAW}</Description></CostCategory>
        </CostCategories></CostCategoriesResult></CostCategoriesResponse>"#
    ));
    let cats = ArchiveClient::parse_cost_categories(&xml).unwrap();
    assert_eq!(cats[0].description, EXPECTED);
}

#[test]
fn payment_methods_keep_entities() {
    let xml = soap(&format!(
        r#"<PaymentMethodsResponse xmlns="http://www.theyukicompany.com/"><PaymentMethodsResult><PaymentMethods xmlns="">
          <PaymentMethod ID="4"><Description>{RAW}</Description></PaymentMethod>
        </PaymentMethods></PaymentMethodsResult></PaymentMethodsResponse>"#
    ));
    let methods = ArchiveClient::parse_payment_methods(&xml).unwrap();
    assert_eq!(methods[0].description, EXPECTED);
}

#[test]
fn contacts_keep_entities() {
    let xml = soap(&format!(
        r#"<SearchContactsResponse xmlns="http://www.theyukicompany.com/"><SearchContactsResult><Contacts xmlns="">
          <Contact ID="contact-001">
            <Name>{RAW}</Name>
            <Type>Supplier</Type>
            <IsSupplier>true</IsSupplier>
          </Contact>
        </Contacts></SearchContactsResult></SearchContactsResponse>"#
    ));
    let contacts = parse_contacts(&xml).unwrap();
    assert_eq!(contacts[0].name, EXPECTED);
    assert_eq!(contacts[0].contact_type, "Supplier");
    assert!(contacts[0].is_supplier);
}

#[test]
fn sales_items_keep_entities() {
    let xml = soap(&format!(
        r#"<GetSalesItemsResponse xmlns="http://www.theyukicompany.com/"><GetSalesItemsResult><SalesItems xmlns="">
          <SalesItem><id>item-001</id><description>{RAW}</description></SalesItem>
        </SalesItems></GetSalesItemsResult></GetSalesItemsResponse>"#
    ));
    let items = SalesClient::parse_sales_items(&xml).unwrap();
    assert_eq!(items[0].description, EXPECTED);
    assert_eq!(items[0].id, "item-001");
}

#[test]
fn vat_returns_keep_entities() {
    let xml = soap(
        r#"<VATReturnListResponse xmlns="http://www.theyukicompany.com/"><VATReturnListResult><VATReturns xmlns="">
          <VATReturnInfo>
            <startDate>2025-01-01T00:00:00</startDate>
            <endDate>2025-03-31T00:00:00</endDate>
            <status>Filed &amp; paid</status>
          </VATReturnInfo>
        </VATReturns></VATReturnListResult></VATReturnListResponse>"#,
    );
    let returns = VatClient::parse_vat_returns(&xml).unwrap();
    assert_eq!(returns[0].status, "Filed & paid");
    assert_eq!(returns[0].period, "2025-01-01 - 2025-03-31");
}

#[test]
fn vat_codes_keep_entities() {
    let xml = soap(&format!(
        r#"<ActiveVATCodesListResponse xmlns="http://www.theyukicompany.com/"><ActiveVATCodesListResult><VATCodes xmlns="">
          <VATCode><type>1</type><description>{RAW}</description></VATCode>
        </VATCodes></ActiveVATCodesListResult></ActiveVATCodesListResponse>"#
    ));
    let codes = VatClient::parse_vat_codes(&xml).unwrap();
    assert_eq!(codes[0].description, EXPECTED);
    assert_eq!(codes[0].code, "1");
}

#[test]
fn single_result_keeps_entities() {
    let xml = soap(&format!(
        r#"<AuthenticateResponse xmlns="http://www.theyukicompany.com/"><AuthenticateResult>{RAW}</AuthenticateResult></AuthenticateResponse>"#
    ));
    let result = SoapClient::parse_single_result(&xml, "AuthenticateResult").unwrap();
    assert_eq!(result, EXPECTED);
}

#[test]
fn soap_fault_keeps_entities() {
    let xml = soap(&format!(
        r#"<soap:Fault><faultcode>soap:Server</faultcode><faultstring>{RAW}</faultstring></soap:Fault>"#
    ));
    match SoapClient::parse_soap_fault(&xml) {
        Some(YukiError::SoapFault { code, message }) => {
            assert_eq!(code, "soap:Server");
            assert_eq!(message, EXPECTED);
        }
        other => panic!("expected SoapFault, got {other:?}"),
    }
}
