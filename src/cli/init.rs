use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};

use owo_colors::OwoColorize;

use crate::client::Region;
use crate::client::accounting::{AccountingClient, Administration};
use crate::config::{AdminEntry, Config};
use crate::error::YukiError;
use crate::output::is_tty;

fn sym_ok() -> String {
    if is_tty() {
        "✔".green().to_string()
    } else {
        "✔".to_owned()
    }
}

fn sym_fail() -> String {
    if is_tty() {
        "✖".red().to_string()
    } else {
        "✖".to_owned()
    }
}

fn bold(s: &str) -> String {
    if is_tty() {
        s.bold().to_string()
    } else {
        s.to_owned()
    }
}

fn dim(s: &str) -> String {
    if is_tty() {
        s.dimmed().to_string()
    } else {
        s.to_owned()
    }
}

/// Convert an administration name to a safe config key.
///
/// Lowercases the name and replaces any non-alphanumeric character with an underscore.
fn safe_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Index discovered administrations by their config key.
fn to_entries(admins: &[Administration]) -> BTreeMap<String, AdminEntry> {
    admins
        .iter()
        .map(|a| {
            (
                safe_name(&a.name),
                AdminEntry::new(&a.domain_id, &a.id).with_name(&a.name),
            )
        })
        .collect()
}

fn read_line(stdin: &io::Stdin) -> String {
    stdin
        .lock()
        .lines()
        .next()
        .and_then(|l| l.ok())
        .map(|l| l.trim().to_string())
        .unwrap_or_default()
}

/// The region `yuki init` stores: only a non-default one, so a Dutch
/// configuration stays exactly as it was before regions existed.
fn stored_region(region: Region) -> Option<Region> {
    (region != Region::default()).then_some(region)
}

/// Tell the user an environment region was used for this run but not stored.
fn note_unstored_region(region: Option<Region>, region_flag: Option<Region>, stored: Region) {
    if let Some(region) = region
        && region_flag.is_none()
        && region != stored
    {
        eprintln!(
            "{}",
            dim(&format!(
                "note: YUKI_REGION={region} applies to this run only; \
                 pass --region {region} to store it."
            ))
        );
    }
}

/// Administrations for a plain `yuki init`: what discovery found, keeping every
/// per-administration setting of an administration that was configured before.
///
/// An earlier entry is matched by `admin_id`, preferring the same config name.
/// The key init was given becomes the shared one, so a matched entry's own key
/// is dropped (discovery entries carry none).
fn rebuild_administrations(
    previous: Option<&BTreeMap<String, AdminEntry>>,
    discovered: BTreeMap<String, AdminEntry>,
) -> BTreeMap<String, AdminEntry> {
    discovered
        .into_iter()
        .map(|(name, entry)| {
            let earlier = previous.and_then(|p| {
                p.get(&name)
                    .filter(|e| e.admin_id == entry.admin_id)
                    .or_else(|| p.values().find(|e| e.admin_id == entry.admin_id))
            });
            let merged = match earlier {
                Some(earlier) => {
                    let mut merged = earlier.clone();
                    merged.refresh(entry);
                    merged
                }
                None => entry,
            };
            (name, merged)
        })
        .collect()
}

/// Authenticate with `api_key` and list the administrations it reaches.
async fn discover(api_key: &str, api_root: &str) -> Result<Vec<Administration>, YukiError> {
    eprint!("Authenticating...");
    io::stderr().flush().ok();
    let mut client = AccountingClient::new().with_api_root(api_root);
    match client.authenticate(api_key).await {
        Ok(_) => eprintln!(" {}", sym_ok()),
        Err(e) => {
            eprintln!(" {}", sym_fail());
            return Err(e);
        }
    }

    eprint!("Fetching administrations...");
    io::stderr().flush().ok();
    match client.administrations().await {
        Ok(admins) => {
            eprintln!(" {}", sym_ok());
            Ok(admins)
        }
        Err(e) => {
            eprintln!(" {}", sym_fail());
            Err(e)
        }
    }
}

