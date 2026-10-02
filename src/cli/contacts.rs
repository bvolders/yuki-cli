use crate::client::contact::{Contact, ContactClient, SEARCH_OPTIONS};
use crate::config::Config;
use crate::error::YukiError;
use crate::output::{
    ListOptions, OutputFormat, apply_pagination, format_json, format_table, is_tty, select_fields,
};

fn yes_no(value: bool) -> String {
    if value { "Yes" } else { "No" }.to_string()
}

fn contacts_to_rows(contacts: &[Contact]) -> Vec<Vec<String>> {
    contacts
        .iter()
        .map(|c| {
            vec![
                c.id.clone(),
                c.name.clone(),
                c.contact_type.clone(),
                c.country.clone(),
                yes_no(c.is_supplier),
                yes_no(c.is_customer),
            ]
        })
        .collect()
}

/// Search contacts by `by` (one of Yuki's search options, any casing).
///
/// Shows the fields an invoice template takes: code (often empty in Yuki),
/// HID, city and VAT number next to name and country.
pub async fn search(
    config: &Config,
    admin: Option<&str>,
    query: &str,
    by: &str,
    format: Option<&str>,
) -> Result<(), YukiError> {
    let option = SEARCH_OPTIONS
        .iter()
        .find(|o| o.eq_ignore_ascii_case(by))
        .ok_or_else(|| {
            YukiError::Config(format!(
                "unknown search field: {by} (expected one of: {})",
                SEARCH_OPTIONS.join(", ")
            ))
        })?;
    let target = config.target(admin)?;
    let mut client = ContactClient::new().with_api_root(target.api_root);
    client.authenticate(target.api_key).await?;
    let contacts = client.search_contacts(option, query).await?;

    let headers: Vec<String> = [
        "ID",
        "HID",
        "Code",
        "Name",
        "Type",
        "City",
        "Country",
        "VAT Number",
        "Supplier",
        "Customer",
    ]
    .map(String::from)
    .to_vec();
    let rows: Vec<Vec<String>> = contacts
        .iter()
        .map(|c| {
            vec![
                c.id.clone(),
                c.hid.clone(),
                c.code.clone(),
                c.name.clone(),
                c.contact_type.clone(),
                c.city.clone(),
                c.country.clone(),
                c.vat_number.clone(),
                yes_no(c.is_supplier),
                yes_no(c.is_customer),
            ]
        })
        .collect();

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}

/// The contact types Yuki's `ContactType` enum accepts.
const CONTACT_TYPES: &[&str] = &["Customer", "Supplier", "Both", "None"];

/// Normalise a contact type to the exact casing Yuki's schema requires.
///
/// Defaults to `Both`. An empty string is not a member of the enum, so passing one
/// through makes the API reject the whole request with a schema validation fault.
fn contact_type_value(requested: Option<&str>) -> Result<&'static str, YukiError> {
    let Some(raw) = requested.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok("Both");
    };
    CONTACT_TYPES
        .iter()
        .find(|valid| valid.eq_ignore_ascii_case(raw))
        .copied()
        .ok_or_else(|| {
            YukiError::Config(format!(
                "unknown contact type: {raw} (expected one of: {})",
                CONTACT_TYPES.join(", ")
            ))
        })
}

pub async fn list(
    config: &Config,
    admin: Option<&str>,
    contact_type: Option<&str>,
    format: Option<&str>,
    opts: ListOptions<'_>,
) -> Result<(), YukiError> {
    let contact_type = contact_type_value(contact_type)?;
    let target = config.target(admin)?;
    let mut client = ContactClient::new().with_api_root(target.api_root);
    client.authenticate(target.api_key).await?;
    let contacts = client.get_suppliers_and_customers(contact_type).await?;

    let mut headers = vec![
        "ID".into(),
        "Name".into(),
        "Type".into(),
        "Country".into(),
        "Supplier".into(),
        "Customer".into(),
    ];
    let mut rows = contacts_to_rows(&contacts);
    apply_pagination(&mut rows, &opts);
    select_fields(&mut headers, &mut rows, &opts)?;

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_both_when_unset() {
        // Regression: `unwrap_or("")` sent an empty ContactType, which Yuki rejects
        // with a schema validation fault, breaking `contacts list` entirely.
        assert_eq!(contact_type_value(None).unwrap(), "Both");
        assert_eq!(contact_type_value(Some("")).unwrap(), "Both");
        assert_eq!(contact_type_value(Some("   ")).unwrap(), "Both");
    }

    #[test]
    fn normalises_casing_to_the_schema() {
        assert_eq!(contact_type_value(Some("customer")).unwrap(), "Customer");
        assert_eq!(contact_type_value(Some("SUPPLIER")).unwrap(), "Supplier");
        assert_eq!(contact_type_value(Some("Both")).unwrap(), "Both");
        assert_eq!(contact_type_value(Some("none")).unwrap(), "None");
    }

    #[test]
    fn rejects_values_outside_the_enum() {
        let err = contact_type_value(Some("vendor")).unwrap_err();
        assert!(err.to_string().contains("unknown contact type"));
    }
}
