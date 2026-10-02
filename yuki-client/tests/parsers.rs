use yuki_client::client::accounting::{AccountingClient, TransactionType};
use yuki_client::client::accounting_info::AccountingInfoClient;
use yuki_client::client::archive::ArchiveClient;
use yuki_client::client::contact::parse_contacts;
use yuki_client::client::sales::SalesClient;
use yuki_client::client::vat::VatClient;

#[test]
fn parses_gl_transactions() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <GLAccountTransactionsResponse xmlns="http://www.theyukicompany.com/">
      <GLAccountTransactionsResult>
        <GLAccountTransactions xmlns="">
          <GLAccountTransaction ID="tx-001">
            <Date>2025-03-15</Date>
            <Description>Payment received</Description>
            <Amount>500.00</Amount>
            <GLAccountCode>11001</GLAccountCode>
          </GLAccountTransaction>
          <GLAccountTransaction ID="tx-002">
            <Date>2025-03-20</Date>
            <Description>Invoice payment</Description>
            <Amount>-125.50</Amount>
            <GLAccountCode>11001</GLAccountCode>
          </GLAccountTransaction>
        </GLAccountTransactions>
      </GLAccountTransactionsResult>
    </GLAccountTransactionsResponse>
  </soap:Body>
</soap:Envelope>"#;

    let txs = AccountingClient::parse_gl_transactions(xml).unwrap();
    assert_eq!(txs.len(), 2);
    assert_eq!(txs[0].id, "tx-001");
    assert_eq!(txs[0].date, "2025-03-15");
    assert_eq!(txs[0].description, "Payment received");
    assert_eq!(txs[0].amount, "500.00");
    assert_eq!(txs[0].gl_account, "11001");
    assert_eq!(txs[1].id, "tx-002");
    assert_eq!(txs[1].amount, "-125.50");
}

#[test]
fn parses_gl_transactions_empty() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <GLAccountTransactionsResponse xmlns="http://www.theyukicompany.com/">
      <GLAccountTransactionsResult />
    </GLAccountTransactionsResponse>
  </soap:Body>
</soap:Envelope>"#;

    let txs = AccountingClient::parse_gl_transactions(xml).unwrap();
    assert!(txs.is_empty());
}

#[test]
fn parses_gl_transactions_with_contact() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <GLAccountTransactionsAndContactResponse xmlns="http://www.theyukicompany.com/">
      <GLAccountTransactionsAndContactResult>
        <GLAccountTransactions xmlns="">
          <GLAccountTransaction ID="tx-001">
            <Date>2025-03-15</Date>
            <Description>Hetzner hosting</Description>
            <Amount>-7.28</Amount>
            <GLAccountCode>11001</GLAccountCode>
            <ContactName>Hetzner Online GmbH</ContactName>
          </GLAccountTransaction>
          <GLAccountTransaction ID="tx-002">
            <Date>2025-03-20</Date>
            <Description>Unknown debit</Description>
            <Amount>-50.00</Amount>
            <GLAccountCode>11001</GLAccountCode>
          </GLAccountTransaction>
        </GLAccountTransactions>
      </GLAccountTransactionsAndContactResult>
    </GLAccountTransactionsAndContactResponse>
  </soap:Body>
</soap:Envelope>"#;

    let txs = AccountingClient::parse_gl_transactions_with_contact(xml).unwrap();
    assert_eq!(txs.len(), 2);
    assert_eq!(txs[0].id, "tx-001");
    assert_eq!(txs[0].contact_name, "Hetzner Online GmbH");
    assert_eq!(txs[0].amount, "-7.28");
    assert_eq!(txs[1].contact_name, "");
    assert_eq!(txs[0].transaction_type, None, "absent means unknown");
}