/// Run `yuki init`.
///
/// `region` is the deployment for this run, from `--region` or `YUKI_REGION`;
/// `region_flag` is the same value only when `--region` was typed on the command
/// line. Only the flag is persisted: an exported environment variable is a
/// per-shell choice and must not silently rewrite the configuration. `base_url`
/// (flag or env) is never persisted.
pub async fn run(
    api_key: Option<&str>,
    default_admin: Option<&str>,
    add: bool,
    region: Option<Region>,
    region_flag: Option<Region>,
    base_url: Option<&str>,
) -> Result<(), YukiError> {
    let stdin = io::stdin();
    let path = Config::default_path();

    // Rotating the shared key touches nothing else, so it skips discovery entirely.
    if !add
        && let (Some(key), None) = (api_key, default_admin)
        && let Ok(mut config) = Config::load_from(&path)
    {
        let key = key.trim().to_string();
        if key.is_empty() {
            return Err(YukiError::Config("API key cannot be empty".to_string()));
        }

        if let Some(region) = region_flag {
            config.region = stored_region(region);
        }
        note_unstored_region(region, region_flag, config.region.unwrap_or_default());
        config.override_endpoint(region, base_url);

        eprintln!("Verifying new API key...");
        let mut client = AccountingClient::new().with_api_root(config.api_root(None));
        client.authenticate(&key).await?;

        config.api_key = key;
        config.save_to(&path)?;
        eprintln!("{} API key updated in {}", sym_ok(), path.display());
        return Ok(());
    }

    let api_key = match api_key {
        Some(k) => k.trim().to_string(),
        None => {
            eprintln!("  {} Yuki Portal → Settings → API keys", dim("→"));
            eprint!("Yuki API key: ");
            io::stderr().flush().ok();
            read_line(&stdin)
        }
    };

    if api_key.is_empty() {
        return Err(YukiError::Config("API key cannot be empty".to_string()));
    }

    if add {
        return add_key(
            &path,
            &api_key,
            default_admin,
            region,
            region_flag,
            base_url,
        )
        .await;
    }

    let existing = Config::load_from(&path).ok();
    // A re-run keeps the stored region unless the flag says otherwise.
    let stored = region_flag
        .or(existing.as_ref().and_then(|c| c.region))
        .unwrap_or_default();
    note_unstored_region(region, region_flag, stored);
    // Start from what will be saved, so discovery resolves the endpoint exactly
    // as later commands will. The run override is never saved.
    let mut config = Config {
        api_key,
        default_admin: String::new(),
        administrations: BTreeMap::new(),
        // Preserve unmatched_ignore from the existing config if present.
        unmatched_ignore: existing
            .as_ref()
            .map(|c| c.unmatched_ignore.clone())
            .unwrap_or_default(),
        region: stored_region(stored),
        base_url: existing.as_ref().and_then(|c| c.base_url.clone()),
        endpoint_override: None,
    };
    config.override_endpoint(region, base_url);

    let admins = discover(&config.api_key, config.api_root(None)).await?;
    if admins.is_empty() {
        return Err(YukiError::NotFound(
            "no administrations found for this API key".to_string(),
        ));
    }

    eprintln!("Found {} administration(s):", admins.len());
    for (i, a) in admins.iter().enumerate() {
        eprintln!("  [{}] {}", i + 1, a.name);
    }

    let default_name = if let Some(name) = default_admin {
        // Use the provided name directly, verifying it exists.
        let key = safe_name(name);
        if !admins.iter().any(|a| safe_name(&a.name) == key) {
            return Err(YukiError::Config(format!(
                "administration not found: {name}"
            )));
        }
        eprintln!("Using \"{name}\" as the default administration.");
        key
    } else if admins.len() == 1 {
        eprintln!(
            "Using \"{}\" as the default administration.",
            admins[0].name
        );
        safe_name(&admins[0].name)
    } else {
        eprint!("Select default administration [1]: ");
        io::stderr().flush().ok();

        let choice = read_line(&stdin);
        let idx: usize = if choice.is_empty() {
            1
        } else {
            choice
                .parse::<usize>()
                .map_err(|_| YukiError::Config(format!("invalid selection: {choice}")))?
        };

        if idx == 0 || idx > admins.len() {
            return Err(YukiError::Config(format!("selection out of range: {idx}")));
        }
        safe_name(&admins[idx - 1].name)
    };

    let administrations = rebuild_administrations(
        existing.as_ref().map(|c| &c.administrations),
        to_entries(&admins),
    );

    // A plain init replaces the administration map, so anything this key cannot reach
    // is about to disappear. Say which, rather than let a second set of books vanish.
    if let Some(previous) = &existing {
        let dropped: Vec<&str> = previous
            .administrations
            .iter()
            .filter(|(name, entry)| {
                administrations
                    .get(*name)
                    .is_none_or(|new| new.admin_id != entry.admin_id)
            })
            .map(|(name, _)| name.as_str())
            .collect();
        if !dropped.is_empty() {
            eprintln!();
            eprintln!(
                "{} this key does not reach {}, which init removes from the config.",
                sym_fail(),
                dropped.join(", ")
            );
            eprintln!(
                "  {}",
                dim("Run 'yuki init --add --api-key <key>' instead to keep both.")
            );
            eprintln!();
        }
    }

    config.default_admin = default_name;
    config.administrations = administrations;
    config.save_to(&path)?;

    eprintln!();
    eprintln!("{} Configuration saved to {}", sym_ok(), path.display());
    eprintln!();
    eprintln!("{}:", bold("Next steps"));
    eprintln!(
        "  yuki documents list  {}",
        dim("# list archived documents")
    );
    eprintln!("  yuki contacts list   {}", dim("# list contacts"));
    eprintln!("  yuki invoices list   {}", dim("# list invoices"));
    eprintln!("  yuki completions zsh {}", dim("# shell completions"));
    eprintln!();

    Ok(())
}

