# Task: Single-process merge — delete the gRPC agent seam

Status: **DONE 2026-08-08** (single commit on `dev`)
Date: 2026-08-07
Supersedes: `docs/tasks/host-port-ownership.md` (re-scoped as follow-up; see end)

## Context

MC2 runs as two processes today: `mc2 server` (scheduler, SQLite, REST) and
`mc2 agent` (join/heartbeat/sync/report over gRPC, drives the embedded
microsandbox SDK). But microsandbox itself is **daemon-less by design** —
its README: *"Embeddable: Spawn VMs right within your code. No setup server.
No long-running daemon."* — `spawn.rs` fork+execs `msb sandbox` as a child
of the calling process. `mc2-agent` is not mirroring any MSB component; it
is an MC2 invention whose only job is to ferry store rows across gRPC and
back.

Decision (user-approved 2026-08-07): **one process**. `mc2 server` hosts
the local reconcile loop directly. The gRPC service, proto, join/token flow,
and the agent crate are deleted outright (pre-release: no shims). Multi-node
becomes future work; nothing in SQLite/REST/scheduler blocks re-adding a
remote transport later.

## What the seam actually is (ground truth from the code)

`mc2-agent/src/lib.rs::reconcile` touches gRPC exactly twice:

1. `client.sync(...)` → server builds desired set from store rows +
   resolvers (`grpc.rs:111-201`: `resolve_injections`, `resolve_ssh_desired`,
   `build_fabric_desired`) → proto `DesiredInstance` →
   `mc2_runtime::desired_from_sync` → `DesiredSandbox`.
2. `client.report_status(...)` → `grpc.rs:203-291` → three store writes
   (`update_instance_status`, `update_instance_ssh_observed`,
   `update_instance_fabric_observed`).

Everything else in the agent loop (backoff, health probes, fabric splices,
ssh serve, ingress files) is local and moves intact.

**Consequence: the entire proto type layer evaporates.** Every
`mc2_api::agent::*` type exists only to cross the gRPC boundary; in-process,
the server can construct `mc2_runtime`'s native types directly.

## Design

### Process model

```text
mc2 server
├── REST (axum, --bind, default 127.0.0.1:7443)   [unchanged surface]
├── scheduler + reschedule + watcher + metrics loops  [unchanged]
└── local node loop (new mc2-server module `node`)
    ├── build desired set straight from store + resolvers
    ├── drive MicrosandboxRuntime / FabricTable / SshServeTable / IngressFileWriter
    └── write reports straight to store
```

- One implicit **local node** row, upserted at startup (hostname, labels,
  capacity). Heartbeat = the node loop touching `last_heartbeat` each tick.
- `watcher::not_ready_loop` stays as a safeguard (marks the node NotReady if
  the node loop stalls); reschedule code stays (degenerate single-node case,
  harmless, and keeps the code path tested).
- `mc2 node ls`, `/v1/nodes`, `/v1/status` keep working against the one row.

### Type migrations (proto → native)

| Proto type (deleted) | Replacement | Home |
| --- | --- | --- |
| `DesiredInstance`, `SecretInjection`, `SshDesired`, `FabricDesired/Allow/Expose` | gone — server builds `DesiredSandbox` directly via resolvers | mc2-server `desired.rs` |
| `FabricObserved`, `FabricEdgeStatus`, `FabricExposeStatus` | plain serde structs (same JSON shape as `fabric_observed_json` today) | mc2-runtime `fabric` |
| `SshObserved` | plain serde struct | mc2-runtime `spec` |
| `IngressRouteDesired` | plain serde struct next to `ReadyIngressRoute` | mc2-runtime `ingress_render` |
| `Join/Heartbeat/Sync/ReportStatus*` | deleted, no replacement | — |

Resolvers fold into native returns:
- `secrets::resolve_injections` output → `InjectedSecret` directly.
- `ssh::resolve_ssh_desired` → returns `DesiredSsh` directly.
- `fabric::build_fabric_desired` → returns `DesiredFabric` directly
  (`fabric_from_proto` deleted).
- `desired_from_sync` replaced by `build_desired_set(store, secrets_key,
  node_id) -> Result<Vec<DesiredSandbox>>` in mc2-server (pure function over
  store rows; unit-testable without a hypervisor).

### Crate changes

| Crate | Change |
| --- | --- |
| `proto/` + `mc2-api` | Delete `proto/`, `build.rs`, tonic/prost deps, `pub mod agent`. mc2-api = REST DTOs + stack YAML only. |
| `mc2-runtime` | Add plain observed/ingress types; drop proto imports. |
| `mc2-store` | Schema: drop `join_token_hash` (001), drop `node_token_hash` (002). Trait: delete `verify_join_token`, `join_auth_required`; `upsert_node_join`→`upsert_local_node` (no token); `heartbeat_node`→`touch_node` (no token check). `init_cluster(api_token_hash)` single arg. |
| `mc2-server` | Delete `grpc.rs`, `tls.rs` (gRPC-only). Add `node.rs` (loop) + `desired.rs` (builder) + moved `fabric_serve.rs`, `ssh_serve.rs`, `ingress_files.rs`. `run()` gains node-loop spawn; args gain `--node-name`, `--label`, `--volume-dir`, `--ingress-config-dir`, `--cpus`, `--memory-mib`, `--reconcile-interval-secs`; args lose `--grpc-bind`, `--grpc-plain`. Deps: + microsandbox (ssh feature), + num_cpus; − tonic, rcgen, tokio-stream. |
| `mc2-agent` | **Deleted.** Reconcile body rewritten into `mc2-server/src/node.rs` (same logic, store-direct). |
| `mc2` (binary) | Delete `Agent` subcommand; doctor text updates; keep `server/apply/node/ps/secret/ssh/doctor/version`. |
| `tests` | Harness drops gRPC listener + join_token; gains auto-created local node row (`cluster.local_node_id`). Test rewrites below. |