/// Belgian (CODA) bank lines: no contact on an unprocessed line, and a GL code in
/// `Contact` for a line booked straight to a ledger account. Data is made up.
#[test]
fn parses_gl_transactions_with_contact_from_a_belgian_bank_account() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <GLAccountTransactionsAndContactResponse xmlns="http://www.theyukicompany.com/">
      <GLAccountTransactionsAndContactResult>
        <GLAccountTransactions xmlns="">
          <GLAccountTransaction ID="tx-be-1"><Date>2026-07-07</Date><Description>Binnenlandse overschrijvingen - SEPA credit transfers : Enkelvoudige overschrijving | Netto bedrag: 88,110 : Overschrijving | EXAMPLE PARTNERS BV</Description><Amount>-88.11</Amount><SalesItem /><Project></Project><GLAccountCode>550003</GLAccountCode><FileName></FileName><TransactionType>0</TransactionType></GLAccountTransaction>
          <GLAccountTransaction ID="tx-be-2"><Date>2026-07-06</Date><Description>Kaarten : Betaling met debetkaart binnen eurozone | Netto bedrag: 4,560 : | Debet ATM/POS</Description><Amount>-4.56</Amount><SalesItem /><Contact>657100</Contact><ContactID>00000000-0000-0000-0000-000000000001</ContactID><Project></Project><GLAccountCode>550003</GLAccountCode><FileName>Example &amp; Co - 0001.pdf</FileName><TransactionType>10</TransactionType></GLAccountTransaction>
        </GLAccountTransactions>
      </GLAccountTransactionsAndContactResult>
    </GLAccountTransactionsAndContactResponse>
  </soap:Body>
</soap:Envelope>"#;

    let txs = AccountingClient::parse_gl_transactions_with_contact(xml).unwrap();
    assert_eq!(txs.len(), 2);
    assert!(txs[0].description.ends_with("| EXAMPLE PARTNERS BV"));
    assert_eq!(txs[0].contact_name, "");
    assert_eq!(txs[0].gl_account, "550003");
    assert_eq!(txs[1].contact_name, "657100");
    assert_eq!(txs[1].amount, "-4.56");
    // The journal type separates bank lines from purchase documents.
    assert_eq!(txs[0].transaction_type, Some(TransactionType::Bank));
    assert_eq!(txs[1].transaction_type, Some(TransactionType::Bank));
    assert_eq!(txs[0].file_name, "");
    assert_eq!(txs[1].file_name, "Example & Co - 0001.pdf");
}

#[test]
fn parses_archive_documents() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <SearchDocumentsResponse xmlns="http://www.theyukicompany.com/">
      <SearchDocumentsResult>
        <Documents xmlns="">
          <Document ID="doc-001">
            <Subject>Hetzner Invoice</Subject>
            <DocumentDate>2025-03-01</DocumentDate>
            <Amount>7.28</Amount>
            <Folder>inkoop</Folder>
            <ContactName>Hetzner</ContactName>
            <FileName>invoice.pdf</FileName>
            <Reference>INV-2025-001</Reference>
          </Document>
        </Documents>
      </SearchDocumentsResult>
    </SearchDocumentsResponse>
  </soap:Body>
</soap:Envelope>"#;

    let docs = ArchiveClient::parse_archive_documents(xml).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].id, "doc-001");
    assert_eq!(docs[0].subject, "Hetzner Invoice");
    assert_eq!(docs[0].document_date, "2025-03-01");
    assert_eq!(docs[0].amount, "7.28");
    assert_eq!(docs[0].folder, "inkoop");
    assert_eq!(docs[0].contact_name, "Hetzner");
    assert_eq!(docs[0].file_name, "invoice.pdf");
    assert_eq!(docs[0].reference, "INV-2025-001");
}

#[test]
fn parses_contacts() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <SearchContactsResponse xmlns="http://www.theyukicompany.com/">
      <SearchContactsResult>
        <Contacts xmlns="">
          <Contact ID="contact-001">
            <Name>Hetzner Online GmbH</Name>
            <Type>Supplier</Type>
            <Country>DE</Country>
            <IsSupplier>true</IsSupplier>
            <IsCustomer>false</IsCustomer>
          </Contact>
          <Contact ID="contact-002">
            <Name>Customer B.V.</Name>
            <Type>Customer</Type>
            <Country>NL</Country>
            <IsSupplier>false</IsSupplier>
            <IsCustomer>true</IsCustomer>
          </Contact>
        </Contacts>
      </SearchContactsResult>
    </SearchContactsResponse>
  </soap:Body>
