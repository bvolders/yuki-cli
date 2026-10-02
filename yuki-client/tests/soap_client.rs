use yuki_client::client::accounting::AccountingClient;
use yuki_client::client::{SoapClient, SoapEnvelope};

#[test]
fn builds_soap_envelope_with_session() {
    let envelope = SoapEnvelope::new("GLAccountBalance")
        .session("session-123")
        .param("glAccountCode", "1000")
        .param("startDate", "2025-01-01")
        .build();

    assert!(envelope.contains("GLAccountBalance"));
    assert!(envelope.contains("<yuki:sessionID>session-123</yuki:sessionID>"));
    assert!(envelope.contains("<yuki:glAccountCode>1000</yuki:glAccountCode>"));
    assert!(envelope.contains("soap:Envelope"));
    assert!(envelope.contains("http://www.theyukicompany.com/"));
}

#[test]
fn param_values_are_escaped_and_survive_a_parse() {
    let name = r#"Tom & Jerry <x> "q" 'a'.pdf"#;
    let envelope = SoapEnvelope::new("UploadDocument")
        .session("s-1")
        .param("fileName", name)
        .param("folder", "7")
        .build();
    assert!(
        envelope.contains("Tom &amp; Jerry &lt;x&gt; &quot;q&quot; &apos;a&apos;.pdf"),
        "{envelope}"
    );
    // A real XML parser reads the envelope and gets the name back unchanged.
    assert_eq!(
        SoapClient::parse_single_result(&envelope, "fileName").unwrap(),
        name
    );
    assert_eq!(
        SoapClient::parse_single_result(&envelope, "folder").unwrap(),
        "7"
    );
}

#[test]
fn builds_soap_envelope_without_session() {
    let envelope = SoapEnvelope::new("Authenticate")
        .param("accessKey", "my-api-key")
        .build();

    assert!(envelope.contains("Authenticate"));
    assert!(envelope.contains("<yuki:accessKey>my-api-key</yuki:accessKey>"));
    assert!(!envelope.contains("sessionID"));
}

#[test]
fn parses_authenticate_response() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"
               xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <soap:Body>
    <AuthenticateResponse xmlns="http://www.theyukicompany.com/">
      <AuthenticateResult>session-id-abc-123</AuthenticateResult>
    </AuthenticateResponse>
  </soap:Body>
</soap:Envelope>"#;

    let result = SoapClient::parse_single_result(xml, "AuthenticateResult").unwrap();
    assert_eq!(result, "session-id-abc-123");
}

#[test]
fn parses_soap_fault() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <soap:Fault>
      <faultcode>soap:Client</faultcode>
      <faultstring>Invalid API key</faultstring>
    </soap:Fault>
  </soap:Body>
</soap:Envelope>"#;

    let err = SoapClient::parse_single_result(xml, "AuthenticateResult");
    assert!(err.is_err());
    let err_msg = format!("{}", err.unwrap_err());
    assert!(err_msg.contains("Invalid API key"));
}

#[test]
fn soap_action_header_format() {
    let action = SoapClient::soap_action("Accounting", "GLAccountBalance");
    assert_eq!(action, "http://www.theyukicompany.com/GLAccountBalance");
}

#[test]
fn parses_administrations_response() {
    // Real Yuki API format: ID is an attribute on Administration, not a child element
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"
               xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <soap:Body>
    <AdministrationsResponse xmlns="http://www.theyukicompany.com/">
      <AdministrationsResult>
        <Administrations xmlns="">
          <Administration ID="admin-001">
            <Name>Acme BV</Name>
            <DomainID>domain-001</DomainID>
          </Administration>
          <Administration ID="admin-002">
            <Name>Widget Corp</Name>
            <DomainID>domain-002</DomainID>
          </Administration>
        </Administrations>
      </AdministrationsResult>
    </AdministrationsResponse>
  </soap:Body>
</soap:Envelope>"#;

    let admins = AccountingClient::parse_administrations(xml).unwrap();
    assert_eq!(admins.len(), 2);
    assert_eq!(admins[0].id, "admin-001");
    assert_eq!(admins[0].name, "Acme BV");
    assert_eq!(admins[0].domain_id, "domain-001");
    assert_eq!(admins[1].id, "admin-002");
    assert_eq!(admins[1].name, "Widget Corp");
    assert_eq!(admins[1].domain_id, "domain-002");
}

#[test]
fn parses_outstanding_debtor_items() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"
               xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <soap:Body>
    <OutstandingDebtorItemsResponse xmlns="http://www.theyukicompany.com/">
      <OutstandingDebtorItemsResult>
        <Item>
          <ContactName>Customer A</ContactName>
          <Description>Invoice 2025-001</Description>
          <Date>2025-03-01</Date>
          <Amount>1000.00</Amount>
          <OpenAmount>500.00</OpenAmount>
        </Item>
      </OutstandingDebtorItemsResult>
    </OutstandingDebtorItemsResponse>
  </soap:Body>
</soap:Envelope>"#;

    let items =
        AccountingClient::parse_outstanding_items(xml, "OutstandingDebtorItemsResult").unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].contact_name, "Customer A");
    assert_eq!(items[0].description, "Invoice 2025-001");
    assert_eq!(items[0].date, "2025-03-01");
    assert_eq!(items[0].amount, "1000.00");
    assert_eq!(items[0].open_amount, "500.00");
}

#[test]
fn a_payload_buys_time_up_to_ten_minutes() {
    use std::time::Duration;
    use yuki_client::client::soap_client::{REQUEST_TIMEOUT, payload_timeout};
    assert_eq!(payload_timeout(""), REQUEST_TIMEOUT);
    assert_eq!(
        payload_timeout(&"a".repeat(4_000_000)),
        Duration::from_secs(180)
    );
    assert_eq!(
        payload_timeout(&"a".repeat(100_000_000)),
        Duration::from_secs(600)
    );
}
