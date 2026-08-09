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
    assert!(stdout.contains("up"));
    assert!(stdout.contains("down"));
    assert!(stdout.contains("config"));
    assert!(stdout.contains("secret"));
    assert!(
        !stdout.contains("  rm"),
        "rm folded into down (alias, not a top-level command): {stdout}"
    );
    assert!(
        !stdout.contains("  apply"),
        "apply removed (compose language): {stdout}"
    );
    assert!(
        !stdout.contains("  agent"),
        "agent command removed: {stdout}"
    );
}

#[test]
fn help_groups_top_level_commands() {
    let out = mc2().arg("--help").output().expect("run");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for heading in [
        "Stacks:",
        "Observe:",
        "Access:",
        "Security:",
        "Admin:",
        "Other:",
    ] {
        assert!(stdout.contains(heading), "missing {heading}: {stdout}");
    }
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
fn up_without_token_reaches_server_or_connection_error() {
    // Token is optional; without a server we get a connection error, not a local "missing token".
    let out = mc2()
        .args([
            "up",
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
fn compose_verbs_validate_locally() {
    // `mc2 config` is client-side: validate a stack without a server.
    let dir = tempdir().unwrap();
    let stack_path = dir.path().join("demo.yaml");
    std::fs::write(
        &stack_path,
        "services:\n  web:\n    image: alpine\n    command: [\"echo\", \"hi\"]\n",
    )
    .unwrap();

    let out = mc2()
        .args(["config", "-f", stack_path.to_str().unwrap()])
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("name: demo"), "{stdout}");
    assert!(stdout.contains("image: alpine"), "{stdout}");

    // Invalid stack → validation error, non-zero exit.
    std::fs::write(&stack_path, "services:\n  web:\n    image: alpine\n    volumes:\n      - name: missing\n        mount: /x\n").unwrap();
    let bad = mc2()
        .args(["config", "-f", stack_path.to_str().unwrap()])
        .output()
        .expect("run");
    assert!(!bad.status.success(), "invalid stack must fail validation");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stdout),
        String::from_utf8_lossy(&bad.stderr)
    );
    assert!(combined.to_lowercase().contains("invalid"), "{combined}");

    // `mc2 down` / `mc2 rm` on a dead server → clean connection error, not a crash.
    for args in [
        vec!["down", "smoke-ingress"],
        vec!["rm", "smoke-ingress"],
        vec!["rm", "smoke-ingress", "--volumes"],
    ] {
        let mut cmd = mc2();
        cmd.args(&args).args(["--api", "http://127.0.0.1:1"]);
        let out = cmd.output().expect("run");
        assert!(!out.status.success(), "expected failure for {args:?}");
    }
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

#[test]
fn help_lists_parity_commands() {
    let out = mc2().arg("--help").output().expect("run");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for cmd in ["status", "network", "ingress", "completions", "ps", "node"] {
        assert!(stdout.contains(cmd), "missing {cmd}: {stdout}");
    }
}

#[test]
fn ssh_help_lists_flattened_key_commands() {
    let out = mc2().args(["ssh", "--help"]).output().expect("run");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for cmd in ["add-key", "show-key", "keys", "rm-key", "open", "close"] {
        assert!(stdout.contains(cmd), "missing {cmd}: {stdout}");
    }
    // The old `ssh key <sub>` subgroup is gone (flattened into add-key/keys/rm-key).
    let old = mc2().args(["ssh", "key", "--help"]).output().expect("run");
    assert!(!old.status.success(), "old `ssh key` subgroup must be gone");
}

#[test]
fn completions_generate_for_all_shells() {
    for shell in ["bash", "zsh", "fish"] {
        let out = mc2().args(["completions", shell]).output().expect("run");
        assert!(
            out.status.success(),
            "{shell}: stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.len() > 100, "{shell} output too short");
        assert!(stdout.contains("mc2"), "{shell} must reference mc2");
    }
}

#[test]
fn network_error_is_unified_api_error() {
    // Server unreachable: connection error, non-zero exit, one-line message.
    let out = mc2()
        .args(["network", "demo/web/0", "--api", "http://127.0.0.1:1"])
        .output()
        .expect("run");
    assert!(!out.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.to_lowercase().contains("error"), "{combined}");
}

#[test]
fn ps_supports_json_output_flag() {
    let out = mc2().args(["ps", "--help"]).output().expect("run");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--output"), "{stdout}");
    assert!(stdout.contains("json"), "{stdout}");
    assert!(stdout.contains("--stack"), "{stdout}");
}

#[test]
fn context_set_use_ls_roundtrip() {
    // Isolate the client config under a temp HOME.
    let dir = tempdir().unwrap();
    let home = dir.path().to_str().unwrap();

    let set = mc2()
        .env("HOME", home)
        .args([
            "context",
            "set",
            "prod",
            "--api",
            "https://mc2.example.com",
            "--token",
            "mc2at_test",
        ])
        .output()
        .expect("context set");
    assert!(
        set.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&set.stderr)
    );

    let use_ = mc2()
        .env("HOME", home)
        .args(["context", "use", "prod"])
        .output()
        .expect("context use");
    assert!(
        use_.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&use_.stderr)
    );

    let ls = mc2()
        .env("HOME", home)
        .args(["context", "ls"])
        .output()
        .expect("context ls");
    assert!(ls.status.success());
    let stdout = String::from_utf8_lossy(&ls.stdout);
    assert!(stdout.contains("prod"), "{stdout}");
    assert!(stdout.contains("mc2.example.com"), "{stdout}");
    assert!(stdout.contains("remote"), "{stdout}");
    // Current marker on the prod row.
    assert!(stdout.contains("*"), "{stdout}");

    // Config file is 0600 and token is stored.
    let cfg = dir.path().join(".mc2/config.toml");
    assert!(cfg.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&cfg).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "config must be 0600");
    }
    let raw = std::fs::read_to_string(&cfg).unwrap();
    assert!(raw.contains("mc2at_test"), "{raw}");
}