</soap:Envelope>"#;

    let contacts = parse_contacts(xml).unwrap();
    assert_eq!(contacts.len(), 2);
    assert_eq!(contacts[0].id, "contact-001");
    assert_eq!(contacts[0].name, "Hetzner Online GmbH");
    assert_eq!(contacts[0].contact_type, "Supplier");
    assert_eq!(contacts[0].country, "DE");
    assert!(contacts[0].is_supplier);
    assert!(!contacts[0].is_customer);
    assert_eq!(contacts[1].id, "contact-002");
    assert!(contacts[1].is_customer);
    assert!(!contacts[1].is_supplier);
}

/// The fields an invoice template needs; a real contact's `Code` is often empty.
#[test]
fn parses_contact_code_hid_city_and_vat_number() {
    let xml = r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body>
<SearchContactsResponse xmlns="http://www.theyukicompany.com/"><SearchContactsResult><Contacts xmlns="">
  <Contact ID="c-1"><HID>42</HID><Code /><Name>Example &amp; Co BV</Name><Type>Customer</Type>
    <City>Gent</City><Country>BE</Country><VATNumber>BE0123456789</VATNumber>
    <IsSupplier>false</IsSupplier><IsCustomer>true</IsCustomer></Contact>
  <Contact ID="c-2"><HID>43</HID><Code>C0043</Code><Name>Other</Name></Contact>
</Contacts></SearchContactsResult></SearchContactsResponse></soap:Body></soap:Envelope>"#;
    let contacts = parse_contacts(xml).unwrap();
    assert_eq!(contacts.len(), 2);
    let c = &contacts[0];
    assert_eq!(
        (c.hid.as_str(), c.code.as_str(), c.name.as_str()),
        ("42", "", "Example & Co BV")
    );
    assert_eq!(
        (c.city.as_str(), c.vat_number.as_str()),
        ("Gent", "BE0123456789")
    );
    assert_eq!(contacts[1].code, "C0043");
}

#[test]
fn parses_cost_categories() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <CostCategoriesResponse xmlns="http://www.theyukicompany.com/">
      <CostCategoriesResult>
        <CostCategories xmlns="">
          <CostCategory ID="45100">
            <Description>Kantoorkosten</Description>
          </CostCategory>
          <CostCategory ID="45200">
            <Description>Reis- en verblijfkosten</Description>
          </CostCategory>
        </CostCategories>
      </CostCategoriesResult>
    </CostCategoriesResponse>
  </soap:Body>
</soap:Envelope>"#;

    let cats = ArchiveClient::parse_cost_categories(xml).unwrap();
    assert_eq!(cats.len(), 2);
    assert_eq!(cats[0].id, "45100");
    assert_eq!(cats[0].description, "Kantoorkosten");
    assert_eq!(cats[1].id, "45200");
}

#[test]
fn parses_payment_methods() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <PaymentMethodsResponse xmlns="http://www.theyukicompany.com/">
      <PaymentMethodsResult>
        <PaymentMethods xmlns="">
          <PaymentMethod ID="1">
            <Description>Contant</Description>
          </PaymentMethod>
          <PaymentMethod ID="4">
            <Description>Pinpas</Description>
          </PaymentMethod>
        </PaymentMethods>
      </PaymentMethodsResult>
    </PaymentMethodsResponse>
  </soap:Body>
</soap:Envelope>"#;

    let methods = ArchiveClient::parse_payment_methods(xml).unwrap();
    assert_eq!(methods.len(), 2);
    assert_eq!(methods[0].id, "1");
    assert_eq!(methods[0].description, "Contant");
    assert_eq!(methods[1].id, "4");
    assert_eq!(methods[1].description, "Pinpas");
}

#[test]
fn parses_transaction_details() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <GetTransactionDetailsResponse xmlns="http://www.theyukicompany.com/">
      <GetTransactionDetailsResult>
        <TransactionDetails xmlns="">
          <TransactionInfo>
            <id>detail-001</id>
            <transactionDate>2025-03-15</transactionDate>
            <description>Hosting fee</description>
            <transactionAmount>7.28</transactionAmount>
            <currency>EUR</currency>
            <glAccountCode>45100</glAccountCode>
          </TransactionInfo>
        </TransactionDetails>
      </GetTransactionDetailsResult>
    </GetTransactionDetailsResponse>
  </soap:Body>
