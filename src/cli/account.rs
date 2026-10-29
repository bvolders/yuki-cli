use serde_json::{Value, json};

use crate::client::accounting::AccountingClient;
use crate::config::{AdminEntry, Config};
use crate::error::YukiError;
use crate::output::{OutputFormat, is_tty};

fn json_output(format: Option<&str>) -> bool {
    matches!(
        OutputFormat::from_flag(format, is_tty()),
        OutputFormat::Json
    )
}

fn print_result(format: Option<&str>, quiet: bool, value: &Value, human: &str) {
    if quiet {
        return;
    }
    if json_output(format) {
        println!(
            "{}",
            serde_json::to_string_pretty(value).expect("serialize account result")
        );
    } else {
        println!("{human}");
    }
}

fn selected<'a>(
    config: &'a Config,
    profile: Option<&str>,
) -> Result<(&'a str, &'a AdminEntry), YukiError> {
    let name = profile.unwrap_or(&config.default_admin);
    config
        .administrations
        .get_key_value(name)
        .map(|(name, entry)| (name.as_str(), entry))
        .ok_or_else(|| {
            YukiError::Config(format!(
                "unknown administration profile: {name}. Run 'yuki profile list'."
            ))
        })
}

fn credential_state<'a>(config: &'a Config, entry: &'a AdminEntry) -> (&'static str, &'a str) {
    match entry.api_key.as_deref() {
        Some(key) => ("profile", key),
        None => ("shared-config", &config.api_key),
    }
}

pub async fn auth_status(
    config: &Config,
    profile: Option<&str>,
    offline: bool,
    format: Option<&str>,
    quiet: bool,
) -> Result<(), YukiError> {
    let (name, entry) = selected(config, profile)?;
    let (credential_source, api_key) = credential_state(config, entry);
    if api_key.is_empty() {
        return Err(YukiError::Config(format!(
            "no API key for administration profile {name}; run 'yuki auth login'"
        )));
    }
    if !offline {
        super::setup_domain(config, Some(name)).await?;
    }

    print_result(
        format,
        quiet,
        &json!({
            "profile": name,
            "status": if offline { "configured" } else { "ok" },
            "configured": true,
            "verified": !offline,
            "credential_source": credential_source,
        }),
        &format!(
            "Profile '{name}' is {} ({credential_source}).",
            if offline {
                "configured; network not checked"
            } else {
                "authenticated"
            }
        ),
    );
    Ok(())
}

pub fn auth_logout(
    config: &mut Config,
    profile: Option<&str>,
    format: Option<&str>,
    quiet: bool,
) -> Result<(), YukiError> {
    let name = selected(config, profile)?.0.to_owned();
    let removed = config
        .administrations
        .get(&name)
        .and_then(|entry| entry.api_key.as_deref())
        .map_or_else(|| !config.api_key.is_empty(), |key| !key.is_empty());
    let entry = config
        .administrations
        .get_mut(&name)
        .expect("selected profile exists");

    // An explicit empty profile key prevents fallback to the shared key. Removing
    // the shared key itself could unexpectedly log out unrelated administrations.
    entry.api_key = Some(String::new());
    config.save_to(&Config::default_path())?;

    print_result(
        format,
        quiet,
        &json!({
            "profile": name,
            "logged_out": true,
            "credential_removed": removed,
            "environment_override": false,
        }),
        &format!("Logged out profile '{name}'."),
    );
    Ok(())
}

pub fn profile_list(config: &Config, format: Option<&str>, quiet: bool) {
    let items = config
        .administrations
        .iter()
        .map(|(name, entry)| {
            let (credential_source, api_key) = credential_state(config, entry);
            json!({
                "name": name,
                "display_name": entry.name,
                "active": *name == config.default_admin,
                "admin_id": entry.admin_id,
                "domain_id": entry.domain_id,
                "configured": !api_key.is_empty(),
                "credential_source": credential_source,
            })
        })
        .collect::<Vec<_>>();

    if json_output(format) {
        print_result(
            format,
            quiet,
            &json!({"items": items, "total": items.len()}),
            "",
        );
    } else if !quiet {
        if items.is_empty() {
            println!("No profiles configured. Run `yuki auth login`.");
        } else {
            for item in items {
                println!(
                    "{} {:<24} {}",
                    if item["active"].as_bool().unwrap_or(false) {
                        "*"
                    } else {
                        " "
                    },
                    item["name"].as_str().unwrap_or_default(),
                    if item["configured"].as_bool().unwrap_or(false) {
                        "configured"
                    } else {
                        "logged out"
                    }
                );
            }
        }
    }
}

