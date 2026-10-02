//! End-to-end: `check matches` against a local mock of Yuki Belgium. The mock
//! answers the four operations the command uses, keyed on the SOAP action and,
//! for GL lines, the requested account. Names and amounts are made up.

mod common;

use common::{RequestLog, mock_yuki, soap_response as response, yuki};
use serde_json::Value;
use tempfile::TempDir;

fn item(contact: &str, date: &str, open: &str, country: &str, method: &str) -> String {
    format!(
        "<Item><Date>{date}</Date><Contact>{contact}</Contact><OpenAmount>{open}</OpenAmount>\
         <OriginalAmount>{open}</OriginalAmount><PaymentMethod>{method}</PaymentMethod>\
         <Country>{country}</Country></Item>"
    )
}

/// One GL line; `kind` is Yuki's TransactionType ("9" purchase, "0" bank).
fn line(
    id: &str,
    date: &str,
    amount: &str,
    description: &str,
    contact: &str,
    kind: &str,
) -> String {
    format!(
        "<GLAccountTransaction ID=\"{id}\"><Date>{date}</Date><Description>{description}</Description>\
         <Amount>{amount}</Amount><Contact>{contact}</Contact><TransactionType>{kind}</TransactionType>\
         </GLAccountTransaction>"
    )
}

fn transfer(name: &str, amount: &str) -> String {
    format!(
        "Binnenlandse overschrijvingen - SEPA credit transfers : Enkelvoudige overschrijving | \
         Netto bedrag: {amount} : Overschrijving | {name}"
    )
}

const CARD: &str = "Kaarten : Betaling met debetkaart binnen eurozone | Netto bedrag: 30,000 : | \
    Debet ATM/POS - Gemaskeerde PAN of kaartnummer: 0000000000000000";

fn gl(account: &str) -> String {
    let lines: Vec<String> = match account {
        "550000" => vec![
            line(
                "b-alpha",
                "2026-06-05",
                "-120.50",
                &transfer("ALPHA TRADING BV", "120,500"),
                "",
                "0",
            ),
            line("b-beta", "2026-05-03", "-30.00", CARD, "", "0"),
            line(
                "b-delta",
                "2026-06-22",
                "-50.00",
                &transfer("DELTA SUPPLIES", "50,000"),
                "",
                "0",
            ),
            line(
                "b-bank",
                "2026-06-12",
                "-33.00",
                &transfer("ING", "33,000"),
                "",
                "0",
            ),
        ],
        "440000" => vec![
            // A closed Beta Office invoice, paid by the card line b-beta.
            line(
                "l-beta-i",
                "2026-05-01",
                "-30.00",
                "Factuur",
                "Beta Office",
                "9",
            ),
            line("l-beta-p", "2026-05-03", "30.00", CARD, "Beta Office", "0"),
        ],
        _ => Vec::new(),
    };
    response(
        "GLAccountTransactionsAndContact",
        &format!(
            "<GLAccountTransactions xmlns=\"\">{}</GLAccountTransactions>",
            lines.concat()
        ),
    )
}

fn outstanding() -> String {
    let items = [
        item("Alpha Trading", "2026-06-01", "120.50", "BE", ""),
        item("Cloud Host Inc", "2026-06-10", "22.10", "US", "Creditcard"),
        item("Beta Office", "2026-06-12", "45.00", "BE", ""),
        item("Gamma Newco", "2026-06-15", "12.30", "BE", ""),
        item("Delta Supplies", "2026-06-01", "-15.00", "BE", ""),
        item("Delta Supplies", "2026-06-20", "65.00", "BE", ""),
        item("Bookings Online", "2026-06-11", "33.00", "BE", ""),
    ];
    response("OutstandingCreditorItems", &items.concat())
}

/// Serve the mock on a random port; returns the API root and the request log.
fn mock() -> (String, RequestLog) {
    mock_yuki(|action, body| match action {
        "Authenticate" => response("Authenticate", "session-1"),
        "SetCurrentDomain" => response("SetCurrentDomain", ""),
        "OutstandingCreditorItems" => outstanding(),
        "GLAccountTransactionsAndContact" => {
            let account = body
                .split("<yuki:GLAccountCode>")
                .nth(1)
                .and_then(|rest| rest.split('<').next())
                .unwrap_or_default();
            gl(account)
        }
        other => response(other, ""),
    })
}

fn home_with_config(root: &str) -> TempDir {
    let home = TempDir::new().expect("temp home");
    let dir = home.path().join(".config/yuki");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(
        dir.join("config.toml"),
        format!(
            r#"api_key = "test-key"
default_admin = "example"
region = "be"

[administrations.example]
domain_id = "domain-1"
admin_id = "admin-1"
name = "Example BV"
base_url = "{root}"
bank_accounts = ["550000"]
"#
        ),
    )
    .expect("write config");
    home
}

#[test]
fn check_matches_labels_every_open_item_from_the_mocked_ledger() {
    let (root, seen) = mock();
    let home = home_with_config(&root);
    let output = yuki(&home, &["check", "matches", "--output", "json"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "check matches failed: {stderr}");
    let json: Value = serde_json::from_slice(&output.stdout).expect("JSON stdout");
    let rows = json["items"].as_array().expect("items");
    let label = |supplier: &str, open: &str| {
        let row = rows
            .iter()
            .find(|r| r["Supplier"] == supplier && r["Open"] == open)
            .unwrap_or_else(|| panic!("no row for {supplier} {open}: {json}"));
        (
            row["Confidence"].as_str().unwrap().to_string(),
            row["Reason"].as_str().unwrap().to_string(),
        )
    };

    let (conf, why) = label("Alpha Trading", "120.50");
    assert_eq!(
        (conf.as_str(), why.as_str()),
        ("high", "same amount and supplier, 4 days apart")
    );
    // The invoice says it was paid by credit card.
    assert_eq!(label("Cloud Host Inc", "22.10").0, "card");
    // Paid from the bank before, but no payment for this one.
    assert_eq!(label("Beta Office", "45.00").0, "none");
    // Never seen paid: neither card nor probably unpaid.
    assert_eq!(label("Gamma Newco", "12.30").0, "unseen");
    // 65.00 invoiced, 15.00 credited, 50.00 paid.
    let (conf, why) = label("Delta Supplies", "65.00");
    assert_eq!(conf, "medium");
    assert!(
        why.starts_with("one payment settles 1 invoice net of 1 credit note"),
        "{why}"
    );
    assert_eq!(label("Delta Supplies", "-15.00").0, "medium");
    // "ING" is no word of "Bookings Online": its 33.00 is not this invoice's.
    assert_eq!(label("Bookings Online", "33.00").0, "unseen");
    assert_eq!(rows.len(), 7);

    assert!(stderr.contains("API calls made: 6"), "{stderr}");
    let actions: Vec<String> = seen
        .lock()
        .expect("log")
        .iter()
        .map(|r| r.action.clone())
        .collect();
    assert_eq!(
        actions
            .iter()
            .filter(|a| *a == "GLAccountTransactionsAndContact")
            .count(),
        3,
        "{actions:?}"
    );
}
