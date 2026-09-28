use std::collections::BTreeMap;

use crate::client::accounting::{AccountingClient, Administration};
use crate::config::Config;
use crate::error::YukiError;
use crate::output::{
    ListOptions, OutputFormat, apply_pagination, format_json, format_table, is_tty, select_fields,
};

/// Reachability of an administration, as reported in the `Status` column.
///
/// A configured administration always gets a row. Reporting one as absent because
/// its key failed would make a broken key indistinguishable from an administration
/// that was never set up.
mod status {
    /// Configured, and its key returned it.
    pub const OK: &str = "ok";
    /// Configured, but its key was rejected.
    pub const AUTH_FAILED: &str = "auth failed";
    /// Configured, key accepted, but the administration was not among the results.
    pub const UNREACHABLE: &str = "unreachable";
    /// Reachable by a configured key but absent from the config.
    pub const NOT_CONFIGURED: &str = "not configured";
    /// `--local` was passed, so nothing was verified against the API.
    pub const NOT_CHECKED: &str = "not checked";
}

/// Placeholder for a display name the configuration has never recorded.
const UNKNOWN_NAME: &str = "-";

#[derive(Debug)]
struct Row {
    name: String,
    config_name: String,
    admin_id: String,
    domain_id: String,
    is_default: bool,
    status: &'static str,
}

impl Row {
    fn into_cells(self) -> Vec<String> {
        vec![
            self.name,
            self.config_name,
            self.admin_id,
            self.domain_id,
            if self.is_default { "Yes" } else { "No" }.to_string(),
            self.status.to_string(),
        ]
    }
}

fn headers() -> Vec<String> {
    vec![
        "Name".into(),
        "Config".into(),
        "Admin ID".into(),
        "Domain ID".into(),
        "Default".into(),
        "Status".into(),
    ]
}

/// Build the rows for `--local`: what the config holds, with nothing verified.
fn local_rows(config: &Config) -> Vec<Row> {
    config
        .administrations
        .iter()
        .map(|(name, entry)| Row {
            name: entry.name.clone().unwrap_or_else(|| UNKNOWN_NAME.into()),
            config_name: name.clone(),
            admin_id: entry.admin_id.clone(),
            domain_id: entry.domain_id.clone(),
            is_default: *name == config.default_admin,
            status: status::NOT_CHECKED,
        })
        .collect()
}

/// Query every distinct configured key and reconcile the results against the config.
///
/// Each key is contacted exactly once. Administrations the keys expose but the config
/// does not name are listed too, so a newly granted one is visible before `init --add`
/// has been run for it.
async fn remote_rows(config: &Config) -> Result<Vec<Row>, YukiError> {
    let keys = config.access_keys();
    if keys.is_empty() {
        return Err(YukiError::Config(
            "no API key configured; run 'yuki init'".into(),
        ));
    }

    // Administrations returned per key, keyed by the key itself.
    let mut reachable: BTreeMap<&str, Vec<Administration>> = BTreeMap::new();
    let mut first_failure: Option<YukiError> = None;

    for key in &keys {
        let mut client = AccountingClient::new().with_api_root(key.api_root);
        let outcome = match client.authenticate(key.api_key).await {
            Ok(_) => client.administrations().await,
            Err(e) => Err(e),
        };
        match outcome {
            Ok(admins) => {
                reachable.insert(key.api_key, admins);
            }
            Err(e) => {
                let used_by = if key.admins.is_empty() {
                    "the shared api_key".to_string()
                } else {
                    key.admins.join(", ")
                };
                eprintln!("warning: key for {used_by} could not be used: {e}");
                if first_failure.is_none() {
                    first_failure = Some(e);
                }
            }
        }
    }

    // Every key failed: this is a broken configuration, not an empty result.
    if reachable.is_empty() {
        return Err(first_failure.unwrap_or_else(|| {
            YukiError::Config("no administrations reachable with the configured keys".into())
        }));
    }

    Ok(reconcile(config, &reachable))
}

