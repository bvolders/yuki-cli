//! `yuki init` finds the region an access key belongs to by trying it on every
//! known Yuki host. One local mock stands in for all of them: each region lives
//! under its own path (`/{code}/ws`), and a key is valid on a region when one of
//! its `-`-separated parts is that region's code (`key-be`, `key-nl-be`).
//! Nothing leaves the machine.

use std::io::{Cursor, Write};
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};

mod common;

use common::{RequestLog, fault, response, stderr, stdout_json, yuki, yuki_with_env};

use tempfile::TempDir;
use yuki_cli::cli::init::{Deployment, InitIo, detect, run_with};
use yuki_cli::client::Region;
use yuki_cli::error::YukiError;

struct Mock {
    base: String,
    log: RequestLog,
}

impl Mock {
    fn root(&self, code: &str) -> String {
        format!("{}/{code}/ws", self.base)
    }

    /// `YUKI_PROBE_ROOTS` pointing every region at this mock.
    fn probe_roots(&self) -> String {
        Region::ALL
            .iter()
            .map(|r| format!("{r}={}", self.root(r.as_str())))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Requests for `operation`, as the region code (first path segment) each went to.
    fn calls(&self, operation: &str) -> Vec<String> {
        let mut regions: Vec<String> = self
            .log
            .lock()
            .expect("log")
            .iter()
            .filter(|r| r.action == operation)
            .map(|r| region_of(&r.path).to_string())
            .collect();
        regions.sort();
        regions
    }
}

/// The region code of a request path, `/{code}/ws/...`.
fn region_of(path: &str) -> &str {
    path.split('/').nth(1).unwrap_or_default()
}

fn mock_yuki() -> Mock {
    let (root, log) = common::mock(|r| {
        let code = region_of(&r.path);
        match r.action.as_str() {
            "Authenticate" if r.param("accessKey").split('-').any(|part| part == code) => {
                (200, response("Authenticate", &format!("session-{code}")))
            }
            "Authenticate" => (500, fault("Invalid access key")),
            _ => {
                let upper = code.to_ascii_uppercase();
                let admins = format!(
                    r#"<Administrations xmlns=""><Administration ID="admin-{code}"><Name>Voorbeeld {upper} BV</Name><DomainID>domain-{code}</DomainID></Administration></Administrations>"#
                );
                (200, response("Administrations", &admins))
            }
        }
    });
    // The mock serves every region under its own path below the host.
    let base = root.trim_end_matches("/ws").to_string();
    Mock { base, log }
}

/// A root nothing listens on, so connecting to it fails.
fn unreachable_root() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let root = format!("http://{}/ws", listener.local_addr().expect("addr"));
    drop(listener);
    root
}

fn config_path(home: &TempDir) -> std::path::PathBuf {
    home.path().join(".config/yuki/config.toml")
}

fn saved_config(home: &TempDir) -> toml::Value {
    toml::from_str(&std::fs::read_to_string(config_path(home)).expect("config"))
        .expect("valid TOML")
}

fn init(home: &TempDir, mock: &Mock, args: &[&str]) -> Output {
    let roots = mock.probe_roots();
    let mut all = vec!["init"];
    all.extend_from_slice(args);
    yuki_with_env(home, &all, &[("YUKI_PROBE_ROOTS", &roots)])
}