</soap:Envelope>"#;

    let details = AccountingInfoClient::parse_transaction_details(xml).unwrap();
    assert_eq!(details.len(), 1);
    assert_eq!(details[0].id, "detail-001");
    assert_eq!(details[0].date, "2025-03-15");
    assert_eq!(details[0].description, "Hosting fee");
    assert_eq!(details[0].amount, "7.28");
    assert_eq!(details[0].currency, "EUR");
    assert_eq!(details[0].gl_account_code, "45100");
}

#[test]
fn transaction_details_envelope_matches_the_wsdl() {
    // AccountingInfo.GetTransactionDetails takes an administration, a GL account
    // and a date range; it has no transaction-id parameter.
    let envelope = AccountingInfoClient::transaction_details_envelope(
        "session-1",
        "admin-1",
        "400000",
        "2025-01-01",
        "2025-12-31",
    );
    assert!(envelope.contains("<yuki:sessionID>session-1</yuki:sessionID>"));
    assert!(envelope.contains("<yuki:administrationID>admin-1</yuki:administrationID>"));
    assert!(envelope.contains("<yuki:GLAccountCode>400000</yuki:GLAccountCode>"));
    assert!(envelope.contains("<yuki:StartDate>2025-01-01</yuki:StartDate>"));
    assert!(envelope.contains("<yuki:EndDate>2025-12-31</yuki:EndDate>"));
    assert!(envelope.contains("<yuki:financialMode>0</yuki:financialMode>"));
    assert!(!envelope.to_lowercase().contains("transactionid"));
}

#[test]
fn parses_transaction_details_as_returned_by_the_api() {
    // Shape per the AccountingInfo WSDL: TransactionInfo directly under the result.
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <GetTransactionDetailsResponse xmlns="http://www.theyukicompany.com/">
      <GetTransactionDetailsResult>
        <TransactionInfo>
          <id>tx-1</id>
          <hID>101</hID>
          <transactionDate>2025-03-15T00:00:00</transactionDate>
          <description>Invoice 2025-001</description>
          <transactionAmount>121.00</transactionAmount>
          <currency>EUR</currency>
          <fullName>Acme B.V.</fullName>
          <glAccountCode>400000</glAccountCode>
        </TransactionInfo>
        <TransactionInfo>
          <id>tx-2</id>
          <transactionDate>2025-04-01T00:00:00</transactionDate>
          <description>Invoice 2025-002</description>
          <transactionAmount>-50.00</transactionAmount>
          <currency>EUR</currency>
          <glAccountCode>400000</glAccountCode>
        </TransactionInfo>
      </GetTransactionDetailsResult>
    </GetTransactionDetailsResponse>
  </soap:Body>
</soap:Envelope>"#;

    let details = AccountingInfoClient::parse_transaction_details(xml).unwrap();
    assert_eq!(details.len(), 2);
    assert_eq!(details[0].id, "tx-1");
    assert_eq!(details[0].contact_name, "Acme B.V.");
    assert_eq!(details[1].id, "tx-2");
    assert_eq!(details[1].contact_name, "");
    assert_eq!(details[1].amount, "-50.00");
}

#[test]
fn parses_vat_returns() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <VATReturnListResponse xmlns="http://www.theyukicompany.com/">
      <VATReturnListResult>
        <VATReturns xmlns="">
          <VATReturnInfo>
            <startDate>2025-01-01T00:00:00</startDate>
            <endDate>2025-03-31T00:00:00</endDate>
            <status>Filed</status>
          </VATReturnInfo>
          <VATReturnInfo>
            <startDate>2025-04-01T00:00:00</startDate>
            <endDate>2025-06-30T00:00:00</endDate>
            <status>Open</status>
          </VATReturnInfo>
        </VATReturns>
      </VATReturnListResult>
    </VATReturnListResponse>
  </soap:Body>
</soap:Envelope>"#;

    let returns = VatClient::parse_vat_returns(xml).unwrap();
    assert_eq!(returns.len(), 2);
    assert_eq!(returns[0].start_date, "2025-01-01T00:00:00");
    assert_eq!(returns[0].end_date, "2025-03-31T00:00:00");
    assert_eq!(returns[0].status, "Filed");
    assert_eq!(returns[0].period, "2025-01-01 - 2025-03-31");
    assert_eq!(returns[1].status, "Open");
    assert_eq!(returns[1].period, "2025-04-01 - 2025-06-30");
}

