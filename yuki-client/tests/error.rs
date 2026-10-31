#[test]
fn auth_failed_returns_exit_code_2() {
    let err = yuki_client::error::YukiError::AuthFailed("bad key".into());
    assert_eq!(err.exit_code(), 2);
}

#[test]
fn not_found_returns_exit_code_3() {
    let err = yuki_client::error::YukiError::NotFound("missing".into());
    assert_eq!(err.exit_code(), 3);
}

#[test]
fn rate_limited_returns_exit_code_4() {
    let err = yuki_client::error::YukiError::RateLimited;
    assert_eq!(err.exit_code(), 4);
}

#[test]
fn soap_fault_returns_exit_code_1() {
    let err = yuki_client::error::YukiError::SoapFault {
        code: "Server".into(),
        message: "internal".into(),
    };
    assert_eq!(err.exit_code(), 1);
}

#[test]
fn config_error_returns_exit_code_1() {
    let err = yuki_client::error::YukiError::Config("missing file".into());
    assert_eq!(err.exit_code(), 1);
}

#[test]
fn xml_error_returns_exit_code_1() {
    let err = yuki_client::error::YukiError::Xml("parse failed".into());
    assert_eq!(err.exit_code(), 1);
}

#[test]
fn a_missing_client_id_fault_is_a_refusal_and_other_faults_are_not() {
    use yuki_client::error::{Delivery, YukiError};
    let fault = |code: &str, message: &str| YukiError::SoapFault {
        code: code.into(),
        message: message.into(),
    };
    let refused = fault("soap:Client", "No Client ID supplied");
    assert!(refused.is_missing_client_id());
    assert_eq!(refused.delivery(), Delivery::Refused);
    assert!(fault("soap:client", "no client id supplied ").is_missing_client_id());
    for other in [
        fault("soap:Server", "No Client ID supplied"),
        fault("soap:Client", "Server was unable to process request."),
    ] {
        assert!(!other.is_missing_client_id(), "{other}");
        assert_eq!(other.delivery(), Delivery::Unknown, "{other}");
    }
}
