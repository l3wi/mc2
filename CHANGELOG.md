# Changelog

All notable changes to MC2. Pre-release: entries are grouped per feature area.

## Unreleased

### BREAKING: single-process merge (gRPC agent deleted)

- `mc2 server` now hosts the **local node** in-process: scheduler + SQLite +
  REST + the reconcile loop that drives the embedded microsandbox SDK
  (MSB-embedded style — no daemon, per microsandbox's own design).
- **Removed**: the `mc2-agent` crate, the `mc2.agent.v1` proto/gRPC service,
  the join/node-token flow (`mc2jt_` tokens), `--grpc-bind` / `--grpc-plain`
  / `--tls-ca` flags, and the `mc2 agent` CLI subcommand. Pre-release: no
  compatibility shims — **delete your data dir** (schema edited in place).
- The cluster registers one implicit **local node** at server start
  (`--node-name`, `--label KEY=VALUE`, `--cpus`, `--memory-mib`); first
  bootstrap now prints only the **API token**.
- Node loop moves into `mc2-server` (`node.rs`, `desired.rs`,
  `fabric_serve.rs`, `ssh_serve.rs`, `ingress_files.rs`); observed
  ssh/fabric/ingress types are plain serde structs in `mc2-runtime`
  (JSON shape unchanged).
- `--volume-dir` and `--ingress-config-dir` are now `mc2 server` flags.
- Multi-node is deferred; the seams (`build_desired_set` + store report
  writes) stay pure functions so a remote transport can wrap them later.
- OTLP metrics: `mc2.agent.reconciles` / `mc2.agent.reconcile_errors`
  renamed to `mc2.node.*`; the `mc2.agent.heartbeats` counter is gone
  (heartbeats are now store touches, not RPCs).

### Stack YAML defaults (DX)

- `mc2 apply` fills missing boilerplate before sending: `apiVersion`
  (`mc2/v1`), `kind` (`Stack`), and `metadata.name` (sanitized file stem).
  Partial files — even `services:` + `image:` only — now apply. Present values
  are never rewritten; complete files pass through verbatim (comments kept).
  Documented in `examples/README.md`.

### Persistent volumes (v1)

- Stack YAML `volumes:` are now validated: mounts must reference declared
  volumes, `kind: dir` is the only supported kind, volume names must match
  `[a-z0-9][a-z0-9._-]*` (no `--`), mount paths must be absolute and unique
  per service, and stack names must not contain `--`.
- Declared volumes mount into sandboxes as microsandbox named directory
  volumes (`ensure_exists().directory()`), resolved to
  `mc2-<stack>--<volume>` at create time. Data persists across sandbox
  recreate/restart and is retained on service/stack removal.
- New server flag `--volume-dir` (`MC2_VOLUME_DIR`) overrides the named-volume
  root (default `~/.microsandbox/volumes`).
- New example `examples/06-persistent-volumes/` with README and lab runbook.
- Integration tests: `tests/tests/volumes.rs` (apply → desired set → mount plan,
  validation 400s). Scheduler unchanged: existing sticky-volume semantics cover
  node-local placement; instances stay Pending while their node is NotReady.
