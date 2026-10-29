//! Regression tests: text containing entity or character references must be
//! parsed whole. Since quick-xml 0.38 such text arrives as several events
//! (`Text("Smith ")`, `GeneralRef("amp")`, `Text(" Jones fee")`), and parsers
//! that assigned per text event kept only the last fragment.
//!
//! One table row per parser: a SOAP body with `{RAW}` placeholders, and the
//! fields the parser must extract from it, in order.

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

type Fields = Result<Vec<String>, YukiError>;

struct Case {
    parser: &'static str,
    body: &'static str,
    fields: fn(&str) -> Fields,
    expected: &'static [&'static str],
}

fn soap(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    {}
  </soap:Body>
</soap:Envelope>"#,
        body.replace("{RAW}", RAW)
    )
}

const CASES: &[Case] = &[
    Case {
        parser: "gl_account_balances",
        body: r#"<GLAccountBalanceResponse xmlns="http://www.theyukicompany.com/"><GLAccountBalanceResult><GLAccountBalance xmlns="">
          <GLAccount Code="20200" BalanceType="B"><Description>{RAW}</Description><Amount>1.00</Amount></GLAccount>
        </GLAccountBalance></GLAccountBalanceResult></GLAccountBalanceResponse>"#,
        fields: |xml| {
            let b = AccountingClient::parse_gl_account_balances(xml)?.remove(0);
            Ok(vec![b.description, b.amount])
        },
        expected: &[EXPECTED, "1.00"],
    },
    Case {
        parser: "gl_transactions",
        body: r#"<GLAccountTransactionsResponse xmlns="http://www.theyukicompany.com/"><GLAccountTransactionsResult><GLAccountTransactions xmlns="">
          <GLAccountTransaction ID="tx-001">
            <Date>2025-03-15</Date>
            <Description>{RAW}</Description>
            <Amount>500.00</Amount>
            <GLAccountCode>11001</GLAccountCode>
          </GLAccountTransaction>
        </GLAccountTransactions></GLAccountTransactionsResult></GLAccountTransactionsResponse>"#,
        fields: |xml| {
            let t = AccountingClient::parse_gl_transactions(xml)?.remove(0);
            Ok(vec![t.description, t.date, t.gl_account])
        },
        expected: &[EXPECTED, "2025-03-15", "11001"],
    },
    Case {
        parser: "gl_transactions_with_contact",
        body: r#"<GLAccountTransactionsAndContactResponse xmlns="http://www.theyukicompany.com/"><GLAccountTransactionsAndContactResult><GLAccountTransactions xmlns="">
          <GLAccountTransaction ID="tx-001">
            <Date>2025-03-15</Date>
            <Description>{RAW}</Description>
            <Amount>-7.28</Amount>
            <GLAccountCode>11001</GLAccountCode>
            <ContactName>Smith &amp; Jones</ContactName>
          </GLAccountTransaction>
        </GLAccountTransactions></GLAccountTransactionsAndContactResult></GLAccountTransactionsAndContactResponse>"#,
        fields: |xml| {
            let t = AccountingClient::parse_gl_transactions_with_contact(xml)?.remove(0);
            Ok(vec![t.description, t.contact_name])
        },
        expected: &[EXPECTED, "Smith & Jones"],
    },
    Case {
        parser: "administrations",
        body: r#"<AdministrationsResponse xmlns="http://www.theyukicompany.com/"><AdministrationsResult><Administrations xmlns="">
          <Administration ID="admin-001">
            <Name>{RAW}</Name>
            <DomainID>domain-001</DomainID>
          </Administration>
        </Administrations></AdministrationsResult></AdministrationsResponse>"#,
        fields: |xml| {
            let a = AccountingClient::parse_administrations(xml)?.remove(0);
            Ok(vec![a.name, a.domain_id])
        },
        expected: &[EXPECTED, "domain-001"],
    },
    Case {
        parser: "outstanding_items",
        body: r#"<OutstandingDebtorItemsResponse xmlns="http://www.theyukicompany.com/"><OutstandingDebtorItemsResult>
          <Item>
            <ContactName>Smith &amp; Jones</ContactName>
            <Description>{RAW}</Description>
            <Date>2025-03-01</Date>
            <Amount>1000.00</Amount>
            <OpenAmount>500.00</OpenAmount>
          </Item>
        </OutstandingDebtorItemsResult></OutstandingDebtorItemsResponse>"#,
        fields: |xml| {
            let i = AccountingClient::parse_outstanding_items(xml, "OutstandingDebtorItemsResult")?
                .remove(0);
            Ok(vec![i.contact_name, i.description, i.open_amount])
        },
        expected: &["Smith & Jones", EXPECTED, "500.00"],
    },
    Case {
        parser: "transaction_details",
        body: r#"<GetTransactionDetailsResponse xmlns="http://www.theyukicompany.com/"><GetTransactionDetailsResult><TransactionDetails xmlns="">
          <TransactionInfo>
            <id>detail-001</id>
            <transactionDate>2025-03-15</transactionDate>
            <description>{RAW}</description>
            <transactionAmount>7.28</transactionAmount>
            <currency>EUR</currency>
            <glAccountCode>45100</glAccountCode>
          </TransactionInfo>
        </TransactionDetails></GetTransactionDetailsResult></GetTransactionDetailsResponse>"#,
        fields: |xml| {
            let d = AccountingInfoClient::parse_transaction_details(xml)?.remove(0);
            Ok(vec![d.description, d.currency])
        },
        expected: &[EXPECTED, "EUR"],
    },
    Case {
        parser: "gl_accounts",
        body: r#"<GetGLAccountSchemeResponse xmlns="http://www.theyukicompany.com/"><GetGLAccountSchemeResult>
          <GlAccount><code>01000</code><type>1</type><descripton>{RAW}</descripton></GlAccount>
        </GetGLAccountSchemeResult></GetGLAccountSchemeResponse>"#,
        fields: |xml| {
            let a = AccountingInfoClient::parse_gl_accounts(xml)?.remove(0);
            Ok(vec![a.description, a.code])
        },
        expected: &[EXPECTED, "01000"],
    },
    Case {
        parser: "start_balances",
        body: r#"<GetStartBalanceByGlAccountResponse xmlns="http://www.theyukicompany.com/"><GetStartBalanceByGlAccountResult>
          <AccountStartBalance><accountID>02300</accountID><startBalance>1216.53</startBalance><accountDescription>{RAW}</accountDescription></AccountStartBalance>
        </GetStartBalanceByGlAccountResult></GetStartBalanceByGlAccountResponse>"#,
        fields: |xml| {
            let b = AccountingInfoClient::parse_start_balances(xml)?.remove(0);
            Ok(vec![b.description, b.balance])
        },
        expected: &[EXPECTED, "1216.53"],
    },
    Case {
        parser: "archive_documents",
        body: r#"<SearchDocumentsResponse xmlns="http://www.theyukicompany.com/"><SearchDocumentsResult><Documents xmlns="">
          <Document ID="doc-001">
            <Subject>{RAW}</Subject>
            <DocumentDate>2025-03-01</DocumentDate>
            <ContactName>Smith &amp; Jones</ContactName>
            <FileName>a&amp;b.pdf</FileName>
          </Document>
        </Documents></SearchDocumentsResult></SearchDocumentsResponse>"#,
        fields: |xml| {
            let d = ArchiveClient::parse_archive_documents(xml)?.remove(0);
            Ok(vec![d.subject, d.contact_name, d.file_name])
        },
        expected: &[EXPECTED, "Smith & Jones", "a&b.pdf"],
    },
    Case {
        parser: "cost_categories",
        body: r#"<CostCategoriesResponse xmlns="http://www.theyukicompany.com/"><CostCategoriesResult><CostCategories xmlns="">
          <CostCategory ID="45100"><Description>{RAW}</Description></CostCategory>
        </CostCategories></CostCategoriesResult></CostCategoriesResponse>"#,
        fields: |xml| {
            Ok(vec![
                ArchiveClient::parse_cost_categories(xml)?
                    .remove(0)
                    .description,
            ])
        },
        expected: &[EXPECTED],
    },
    Case {
        parser: "payment_methods",
        body: r#"<PaymentMethodsResponse xmlns="http://www.theyukicompany.com/"><PaymentMethodsResult><PaymentMethods xmlns="">
          <PaymentMethod ID="4"><Description>{RAW}</Description></PaymentMethod>
        </PaymentMethods></PaymentMethodsResult></PaymentMethodsResponse>"#,
        fields: |xml| {
            Ok(vec![
                ArchiveClient::parse_payment_methods(xml)?
                    .remove(0)
                    .description,
            ])
        },
        expected: &[EXPECTED],
    },
    Case {
        parser: "contacts",
        body: r#"<SearchContactsResponse xmlns="http://www.theyukicompany.com/"><SearchContactsResult><Contacts xmlns="">
          <Contact ID="contact-001">
            <Name>{RAW}</Name>
            <Type>Supplier</Type>
            <IsSupplier>true</IsSupplier>
          </Contact>
        </Contacts></SearchContactsResult></SearchContactsResponse>"#,
        fields: |xml| {
            let c = parse_contacts(xml)?.remove(0);
            Ok(vec![c.name, c.contact_type, c.is_supplier.to_string()])
        },
        expected: &[EXPECTED, "Supplier", "true"],
    },
    Case {
        parser: "sales_items",
        body: r#"<GetSalesItemsResponse xmlns="http://www.theyukicompany.com/"><GetSalesItemsResult><SalesItems xmlns="">
          <SalesItem><id>item-001</id><description>{RAW}</description></SalesItem>
        </SalesItems></GetSalesItemsResult></GetSalesItemsResponse>"#,
        fields: |xml| {
            let i = SalesClient::parse_sales_items(xml)?.remove(0);
            Ok(vec![i.description, i.id])
        },
        expected: &[EXPECTED, "item-001"],
    },
    Case {
        parser: "vat_returns",
        body: r#"<VATReturnListResponse xmlns="http://www.theyukicompany.com/"><VATReturnListResult><VATReturns xmlns="">
          <VATReturnInfo>
            <startDate>2025-01-01T00:00:00</startDate>
            <endDate>2025-03-31T00:00:00</endDate>
            <status>Filed &amp; paid</status>
          </VATReturnInfo>
        </VATReturns></VATReturnListResult></VATReturnListResponse>"#,
        fields: |xml| {
            let r = VatClient::parse_vat_returns(xml)?.remove(0);
            Ok(vec![r.status, r.period])
        },
        expected: &["Filed & paid", "2025-01-01 - 2025-03-31"],
    },
    Case {
        parser: "vat_codes",
        body: r#"<ActiveVATCodesListResponse xmlns="http://www.theyukicompany.com/"><ActiveVATCodesListResult><VATCodes xmlns="">
          <VATCode><type>1</type><description>{RAW}</description></VATCode>
        </VATCodes></ActiveVATCodesListResult></ActiveVATCodesListResponse>"#,
        fields: |xml| {
            let c = VatClient::parse_vat_codes(xml)?.remove(0);
            Ok(vec![c.description, c.code])
        },
        expected: &[EXPECTED, "1"],
    },
    Case {
        parser: "single_result",
        body: r#"<AuthenticateResponse xmlns="http://www.theyukicompany.com/"><AuthenticateResult>{RAW}</AuthenticateResult></AuthenticateResponse>"#,
        fields: |xml| {
            Ok(vec![SoapClient::parse_single_result(
                xml,
                "AuthenticateResult",
            )?])
        },
        expected: &[EXPECTED],
    },
    Case {
        parser: "soap_fault",
        body: r#"<soap:Fault><faultcode>soap:Server</faultcode><faultstring>{RAW}</faultstring></soap:Fault>"#,
        fields: |xml| match SoapClient::parse_soap_fault(xml) {
            Some(YukiError::SoapFault { code, message }) => Ok(vec![code, message]),
            other => panic!("expected SoapFault, got {other:?}"),
        },
        expected: &["soap:Server", EXPECTED],
    },
];

#[test]
fn every_parser_keeps_entities() {
    for case in CASES {
        let fields = (case.fields)(&soap(case.body))
            .unwrap_or_else(|e| panic!("{}: parse failed: {e}", case.parser));
        assert_eq!(fields, case.expected, "{}", case.parser);
    }
}
