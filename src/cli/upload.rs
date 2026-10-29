use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::client::archive::ArchiveClient;
use crate::config::Config;
use crate::error::YukiError;
use crate::output::{OutputFormat, format_json, format_table, is_tty};

/// Options for uploading a document with invoice metadata.
pub struct UploadOptions<'a> {
    pub folder: &'a str,
    pub amount: Option<f64>,
    pub category: Option<&'a str>,
    pub payment_method: Option<&'a str>,
    pub project: Option<&'a str>,
    pub remarks: Option<&'a str>,
    pub currency: &'a str,
}

use crate::folders::folder_id;

/// Upload a document to the Yuki archive.
///
/// When `options.amount` is provided, the richer `UploadDocumentWithData` operation is used,
/// allowing cost category, payment method, project, and remarks to be attached.
/// Otherwise `UploadDocument` is used.
pub async fn run(
    config: &Config,
    admin: Option<&str>,
    file: &str,
    options: UploadOptions<'_>,
    format: Option<&str>,
    quiet: bool,
) -> Result<(), YukiError> {
    let fid = folder_id(options.folder)?;

    let bytes = std::fs::read(file).map_err(|e| YukiError::Config(format!("{file}: {e}")))?;
    let data_base64 = BASE64.encode(&bytes);
    let filename = Path::new(file)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(file);

    let target = config.target(admin)?;
    let mut client = ArchiveClient::new().with_api_root(target.api_root);
    client.authenticate(target.api_key).await?;

    let doc_id = match options.amount {
        Some(amt) => {
            client
                .upload_document_with_data(
                    target.admin_id,
                    filename,
                    &data_base64,
                    fid,
                    options.currency,
                    amt,
                    options.category,
                    options.payment_method,
                    options.project,
                    options.remarks,
                )
                .await?
        }
        None => {
            client
                .upload_document(target.admin_id, filename, &data_base64, fid)
                .await?
        }
    };

    if quiet {
        return Ok(());
    }

    let headers = vec!["Document ID".to_string()];
    let rows = vec![vec![doc_id]];

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }

    Ok(())
}

/// List all available cost categories.
pub async fn categories(
    config: &Config,
    admin: Option<&str>,
    format: Option<&str>,
) -> Result<(), YukiError> {
    let target = config.target(admin)?;
    let mut client = ArchiveClient::new().with_api_root(target.api_root);
    client.authenticate(target.api_key).await?;
    let cats = client.cost_categories().await?;

    let headers = vec!["ID".to_string(), "Description".to_string()];
    let rows: Vec<Vec<String>> = cats
        .into_iter()
        .map(|c| vec![c.id, c.description])
        .collect();

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }

    Ok(())
}

/// List all available payment methods.
pub async fn payment_methods(
    config: &Config,
    admin: Option<&str>,
    format: Option<&str>,
) -> Result<(), YukiError> {
    let target = config.target(admin)?;
    let mut client = ArchiveClient::new().with_api_root(target.api_root);
    client.authenticate(target.api_key).await?;
    let methods = client.payment_methods().await?;

    let headers = vec!["ID".to_string(), "Description".to_string()];
    let rows: Vec<Vec<String>> = methods
        .into_iter()
        .map(|m| vec![m.id, m.description])
        .collect();

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
    fn folder_id_inkoop() {
        assert_eq!(folder_id("inkoop").unwrap(), 1);
        assert_eq!(folder_id("purchase").unwrap(), 1);
    }

    #[test]
    fn folder_id_verkoop() {
        assert_eq!(folder_id("verkoop").unwrap(), 2);
        assert_eq!(folder_id("sales").unwrap(), 2);
    }

    #[test]
    fn folder_id_uitzoeken_default() {
        assert_eq!(folder_id("uitzoeken").unwrap(), 7);
    }

    #[test]
    fn folder_id_all_known() {
        assert_eq!(folder_id("bank").unwrap(), 3);
        assert_eq!(folder_id("personeel").unwrap(), 4);
        assert_eq!(folder_id("personnel").unwrap(), 4);
        assert_eq!(folder_id("belasting").unwrap(), 5);
        assert_eq!(folder_id("tax").unwrap(), 5);
        assert_eq!(folder_id("overig-financieel").unwrap(), 8);
        assert_eq!(folder_id("other").unwrap(), 8);
    }

    #[test]
    fn folder_id_unknown_errors() {
        assert!(folder_id("nonexistent").is_err());
    }
}
