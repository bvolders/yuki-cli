use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn config_path(home: &TempDir) -> std::path::PathBuf {
    home.path().join(".config/yuki/config.toml")
}

fn write_config(home: &TempDir) {
    let path = config_path(home);
    std::fs::create_dir_all(path.parent().expect("config parent")).expect("config directory");
    std::fs::write(
        path,
        r#"api_key = "shared-key"
default_admin = "company_a"
unmatched_ignore = []

[administrations.company_a]
domain_id = "domain-a"
admin_id = "admin-a"
name = "Company A"

[administrations.holding]
domain_id = "domain-b"
admin_id = "admin-b"
name = "Holding"
api_key = "holding-key"
"#,
    )
    .expect("config file");
}

fn yuki(home: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_yuki"))
        .args(args)
        .env("HOME", home.path())
        .env_remove("YUKI_REGION")
        .env_remove("YUKI_BASE_URL")
        .output()
        .expect("yuki command")
}

fn stdout_json(output: Output) -> Value {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON stdout")
}

#[test]
fn canonical_local_account_workflow_preserves_unrelated_credentials() {
    let home = TempDir::new().expect("temp home");
    write_config(&home);

    let profiles = stdout_json(yuki(&home, &["profile", "list", "--output", "json"]));
    assert_eq!(profiles["total"], 2);
    assert_eq!(profiles["items"][0]["name"], "company_a");
    assert_eq!(profiles["items"][0]["active"], true);
    assert_eq!(profiles["items"][1]["credential_source"], "profile");

    let selected = stdout_json(yuki(
        &home,
        &["profile", "use", "holding", "--output", "json"],
    ));
    assert_eq!(selected["profile"], "holding");
    assert_eq!(selected["active"], true);

    let status = stdout_json(yuki(
        &home,
        &["auth", "status", "--offline", "--output", "json"],
    ));
    assert_eq!(status["profile"], "holding");
    assert_eq!(status["status"], "configured");
    assert_eq!(status["verified"], false);

    let doctor = stdout_json(yuki(&home, &["doctor", "--offline", "--output", "json"]));
    assert_eq!(doctor["ok"], true);
    assert_eq!(doctor["offline"], true);
    assert_eq!(doctor["checks"].as_array().map(Vec::len), Some(5));

    let shown = stdout_json(yuki(&home, &["config", "show", "--output", "json"]));
    assert_eq!(shown["active_profile"], "holding");
    assert_eq!(shown["shared_api_key_configured"], true);
    assert!(!shown.to_string().contains("shared-key"));
    assert!(!shown.to_string().contains("holding-key"));

    let path = stdout_json(yuki(&home, &["config", "path", "--output", "json"]));
    assert_eq!(
        path["config_path"].as_str(),
        Some(config_path(&home).to_string_lossy().as_ref())
    );

    let logout = stdout_json(yuki(&home, &["auth", "logout", "--output", "json"]));
    assert_eq!(logout["profile"], "holding");
    assert_eq!(logout["credential_removed"], true);
    let repeated_logout = stdout_json(yuki(&home, &["auth", "logout", "--output", "json"]));
    assert_eq!(repeated_logout["credential_removed"], false);

    let saved: toml::Value =
        toml::from_str(&std::fs::read_to_string(config_path(&home)).expect("updated config"))
            .expect("valid TOML");
    assert_eq!(saved["api_key"].as_str(), Some("shared-key"));
    assert_eq!(
        saved["administrations"]["holding"]["api_key"].as_str(),
        Some("")
    );
    assert!(
        saved["administrations"]["company_a"]
            .get("api_key")
            .is_none()
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(config_path(&home))
            .expect("config metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn profile_remove_requires_confirmation_and_repairs_the_default() {
    let home = TempDir::new().expect("temp home");
    write_config(&home);
    stdout_json(yuki(
        &home,
        &["profile", "use", "holding", "--output", "json"],
    ));

    let denied = yuki(&home, &["profile", "remove", "holding", "--output", "json"]);
    assert_eq!(denied.status.code(), Some(1));
    let error: Value = serde_json::from_slice(
        denied
            .stderr
            .split(|byte| *byte == b'\n')
            .rfind(|line| !line.is_empty())
            .expect("error line"),
    )
    .expect("structured error");
    assert_eq!(error["error"]["kind"], "confirmation_required");

    let removed = stdout_json(yuki(
        &home,
        &["profile", "remove", "holding", "--yes", "--output", "json"],
    ));
    assert_eq!(removed["profile"], "holding");
    assert_eq!(removed["removed"], true);

    let shown = stdout_json(yuki(&home, &["config", "show", "--output", "json"]));
    assert_eq!(shown["active_profile"], "company_a");
    assert!(shown["profiles"].get("holding").is_none());
}

#[test]
fn profile_flag_and_service_native_admin_flag_are_compatible() {
    let home = TempDir::new().expect("temp home");
    write_config(&home);

    for flag in ["--profile", "--admin"] {
        let status = stdout_json(yuki(
            &home,
            &[
                "auth",
                "status",
                "--offline",
                flag,
                "holding",
                "--output",
                "json",
            ],
        ));
        assert_eq!(status["profile"], "holding");
    }
}

#[test]
fn schema_advertises_the_standard_account_contract() {
    let home = TempDir::new().expect("temp home");
    let schema = stdout_json(yuki(&home, &["schema"]));
    let names = schema["commands"]
        .as_array()
        .expect("commands")
        .iter()
        .filter_map(|command| command["name"].as_str())
        .collect::<Vec<_>>();
    for required in [
        "auth login",
        "auth status",
        "auth logout",
        "profile list",
        "profile use",
        "profile remove",
        "config show",
        "config path",
        "doctor",
    ] {
        assert!(names.contains(&required), "schema missing {required}");
    }
}
