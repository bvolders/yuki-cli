use yuki_cli::client::Region;
use yuki_cli::config::{AdminEntry, Config};

/// Build a config from a shared key, a default administration, and entries.
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

#[test]
fn loads_valid_config() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
api_key = "00000000-0000-0000-0000-000000000000"
default_admin = "company_a"

[administrations.company_a]
domain_id = "uuid-1"
admin_id = "admin-1"

[administrations.company_b]
domain_id = "uuid-2"
admin_id = "admin-2"
"#,
    )
    .unwrap();

    let config = Config::load_from(&path).unwrap();
    assert_eq!(config.api_key, "00000000-0000-0000-0000-000000000000");
    assert_eq!(config.default_admin, "company_a");
    assert_eq!(config.administrations.len(), 2);
    assert_eq!(config.administrations["company_a"].domain_id, "uuid-1");
    assert_eq!(config.administrations["company_a"].admin_id, "admin-1");
}

#[test]
fn returns_error_for_missing_file() {
    let result = Config::load_from(std::path::Path::new("/nonexistent/config.toml"));
    assert!(result.is_err());
}

#[test]
fn returns_error_for_invalid_toml() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "not valid toml {{{{").unwrap();

    let result = Config::load_from(&path);
    assert!(result.is_err());
}

#[test]
fn saves_config_roundtrip() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("config.toml");

    let config = config_with(
        "test-key",
        "my_company",
        [("my_company", AdminEntry::new("uuid-123", "admin-123"))],
    );

    config.save_to(&path).unwrap();
    let loaded = Config::load_from(&path).unwrap();
    assert_eq!(loaded.api_key, "test-key");
    assert_eq!(loaded.default_admin, "my_company");
    assert_eq!(loaded.administrations["my_company"].domain_id, "uuid-123");
    assert_eq!(loaded.administrations["my_company"].admin_id, "admin-123");
}

#[test]
fn saves_per_administration_key_and_name_roundtrip() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("config.toml");

    let config = config_with(
        "shared-key",
        "co_a",
        [
            (
                "co_a",
                AdminEntry::new("uuid-a", "admin-a").with_name("Example Trading B.V."),
            ),
            (
                "co_b",
                AdminEntry::new("uuid-b", "admin-b")
                    .with_name("Example Holding B.V.")
                    .with_api_key("holding-key"),
            ),
        ],
    );

    config.save_to(&path).unwrap();
    let loaded = Config::load_from(&path).unwrap();

    let a = &loaded.administrations["co_a"];
    assert_eq!(a.name.as_deref(), Some("Example Trading B.V."));
    assert_eq!(a.api_key, None, "an entry on the shared key stores no key");

    let b = &loaded.administrations["co_b"];
    assert_eq!(b.name.as_deref(), Some("Example Holding B.V."));
    assert_eq!(b.api_key.as_deref(), Some("holding-key"));
}

#[test]
fn omits_absent_optional_fields_from_the_written_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("config.toml");

    config_with(
        "shared-key",
        "co",
        [("co", AdminEntry::new("uuid-1", "admin-1"))],
    )
    .save_to(&path)
    .unwrap();

    // An absent per-administration key must not be written as an empty string:
    // reloading one would resolve to a key that authenticates against nothing.
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        !written.contains("name ="),
        "wrote an absent name: {written}"
    );
    assert_eq!(
        written.matches("api_key").count(),
        1,
        "only the shared key is written: {written}"
    );
    assert!(
        !written.contains("\"\""),
        "wrote an empty string where a value is absent: {written}"
    );
}

#[test]
fn config_path_returns_xdg_path() {
    let path = Config::default_path();
    let path_str = path.to_string_lossy();
    assert!(path_str.ends_with(".config/yuki/config.toml") || path_str.contains("yuki"));
}

#[test]
fn target_returns_the_default_administration() {
    let config = config_with(
        "key",
        "co_a",
        [("co_a", AdminEntry::new("uuid-a", "admin-a"))],
    );
    let target = config.target(None).unwrap();
    assert_eq!(target.config_name, "co_a");
    assert_eq!(target.domain_id, "uuid-a");
    assert_eq!(target.admin_id, "admin-a");
    assert_eq!(target.api_key, "key");
}