/// Match what the keys returned against what the configuration claims.
///
/// `reachable` holds the administrations each key returned, keyed by that key; a key
/// that failed is absent from it, which is what separates `auth failed` from
/// `unreachable`.
fn reconcile(config: &Config, reachable: &BTreeMap<&str, Vec<Administration>>) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut claimed: Vec<&str> = Vec::new();

    for (name, entry) in &config.administrations {
        let api_key = entry.api_key.as_deref().unwrap_or(&config.api_key);
        let live = reachable
            .get(api_key)
            .and_then(|admins| admins.iter().find(|a| a.id == entry.admin_id));

        let status = match (reachable.contains_key(api_key), live.is_some()) {
            (_, true) => status::OK,
            (true, false) => status::UNREACHABLE,
            (false, false) => status::AUTH_FAILED,
        };
        if live.is_some() {
            claimed.push(&entry.admin_id);
        }

        rows.push(Row {
            name: live
                .map(|a| a.name.clone())
                .or_else(|| entry.name.clone())
                .unwrap_or_else(|| UNKNOWN_NAME.into()),
            config_name: name.clone(),
            admin_id: entry.admin_id.clone(),
            domain_id: entry.domain_id.clone(),
            is_default: *name == config.default_admin,
            status,
        });
    }

    // Anything a key can reach that no configured entry accounts for.
    let mut extra: Vec<&Administration> = reachable
        .values()
        .flatten()
        .filter(|a| !claimed.contains(&a.id.as_str()))
        .collect();
    extra.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    extra.dedup_by(|a, b| a.id == b.id);

    rows.extend(extra.into_iter().map(|a| Row {
        name: a.name.clone(),
        config_name: String::new(),
        admin_id: a.id.clone(),
        domain_id: a.domain_id.clone(),
        is_default: false,
        status: status::NOT_CONFIGURED,
    }));

    rows
}

pub async fn list(
    config: &Config,
    local: bool,
    format: Option<&str>,
    opts: ListOptions<'_>,
) -> Result<(), YukiError> {
    let rows = if local {
        local_rows(config)
    } else {
        remote_rows(config).await?
    };

    let mut headers = headers();
    let mut rows: Vec<Vec<String>> = rows.into_iter().map(Row::into_cells).collect();

    apply_pagination(&mut rows, &opts);
    select_fields(&mut headers, &mut rows, &opts)?;

    let fmt = OutputFormat::from_flag(format, is_tty());
    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}

