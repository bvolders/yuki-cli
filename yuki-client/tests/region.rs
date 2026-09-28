use yuki_client::client::Region;
use yuki_client::client::accounting::AccountingClient;
use yuki_client::client::accounting_info::AccountingInfoClient;
use yuki_client::client::archive::ArchiveClient;
use yuki_client::client::contact::ContactClient;
use yuki_client::client::sales::SalesClient;
use yuki_client::client::vat::VatClient;

#[test]
fn region_defaults_to_the_netherlands() {
    assert_eq!(Region::default(), Region::Nl);
    assert_eq!(Region::Nl.api_root(), "https://api.yukiworks.nl/ws");
    assert_eq!(Region::Be.api_root(), "https://api.yukiworks.be/ws");
}

#[test]
fn region_parses_case_insensitively_and_round_trips() {
    assert_eq!("be".parse::<Region>(), Ok(Region::Be));
    assert_eq!(" NL ".parse::<Region>(), Ok(Region::Nl));
    assert_eq!(Region::Be.to_string(), "be");
    assert!(
        "de".parse::<Region>()
            .unwrap_err()
            .contains("expected nl or be")
    );
}

#[test]
fn clients_default_to_the_dutch_host() {
    assert_eq!(
        AccountingClient::new().base_url(),
        "https://api.yukiworks.nl/ws/Accounting.asmx"
    );
    assert_eq!(
        VatClient::with_client(reqwest::Client::new()).base_url(),
        "https://api.yukiworks.nl/ws/Vat.asmx"
    );
}

#[test]
fn every_client_follows_the_api_root() {
    let be = Region::Be.api_root();
    assert_eq!(
        AccountingClient::new().with_api_root(be).base_url(),
        "https://api.yukiworks.be/ws/Accounting.asmx"
    );
    assert_eq!(
        AccountingInfoClient::new().with_api_root(be).base_url(),
        "https://api.yukiworks.be/ws/AccountingInfo.asmx"
    );
    assert_eq!(
        ArchiveClient::new().with_api_root(be).base_url(),
        "https://api.yukiworks.be/ws/Archive.asmx"
    );
    assert_eq!(
        ContactClient::new().with_api_root(be).base_url(),
        "https://api.yukiworks.be/ws/Contact.asmx"
    );
    assert_eq!(
        SalesClient::new().with_api_root(be).base_url(),
        "https://api.yukiworks.be/ws/Sales.asmx"
    );
    assert_eq!(
        VatClient::new().with_api_root(be).base_url(),
        "https://api.yukiworks.be/ws/Vat.asmx"
    );
}

#[test]
fn a_trailing_slash_on_the_api_root_is_tolerated() {
    assert_eq!(
        SalesClient::new()
            .with_api_root("http://127.0.0.1:8080/ws/")
            .base_url(),
        "http://127.0.0.1:8080/ws/Sales.asmx"
    );
}
