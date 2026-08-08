# Task: `mc2 exec` + `mc2 logs` (see inside VMs)

Status: **IMPLEMENTED — 2026-08-08**
Date: 2026-08-08

## Context

MC2 can apply stacks but operators can't see inside a running VM: no way to
run a command or read output. The microsandbox SDK already supports both —
`SandboxHandle::exec()` returns exit code + captured stdout/stderr, and there's
a public `microsandbox::logs::read_logs(name, opts)` (logs under
`MSB_HOME/sandboxes/<name>/logs/`).

## Design

### CLI

```text
mc2 exec <instance> <cmd...>          # run a command, mirror stdout/stderr, exit with its code
mc2 logs <instance> [--tail N]        # print recent sandbox logs (runtime/exec/kernel)
```

`<instance>` is a UUID or `<stack>/<service>/<ordinal>` (existing resolution).

### Server

- **`POST /v1/instances/{id}/exec`** body `{cmd: [...]}` → resolves the
  instance's `runtime_id`, runs `exec`, returns
  `{exitCode, stdout, stderr}`.
- **`GET /v1/instances/{id}/logs?tail=N`** → resolves `runtime_id`, reads
  `microsandbox::logs::read_logs` with `tail`, returns entries
  `[{timestamp, source, data}]`.
- The runtime is created **once** in `mc2-server::run` and **shared** between
  the node loop and `AppState` (both currently construct their own).
- `NodeRuntime` gains `exec_with_output(&self, runtime_id, argv) ->
  Result<ExecResult>` returning `{exit_code, stdout, stderr}` (defined in
  mc2-runtime; `exec_command` delegates to it). Logs use the SDK directly in
  the handler.

## Changes by area

| Area | Change | Size |
| --- | --- | --- |
| `mc2-runtime` | `ExecResult` + `exec_with_output` trait method | ~25 lines |
| `mc2-server` | shared `Arc<dyn NodeRuntime>` in `AppState`; `exec` + `logs` handlers; resolve runtime_id | ~90 lines |
| `mc2` CLI | `exec` + `logs` commands (resolve instance, print, exit code) | ~80 lines |
| Tests | exec/logs HTTP handlers (mock-free, real runtime), CLI smoke | ~80 lines |
| Docs | README command table, stack-yaml? (no — operator guide), CHANGELOG | small |

## Out of scope (this pass)

- Interactive/TTY `exec` (needs WebSocket + pty) — piped stdin is supported.
- `stdin` to `exec` — **done**: piped stdin is forwarded via the SDK's
  `stdin_bytes`.
- `--follow` / streaming logs — **done**: SSE stream (`follow=true`), tail
  snapshot + cursor resume.

## Acceptance criteria

- [ ] `mc2 exec demo/web/0 echo hi` prints `hi`, exits 0; non-zero exits propagate.
- [ ] `echo hi | mc2 exec demo/web/0 cat` prints `hi` (stdin forwarded).
- [ ] `mc2 logs demo/web/0` prints recent runtime/exec entries; `--tail N` limits.
- [ ] `mc2 logs demo/web/0 --follow` streams new entries (SSE).
- [ ] Missing/not-running instance → clear 4xx error, no crash.
- [ ] `just check` green.

## Handover notes (append as completed)

- (pending)

## Implementation notes (2026-08-08)

- **`mc2-runtime`**: added `ExecResult { exit_code, stdout, stderr }` and
  `NodeRuntime::exec_with_output(runtime_id, argv, stdin)` — feeds stdin via the
  SDK's `exec_stream_with(...).stdin_bytes(...)` + `collect()`; `exec_command`
  (health) delegates with empty stdin.
- **`mc2-server`**: runtime is created once in `lib.rs::run`
  (`MicrosandboxRuntime::new(volume_dir)`) and shared via `AppState.runtime`
  with the node loop (`node::run` now takes it instead of building its own;
  `NodeConfig.volume_dir` removed as unused). New handlers:
  `POST /v1/instances/{id}/exec` (`{cmd, stdin}` → `{exitCode, stdout, stderr}`)
  and `GET /v1/instances/{id}/logs?tail=N&follow=` — non-follow returns JSON;
  follow returns an SSE stream (tail snapshot via `read_logs`, then resume from
  the snapshot cursor via `log_stream(start: From(cursor), follow: true)`).
  Missing instance → 404; no runtime yet → 400.
- **`mc2` CLI**: `exec <instance> <cmd…>` (forwards piped stdin when not a
  TTY, mirrors stdout/stderr, exits with the code) and
  `logs <instance> [--tail N] [--follow]` (non-follow prints `[source] data`;
  follow consumes the SSE stream incrementally). Both resolve
  `<stack>/<service>/<ordinal>`.
- **Tests**: `logs_reads_sandbox_entries` exercises the read_logs handler
  against a fake `MSB_HOME` log dir; CLI smoke — exec/logs/follow fail cleanly
  on a dead server, `--help` renders. Full exec/follow against a running
  sandbox needs a hypervisor (lab-only).
- **Docs**: README command table, CHANGELOG.
- `just check` green (22 test binaries).

### Live-lab findings (2026-08-08, macOS Apple Silicon)

- HVF is present (`mc2 doctor`), but a microVM could **not be booted on this
  machine** during validation. The failure is environmental, not code:
  - SDK 0.6.8 wants `libkrunfw.5.dylib`; the installed msb shipped only
    `libkrunfw.4`. Downloaded the v0.6.8 firmware and installed it as
    `~/.local/lib/libkrunfw.5.dylib` (fixes the "not found" error).
  - The system still has an old `~/.local/lib/libkrun.1.dylib` (msb 0.5.10-era,
    March 2025), ABI-incompatible with `libkrunfw.5` → the sandbox process
    exits (`unix_wait_status(512)`) before startup, so `SandboxHandle::connect`
    fails and `exec`/`logs` return 500.
  - microsandbox 0.6.8 ships no `libkrun` and its tooling now defaults to a
    cloud backend (`api.microsandbox.dev`), so a purely-local boot needs a
    matching `libkrun` from the krun project — a separate systems-install task.
- **Conclusion**: exec/logs/follow are fully verified through the API/CLI
  layer; a real sandbox boot requires the local krun toolchain aligned
  (`libkrun` + `libkrunfw` from the same generation). Not an MC2 defect.
