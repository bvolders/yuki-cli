//! Helpers shared by the end-to-end tests that run the `yuki` binary.
// Each test crate compiles this module on its own and uses only part of it.
#![allow(dead_code)]

use std::process::{Command, Output};

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

/// Stdout of a successful command, parsed as JSON.
pub fn stdout_json(output: Output) -> Value {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON stdout")
}

/// One request a [`mock_yuki`] received.
#[derive(Debug, Clone)]
pub struct Request {
    /// The SOAP action's operation, e.g. `Authenticate`.
    pub action: String,
    pub body: String,
}

/// Every request a [`mock_yuki`] received, in order.
pub type RequestLog = std::sync::Arc<std::sync::Mutex<Vec<Request>>>;

/// Serve a local Yuki mock on a random port. `reply(action, body)` gives the
/// response body to each request, sent as HTTP 200; an empty reply drops the
/// connection without answering. Returns the API root to put in the config
/// and the request log.
pub fn mock_yuki(reply: impl Fn(&str, &str) -> String + Send + 'static) -> (String, RequestLog) {
    use std::io::{BufRead, BufReader, Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let root = format!("http://{}/ws", listener.local_addr().expect("addr"));
    let log = RequestLog::default();
    let seen = std::sync::Arc::clone(&log);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            let mut request_line = String::new();
            reader.read_line(&mut request_line).ok();
            let (mut action, mut length) = (String::new(), 0usize);
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                    break;
                }
                let lower = header.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap_or(0);
                } else if lower.starts_with("soapaction:") {
                    action = header["soapaction:".len()..]
                        .trim()
                        .trim_matches('"')
                        .rsplit('/')
                        .next()
                        .unwrap_or_default()
                        .to_string();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).ok();
            let body = String::from_utf8_lossy(&body).into_owned();
            let response = reply(&action, &body);
            seen.lock().expect("log").push(Request { action, body });
            if response.is_empty() {
                continue;
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            )
            .ok();
        }
    });
    (root, log)
}

/// A SOAP response for `op` whose `{op}Result` holds `inner`.
pub fn soap_response(op: &str, inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body><{op}Response xmlns="http://www.theyukicompany.com/"><{op}Result>{inner}</{op}Result></{op}Response></soap:Body></soap:Envelope>"#
    )
}
