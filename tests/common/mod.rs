//! Helpers shared by the end-to-end tests that run the `yuki` binary, with a
//! local SOAP mock standing in for Yuki; nothing leaves the machine.
// Each test crate compiles this module on its own and uses only part of it.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tempfile::TempDir;

/// Run `yuki` with `home` as `$HOME` and no endpoint variables from the caller's shell.
pub fn yuki(home: &TempDir, args: &[&str]) -> Output {
    yuki_with_env(home, args, &[])
}

/// [`yuki`] with extra environment variables set.
pub fn yuki_with_env(home: &TempDir, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_yuki"));
    command
        .args(args)
        .env("HOME", home.path())
        .env_remove("YUKI_REGION")
        .env_remove("YUKI_BASE_URL")
        .env_remove("YUKI_PROBE_ROOTS");
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("yuki command")
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Stdout parsed as JSON, whatever the exit status.
#[track_caller]
pub fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "JSON stdout ({e}): {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            stderr(output)
        )
    })
}

/// Stdout of a successful command, parsed as JSON.
#[track_caller]
pub fn stdout_json(output: Output) -> Value {
    assert!(
        output.status.success(),
        "command failed: {}",
        stderr(&output)
    );
    json(&output)
}

/// A home whose config has one administration, `example` (domain `domain-1`,
/// admin `admin-1`), on `root` with access key `key`. `extra_toml` is
/// appended: more fields of `example`, or further tables.
pub fn home_with_config(root: &str, key: &str, extra_toml: &str) -> TempDir {
    let home = TempDir::new().expect("temp home");
    let dir = home.path().join(".config/yuki");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(
        dir.join("config.toml"),
        format!(
            r#"api_key = "{key}"
default_admin = "example"
region = "be"

[administrations.example]
domain_id = "domain-1"
admin_id = "admin-1"
name = "Example BV"
base_url = "{root}"
{extra_toml}"#
        ),
    )
    .expect("write config");
    home
}

/// A SOAP envelope around `body`.
pub fn envelope(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body>{body}</soap:Body></soap:Envelope>"#
    )
}

/// A SOAP response for `op` whose `{op}Result` holds `inner`.
pub fn response(op: &str, inner: &str) -> String {
    envelope(&format!(
        r#"<{op}Response xmlns="http://www.theyukicompany.com/"><{op}Result>{inner}</{op}Result></{op}Response>"#
    ))
}

/// A SOAP fault, which Yuki sends with HTTP 500.
pub fn fault(message: &str) -> String {
    envelope(&format!(
        "<soap:Fault><faultcode>soap:Server</faultcode><faultstring>{message}</faultstring></soap:Fault>"
    ))
}

/// One request a [`mock`] received.
#[derive(Debug, Clone)]
pub struct Request {
    /// The URL path, e.g. `/ws/Accounting.asmx`.
    pub path: String,
    /// The SOAP action's operation, e.g. `Authenticate`.
    pub action: String,
    /// The `SOAPAction` header as sent, quotes included.
    pub soap_action: String,
    pub body: String,
}

impl Request {
    /// The raw (still escaped) text of the request parameter `name`.
    pub fn param(&self, name: &str) -> &str {
        self.body
            .split(&format!("<yuki:{name}>"))
            .nth(1)
            .and_then(|rest| rest.split("</yuki:").next())
            .unwrap_or_default()
    }
}

/// Every request a [`mock`] received, in order.
pub type RequestLog = Arc<Mutex<Vec<Request>>>;

/// The operations of every request, in order.
pub fn actions(log: &RequestLog) -> Vec<String> {
    log.lock()
        .expect("log")
        .iter()
        .map(|r| r.action.clone())
        .collect()
}

/// The bodies of the requests for `action`, in order.
pub fn bodies(log: &RequestLog, action: &str) -> Vec<String> {
    log.lock()
        .expect("log")
        .iter()
        .filter(|r| r.action == action)
        .map(|r| r.body.clone())
        .collect()
}

/// Serve a local Yuki mock on a random port, each connection on its own
/// thread. `handler` gives each request its HTTP status and body; status 0
/// drops the connection without answering, and a handler that never returns
/// is a request that never gets an answer. A request is logged before the
/// handler runs. Returns the API root to put in the config, and the log.
pub fn mock(
    handler: impl Fn(&Request) -> (u16, String) + Send + Sync + 'static,
) -> (String, RequestLog) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let root = format!("http://{}/ws", listener.local_addr().expect("addr"));
    let log = RequestLog::default();
    let seen = Arc::clone(&log);
    let handler = Arc::new(handler);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (seen, handler) = (Arc::clone(&seen), Arc::clone(&handler));
            std::thread::spawn(move || serve(stream, &seen, &*handler));
        }
    });
    (root, log)
}

fn serve(mut stream: TcpStream, log: &RequestLog, handler: &dyn Fn(&Request) -> (u16, String)) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok();
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    let (mut soap_action, mut length) = (String::new(), 0usize);
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
            break;
        }
        let lower = header.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        } else if lower.starts_with("soapaction:") {
            soap_action = header["soapaction:".len()..].trim().to_string();
        }
    }
    let action = soap_action
        .trim_matches('"')
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok();
    let request = Request {
        path,
        action,
        soap_action,
        body: String::from_utf8_lossy(&body).into_owned(),
    };
    log.lock().expect("log").push(request.clone());
    let (status, reply) = handler(&request);
    if status == 0 {
        return;
    }
    write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Type: text/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    )
    .ok();
}
