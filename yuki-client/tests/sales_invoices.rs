//! `ProcessSalesInvoices`: the request envelope and both response forms.

use yuki_client::client::sales::{ImportedInvoice, SalesClient, SalesInvoicesImport};
use yuki_client::error::YukiError;

fn envelope(result: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body>
<ProcessSalesInvoicesResponse xmlns="http://www.theyukicompany.com/">
<ProcessSalesInvoicesResult>{result}</ProcessSalesInvoicesResult>
</ProcessSalesInvoicesResponse></soap:Body></soap:Envelope>"#
    )
}

const SUCCESS: &str = r#"<SalesInvoicesImportResponse xmlns="urn:xmlns:http://www.theyukicompany.com:salesinvoicesresponse">
  <TimeStamp>2026-10-02T10:00:00</TimeStamp>
  <AdministrationId>admin-1</AdministrationId>
  <TotalSucceeded>1</TotalSucceeded>
  <TotalFailed>0</TotalFailed>
  <TotalSkipped>0</TotalSkipped>
  <Invoice>
    <Succeeded>true</Succeeded>
    <Processed>true</Processed>
    <EmailSent>true</EmailSent>
    <Reference>2026-0042</Reference>
    <Subject>Hosting &amp; support</Subject>
    <Message></Message>
  </Invoice>
</SalesInvoicesImportResponse>"#;

#[test]
fn parses_a_success_sent_as_child_elements() {
    let import = SalesClient::parse_process_sales_invoices(&envelope(SUCCESS)).unwrap();
    assert_eq!(
        import,
        SalesInvoicesImport {
            total_succeeded: 1,
            total_failed: 0,
            total_skipped: 0,
            invoices: vec![ImportedInvoice {
                succeeded: true,
                processed: true,
                email_sent: true,
                reference: "2026-0042".into(),
                subject: "Hosting & support".into(),
                message: String::new(),
            }],
        }
    );
    assert_eq!(import.failure(), None);
}

#[test]
fn parses_a_failure_sent_as_escaped_text() {
    let inner = r#"<?xml version="1.0" encoding="utf-16"?>
<SalesInvoicesImportResponse xmlns="urn:xmlns:http://www.theyukicompany.com:salesinvoicesresponse">
<TotalSucceeded>0</TotalSucceeded><TotalFailed>1</TotalFailed><TotalSkipped>0</TotalSkipped>
<Invoice><Succeeded>false</Succeeded><Processed>false</Processed><EmailSent>false</EmailSent>
<Reference></Reference><Subject>Hosting</Subject>
<Message>No tax code for VATPercentage 21 &amp; VATType 9</Message></Invoice>
</SalesInvoicesImportResponse>"#;
    let escaped = inner
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let import = SalesClient::parse_process_sales_invoices(&envelope(&escaped)).unwrap();
    assert_eq!(import.total_failed, 1);
    assert_eq!(import.invoices.len(), 1);
    let invoice = &import.invoices[0];
    assert!(!invoice.succeeded && !invoice.processed && !invoice.email_sent);
    assert_eq!(
        invoice.message,
        "No tax code for VATPercentage 21 & VATType 9"
    );
    let failure = import.failure().expect("a failed invoice is a failure");
    assert!(
        failure.contains("0 succeeded, 1 failed, 0 skipped"),
        "{failure}"
    );
    assert!(failure.contains("No tax code"), "{failure}");
}

#[test]
fn a_skipped_or_missing_invoice_is_a_failure() {
    let skipped = SalesInvoicesImport {
        total_skipped: 1,
        invoices: vec![ImportedInvoice {
            succeeded: true,
            ..Default::default()
        }],
        ..Default::default()
    };
    assert!(skipped.failure().is_some());
    let empty = SalesInvoicesImport::default();
    assert!(empty.failure().unwrap().contains("no invoice was reported"));
}

#[test]
fn a_result_that_is_not_an_import_response_is_an_error_that_quotes_it() {
    let err = SalesClient::parse_process_sales_invoices(&envelope("Invalid xmlDoc")).unwrap_err();
    assert!(
        matches!(&err, YukiError::Xml(m) if m.contains("Invalid xmlDoc")),
        "{err}"
    );
}

#[test]
fn a_soap_fault_is_returned_as_such() {
    let fault = r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body><soap:Fault>
<faultcode>soap:Server</faultcode><faultstring>Server was unable to process request.</faultstring>
</soap:Fault></soap:Body></soap:Envelope>"#;
    let err = SalesClient::parse_process_sales_invoices(fault).unwrap_err();
    assert!(matches!(err, YukiError::SoapFault { .. }), "{err}");
}

#[test]
fn the_envelope_uses_lowercase_id_parameters_and_embeds_the_document() {
    let doc = r#"<SalesInvoices xmlns="urn:xmlns:http://www.theyukicompany.com:salesinvoices"><SalesInvoice/></SalesInvoices>"#;
    let envelope = SalesClient::process_sales_invoices_envelope("sid-1", "admin-1", doc);
    assert!(
        envelope.contains("<yuki:ProcessSalesInvoices>"),
        "{envelope}"
    );
    assert!(envelope.contains("<yuki:sessionId>sid-1</yuki:sessionId>"));
    assert!(envelope.contains("<yuki:administrationId>admin-1</yuki:administrationId>"));
    assert!(!envelope.contains("sessionID"), "{envelope}");
    assert!(
        envelope.contains(&format!("<yuki:xmlDoc>{doc}</yuki:xmlDoc>")),
        "the document goes in as child elements, not escaped text: {envelope}"
    );
}
