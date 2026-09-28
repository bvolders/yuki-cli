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
        .env_remove("YUKI_BASE_URL");
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