#[test]
fn target_override_takes_precedence() {
    let config = config_with(
        "key",
        "co_a",
        [
            ("co_a", AdminEntry::new("uuid-a", "admin-a")),
            ("co_b", AdminEntry::new("uuid-b", "admin-b")),
        ],
    );
    let target = config.target(Some("co_b")).unwrap();
    assert_eq!(target.config_name, "co_b");
    assert_eq!(target.domain_id, "uuid-b");
    assert_eq!(target.admin_id, "admin-b");
}

#[test]
fn target_errors_on_unknown_name() {
    let config = config_with(
        "key",
        "co_a",
        [("co_a", AdminEntry::new("uuid-a", "admin-a"))],
    );
    let err = config.target(Some("unknown")).unwrap_err().to_string();
    assert!(err.contains("unknown"), "{err}");
    assert!(
        err.contains("co_a"),
        "the error names what is configured: {err}"
    );
}

#[test]
fn target_error_distinguishes_an_empty_configuration() {
    let config = config_with("key", "co_a", []);
    let err = config.target(None).unwrap_err().to_string();
    assert!(err.contains("yuki init"), "{err}");
}

#[test]
fn target_prefers_the_per_administration_key() {
    let config = config_with(
        "shared-key",
        "co_a",
        [
            ("co_a", AdminEntry::new("uuid-a", "admin-a")),
            (
                "co_b",
                AdminEntry::new("uuid-b", "admin-b").with_api_key("holding-key"),
            ),
        ],
    );
    // The key travels with the identifiers: co_b's admin_id is only meaningful to a
    // session opened with co_b's own key.
    let b = config.target(Some("co_b")).unwrap();
    assert_eq!(b.api_key, "holding-key");
    assert_eq!(b.admin_id, "admin-b");

    let a = config.target(Some("co_a")).unwrap();
    assert_eq!(a.api_key, "shared-key");
}

#[test]
fn target_errors_when_no_key_reaches_the_administration() {
    // An administration configured while the shared key was empty and never given one
    // of its own must report that, not authenticate with an empty string.
    let config = config_with("", "co_a", [("co_a", AdminEntry::new("uuid-a", "admin-a"))]);
    let err = config.target(None).unwrap_err().to_string();
    assert!(err.contains("no API key"), "{err}");
    assert!(err.contains("co_a"), "{err}");
}

#[test]
fn access_keys_groups_administrations_under_their_key() {
    let config = config_with(
        "shared-key",
        "co_a",
        [
            ("co_a", AdminEntry::new("uuid-a", "admin-a")),
            (
                "co_b",
                AdminEntry::new("uuid-b", "admin-b").with_api_key("holding-key"),
            ),
            (
                "co_c",
                AdminEntry::new("uuid-c", "admin-c").with_api_key("holding-key"),
            ),
        ],
    );

    let keys = config.access_keys();
    assert_eq!(keys.len(), 2, "one entry per distinct key: {keys:?}");
    assert_eq!(keys[0].api_key, "shared-key", "the shared key comes first");
    assert_eq!(keys[0].admins, vec!["co_a"]);
    assert_eq!(keys[1].api_key, "holding-key");
    assert_eq!(
        keys[1].admins,
        vec!["co_b", "co_c"],
        "administrations sharing a key are grouped, so it authenticates once"
    );
}

#[test]
fn access_keys_dedupes_a_key_spelled_out_explicitly() {
    let config = config_with(
        "shared-key",
        "co_a",
        [(
            "co_a",
            AdminEntry::new("uuid-a", "admin-a").with_api_key("shared-key"),
        )],
    );
    let keys = config.access_keys();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].admins, vec!["co_a"]);
}

#[test]
fn access_keys_skips_administrations_no_key_reaches() {
    let config = config_with(
        "",
        "co_a",
        [
            ("co_a", AdminEntry::new("uuid-a", "admin-a")),
            (
                "co_b",
                AdminEntry::new("uuid-b", "admin-b").with_api_key("holding-key"),
            ),
        ],
    );
    let keys = config.access_keys();
    assert_eq!(keys.len(), 1, "an empty shared key is not an access key");
    assert_eq!(keys[0].api_key, "holding-key");
    assert_eq!(keys[0].admins, vec!["co_b"]);
}