/// Merge a second key's administrations into an existing configuration.
///
/// A Yuki access key is scoped to the administration it was created in, so this is
/// how a second set of books becomes reachable: the key is stored against the
/// administrations it opens, and every command that targets one authenticates with it.
async fn add_key(
    path: &std::path::Path,
    api_key: &str,
    default_admin: Option<&str>,
    region: Option<Region>,
    region_flag: Option<Region>,
    base_url: Option<&str>,
) -> Result<(), YukiError> {
    // Distinguish "nothing to add to" from a config that exists but will not parse;
    // the second is a different problem and keeps its own error.
    if !path.exists() {
        return Err(YukiError::Config(format!(
            "no configuration at {}. Run 'yuki init' first; --add extends an existing one.",
            path.display()
        )));
    }
    let mut config = Config::load_from(path)?;

    // Administrations on another deployment than the configured default carry
    // their own region, so a Dutch and a Belgian set of books can coexist.
    // Only the flag stamps a region: an exported YUKI_REGION is for this run.
    let default_region = config.region.unwrap_or_default();
    let stamp = region_flag.filter(|r| *r != default_region);
    note_unstored_region(region, region_flag, default_region);

    // Resolve the endpoint as for an administration carrying the stamp.
    let probe = AdminEntry {
        region: stamp,
        ..AdminEntry::new("", "")
    };
    config.override_endpoint(region, base_url);
    let api_root = config.api_root(Some(&probe)).to_string();

    let admins = discover(api_key, &api_root).await?;
    if admins.is_empty() {
        return Err(YukiError::NotFound(
            "no administrations found for this API key".to_string(),
        ));
    }

    let mut entries = to_entries(&admins);
    if stamp.is_some() {
        for entry in entries.values_mut() {
            entry.region = stamp;
        }
    }
    let (added, updated) = config.merge_administrations(entries, api_key);

    if let Some(name) = default_admin {
        let key = safe_name(name);
        if !config.administrations.contains_key(&key) {
            return Err(YukiError::Config(format!(
                "administration not found: {name}"
            )));
        }
        config.default_admin = key;
    }

    config.save_to(path)?;

    eprintln!();
    for name in &added {
        eprintln!("{} added {name}", sym_ok());
    }
    for name in &updated {
        eprintln!("{} updated {name}", sym_ok());
    }
    eprintln!(
        "{} Configuration saved to {} (default: {})",
        sym_ok(),
        path.display(),
        config.default_admin
    );
    eprintln!();
    eprintln!("{}:", bold("Next steps"));
    eprintln!("  yuki admin list                {}", dim("# verify both"));
    for name in added.iter().chain(updated.iter()) {
        eprintln!("  yuki --admin {name} documents list");
    }
    eprintln!();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_name_lowercases_and_replaces_punctuation() {
        assert_eq!(safe_name("Example Holding B.V."), "example_holding_b_v_");
        // Digits survive; only non-alphanumerics become underscores.
        assert_eq!(safe_name("Acme 42 B.V."), "acme_42_b_v_");
    }

    #[test]
    fn to_entries_records_the_display_name() {
        let admins = vec![Administration {
            name: "Example Holding B.V.".into(),
            id: "admin-1".into(),
            domain_id: "domain-1".into(),
        }];
        let entries = to_entries(&admins);
        let entry = &entries["example_holding_b_v_"];
        assert_eq!(entry.name.as_deref(), Some("Example Holding B.V."));
        assert_eq!(entry.admin_id, "admin-1");
        assert_eq!(entry.domain_id, "domain-1");
        // Discovery does not decide which key an entry belongs to; merging does.
        assert_eq!(entry.api_key, None);
        assert_eq!(entry.region, None);
    }

    #[test]
    fn a_plain_reinit_keeps_the_settings_of_administrations_that_still_exist() {
        let mut earlier = AdminEntry::new("domain-old", "admin-be").with_api_key("stale-key");
        earlier.region = Some(Region::Be);
        earlier.bank_accounts = vec!["550002".into()];
        earlier.creditor_accounts = Some(vec!["440000".into()]);
        earlier.unmatched_ignore_descriptions = Some(vec![]);
        let previous = BTreeMap::from([("voorbeeld_bv".to_string(), earlier)]);
        let discovered = to_entries(&[Administration {
            name: "Voorbeeld BV".into(),
            id: "admin-be".into(),
            domain_id: "domain-be".into(),
        }]);

        let rebuilt = rebuild_administrations(Some(&previous), discovered);
        let entry = &rebuilt["voorbeeld_bv"];
        assert_eq!(entry.domain_id, "domain-be");
        assert_eq!(entry.region, Some(Region::Be));
        assert_eq!(entry.bank_accounts, ["550002"]);
        assert_eq!(entry.creditor_accounts, Some(vec!["440000".to_string()]));
        assert_eq!(entry.unmatched_ignore_descriptions, Some(vec![]));
        // The new key is the shared one now, so the stale per-admin key goes.
        assert_eq!(entry.api_key, None);
    }

    #[test]
    fn only_a_non_default_region_is_stored() {
        assert_eq!(stored_region(Region::Nl), None);
        assert_eq!(stored_region(Region::Be), Some(Region::Be));
    }
}