pub fn switch(config: &mut Config, name: &str) -> Result<(), YukiError> {
    if !config.administrations.contains_key(name) {
        return Err(YukiError::Config(format!(
            "unknown administration: {name}. Run 'yuki admin list' to see available administrations."
        )));
    }
    config.default_admin = name.to_string();
    config.save_to(&Config::default_path())?;
    eprintln!("Switched default administration to: {name}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AdminEntry;

    fn admin(id: &str, name: &str, domain_id: &str) -> Administration {
        Administration {
            id: id.into(),
            name: name.into(),
            domain_id: domain_id.into(),
        }
    }

    fn config_with(
        api_key: &str,
        default_admin: &str,
        entries: impl IntoIterator<Item = (&'static str, AdminEntry)>,
    ) -> Config {
        Config {
            api_key: api_key.into(),
            default_admin: default_admin.into(),
            administrations: entries
                .into_iter()
                .map(|(name, entry)| (name.to_string(), entry))
                .collect(),
            unmatched_ignore: Vec::new(),
            region: None,
            base_url: None,
            endpoint_override: None,
        }
    }

    /// Find a row by its config name, or by display name for unconfigured ones.
    fn row<'a>(rows: &'a [Row], key: &str) -> &'a Row {
        rows.iter()
            .find(|r| r.config_name == key || (r.config_name.is_empty() && r.name == key))
            .unwrap_or_else(|| panic!("no row for {key}"))
    }

    #[test]
    fn each_key_reports_only_the_administrations_it_reaches() {
        let config = config_with(
            "trading-key",
            "trading",
            [
                ("trading", AdminEntry::new("domain-a", "admin-a")),
                (
                    "holding",
                    AdminEntry::new("domain-b", "admin-b").with_api_key("holding-key"),
                ),
            ],
        );
        let reachable = BTreeMap::from([
            (
                "trading-key",
                vec![admin("admin-a", "Example Trading B.V.", "domain-a")],
            ),
            (
                "holding-key",
                vec![admin("admin-b", "Example Holding B.V.", "domain-b")],
            ),
        ]);

        let rows = reconcile(&config, &reachable);
        assert_eq!(rows.len(), 2);
        assert_eq!(row(&rows, "trading").status, status::OK);
        assert_eq!(row(&rows, "holding").status, status::OK);
        // The live name wins over whatever the config recorded earlier.
        assert_eq!(row(&rows, "holding").name, "Example Holding B.V.");
        assert!(row(&rows, "trading").is_default);
        assert!(!row(&rows, "holding").is_default);
    }

    #[test]
    fn a_rejected_key_leaves_its_administration_visible() {
        // The defect this guards: dropping the row would make a broken key
        // indistinguishable from an administration that was never configured.
        let config = config_with(
            "trading-key",
            "trading",
            [
                ("trading", AdminEntry::new("domain-a", "admin-a")),
                (
                    "holding",
                    AdminEntry::new("domain-b", "admin-b")
                        .with_name("Example Holding B.V.")
                        .with_api_key("revoked-key"),
                ),
            ],
        );
        let reachable = BTreeMap::from([(
            "trading-key",
            vec![admin("admin-a", "Example Trading B.V.", "domain-a")],
        )]);

        let rows = reconcile(&config, &reachable);
        assert_eq!(rows.len(), 2, "the rejected key still gets a row: {rows:?}");
        assert_eq!(row(&rows, "holding").status, status::AUTH_FAILED);
        // With nothing live to name it, the recorded name carries the row.
        assert_eq!(row(&rows, "holding").name, "Example Holding B.V.");
        assert_eq!(row(&rows, "holding").admin_id, "admin-b");
    }

    #[test]
    fn a_working_key_that_no_longer_returns_an_administration_reads_as_unreachable() {
        // Distinct from auth failed: the key works, so access was withdrawn or the
        // recorded admin_id is stale. Collapsing the two would hide which it is.
        let config = config_with(
            "trading-key",
            "trading",
            [
                ("trading", AdminEntry::new("domain-a", "admin-a")),
                ("gone", AdminEntry::new("domain-c", "admin-c")),
            ],
        );
        let reachable = BTreeMap::from([(
            "trading-key",
            vec![admin("admin-a", "Example Trading B.V.", "domain-a")],
        )]);

        let rows = reconcile(&config, &reachable);
        assert_eq!(row(&rows, "gone").status, status::UNREACHABLE);
        assert_eq!(row(&rows, "gone").name, UNKNOWN_NAME);
    }

    #[test]
    fn an_administration_the_config_does_not_name_is_reported_as_unconfigured() {
        let config = config_with(
            "trading-key",
            "trading",
            [("trading", AdminEntry::new("domain-a", "admin-a"))],
        );
        let reachable = BTreeMap::from([(
            "trading-key",
            vec![
                admin("admin-a", "Example Trading B.V.", "domain-a"),
                admin("admin-b", "Example Holding B.V.", "domain-b"),
            ],
        )]);

        let rows = reconcile(&config, &reachable);
        assert_eq!(rows.len(), 2);
        let extra = row(&rows, "Example Holding B.V.");
        assert_eq!(extra.status, status::NOT_CONFIGURED);
        assert_eq!(extra.admin_id, "admin-b");
        assert_eq!(extra.domain_id, "domain-b");
        assert!(extra.config_name.is_empty());
    }

    #[test]
    fn an_administration_two_keys_both_reach_is_listed_once() {
        let config = config_with(
            "trading-key",
            "trading",
            [(
                "other",
                AdminEntry::new("domain-z", "admin-z").with_api_key("holding-key"),
            )],
        );
        let shared = admin("admin-a", "Example Trading B.V.", "domain-a");
        let reachable = BTreeMap::from([
            ("trading-key", vec![shared.clone()]),
            (
                "holding-key",
                vec![shared, admin("admin-z", "Other B.V.", "domain-z")],
            ),
        ]);

        let rows = reconcile(&config, &reachable);
        assert_eq!(
            rows.len(),
            2,
            "one configured row plus one unconfigured row: {rows:?}"
        );
        for id in ["admin-a", "admin-z"] {
            assert_eq!(
                rows.iter().filter(|r| r.admin_id == id).count(),
                1,
                "{id} is listed once: {rows:?}"
            );
        }
        assert_eq!(row(&rows, "other").status, status::OK);
        assert_eq!(
            row(&rows, "Example Trading B.V.").status,
            status::NOT_CONFIGURED
        );
    }

    #[test]
    fn local_rows_verify_nothing() {
        let config = config_with(
            "trading-key",
            "trading",
            [(
                "trading",
                AdminEntry::new("domain-a", "admin-a").with_name("Example Trading B.V."),
            )],
        );
        let rows = local_rows(&config);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, status::NOT_CHECKED);
        assert_eq!(rows[0].name, "Example Trading B.V.");
        assert!(rows[0].is_default);
    }

    #[test]
    fn local_rows_do_not_invent_a_name() {
        let config = config_with(
            "trading-key",
            "trading",
            [("trading", AdminEntry::new("domain-a", "admin-a"))],
        );
        let rows = local_rows(&config);
        assert_eq!(rows[0].name, UNKNOWN_NAME);
    }
}
