//! End-to-end: `contacts search` sends SearchContacts' real parameters to a
//! local mock and shows the fields an invoice template needs.

mod common;

use common::{mock_yuki, soap_response, yuki};
use serde_json::Value;
use tempfile::TempDir;

const CONTACTS: &str = r#"<Contacts xmlns=""><Contact ID="c-1"><HID>42</HID><Code /><Name>Example Customer BV</Name>
<Type>Customer</Type><City>Gent</City><Country>BE</Country><VATNumber>BE0123456789</VATNumber>
<IsSupplier>false</IsSupplier><IsCustomer>true</IsCustomer></Contact></Contacts>"#;

fn home_with_config(root: &str) -> TempDir {
    let home = TempDir::new().expect("temp home");
    let dir = home.path().join(".config/yuki");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(
        dir.join("config.toml"),
        format!(
            r#"api_key = "test-key"
default_admin = "example"

[administrations.example]
domain_id = "domain-1"
admin_id = "admin-1"
base_url = "{root}"
"#
        ),
    )
    .expect("write config");
    home
}

#[test]
fn search_sends_the_schema_parameters_and_shows_template_fields() {
    let (root, log) = mock_yuki(|action, _| match action {
        "Authenticate" => soap_response("Authenticate", "session-1"),
        "SearchContacts" => soap_response("SearchContacts", CONTACTS),
        other => panic!("unexpected call {other}"),
    });
    let home = home_with_config(&root);
    let output = yuki(
        &home,
        &[
            "contacts",
            "search",
            "Example",
            "--by",
            "vatnumber",
            "--output",
            "json",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: Value = serde_json::from_slice(&output.stdout).expect("JSON stdout");
    let row = &json["items"][0];
    assert_eq!(json["total"], 1);
    assert_eq!(row["HID"], "42");
    assert_eq!(row["Code"], "");
    assert_eq!(row["City"], "Gent");
    assert_eq!(row["VAT Number"], "BE0123456789");

    let requests = log.lock().expect("log");
    let search: Vec<_> = requests
        .iter()
        .filter(|r| r.action == "SearchContacts")
        .collect();
    // One short page: no second request.
    assert_eq!(search.len(), 1);
    let body = &search[0].body;
    for part in [
        "<yuki:searchOption>VATNumber</yuki:searchOption>",
        "<yuki:searchValue>Example</yuki:searchValue>",
        "<yuki:sortOrder>Name</yuki:sortOrder>",
        "<yuki:modifiedAfter xsi:nil=\"true\"",
        "<yuki:active>Both</yuki:active>",
        "<yuki:pageNumber>1</yuki:pageNumber>",
    ] {
        assert!(body.contains(part), "{part} missing from {body}");
    }
    assert!(!body.contains("searchQuery"), "{body}");
}

#[test]
fn search_defaults_to_all_fields() {
    let (root, log) = mock_yuki(|action, _| match action {
        "Authenticate" => soap_response("Authenticate", "session-1"),
        _ => soap_response("SearchContacts", CONTACTS),
    });
    let home = home_with_config(&root);
    let output = yuki(&home, &["contacts", "search", "Gent", "--output", "json"]);
    assert!(output.status.success());
    let requests = log.lock().expect("log");
    let body = &requests.last().expect("a search").body;
    assert!(
        body.contains("<yuki:searchOption>All</yuki:searchOption>"),
        "{body}"
    );
}
