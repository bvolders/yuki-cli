//! End-to-end: `contacts search` and `contacts list` against a local mock:
//! the real SearchContacts parameters, the selected administration's domain,
//! the template fields, and pagination that always ends.

mod common;

use common::{RequestLog, mock_yuki, soap_response, yuki};
use serde_json::Value;
use tempfile::TempDir;

const ONE_CONTACT: &str = r#"<Contact ID="c-1"><HID>42</HID><Code /><Name>Example Customer BV</Name>
<Type>Customer</Type><City>Gent</City><Country>BE</Country><VATNumber>BE0123456789</VATNumber>
<IsSupplier>false</IsSupplier><IsCustomer>true</IsCustomer></Contact>"#;

/// `count` made-up contacts with IDs `{prefix}-0`, `{prefix}-1`, ….
fn contacts(prefix: &str, count: usize) -> String {
    (0..count)
        .map(|i| {
            format!("<Contact ID=\"{prefix}-{i}\"><Name>Contact {prefix} {i}</Name></Contact>")
        })
        .collect()
}

fn page_number(body: &str) -> u32 {
    body.split("<yuki:pageNumber>")
        .nth(1)
        .and_then(|rest| rest.split('<').next())
        .and_then(|n| n.parse().ok())
        .expect("pageNumber sent")
}

/// A mock whose listings answer `page(n)` for page `n`, and only for the
/// domain of the administration selected with `--admin second`.
fn mock(page: fn(u32) -> String) -> (String, RequestLog) {
    mock_yuki(move |action, body| match action {
        "Authenticate" => soap_response("Authenticate", "session-1"),
        "SearchContacts" | "GetSuppliersAndCustomers" => {
            let inner = if body.contains("<yuki:domainID>domain-2</yuki:domainID>") {
                page(page_number(body))
            } else {
                String::new()
            };
            soap_response(action, &format!("<Contacts xmlns=\"\">{inner}</Contacts>"))
        }
        other => panic!("unexpected call {other}"),
    })
}

/// Two administrations; the tests select the second, which is not the default.
fn home_with_config(root: &str) -> TempDir {
    let home = TempDir::new().expect("temp home");
    let dir = home.path().join(".config/yuki");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(
        dir.join("config.toml"),
        format!(
            r#"api_key = "test-key"
default_admin = "first"

[administrations.first]
domain_id = "domain-1"
admin_id = "admin-1"
base_url = "{root}"

[administrations.second]
domain_id = "domain-2"
admin_id = "admin-2"
base_url = "{root}"
"#
        ),
    )
    .expect("write config");
    home
}

fn run(home: &TempDir, args: &[&str]) -> Value {
    let mut all = vec!["--admin", "second", "--output", "json"];
    all.extend_from_slice(args);
    let output = yuki(home, &all);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON stdout")
}

fn calls(log: &RequestLog, action: &str) -> Vec<String> {
    log.lock()
        .expect("log")
        .iter()
        .filter(|r| r.action == action)
        .map(|r| r.body.clone())
        .collect()
}

#[test]
fn search_sends_the_schema_parameters_and_shows_template_fields() {
    let (root, log) = mock(|_| ONE_CONTACT.to_string());
    let home = home_with_config(&root);
    let json = run(
        &home,
        &["contacts", "search", "Example", "--by", "vatnumber"],
    );
    let row = &json["items"][0];
    assert_eq!(json["total"], 1);
    assert_eq!(row["HID"], "42");
    assert_eq!(row["Code"], "");
    assert_eq!(row["City"], "Gent");
    assert_eq!(row["VAT Number"], "BE0123456789");

    // One short page: no second request.
    let search = calls(&log, "SearchContacts");
    assert_eq!(search.len(), 1);
    for part in [
        "<yuki:domainID>domain-2</yuki:domainID>",
        "<yuki:searchOption>VATNumber</yuki:searchOption>",
        "<yuki:searchValue>Example</yuki:searchValue>",
        "<yuki:sortOrder>Name</yuki:sortOrder>",
        "<yuki:modifiedAfter xsi:nil=\"true\"",
        "<yuki:active>Both</yuki:active>",
        "<yuki:pageNumber>1</yuki:pageNumber>",
    ] {
        assert!(
            search[0].contains(part),
            "{part} missing from {}",
            search[0]
        );
    }
    assert!(!search[0].contains("searchQuery"));
}

#[test]
fn search_defaults_to_all_fields() {
    let (root, log) = mock(|_| ONE_CONTACT.to_string());
    let home = home_with_config(&root);
    run(&home, &["contacts", "search", "Gent"]);
    let search = calls(&log, "SearchContacts");
    assert!(search[0].contains("<yuki:searchOption>All</yuki:searchOption>"));
}

#[test]
fn list_sends_the_selected_administrations_domain() {
    let (root, log) = mock(|_| ONE_CONTACT.to_string());
    let home = home_with_config(&root);
    let json = run(&home, &["contacts", "list"]);
    // The mock answers nothing for any other domain.
    assert_eq!(json["total"], 1);
    let list = calls(&log, "GetSuppliersAndCustomers");
    assert!(list[0].contains("<yuki:domainID>domain-2</yuki:domainID>"));
}

#[test]
fn a_full_page_is_followed_by_the_next() {
    let (root, log) = mock(|page| match page {
        1 => contacts("a", 100),
        2 => contacts("b", 3),
        _ => panic!("no page {page}"),
    });
    let home = home_with_config(&root);
    let json = run(&home, &["contacts", "search", "x"]);
    assert_eq!(json["total"], 103);
    assert_eq!(calls(&log, "SearchContacts").len(), 2);
}

#[test]
fn a_repeated_page_ends_the_listing() {
    // Yuki ignoring pageNumber: every page is the first one again.
    let (root, log) = mock(|_| contacts("a", 100));
    let home = home_with_config(&root);
    let json = run(&home, &["contacts", "search", "x"]);
    assert_eq!(json["total"], 100);
    assert_eq!(calls(&log, "SearchContacts").len(), 2);
}

#[test]
fn a_listing_stops_at_the_page_cap_and_says_so() {
    let (root, log) = mock(|page| contacts(&format!("p{page}"), 100));
    let home = home_with_config(&root);
    let output = yuki(
        &home,
        &["--admin", "second", "--output", "json", "contacts", "list"],
    );
    assert!(output.status.success());
    let json: Value = serde_json::from_slice(&output.stdout).expect("JSON stdout");
    assert_eq!(json["total"], 5000);
    assert_eq!(calls(&log, "GetSuppliersAndCustomers").len(), 50);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("stopped after 5000 contacts"), "{err}");
}