#[test]
fn merge_administrations_suffixes_a_name_another_administration_holds() {
    // Two Yuki tenants may use the same company name. Overwriting would leave the
    // first set of books configured under a name that now points at the second.
    let mut config = config_with(
        "shared-key",
        "example_b_v_",
        [(
            "example_b_v_",
            AdminEntry::new("uuid-a", "admin-a").with_name("Example B.V."),
        )],
    );

    let (added, updated) = config.merge_administrations(
        [(
            "example_b_v_",
            AdminEntry::new("uuid-b", "admin-b").with_name("Example B.V."),
        )],
        "second-key",
    );

    assert_eq!(added, vec!["example_b_v__2"]);
    assert!(updated.is_empty());
    assert_eq!(config.administrations.len(), 2, "both remain reachable");
    assert_eq!(config.administrations["example_b_v_"].admin_id, "admin-a");
    assert_eq!(config.administrations["example_b_v__2"].admin_id, "admin-b");
    assert_eq!(
        config.administrations["example_b_v_"].api_key, None,
        "the first administration keeps the key that reaches it"
    );
    assert_eq!(
        config.administrations["example_b_v__2"].api_key.as_deref(),
        Some("second-key")
    );
}

#[test]
fn merge_administrations_updates_in_place_when_the_name_is_the_same_administration() {
    // The suffix is for collisions only: re-running against the same administration
    // must refresh it, not accumulate example_b_v__2, _3, _4 on every run.
    let mut config = config_with(
        "shared-key",
        "co_a",
        [("co_a", AdminEntry::new("stale-uuid", "admin-a"))],
    );

    for _ in 0..3 {
        config.merge_administrations(
            [("co_a", AdminEntry::new("uuid-a", "admin-a"))],
            "shared-key",
        );
    }

    assert_eq!(config.administrations.len(), 1);
    assert_eq!(config.administrations["co_a"].domain_id, "uuid-a");
}

#[test]
fn merge_administrations_preserves_existing_entries() {
    // The defect this guards: `yuki init` replaced the whole map, so re-running it
    // with a second key dropped every administration the first key had reached.
    let mut config = config_with(
        "shared-key",
        "co_a",
        [("co_a", AdminEntry::new("uuid-a", "admin-a"))],
    );

    let (added, updated) = config.merge_administrations(
        [(
            "co_b",
            AdminEntry::new("uuid-b", "admin-b").with_name("Example Holding B.V."),
        )],
        "holding-key",
    );

    assert_eq!(added, vec!["co_b"]);
    assert!(updated.is_empty());
    assert_eq!(config.administrations.len(), 2);
    assert_eq!(config.administrations["co_a"].admin_id, "admin-a");
    assert_eq!(
        config.administrations["co_b"].api_key.as_deref(),
        Some("holding-key"),
        "an entry reached by another key records it"
    );
}

#[test]
fn merge_administrations_leaves_the_shared_key_implicit() {
    let mut config = config_with("shared-key", "co_a", []);
    let (added, _) = config.merge_administrations(
        [("co_a", AdminEntry::new("uuid-a", "admin-a"))],
        "shared-key",
    );

    assert_eq!(added, vec!["co_a"]);
    assert_eq!(
        config.administrations["co_a"].api_key, None,
        "rotating the shared key must keep reaching this administration"
    );
}

#[test]
fn merge_administrations_reports_updates_separately() {
    // Same administration, refreshed metadata: an update, not an addition. The
    // recorded domain_id was stale and the display name had never been recorded.
    let mut config = config_with(
        "shared-key",
        "co_a",
        [("co_a", AdminEntry::new("stale-uuid", "admin-a"))],
    );

    let (added, updated) = config.merge_administrations(
        [(
            "co_a",
            AdminEntry::new("uuid-a", "admin-a").with_name("Example Trading B.V."),
        )],
        "shared-key",
    );

    assert!(added.is_empty());
    assert_eq!(updated, vec!["co_a"]);
    assert_eq!(config.administrations["co_a"].domain_id, "uuid-a");
    assert_eq!(
        config.administrations["co_a"].name.as_deref(),
        Some("Example Trading B.V.")
    );
}

#[test]
fn loads_config_with_unmatched_ignore() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
api_key = "test-key"
default_admin = "co"
unmatched_ignore = ["Belastingdienst", "ING bankkosten"]

