//! Black-box CLI smoke tests for the `mc2` binary.
//!
//! These run as Cargo integration tests of the `mc2` package so
//! `CARGO_BIN_EXE_mc2` resolves correctly.

use std::process::Command;
use tempfile::tempdir;

fn mc2() -> Command {
    Command::new(env!("CARGO_BIN_EXE_mc2"))
}

#[test]
fn version_prints_name() {
    let out = mc2().arg("version").output().expect("run");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("mc2"), "{stdout}");
    assert!(stdout.contains("MC2"), "{stdout}");
    assert!(stdout.contains("MicroCommandControl"), "{stdout}");
    assert!(stdout.contains("mc2/v1"), "{stdout}");
    assert!(stdout.contains("otlp"), "{stdout}");
}

#[test]
fn doctor_runs() {
    let out = mc2().arg("doctor").output().expect("run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("mc2 doctor"), "{stdout}");
    assert!(stdout.contains("runtime:"), "{stdout}");
}

#[test]
fn help_lists_core_commands() {
    let out = mc2().arg("--help").output().expect("run");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("server"));
    assert!(stdout.contains("apply"));
    assert!(stdout.contains("secret"));
    assert!(
        !stdout.contains("  agent"),
        "agent command removed: {stdout}"
    );
}

#[test]
fn secret_help_lists_set_ls_rm() {
    let out = mc2().args(["secret", "--help"]).output().expect("run");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("set"), "{stdout}");
    assert!(stdout.contains("ls"), "{stdout}");
    assert!(stdout.contains("rm"), "{stdout}");
}

#[test]
fn apply_without_token_reaches_server_or_connection_error() {
    // Token is optional; without a server we get a connection error, not a local "missing token".
    let out = mc2()
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
    let out = mc2()
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
    assert!(dir.path().join("mc2.db").is_file());
    assert!(dir.path().join("secrets.key").is_file());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("API token") || combined.contains("mc2at_"),
        "expected bootstrap credentials in output: {combined}"
    );
}

#[test]
fn server_init_only_second_run_no_fresh_tokens_banner() {
    let dir = tempdir().unwrap();
    let path = dir.path().to_str().unwrap();

    let first = mc2()
        .args(["server", "--data-dir", path, "--init-only"])
        .output()
        .expect("first");
    assert!(first.status.success());

    let second = mc2()
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