#[test]
fn context_ls_empty_prints_default_hint() {
    let dir = tempdir().unwrap();
    let out = mc2()
        .env("HOME", dir.path().to_str().unwrap())
        .args(["context", "ls"])
        .output()
        .expect("run");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("127.0.0.1:7443"), "{stdout}");
    assert!(stdout.contains("context set"), "{stdout}");
}

#[test]
fn context_missing_fails_fast() {
    let dir = tempdir().unwrap();
    let home = dir.path().to_str().unwrap();
    mc2()
        .env("HOME", home)
        .args(["context", "set", "prod", "--api", "https://mc2.example.com"])
        .output()
        .expect("set");
    // Referencing an unknown context must fail client-side before any network call.
    let out = mc2()
        .env("HOME", home)
        .args(["status", "--context", "nope"])
        .output()
        .expect("run");
    assert!(!out.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("not found"),
        "expected client-side context error: {combined}"
    );
    assert!(
        !combined.contains("unreachable"),
        "should not reach the network: {combined}"
    );
}

#[test]
fn setup_help_lists_server_and_client_trees() {
    let out = mc2().args(["setup", "--help"]).output().expect("run");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("server"), "{stdout}");
    assert!(stdout.contains("client"), "{stdout}");

    for sub in ["server", "client"] {
        let out = mc2().args(["setup", sub, "--help"]).output().expect("run");
        assert!(
            out.status.success(),
            "{sub}: stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn setup_requires_a_terminal() {
    // Piped/null stdin is not interactive → friendly error, not a hang.
    use std::process::Stdio;
    let out = mc2()
        .arg("setup")
        .stdin(Stdio::null())
        .output()
        .expect("run");
    assert!(!out.status.success(), "setup must fail without a TTY");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.to_lowercase().contains("interactive"),
        "{combined}"
    );
}