#[test]
fn a_belgian_key_is_detected_stored_and_its_probe_session_reused() {
    let home = TempDir::new().expect("temp home");
    let mock = mock_yuki();

    let out = init(&home, &mock, &["--api-key", "key-be"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("Detected Yuki Belgium (api.yukiworks.be)"),
        "{}",
        stderr(&out)
    );

    // Every region is probed once; discovery runs on the Belgian session
    // without authenticating again.
    assert_eq!(mock.calls("Authenticate"), ["be", "nl"]);
    assert_eq!(mock.calls("Administrations"), ["be"]);

    let saved = saved_config(&home);
    assert_eq!(saved["region"].as_str(), Some("be"));
    assert_eq!(saved["default_admin"].as_str(), Some("voorbeeld_be_bv"));
    assert!(saved.get("base_url").is_none(), "{saved}");
}

#[test]
fn a_dutch_key_is_detected_and_nl_is_stored_explicitly() {
    let home = TempDir::new().expect("temp home");
    let mock = mock_yuki();

    let out = init(&home, &mock, &["--api-key", "key-nl"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("Detected Yuki Netherlands (api.yukiworks.nl)"),
        "{}",
        stderr(&out)
    );
    assert_eq!(saved_config(&home)["region"].as_str(), Some("nl"));
}

#[test]
fn a_key_no_region_accepts_is_the_usual_auth_error() {
    let home = TempDir::new().expect("temp home");
    let mock = mock_yuki();

    let out = init(&home, &mock, &["--api-key", "key-none"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("auth_failed"), "{err}");
    assert!(err.contains("--base-url"), "{err}");
    assert_eq!(mock.calls("Administrations"), Vec::<String>::new());
    assert!(!config_path(&home).exists());
}

#[test]
fn a_key_valid_on_several_regions_needs_region_when_stdin_is_not_a_terminal() {
    let home = TempDir::new().expect("temp home");
    let mock = mock_yuki();

    let out = init(&home, &mock, &["--api-key", "key-nl-be"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("--region"), "{err}");
    assert_eq!(mock.calls("Administrations"), Vec::<String>::new());
    assert!(!config_path(&home).exists());
}

#[test]
fn an_unreachable_host_is_reported_rather_than_skipped() {
    let home = TempDir::new().expect("temp home");
    let mock = mock_yuki();
    let dead = unreachable_root();
    let roots = format!("nl={dead},be={}", mock.root("be"));

    let out = yuki_with_env(
        &home,
        &["init", "--api-key", "key-be"],
        &[("YUKI_PROBE_ROOTS", &roots)],
    );
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains(&dead), "{err}");
    assert!(err.contains("--region"), "{err}");
    assert!(!config_path(&home).exists());
}

#[test]
fn an_explicit_endpoint_skips_detection() {
    let mock = mock_yuki();
    let roots = mock.probe_roots();
    let be = mock.root("be");

    for env in [
        vec![("YUKI_BASE_URL", be.as_str())],
        vec![("YUKI_REGION", "be"), ("YUKI_BASE_URL", be.as_str())],
    ] {
        let home = TempDir::new().expect("temp home");
        let mut env = env;
        env.push(("YUKI_PROBE_ROOTS", &roots));
        let out = yuki_with_env(
            &home,
            &["init", "--api-key", "key-be", "--region", "be"],
            &env,
        );
        assert!(out.status.success(), "{}", stderr(&out));
        assert!(!stderr(&out).contains("Detected"), "{}", stderr(&out));
    }
    // Only the Belgian host was ever contacted: nothing probed nl.
    assert_eq!(mock.calls("Authenticate"), ["be", "be"]);
}

#[test]
fn init_add_detects_each_key_and_stamps_its_administrations() {
    let home = TempDir::new().expect("temp home");
    let mock = mock_yuki();

    let first = init(&home, &mock, &["--api-key", "key-nl"]);
    assert!(first.status.success(), "{}", stderr(&first));
    let add = init(&home, &mock, &["--add", "--api-key", "key-be"]);
    assert!(add.status.success(), "{}", stderr(&add));
    assert!(
        stderr(&add).contains("Detected Yuki Belgium"),
        "{}",
        stderr(&add)
    );

    let saved = saved_config(&home);
    assert_eq!(saved["region"].as_str(), Some("nl"));
    let be = &saved["administrations"]["voorbeeld_be_bv"];
    assert_eq!(be["region"].as_str(), Some("be"));
    assert_eq!(be["api_key"].as_str(), Some("key-be"));

    let shown = stdout_json(yuki(&home, &["config", "show", "--output", "json"]));
    assert_eq!(
        shown["profiles"]["voorbeeld_be_bv"]["api_root"],
        "https://api.yukiworks.be/ws"
    );
    assert_eq!(
        shown["profiles"]["voorbeeld_nl_bv"]["api_root"],
        "https://api.yukiworks.nl/ws"
    );
}

#[test]
fn a_key_piped_on_stdin_is_detected_without_reading_further() {
    let home = TempDir::new().expect("temp home");
    let mock = mock_yuki();

    let mut child = Command::new(env!("CARGO_BIN_EXE_yuki"))
        .arg("init")
        .env("HOME", home.path())
        .env_remove("YUKI_REGION")
        .env_remove("YUKI_BASE_URL")
        .env("YUKI_PROBE_ROOTS", mock.probe_roots())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn yuki");
    // Like `pbpaste | yuki init`: the key, and then end of input.
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"key-be\n")
        .expect("write key");
    let out = child.wait_with_output().expect("yuki init");

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("Detected Yuki Belgium"),
        "{}",
        stderr(&out)
    );
    assert_eq!(saved_config(&home)["region"].as_str(), Some("be"));
}

// The interactive paths run in-process: a test cannot give the binary a terminal.

fn io(home: &TempDir, mock: &Mock, answers: &str, interactive: bool) -> InitIo<Cursor<Vec<u8>>> {
    InitIo {
        path: config_path(home),
        input: Cursor::new(answers.as_bytes().to_vec()),
        interactive,
        probes: Region::ALL
            .iter()
            .map(|r| (*r, mock.root(r.as_str())))
            .collect(),
    }
}

#[tokio::test]
async fn detection_is_driven_by_the_region_list_not_a_fixed_pair() {
    let mock = mock_yuki();
    let candidates: Vec<(&str, String)> = ["nl", "be", "xx"]
        .into_iter()
        .map(|code| (code, mock.root(code)))
        .collect();

    let found = detect("key-xx", &candidates, &mut Cursor::new(Vec::new()), false)
        .await
        .expect("detected");
    assert_eq!(found.deployment, Deployment::Known("xx"));
    assert_eq!(mock.calls("Authenticate"), ["be", "nl", "xx"]);

    let err = detect(
        "key-be-xx",
        &candidates,
        &mut Cursor::new(Vec::new()),
        false,
    )
    .await
    .err()
    .expect("ambiguous");
    assert!(matches!(err, YukiError::Config(_)), "{err}");
    assert!(err.to_string().contains("be, xx"), "{err}");
}

#[tokio::test]
async fn an_ambiguous_key_asks_which_region_without_a_default() {
    let mock = mock_yuki();
    let candidates: Vec<(Region, String)> = Region::ALL
        .iter()
        .map(|r| (*r, mock.root(r.as_str())))
        .collect();

    // An empty answer and an unknown one are asked again, not defaulted.
    let mut answers = Cursor::new(b"\nde\nbe\n".to_vec());
    let found = detect("key-nl-be", &candidates, &mut answers, true)
        .await
        .expect("chosen");
    assert_eq!(found.deployment, Deployment::Known(Region::Be));

    // End of input is no choice at all.
    let err = detect("key-nl-be", &candidates, &mut Cursor::new(Vec::new()), true)
        .await
        .err()
        .expect("no answer");
    assert!(matches!(err, YukiError::Config(_)), "{err}");
}

#[tokio::test]
async fn an_unmatched_key_can_be_pointed_at_another_deployment() {
    let mock = mock_yuki();
    let candidates: Vec<(Region, String)> = Region::ALL
        .iter()
        .map(|r| (*r, mock.root(r.as_str())))
        .collect();

    // A malformed URL is asked again; the valid one is verified with one call.
    let answers = format!("other\nnot a url\n{}/\n", mock.root("custom"));
    let found = detect(
        "key-custom",
        &candidates,
        &mut Cursor::new(answers.into_bytes()),
        true,
    )
    .await
    .expect("other");
    assert_eq!(found.deployment, Deployment::BaseUrl(mock.root("custom")));
    assert_eq!(mock.calls("Authenticate"), ["be", "custom", "nl"]);

    // Picking a region that already rejected the key is the auth error.
    let err = detect(
        "key-none",
        &candidates,
        &mut Cursor::new(b"nl\n".to_vec()),
        true,
    )
    .await
    .err()
    .expect("rejected");
    assert_eq!(err.exit_code(), 2, "{err}");
    let err = detect("key-none", &candidates, &mut Cursor::new(Vec::new()), true)
        .await
        .err()
        .expect("no answer");
    assert_eq!(err.exit_code(), 2, "{err}");
}

#[tokio::test]
async fn another_deployment_is_stored_as_a_base_url() {
    let home = TempDir::new().expect("temp home");
    let mock = mock_yuki();
    let custom = mock.root("custom");

    let answers = format!("other\n{custom}\n");
    run_with(
        io(&home, &mock, &answers, true),
        Some("key-custom"),
        None,
        false,
        None,
        None,
        None,
    )
    .await
    .expect("init");
    let saved = saved_config(&home);
    assert_eq!(saved["base_url"].as_str(), Some(custom.as_str()), "{saved}");
    assert!(saved.get("region").is_none(), "{saved}");

    // --add keeps it per administration, so the shared key's books stay put.
    let home = TempDir::new().expect("temp home");
    run_with(
        io(&home, &mock, "", false),
        Some("key-be"),
        None,
        false,
        None,
        None,
        None,
    )
    .await
    .expect("init");
    run_with(
        io(&home, &mock, &answers, true),
        Some("key-custom"),
        None,
        true,
        None,
        None,
        None,
    )
    .await
    .expect("init --add");
    let saved = saved_config(&home);
    assert_eq!(saved["region"].as_str(), Some("be"));
    assert!(saved.get("base_url").is_none(), "{saved}");
    let entry = &saved["administrations"]["voorbeeld_custom_bv"];
    assert_eq!(entry["base_url"].as_str(), Some(custom.as_str()), "{saved}");
    assert!(entry.get("region").is_none(), "{saved}");
}
