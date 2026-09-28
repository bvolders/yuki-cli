use yuki_client::client::Region;
use yuki_client::client::accounting::AccountingClient;
use yuki_client::client::accounting_info::AccountingInfoClient;
use yuki_client::client::archive::ArchiveClient;
use yuki_client::client::contact::ContactClient;
use yuki_client::client::sales::SalesClient;
use yuki_client::client::vat::VatClient;

#[test]
fn an_unspecified_region_falls_back_to_the_netherlands() {
    // Legacy fallback for callers and configurations that predate regions.
    assert_eq!(Region::default(), Region::Nl);
    assert_eq!(Region::Nl.api_root(), "https://api.yukiworks.nl/ws");
    assert_eq!(Region::Be.api_root(), "https://api.yukiworks.be/ws");
}

#[test]
fn every_region_is_listed_once_and_describes_itself() {
    let codes: Vec<&str> = Region::ALL.iter().map(|r| r.as_str()).collect();
    assert_eq!(codes, ["nl", "be"]);
    for region in Region::ALL {
        assert_eq!(region.as_str().parse::<Region>(), Ok(region));
        assert_eq!(Region::from_api_root(region.api_root()), Some(region));
        assert_eq!(
            region.api_root(),
            format!("https://{}/ws", region.host()),
            "{region}"
        );
    }
    assert_eq!(Region::Be.host(), "api.yukiworks.be");
    assert_eq!(Region::Be.country(), "Belgium");
    assert_eq!(Region::Nl.country(), "Netherlands");
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
    let clients = [
        (
            "Accounting",
            AccountingClient::new()
                .with_api_root(be)
                .base_url()
                .to_string(),
        ),
        (
            "AccountingInfo",
            AccountingInfoClient::new()
                .with_api_root(be)
                .base_url()
                .to_string(),
        ),
        (
            "Archive",
            ArchiveClient::new()
                .with_api_root(be)
                .base_url()
                .to_string(),
        ),
        (
            "Contact",
            ContactClient::new()
                .with_api_root(be)
                .base_url()
                .to_string(),
        ),
        (
            "Sales",
            SalesClient::new().with_api_root(be).base_url().to_string(),
        ),
        (
            "Vat",
            VatClient::new().with_api_root(be).base_url().to_string(),
        ),
    ];
    for (service, url) in clients {
        assert_eq!(url, format!("https://api.yukiworks.be/ws/{service}.asmx"));
    }
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