#[test]
fn exec_and_logs_reach_server_or_clean_error() {
    // exec/logs are server commands; on a dead server they fail cleanly.
    for args in [
        vec!["exec", "demo/web/0", "echo", "hi"],
        vec!["logs", "demo/web/0"],
        vec!["logs", "demo/web/0", "--tail", "10"],
        vec!["logs", "demo/web/0", "--follow"],
        vec!["logs", "demo/web/0", "--tail", "5", "--follow"],
    ] {
        let mut cmd = mc2();
        cmd.args(&args).args(["--api", "http://127.0.0.1:1"]);
        let out = cmd.output().expect("run");
        assert!(!out.status.success(), "expected failure for {args:?}");
    }
    let out = mc2().args(["exec", "--help"]).output().expect("run");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("instance"));
    let out = mc2().args(["logs", "--help"]).output().expect("run");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("--tail"));
    assert!(String::from_utf8_lossy(&out.stdout).contains("--follow"));
}

// ---------------------------------------------------------------------------
// Live-server harness: spawn a real `mc2 server` subprocess (no auth, temp
// data dir) and drive the CLI against it end-to-end. Sandboxes can't start
// without a hypervisor, but every control-plane command works.
// ---------------------------------------------------------------------------

use std::net::TcpStream;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

struct LiveServer {
    child: Child,
    base: String,
    _dir: tempfile::TempDir,
}

impl LiveServer {
    fn start() -> LiveServer {
        let dir = tempdir().unwrap();
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let base = format!("http://127.0.0.1:{port}");
        let child = mc2()
            .args([
                "server",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "--bind",
                &format!("127.0.0.1:{port}"),
                "--no-auth",
                "--reconcile-interval-secs",
                "1",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mc2 server");
        let mut srv = LiveServer {
            child,
            base,
            _dir: dir,
        };
        srv.wait_ready();
        srv
    }

    fn wait_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if TcpStream::connect(self.addr()).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("server did not become ready at {}", self.base);
    }

    fn addr(&self) -> String {
        self.base.trim_start_matches("http://").to_string()
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut c = mc2();
        c.args(args).args(["--api", self.base.as_str()]);
        c.output().expect("run cli")
    }

    fn run_raw(&self, args: &[&str]) -> std::process::Output {
        mc2().args(args).output().expect("run cli")
    }
}

impl Drop for LiveServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn live_server_end_to_end() {
    let srv = LiveServer::start();

    // Config validates a minimal stack.
    let config = srv.run_raw(&["config", "-f", "/nonexistent.yaml"]);
    assert!(!config.status.success(), "missing file must fail");

    // Apply a stack with a target-only (auto host) port.
    let stack = srv._dir.path().join("smoke.yaml");
    std::fs::write(
        &stack,
        "name: smoke\nservices:\n  web:\n    image: alpine\n    ports:\n      - \"3001\"\n",
    )
    .unwrap();
    let up = srv.run(&["up", "-f", stack.to_str().unwrap()]);
    assert!(
        up.status.success(),
        "up stderr={}",
        String::from_utf8_lossy(&up.stderr)
    );

    let ps = srv.run(&["ps"]);
    assert!(ps.status.success());
    let ps_out = String::from_utf8_lossy(&ps.stdout);
    assert!(ps_out.contains("smoke"), "ps: {ps_out}");

    // Network summary + per-network detail (implicit default network `smoke`).
    let net = srv.run(&["network"]);
    assert!(net.status.success());
    let net_out = String::from_utf8_lossy(&net.stdout);
    assert!(net_out.contains("NETWORK"), "network: {net_out}");
    assert!(net_out.contains("smoke"), "network: {net_out}");
    let net_detail = srv.run(&["network", "smoke"]);
    assert!(net_detail.status.success());
    let detail_out = String::from_utf8_lossy(&net_detail.stdout);
    assert!(detail_out.contains("smoke"), "detail: {detail_out}");
    assert!(detail_out.contains("web"), "detail: {detail_out}");
    // Unknown network name → clean error listing known networks.
    let missing = srv.run(&["network", "nope"]);
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("smoke"),
        "missing: {}",
        String::from_utf8_lossy(&missing.stderr)
    );