[administrations.co]
domain_id = "uuid-1"
admin_id = "admin-1"
"#,
    )
    .unwrap();

    let config = Config::load_from(&path).unwrap();
    assert_eq!(config.unmatched_ignore.len(), 2);
    assert_eq!(config.unmatched_ignore[0], "Belastingdienst");
    assert_eq!(config.unmatched_ignore[1], "ING bankkosten");
}

#[test]
fn loads_config_without_unmatched_ignore() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
api_key = "test-key"
default_admin = "co"

[administrations.co]
domain_id = "uuid-1"
admin_id = "admin-1"
"#,
    )
    .unwrap();

    let config = Config::load_from(&path).unwrap();
    assert!(config.unmatched_ignore.is_empty());
}

#[test]
fn a_config_without_region_targets_the_netherlands() {
    let config = config_with("key", "a", [("a", AdminEntry::new("d", "x"))]);
    let target = config.target(None).unwrap();
    assert_eq!(target.api_root, "https://api.yukiworks.nl/ws");
}

#[test]
fn loads_top_level_and_per_administration_regions() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
api_key = "shared-key"
default_admin = "brussels"
region = "be"

[administrations.brussels]
domain_id = "d1"
admin_id = "a1"

[administrations.amsterdam]
domain_id = "d2"
admin_id = "a2"
api_key = "nl-key"
region = "nl"
"#,
    )
    .unwrap();

    let config = Config::load_from(&path).unwrap();
    assert_eq!(config.region, Some(Region::Be));
    assert_eq!(
        config.target(Some("brussels")).unwrap().api_root,
        "https://api.yukiworks.be/ws"
    );
    assert_eq!(
        config.target(Some("amsterdam")).unwrap().api_root,
        "https://api.yukiworks.nl/ws"
    );

    let keys = config.access_keys();
    assert_eq!(keys[0].api_key, "shared-key");
    assert_eq!(keys[0].api_root, "https://api.yukiworks.be/ws");
    assert_eq!(keys[1].api_key, "nl-key");
    assert_eq!(keys[1].api_root, "https://api.yukiworks.nl/ws");
}

#[test]
fn rejects_an_unknown_region() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "api_key = \"k\"\ndefault_admin = \"a\"\nregion = \"de\"\n[administrations]\n",
    )
    .unwrap();

    let err = Config::load_from(&path).unwrap_err().to_string();
    assert!(err.contains("unknown region 'de'"), "{err}");
}

#[test]
fn base_url_beats_regions_and_the_runtime_override_beats_both() {
    let mut entry = AdminEntry::new("d", "x");
    entry.region = Some(Region::Nl);
    let mut config = config_with("key", "a", [("a", entry)]);
    config.region = Some(Region::Nl);
    config.base_url = Some("http://file.example/ws".into());
    assert_eq!(
        config.target(None).unwrap().api_root,
        "http://file.example/ws"
    );

    config.override_endpoint(Some(Region::Be), None);
    assert_eq!(
        config.target(None).unwrap().api_root,
        "https://api.yukiworks.be/ws"
    );

    config.override_endpoint(Some(Region::Nl), Some("http://127.0.0.1:1/ws"));
    assert_eq!(
        config.target(None).unwrap().api_root,
        "http://127.0.0.1:1/ws"
    );
}

#[test]
fn neither_region_nor_override_is_written_for_a_dutch_config() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    let mut config = config_with("key", "a", [("a", AdminEntry::new("d", "x"))]);
    config.override_endpoint(Some(Region::Be), Some("http://127.0.0.1:1/ws"));
    config.save_to(&path).unwrap();

    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(!saved.contains("region"), "{saved}");
    assert!(!saved.contains("base_url"), "{saved}");
    assert!(!saved.contains("127.0.0.1"), "{saved}");
}

#[test]
fn region_roundtrips_through_save() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    let mut entry = AdminEntry::new("d", "x");
    entry.region = Some(Region::Nl);
    let mut config = config_with("key", "a", [("a", entry)]);
    config.region = Some(Region::Be);
    config.save_to(&path).unwrap();

    let loaded = Config::load_from(&path).unwrap();
    assert_eq!(loaded.region, Some(Region::Be));
    assert_eq!(loaded.administrations["a"].region, Some(Region::Nl));
}