pub fn profile_use(
    config: &mut Config,
    name: &str,
    format: Option<&str>,
    quiet: bool,
) -> Result<(), YukiError> {
    selected(config, Some(name))?;
    config.default_admin = name.to_owned();
    config.save_to(&Config::default_path())?;
    print_result(
        format,
        quiet,
        &json!({"profile": name, "active": true}),
        &format!("Active profile set to '{name}'."),
    );
    Ok(())
}

pub fn profile_remove(
    config: &mut Config,
    name: &str,
    format: Option<&str>,
    quiet: bool,
) -> Result<(), YukiError> {
    if config.administrations.remove(name).is_none() {
        return Err(YukiError::Config(format!(
            "unknown administration profile: {name}. Run 'yuki profile list'."
        )));
    }
    if config.default_admin == name {
        config.default_admin = config
            .administrations
            .keys()
            .next()
            .cloned()
            .unwrap_or_default();
    }
    config.save_to(&Config::default_path())?;
    print_result(
        format,
        quiet,
        &json!({"profile": name, "removed": true}),
        &format!("Profile '{name}' removed."),
    );
    Ok(())
}

pub fn config_show(config: &Config, format: Option<&str>, quiet: bool) {
    let profiles = config
        .administrations
        .iter()
        .map(|(name, entry)| {
            let (credential_source, api_key) = credential_state(config, entry);
            (
                name.clone(),
                json!({
                    "display_name": entry.name,
                    "admin_id": entry.admin_id,
                    "domain_id": entry.domain_id,
                    "configured": !api_key.is_empty(),
                    "credential_source": credential_source,
                    "api_root": config.api_root(Some(entry)),
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let path = Config::default_path();
    let value = json!({
        "config_file": path,
        "file_exists": path.exists(),
        "active_profile": config.default_admin,
        "profiles": profiles,
        "shared_api_key_configured": !config.api_key.is_empty(),
    });
    let human = format!(
        "Config file: {}\nActive profile: {}\nProfiles: {}",
        path.display(),
        config.default_admin,
        config.administrations.len()
    );
    print_result(format, quiet, &value, &human);
}

pub fn config_path(format: Option<&str>, quiet: bool) {
    let path = Config::default_path();
    print_result(
        format,
        quiet,
        &json!({"config_path": path}),
        &path.display().to_string(),
    );
}

pub async fn doctor(
    config: &Config,
    profile: Option<&str>,
    offline: bool,
    format: Option<&str>,
    quiet: bool,
) -> Result<(), YukiError> {
    let (name, entry) = selected(config, profile)?;
    let (credential_source, api_key) = credential_state(config, entry);
    if api_key.is_empty() {
        return Err(YukiError::Config(format!(
            "no API key for administration profile {name}; run 'yuki auth login'"
        )));
    }
    let api_root = config.api_root(Some(entry));
    if !offline {
        let mut client = AccountingClient::new().with_api_root(api_root);
        client.authenticate(api_key).await?;
        client.set_current_domain(&entry.domain_id).await?;
    }
    let checks = json!([
        {"name": "configuration", "ok": true, "detail": Config::default_path()},
        {"name": "profile", "ok": true, "detail": name},
        {"name": "credentials", "ok": true, "detail": credential_source},
        {"name": "endpoint", "ok": true, "detail": api_root},
        {"name": "authentication", "ok": true, "detail": if offline { "network check skipped" } else { "Yuki session and administration verified" }},
    ]);
    let human = format!(
        "Yuki connection{}\n  ✓ configuration\n  ✓ profile         {name}\n  ✓ credentials     {credential_source}\n  ✓ endpoint        {api_root}\n  ✓ authentication  {}",
        if offline { " (offline)" } else { "" },
        if offline {
            "network check skipped"
        } else {
            "verified"
        }
    );
    print_result(
        format,
        quiet,
        &json!({"ok": true, "offline": offline, "checks": checks}),
        &human,
    );
    Ok(())
}
