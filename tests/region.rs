//! End-to-end: the region and base URL reach the HTTP layer, and `init` stores
//! the region. A local mock stands in for Yuki; nothing leaves the machine.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use serde_json::Value;
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

fn yuki(home: &TempDir, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_yuki"));
    command
        .args(args)
        .env("HOME", home.path())
        .env_remove("YUKI_REGION")
        .env_remove("YUKI_BASE_URL");
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("yuki command")
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
fn init_with_region_be_stores_it_and_later_commands_use_the_belgian_host() {
    let home = TempDir::new().expect("temp home");
    let (root, seen) = mock_yuki();

    let init = yuki(
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

    let shown = stdout_json(yuki(&home, &["config", "show", "--output", "json"], &[]));
    assert_eq!(
        shown["profiles"]["voorbeeld_bv"]["api_root"],
        "https://api.yukiworks.be/ws"
    );
    let doctor = stdout_json(yuki(
        &home,
        &["doctor", "--offline", "--output", "json"],
        &[],
    ));
    assert_eq!(doctor["checks"][3]["name"], "endpoint");
    assert_eq!(doctor["checks"][3]["detail"], "https://api.yukiworks.be/ws");
}

#[test]
fn region_flag_and_env_override_the_config_without_saving() {
    let home = TempDir::new().expect("temp home");
    let path = config_path(&home);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("config dir");
    let original = "api_key = \"k\"\ndefault_admin = \"a\"\n\n[administrations.a]\ndomain_id = \"d\"\nadmin_id = \"x\"\n";
    std::fs::write(&path, original).expect("config");

    let root_of = |shown: Value| shown["profiles"]["a"]["api_root"].clone();
    let default = stdout_json(yuki(&home, &["config", "show", "--output", "json"], &[]));
    assert_eq!(root_of(default), "https://api.yukiworks.nl/ws");

    let flag = stdout_json(yuki(
        &home,
        &["config", "show", "--region", "be", "--output", "json"],
        &[],
    ));
    assert_eq!(root_of(flag), "https://api.yukiworks.be/ws");

    let env = stdout_json(yuki(
        &home,
        &["config", "show", "--output", "json"],
        &[("YUKI_REGION", "be")],
    ));
    assert_eq!(root_of(env), "https://api.yukiworks.be/ws");

    // A profile write under an override must not persist it.
    stdout_json(yuki(
        &home,
        &["profile", "use", "a", "--region", "be", "--output", "json"],
        &[],
    ));
    let saved = std::fs::read_to_string(&path).expect("config");
    assert!(!saved.contains("region"), "{saved}");

    let bad = yuki(&home, &["config", "show", "--region", "de"], &[]);
    assert!(!bad.status.success());
}

#[test]
fn base_url_reaches_every_authenticated_command() {
    let home = TempDir::new().expect("temp home");
    let path = config_path(&home);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("config dir");
    std::fs::write(
        &path,
        "api_key = \"k\"\ndefault_admin = \"a\"\nregion = \"be\"\n\n[administrations.a]\ndomain_id = \"d\"\nadmin_id = \"x\"\n",
    )
    .expect("config");
    let (root, seen) = mock_yuki();

    // The mock answers Authenticate but nothing else, so the command fails after
    // its first request; which service it reached is what matters here.
    yuki(
        &home,
        &["vat", "codes", "--base-url", &root, "--output", "json"],
        &[],
    );
    let requests = seen.lock().expect("log").clone();
    assert_eq!(requests[0].0, "/ws/Vat.asmx");
    assert_eq!(requests[0].1, "http://www.theyukicompany.com/Authenticate");
}