    let status = srv.run(&["status"]);
    assert!(status.status.success());
    let status_out = String::from_utf8_lossy(&status.stdout);
    assert!(status_out.contains("local:"), "status: {status_out}");

    // Secrets lifecycle.
    let set = srv.run(&["secret", "set", "SMOKE", "--value", "x"]);
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    let ls = srv.run(&["secret", "ls"]);
    assert!(String::from_utf8_lossy(&ls.stdout).contains("SMOKE"));
    assert!(srv.run(&["secret", "rm", "SMOKE"]).status.success());

    // SSH keys lifecycle (flattened: add-key / keys / rm-key).
    let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJustAFakeKeyMaterialHere0000 test@mc2";
    assert!(srv
        .run(&["ssh", "add-key", "dev", "--key", key])
        .status
        .success());
    assert!(String::from_utf8_lossy(&srv.run(&["ssh", "keys"]).stdout).contains("dev"));

    // `ssh: true` on a service → `mc2 up` prints the declared SSH front end.
    let ssh_stack = srv._dir.path().join("ssh.yaml");
    std::fs::write(
        &ssh_stack,
        "name: sshsmoke\nservices:\n  web:\n    image: alpine\n    ssh: true\n",
    )
    .unwrap();
    let up_ssh = srv.run(&["up", "-f", ssh_stack.to_str().unwrap()]);
    assert!(
        up_ssh.status.success(),
        "{}",
        String::from_utf8_lossy(&up_ssh.stderr)
    );
    let up_ssh_out = String::from_utf8_lossy(&up_ssh.stdout);
    assert!(up_ssh_out.contains("ssh:"), "up ssh: {up_ssh_out}");
    assert!(
        up_ssh_out.contains("auto"),
        "up ssh auto port: {up_ssh_out}"
    );
    assert!(srv.run(&["down", "sshsmoke"]).status.success());
    assert!(srv.run(&["ssh", "rm-key", "dev"]).status.success());

    // Contexts + mode-aware status JSON (context is client-local, no --api).
    assert!(srv
        .run_raw(&["context", "set", "local", "--api", srv.base.as_str()])
        .status
        .success());
    assert!(srv.run_raw(&["context", "use", "local"]).status.success());
    let status_json = srv.run(&["status", "-o", "json"]);
    assert!(status_json.status.success());
    let v: serde_json::Value = serde_json::from_slice(&status_json.stdout).unwrap();
    assert_eq!(v["mode"], "local");
    assert_eq!(v["stacks"], 1);

    // exec/logs reach the server cleanly (no hypervisor → instance has no
    // sandbox runtime, so a clean 4xx, never a crash or hang).
    let exec = srv.run(&["exec", "smoke/web/0", "echo", "hi"]);
    assert!(!exec.status.success());
    assert!(
        String::from_utf8_lossy(&exec.stdout).contains("runtime")
            || String::from_utf8_lossy(&exec.stderr).contains("runtime")
    );
    let logs = srv.run(&["logs", "smoke/web/0"]);
    assert!(!logs.status.success());
    assert!(
        String::from_utf8_lossy(&logs.stdout).contains("runtime")
            || String::from_utf8_lossy(&logs.stderr).contains("runtime")
    );

    // Tear down: down then rm --volumes.
    let down = srv.run(&["down", "smoke"]);
    assert!(
        down.status.success(),
        "{}",
        String::from_utf8_lossy(&down.stderr)
    );
    let ps_after = srv.run(&["ps"]);
    assert!(String::from_utf8_lossy(&ps_after.stdout).contains("No instances."));
    assert!(srv
        .run(&["up", "-f", stack.to_str().unwrap()])
        .status
        .success());
    let rm = srv.run(&["rm", "smoke", "--volumes"]);
    assert!(
        rm.status.success(),
        "{}",
        String::from_utf8_lossy(&rm.stderr)
    );
}