#[test]
fn parses_vat_codes() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <ActiveVATCodesListResponse xmlns="http://www.theyukicompany.com/">
      <ActiveVATCodesListResult>
        <VATCodes xmlns="">
          <VATCode>
            <type>1</type>
            <description>BTW 21%</description>
          </VATCode>
          <VATCode>
            <type>2</type>
            <description>BTW 9%</description>
          </VATCode>
        </VATCodes>
      </ActiveVATCodesListResult>
    </ActiveVATCodesListResponse>
  </soap:Body>
</soap:Envelope>"#;

    let codes = VatClient::parse_vat_codes(xml).unwrap();
    assert_eq!(codes.len(), 2);
    assert_eq!(codes[0].code, "1");
    assert_eq!(codes[0].description, "BTW 21%");
    assert_eq!(codes[1].code, "2");
    assert_eq!(codes[1].description, "BTW 9%");
}

#[test]
fn parses_sales_items() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <GetSalesItemsResponse xmlns="http://www.theyukicompany.com/">
      <GetSalesItemsResult>
        <SalesItems xmlns="">
          <SalesItem>
            <id>item-001</id>
            <description>Consulting services</description>
          </SalesItem>
          <SalesItem>
            <id>item-002</id>
            <description>Software license</description>
          </SalesItem>
        </SalesItems>
      </GetSalesItemsResult>
    </GetSalesItemsResponse>
  </soap:Body>
</soap:Envelope>"#;

    let items = SalesClient::parse_sales_items(xml).unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].id, "item-001");
    assert_eq!(items[0].description, "Consulting services");
    assert_eq!(items[1].id, "item-002");
    assert_eq!(items[1].description, "Software license");
}

#[test]
fn parses_outstanding_creditor_items() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <OutstandingCreditorItemsResponse xmlns="http://www.theyukicompany.com/">
      <OutstandingCreditorItemsResult>
        <Item>
          <Contact>Supplier X</Contact>
          <Description>Purchase order 42</Description>
          <Date>2025-06-01</Date>
          <OriginalAmount>250.00</OriginalAmount>
          <OpenAmount>250.00</OpenAmount>
        </Item>
      </OutstandingCreditorItemsResult>
    </OutstandingCreditorItemsResponse>
  </soap:Body>
</soap:Envelope>"#;

    let items =
        AccountingClient::parse_outstanding_items(xml, "OutstandingCreditorItemsResult").unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].contact_name, "Supplier X");
    assert_eq!(items[0].amount, "250.00");
    assert_eq!(items[0].open_amount, "250.00");
    assert_eq!(items[0].country, "");
}

#[test]
fn parses_the_supplier_country_of_outstanding_items() {
    // Real shape: the address block follows the amounts, empty fields as <X />.
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <OutstandingCreditorItemsResponse xmlns="http://www.theyukicompany.com/">
      <OutstandingCreditorItemsResult>
        <Item ID="item-1">
          <Date>2026-09-07</Date>
          <Contact>Hosting Inc</Contact>
          <OpenAmount>16.40</OpenAmount>
          <OriginalAmount>16.40</OriginalAmount>
          <Type ID="2">Aankoopfactuur</Type>
          <PaymentMethod>Creditcard</PaymentMethod>
          <Postcode />
          <Country>US</Country>
        </Item>
      </OutstandingCreditorItemsResult>
    </OutstandingCreditorItemsResponse>
  </soap:Body>
</soap:Envelope>"#;
    let items =
        AccountingClient::parse_outstanding_items(xml, "OutstandingCreditorItemsResult").unwrap();
    assert_eq!(items[0].country, "US");
    assert_eq!(items[0].open_amount, "16.40");
    assert_eq!(items[0].payment_method, "Creditcard");
}

