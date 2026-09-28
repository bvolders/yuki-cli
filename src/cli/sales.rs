use crate::client::sales::SalesClient;
use crate::config::Config;
use crate::error::YukiError;
use crate::output::{
    ListOptions, OutputFormat, apply_pagination, format_json, format_table, is_tty, select_fields,
};

/// List the sales items catalogue (products and services available for invoicing).
pub async fn items(
    config: &Config,
    admin: Option<&str>,
    format: Option<&str>,
    opts: ListOptions<'_>,
) -> Result<(), YukiError> {
    let target = config.target(admin)?;
    let mut client = SalesClient::new().with_api_root(target.api_root);
    client.authenticate(target.api_key).await?;
    let items = client.get_sales_items().await?;

    let mut headers = vec!["ID".into(), "Description".into()];
    let mut rows: Vec<Vec<String>> = items
        .iter()
        .map(|i| vec![i.id.clone(), i.description.clone()])
        .collect();
    apply_pagination(&mut rows, &opts);
    select_fields(&mut headers, &mut rows, &opts)?;

    match OutputFormat::from_flag(format, is_tty()) {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}
