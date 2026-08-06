//! Black-box CLI smoke tests for the `mcc` binary.
//!
//! These run as Cargo integration tests of the `mcc` package so
//! `CARGO_BIN_EXE_mcc` resolves correctly.

use std::process::Command;
use tempfile::tempdir;

fn mcc() -> Command {
    Command::new(env!("CARGO_BIN_EXE_mcc"))
}

#[test]
fn version_prints_name() {
    let out = mcc().arg("version").output().expect("run");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("mcc"), "{stdout}");
    assert!(stdout.contains("MicroCommandControl"), "{stdout}");
    assert!(stdout.contains("mcc/v1"), "{stdout}");
}

#[test]
fn help_lists_core_commands() {
    let out = mcc().arg("--help").output().expect("run");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("server"));
    assert!(stdout.contains("agent"));
    assert!(stdout.contains("apply"));
}

#[test]
fn apply_without_token_reaches_server_or_connection_error() {
    // Token is optional; without a server we get a connection error, not a local "missing token".
    let out = mcc()
        .args([
            "apply",
            "-f",
            "examples/stacks/smoke.yaml",
            "--api",
            "http://127.0.0.1:1",
        ])
        .output()
        .expect("run");
    assert!(!out.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !err.contains("missing --token"),
        "should not require token client-side: {err}"
    );
}

#[test]
fn server_init_only_writes_data_dir() {
    let dir = tempdir().unwrap();
    let out = mcc()
        .args([
            "server",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--init-only",
        ])
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir.path().join("mcc.db").is_file());
    assert!(dir.path().join("secrets.key").is_file());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("API token") || combined.contains("mccat_"),
        "expected bootstrap credentials in output: {combined}"
    );
}

#[test]
fn server_init_only_second_run_no_fresh_tokens_banner() {
    let dir = tempdir().unwrap();
    let path = dir.path().to_str().unwrap();

    let first = mcc()
        .args(["server", "--data-dir", path, "--init-only"])
        .output()
        .expect("first");
    assert!(first.status.success());

    let second = mcc()
        .args(["server", "--data-dir", path, "--init-only"])
        .output()
        .expect("second");
    assert!(second.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(
        !combined.contains("shown once"),
        "second init should not re-print bootstrap banner: {combined}"
    );
}