### Test rewrites (all mechanical)

| File | Fate |
| --- | --- |
| `agent_join.rs` | **Deleted** (join flow gone). |
| `apply_schedule.rs` | Apply → assert instances bound to local node via store/REST; desired-set assertions move to `build_desired_set` unit/integration calls. |
| `reschedule.rs` | Single-node flow: mark node NotReady → reschedule unbinds non-sticky → touch node Ready → scheduler rebinds. Store-driven, no gRPC. |
| `ingress.rs` | Phases set via `store.update_instance_status`; assert `/v1/ingress` + `build_ingress_routes_for_node` output. |
| `secrets.rs` | `build_desired_set` returns injected plaintext for authorized stack. |
| `ssh.rs` | Drop join; REST PUT override unchanged; observed via store write. |
| `volumes.rs` | `build_desired_set` + `volume_mount_plan` assertions. |
| `server_rest.rs` | Replace join-token bearer test with API-auth negative test. |
| `store_persistence.rs` | Drop join-token asserts. |
| `cli_smoke.rs` | Help output: no `agent`; doctor text updated. |

### Docs / tooling

- `README.md`, `docs/guides/quickstart.md`: one-terminal run (`mc2 server`),
  no join token, no `mc2 agent`.
- `justfile`: drop `run-agent`; `run-server` args updated.
- `.github/workflows/*`: verify no two-process assumptions.
- `CHANGELOG.md`: breaking — single process; **delete your data dir**
  (schema edited in place; pre-release).

### Bootstrap / auth

- First run prints **API token only** (no join token). `--no-auth` unchanged.
- `FreshCredentials` loses `join_token`.

## Implementation order

1. `mc2-runtime`: add native observed/ingress types (compile-clean first).
2. `mc2-store`: schema + trait cleanup (both backends: sqlite + memory).
3. `mc2-server`: resolvers → native types; `desired.rs`; delete `grpc.rs`/`tls.rs`; move serve modules; `node.rs`; args; `run()` wiring.
4. Delete `mc2-agent` crate + proto + mc2-api proto surface.
5. `mc2` binary CLI cleanup.
6. Tests: harness then per-file rewrites.
7. Docs, justfile, CHANGELOG.
8. `just check` (fmt + clippy -D warnings + full suite).

## Acceptance criteria

- [x] `cargo build` with no `proto/`, no tonic/prost/rcgen in the tree.
- [x] `mc2 server` alone applies a stack and runs sandboxes (lab).
- [x] `mc2 agent` no longer exists in CLI help or code.
- [x] REST surface (`/v1/*`) behavior identical for operators.
- [x] All integration tests green without a hypervisor (fake node = store writes).
- [x] `just check` green.

## Risks & notes

- **Migration checksums**: editing 001/002 in place invalidates existing
  SQLite data dirs → documented "delete your data dir" in CHANGELOG
  (pre-release, acceptable).
- **Feature unification**: microsandbox `ssh` feature now requested by
  mc2-server; already unified in workspace today via mc2-agent, so no change
  in resolved features.
- **Lost capability**: multi-node. Re-adding later means a transport around
  `build_desired_set` + report writes — the seams are preserved by keeping
  those as pure functions.

## Follow-up (re-scoped after this lands)

- **Host port ownership** (was `docs/tasks/host-port-ownership.md`): with one
  process, the allocator can bind-probe the host directly; persisted
  `port_allocations` table + `PortSpec.host: Option<u16>` still apply, minus
  the `occupied_ports` heartbeat channel (the uncommitted proto field added
  2026-08-06 is deleted by this merge).
- **Traefik multi-backend LB** (`backends[]` per ready replica) — unchanged,
  builds on per-instance ports.

## Handover notes (append as completed)

- **Implemented 2026-08-08.** All 8 steps; single commit on `dev`.
- **Lab evidence** (macOS Apple Silicon): `mc2 server` alone applied
  `01-hello-service` → sandbox Running → `curl :18091` → "hello world".
  SIGKILL of the server left the detached VM serving; restart re-adopted it
  (`runtime reconciled ... Running`, endpoint still answering).
- **Known quirk (pre-existing, unchanged)**: a `Stopped` detached sandbox is
  resumed via `Sandbox::start_detached` which does not re-run the guest
  command — if the VM ever stops, the workload process is gone until MC2
  recreates the sandbox. Health/restart policy covers `Running`-flapping,
  not external stops. Same semantics as the old agent.
- **Lab hygiene**: sandbox names are deterministic (`<stack>-<svc>-<ord>`)
  in the user-global msb namespace — two data dirs share one VM pool. Use
  `cargo run -p mc2-runtime --example msb_rm -- <name>` to clear orphans
  (the host `msb` CLI is v0.2.6 and cannot manage 0.6.8 sandboxes).
- **Tests**: 117 green (`just check`: fmt + clippy -D warnings + suite).
  Harness now auto-creates a local node row (`cluster.local_node_id`);
  `start_with_node(false)` covers no-node/scheduler-pending paths.
