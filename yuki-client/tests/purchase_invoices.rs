//! `ProcessPurchaseInvoices`: the request envelope and both response forms.

use yuki_client::client::purchase::PurchaseClient;
use yuki_client::error::YukiError;

fn envelope(result: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body>
<ProcessPurchaseInvoicesResponse xmlns="http://www.theyukicompany.com/">
<ProcessPurchaseInvoicesResult>{result}</ProcessPurchaseInvoicesResult>
</ProcessPurchaseInvoicesResponse></soap:Body></soap:Envelope>"#
    )
}

const SUCCESS: &str = r#"<PurchaseInvoicesImportResponse xmlns="urn:xmlns:http://www.theyukicompany.com:purchaseinvoicesresponse">
  <TotalSucceeded>1</TotalSucceeded>
  <TotalFailed>0</TotalFailed>
  <TotalSkipped>0</TotalSkipped>
  <Invoice>
    <Succeeded>true</Succeeded>
    <Processed>false</Processed>
    <Reference>F2026-0042</Reference>
    <Subject>Hosting &amp; support</Subject>
    <Contact>Leverancier BV</Contact>
    <Message></Message>
  </Invoice>
</PurchaseInvoicesImportResponse>"#;

#[test]
fn parses_a_success_sent_as_child_elements() {
    let import = PurchaseClient::parse_process_purchase_invoices(&envelope(SUCCESS)).unwrap();
    assert_eq!(import.total_succeeded, 1);
    assert_eq!(import.invoices.len(), 1);
    let invoice = &import.invoices[0];
    assert!(invoice.succeeded && !invoice.processed);
    assert_eq!(invoice.reference, "F2026-0042");
    assert_eq!(invoice.subject, "Hosting & support");
    assert_eq!(import.failure(), None);
}

#[test]
fn parses_a_failure_sent_as_escaped_text() {
    let inner = r#"<?xml version="1.0" encoding="utf-16"?>
<PurchaseInvoicesImportResponse><TotalSucceeded>0</TotalSucceeded><TotalFailed>1</TotalFailed>
<TotalSkipped>0</TotalSkipped><Invoice><Succeeded>false</Succeeded><Processed>false</Processed>
<Reference>F1</Reference><Message>Unknown VAT code</Message></Invoice></PurchaseInvoicesImportResponse>"#;
    let escaped = inner
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let import = PurchaseClient::parse_process_purchase_invoices(&envelope(&escaped)).unwrap();
    assert_eq!(import.total_failed, 1);
    let failure = import.failure().expect("a failed invoice is a failure");
    assert!(failure.contains("Unknown VAT code"), "{failure}");
}

#[test]
fn a_sales_response_is_not_a_purchase_response() {
    let sales = SUCCESS.replace(
        "PurchaseInvoicesImportResponse",
        "SalesInvoicesImportResponse",
    );
    let err = PurchaseClient::parse_process_purchase_invoices(&envelope(&sales)).unwrap_err();
    assert!(matches!(err, YukiError::Xml(_)), "{err}");
}

#[test]
fn the_envelope_uses_lowercase_id_parameters_and_embeds_the_document() {
    let doc = r#"<PurchaseInvoices xmlns="urn:xmlns:http://www.theyukicompany.com:purchaseinvoices"><PurchaseInvoice/></PurchaseInvoices>"#;
    let envelope = PurchaseClient::process_purchase_invoices_envelope("sid-1", "admin-1", doc);
    for fragment in [
        "<yuki:ProcessPurchaseInvoices>",
        "<yuki:sessionId>sid-1</yuki:sessionId>",
        "<yuki:administrationId>admin-1</yuki:administrationId>",
        "<yuki:disableAutoCorrect>false</yuki:disableAutoCorrect>",
    ] {
        assert!(envelope.contains(fragment), "{fragment} in {envelope}");
    }
    assert!(
        envelope.contains(&format!("<yuki:xmlDoc>{doc}</yuki:xmlDoc>")),
        "the document goes in as child elements, not escaped text: {envelope}"
    );
}
