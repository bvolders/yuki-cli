//! End-to-end: `contacts search` and `contacts list` against a local mock:
//! the real SearchContacts parameters, the selected administration's domain,
//! the template fields, and pagination that always ends.

mod common;

use common::{RequestLog, bodies, response, stdout_json, yuki};
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
    common::mock(move |r| match r.action.as_str() {
        "Authenticate" => (200, response("Authenticate", "session-1")),
        action @ ("SearchContacts" | "GetSuppliersAndCustomers") => {
            let inner = if r.param("domainID") == "domain-2" {
                page(page_number(&r.body))
            } else {
                String::new()
            };
            let reply = format!("<Contacts xmlns=\"\">{inner}</Contacts>");
            (200, response(action, &reply))
        }
        other => panic!("unexpected call {other}"),
    })
}

/// Two administrations; the tests select the second, which is not the default.
fn home_with_config(root: &str) -> TempDir {
    let second = format!(
        "\n[administrations.second]\ndomain_id = \"domain-2\"\nadmin_id = \"admin-2\"\nbase_url = \"{root}\"\n"
    );
    common::home_with_config(root, "test-key", &second)
}

fn run(home: &TempDir, args: &[&str]) -> Value {
    let mut all = vec!["--admin", "second", "--output", "json"];
    all.extend_from_slice(args);
    stdout_json(yuki(home, &all))
}

fn calls(log: &RequestLog, action: &str) -> Vec<String> {
    bodies(log, action)
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
    let json = common::json(&output);
    assert_eq!(json["total"], 5000);
    assert_eq!(calls(&log, "GetSuppliersAndCustomers").len(), 50);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("stopped after 5000 contacts"), "{err}");
}
