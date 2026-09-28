//! End-to-end smoke tests: `init --region` stores the region and later commands
//! resolve the Belgian host. A fresh `init` (nothing to overwrite) stores the
//! endpoint an exported YUKI_REGION/YUKI_BASE_URL resolved to, exactly as a typed
//! flag would; re-running `init` on an existing config does not let the
//! environment silently rewrite it. Precedence itself is unit-tested in
//! tests/config.rs and cli::tests. A local mock stands in for Yuki; nothing
//! leaves the machine.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

mod common;

use common::{stdout_json, yuki, yuki_with_env};

use tempfile::TempDir;

const AUTHENTICATE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <AuthenticateResponse xmlns="http://www.theyukicompany.com/">
      <AuthenticateResult>session-1</AuthenticateResult>
    </AuthenticateResponse>
  </soap:Body>
</soap:Envelope>"#;

const ADMINISTRATIONS: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <AdministrationsResponse xmlns="http://www.theyukicompany.com/">
      <AdministrationsResult>
        <Administrations xmlns="">
          <Administration ID="admin-be">
            <Name>Voorbeeld BV</Name>
            <DomainID>domain-be</DomainID>
          </Administration>
        </Administrations>
      </AdministrationsResult>
    </AdministrationsResponse>
  </soap:Body>
</soap:Envelope>"#;

/// `(path, SOAPAction)` of every request the mock received.
type RequestLog = Arc<Mutex<Vec<(String, String)>>>;

/// Serve Authenticate/Administrations on a random port. Returns the API root and
/// the request log.
fn mock_yuki() -> (String, RequestLog) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let root = format!("http://{}/ws", listener.local_addr().expect("addr"));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            let mut request_line = String::new();
            reader.read_line(&mut request_line).ok();
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();
            let (mut action, mut length) = (String::new(), 0usize);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap_or(0);
                } else if lower.starts_with("soapaction:") {
                    action = line["soapaction:".len()..]
                        .trim()
                        .trim_matches('"')
                        .to_string();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).ok();
            let reply = if action.ends_with("Administrations") {
                ADMINISTRATIONS
            } else {
                AUTHENTICATE
            };
            log.lock().expect("log").push((path, action));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .ok();
        }
    });
    (root, seen)
}

fn config_path(home: &TempDir) -> std::path::PathBuf {
    home.path().join(".config/yuki/config.toml")
}

#[test]
fn init_with_region_be_stores_it_and_later_commands_use_the_belgian_host() {
    let home = TempDir::new().expect("temp home");
    let (root, seen) = mock_yuki();

    let init = yuki_with_env(
        &home,
        &["init", "--api-key", "be-key", "--region", "be"],
        &[("YUKI_BASE_URL", &root)],
    );
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let requests = seen.lock().expect("log").clone();
    assert_eq!(
        requests,
        vec![
            (
                "/ws/Accounting.asmx".to_string(),
                "http://www.theyukicompany.com/Authenticate".to_string()
            ),
            (
                "/ws/Accounting.asmx".to_string(),
                "http://www.theyukicompany.com/Administrations".to_string()
            ),
        ]
    );

    let saved: toml::Value =
        toml::from_str(&std::fs::read_to_string(config_path(&home)).expect("config"))
            .expect("valid TOML");
    assert_eq!(saved["region"].as_str(), Some("be"));
    assert_eq!(saved["default_admin"].as_str(), Some("voorbeeld_bv"));
    assert!(saved.get("base_url").is_none(), "runtime URL was persisted");

    let shown = stdout_json(yuki(&home, &["config", "show", "--output", "json"]));
    assert_eq!(
        shown["profiles"]["voorbeeld_bv"]["api_root"],
        "https://api.yukiworks.be/ws"
    );
    let doctor = stdout_json(yuki(&home, &["doctor", "--offline", "--output", "json"]));
    assert_eq!(doctor["checks"][3]["name"], "endpoint");
    assert_eq!(doctor["checks"][3]["detail"], "https://api.yukiworks.be/ws");
}

fn saved_config(home: &TempDir) -> toml::Value {
    toml::from_str(&std::fs::read_to_string(config_path(home)).expect("config"))
        .expect("valid TOML")
}

#[test]
fn a_fresh_init_persists_the_env_resolved_endpoint_like_a_typed_region() {
    let (root, _) = mock_yuki();
    let env = [("YUKI_REGION", "be"), ("YUKI_BASE_URL", root.as_str())];

    // Nothing exists yet to protect from the environment, so the endpoint this
    // run actually used is the one `init` has to record — otherwise the next
    // run without YUKI_REGION silently falls back to the legacy `nl` default.
    let home = TempDir::new().expect("temp home");
    let init = yuki_with_env(&home, &["init", "--api-key", "be-key"], &env);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let saved = saved_config(&home);
    assert_eq!(
        saved["region"].as_str(),
        Some("be"),
        "env region was not persisted on a fresh init: {saved}"
    );
    // The mock URL is just how the test reaches the Belgian host; a known
    // region is recorded as `region`, not as the runtime URL that reached it.
    assert!(
        saved.get("base_url").is_none(),
        "runtime URL was persisted: {saved}"
    );
}

#[test]
fn an_exported_region_is_never_persisted_over_an_existing_config() {
    let (root, _) = mock_yuki();
    let env = [("YUKI_REGION", "be"), ("YUKI_BASE_URL", root.as_str())];

    // Seed a config the ordinary way, with a region env will disagree with, so
    // there is something for the environment to try (and fail) to overwrite.
    let home = TempDir::new().expect("temp home");
    let seed = yuki_with_env(
        &home,
        &["init", "--api-key", "key-1", "--region", "nl"],
        &[("YUKI_BASE_URL", root.as_str())],
    );
    assert!(
        seed.status.success(),
        "{}",
        String::from_utf8_lossy(&seed.stderr)
    );
    assert_eq!(saved_config(&home)["region"].as_str(), Some("nl"));

    // Key rotation.
    let rotate = yuki_with_env(&home, &["init", "--api-key", "other-key"], &env);
    assert!(
        rotate.status.success(),
        "{}",
        String::from_utf8_lossy(&rotate.stderr)
    );
    assert_eq!(
        saved_config(&home)["region"].as_str(),
        Some("nl"),
        "env region overwrote the stored one on rotation"
    );

    // --add stamps nothing either.
    let add = yuki_with_env(&home, &["init", "--add", "--api-key", "third-key"], &env);
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    let saved = saved_config(&home);
    let entry = &saved["administrations"]["voorbeeld_bv"];
    assert!(entry.get("region").is_none(), "env region stamped: {saved}");

    // The flag, by contrast, is stored.
    let flag = yuki_with_env(
        &home,
        &["init", "--add", "--api-key", "third-key", "--region", "be"],
        &env,
    );
    assert!(
        flag.status.success(),
        "{}",
        String::from_utf8_lossy(&flag.stderr)
    );
    let saved = saved_config(&home);
    assert_eq!(
        saved["administrations"]["voorbeeld_bv"]["region"].as_str(),
        Some("be")
    );
}