#[test]
fn parses_gl_account_balances() {
    // Real GLAccountBalance shape: the operation returns every account, each a
    // <GLAccount> with Code/BalanceType attributes and Description/Amount children.
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <GLAccountBalanceResponse xmlns="http://www.theyukicompany.com/">
      <GLAccountBalanceResult>
        <GLAccountBalance xmlns="">
          <GLAccount Code="20200" BalanceType="B"><Description>RC Ruben Jongejan</Description><Amount>3472.31</Amount></GLAccount>
          <GLAccount Code="80000" BalanceType="W"><Description>Omzet</Description><Amount>-155843.75</Amount></GLAccount>
        </GLAccountBalance>
      </GLAccountBalanceResult>
    </GLAccountBalanceResponse>
  </soap:Body>
</soap:Envelope>"#;

    let balances = AccountingClient::parse_gl_account_balances(xml).unwrap();
    assert_eq!(balances.len(), 2);
    assert_eq!(balances[0].code, "20200");
    assert_eq!(balances[0].description, "RC Ruben Jongejan");
    assert_eq!(balances[0].balance_type, "B");
    assert_eq!(balances[0].amount, "3472.31");
    assert_eq!(balances[1].code, "80000");
    assert_eq!(balances[1].balance_type, "W");
    assert_eq!(balances[1].amount, "-155843.75");
}

#[test]
fn parses_gl_account_scheme_with_misspelled_description() {
    // Yuki's GetGLAccountScheme uses lowercase children and misspells the
    // description element as <descripton>; the parser must read it regardless.
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <GetGLAccountSchemeResponse xmlns="http://www.theyukicompany.com/">
      <GetGLAccountSchemeResult>
        <GlAccount><code>01000</code><type>1</type><subtype>90</subtype><isEnabled>true</isEnabled><descripton>Oprichtingskosten</descripton></GlAccount>
        <GlAccount><code>20200</code><type>2</type><descripton>RC Ruben Jongejan</descripton></GlAccount>
      </GetGLAccountSchemeResult>
    </GetGLAccountSchemeResponse>
  </soap:Body>
</soap:Envelope>"#;

    let accounts = AccountingInfoClient::parse_gl_accounts(xml).unwrap();
    assert_eq!(accounts.len(), 2);
    assert_eq!(accounts[0].code, "01000");
    assert_eq!(accounts[0].account_type, "1");
    assert_eq!(accounts[0].description, "Oprichtingskosten");
    assert_eq!(accounts[1].code, "20200");
    assert_eq!(accounts[1].description, "RC Ruben Jongejan");
}

#[test]
fn parses_start_balances_with_account_id_fields() {
    // Real GetStartBalanceByGlAccount shape: <accountID>, <startBalance>,
    // <accountDescription>.
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <GetStartBalanceByGlAccountResponse xmlns="http://www.theyukicompany.com/">
      <GetStartBalanceByGlAccountResult>
        <AccountStartBalance><accountID>02300</accountID><startBalance>1216.53</startBalance><accountDescription>Inventaris en inrichting</accountDescription></AccountStartBalance>
        <AccountStartBalance><accountID>20200</accountID><startBalance>-89018.96</startBalance><accountDescription>RC Ruben Jongejan</accountDescription></AccountStartBalance>
      </GetStartBalanceByGlAccountResult>
    </GetStartBalanceByGlAccountResponse>
  </soap:Body>
</soap:Envelope>"#;

    let balances = AccountingInfoClient::parse_start_balances(xml).unwrap();
    assert_eq!(balances.len(), 2);
    assert_eq!(balances[0].gl_account_code, "02300");
    assert_eq!(balances[0].balance, "1216.53");
    assert_eq!(balances[0].description, "Inventaris en inrichting");
    assert_eq!(balances[1].gl_account_code, "20200");
    assert_eq!(balances[1].description, "RC Ruben Jongejan");
}

#[test]
fn transaction_type_codes_map_to_journals() {
    assert_eq!(
        TransactionType::from_code(" 9 "),
        Some(TransactionType::Purchase)
    );
    assert_eq!(TransactionType::from_code("0"), Some(TransactionType::Bank));
    assert_eq!(
        TransactionType::from_code("10"),
        Some(TransactionType::Bank)
    );
    assert_eq!(
        TransactionType::from_code("42"),
        Some(TransactionType::Unknown("42".into()))
    );
    assert_eq!(TransactionType::from_code(""), None);
}
