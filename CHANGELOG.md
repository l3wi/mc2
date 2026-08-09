# Changelog

All notable changes to MC2. Pre-release: entries are grouped per feature area.

## Unreleased

### CLI surface cleanup

- **Grouped top-level help.** `mc2 --help` (and bare `mc2`) now renders
  commands under `Stacks` / `Observe` / `Access` / `Security` / `Admin`
  headings (clap has no native subcommand grouping; a custom renderer splices
  clap's default help, the same approach the msb CLI uses). Subcommand help is
  unchanged.
- **Connection flags are global.** `--api` / `--token` / `--context` /
  `--allow-insecure-http` move off every subcommand and onto the top-level
  command (accepted before or after the subcommand). Resolution order and the
  `MC2_API` / `MC2_API_KEY` / `MC2_CONTEXT` env vars are unchanged.
- **`mc2 ssh` keys are flat.** `mc2 ssh key add|show|ls|rm` → `mc2 ssh add-key|
  show-key|keys|rm-key`.
- **`rm` folds into `down`.** `mc2 down <stack> [--volumes]` (docker-compose
  parity); `rm` remains a hidden alias. `mc2 context set <name> --api <url>
  [--token <key>]` is unchanged — the global flags are the source of the URL/key.

## 0.1.0

### Simpler SSH (`ssh: true`) + `mc2 up` prints SSH endpoints

- **`ssh:` accepts a boolean flag**: `ssh: true` enables the host-side SSH
  front end with all defaults and authenticates with **every registered key**
  (`mc2 ssh key add …`); the long map form still works and now treats an empty
  `authorizedKeys` as "all registered keys" (fails closed if none).
- **`mc2 up` prints declared SSH endpoints** after the apply result: per
  service the listener `bind:port` (or `auto` until reconcile) and the
  `ingress.tcp` entrypoint when a TCP route targets the service.

### `mc2 network` + fabric→network rename

- **`mc2 network`** replaces `mc2 fabric` and adds network summaries:
  - no args → table of every network (default + named) with stacks, services, instance counts.
  - `mc2 network <name>` → that network's member instances and their expose + host ports.
  - `mc2 network <stack>/<service>/<ordinal>` → one instance's observed exposes/edges.
  - Backed by `GET /v1/networks` (membership) and `GET /v1/instances/{id}/network`.
- **"fabric" is gone from the language**: `DesiredFabric`→`DesiredNetwork`,
  `FabricTable`→`NetworkTable`, `build_network_desired`, `instance_fabric`
  table→`instance_network` (migration 008), `/v1/instances/{id}/network`,
  `mc2 network`; docs/examples updated (`03-service-fabric`→`03-networks`,
  `smoke-fabric`→`smoke-networks`). The east–west layer is now just
  "networks": `expose` listeners + default-allow + `svc.<network>.svc.mc2` DNS.

### Compose parity round 2 — `environment`, string `command`, full `healthcheck`, `depends_on`, per-replica ports

- **`environment:` replaces `env:`** as the guest env key (map or `KEY=VALUE`
  list). The old `env:` key is rejected by the canonical parser. An explicit
  `environment:` entry overrides a colliding `secrets[].env` (env wins; the
  secret is dropped, not decrypted).
- **`command` string form**: `command: 'echo "hi there"'` is split shell-like
  (quotes + backslash escapes) into argv; list form still works.
- **`healthcheck` full field set**: `timeout`, `retries` (default 3),
  `start_period`, `disable` — alongside `test` and `interval`. The node runs
  probes with a per-probe timeout, ignores failures during `start_period`, and
  only marks the service unhealthy after `retries` consecutive failures.
- **`depends_on`** startup ordering: list form (`[db]`) or map form
  (`{db: {condition: service_healthy}}`). `service_healthy` waits until the
  dependency's healthcheck passes (new persisted `instances.healthy` signal).
  Cross-stack refs, unknown conditions, and cycles are rejected at parse time.
- **`scale` + published ports**: per-replica host-port allocation — a fixed
  `published: P` becomes the block `P, P+1, …, P+N-1`; target-only ports get a
  distinct auto port per replica. Stable across re-applies.
- Cross-service published-host-port conflicts are now rejected at apply (400);
  published host ports are effectively unique server-wide.
- Docs: new [Environment variables vs secrets](docs/guides/secrets.md) guide
  clarifies the direct-`environment` vs host-gated `secrets[].env` mechanisms
  and the server-wide (not per-stack) scoping of the secret store.

### `mc2 exec` + `mc2 logs` (see inside VMs)

- **`mc2 exec <instance> <cmd…>`** runs a command inside the instance's
  sandbox: captures stdout/stderr and mirrors them, then exits with the
  command's exit code. `POST /v1/instances/{id}/exec`.
- **`mc2 logs <instance> [--tail N]`** prints recent sandbox logs
  (runtime/exec/kernel) via the SDK's log registry (`read_logs`).
  `GET /v1/instances/{id}/logs`.
- Instances are addressed by UUID or `<stack>/<service>/<ordinal>`; the
  runtime is now created once in `mc2-server::run` and shared between the node
  loop and the REST handlers.
- `NodeRuntime` gained `exec_with_output` returning `{exit_code, stdout,
  stderr}` (health `exec_command` delegates to it).
- **`mc2 logs --follow`** streams new log entries as they arrive (SSE:
  `GET /v1/instances/{id}/logs?follow=true`; `--tail N --follow` shows the
  last N then continues from that cursor).
- **`mc2 exec` stdin**: piped stdin is forwarded to the sandbox
  (`echo hi | mc2 exec demo/web/0 cat`); interactive TTY stdin is untouched.
  `exec_with_output` feeds stdin via the SDK's `stdin_bytes`.

### Compose-faithful networking (ports north-south, expose listeners, server-wide networks)

- **`ports` = north-south**, compose-faithful: `"5000:3001"`, target-only
  `"3001"` (→ **auto host port**, resolved at apply and persisted), and a
  **hostname sugar** `"mcp.example.com:3000"` (→ TLS ingress route for that
  hostname, `le` resolver). `ingress` backends resolve auto-allocated host
  ports from the stored spec.
- **`expose` = internal listeners**, compose list form (`expose: [5432]` /
  `["5432"]`) or map form — drives the fabric dataplane.
- **Server-wide named networks**: a service's `networks: [name]` joins a
  server-wide network (no top-level declaration needed). Services in different
  stacks on the same named network reach each other (default-allow) via
  `svc.<network>.svc.mc2` DNS. The stack's implicit default network keeps
  `svc.<stack>.svc.mc2`.
- Fabric edges are network-scoped (cross-stack peers on a shared network are
  reachable); splices stay shared per (service, port) with round-robin.
- Documented limitation: exposed guest ports are effectively unique
  server-wide (L4 splice on the shared loopback — same-port across different
  networks can't be isolated, unlike Docker's kernel bridges).

### Compose-style stack config (canonical parser, k8s wrapper removed)

- Stack YAML is now **Docker Compose-shaped** with a **canonical parser**:
  `#[serde(deny_unknown_fields)]` on the schema, so any unrecognized key is
  rejected (`unknown field …`) — no compatibility layer for the old k8s form.
- Top-level `name:` replaces `apiVersion`/`kind`/`metadata.name` (filled from
  the file stem when missing).
- Field renames: `replicas`→`scale`, `restartPolicy`→`restart`
  (`no|on-failure|always|unless-stopped`), `resources`→`cpus`+`mem_limit`
  (accepts `512m`/`1g`/bytes), `health`→`healthcheck` (compose `test`/`interval`
  with `CMD`/`CMD-SHELL` stripping), `volumes[].mount`→`target`.
- `ports` accepts compose syntax: `"18080:8000"`, `"ip:18080:8000"`,
  `"18080:8000/tcp"`, and long `{target, published, protocol}`. Target-only
  (auto host port) is rejected with a clear message for now. Ports always bind
  loopback.
- `env` accepts a map or a `KEY=VALUE` list. Stack `metadata.labels` dropped.
- Old k8s-style keys (`apiVersion`, `kind`, `metadata`, `replicas`,
  `resources`, `restartPolicy`, `health`, `allow`, `mount`, `host`/`guest`
  ports) are **rejected**, not silently ignored.
- Stored `spec_json` now uses the compose field names; pre-release, delete the
  data dir (schema edited in place).

### Fabric shared-backend splices (full-mesh round-robin)

- The service fabric is now a **full mesh within the stack** (default allow):
  every service reaches every `expose`d port of every peer service with no
  declaration required — the L4 splice + `*.svc.mc2` DNS stay, only the policy
  changed from default-deny to default-allow.
- **Removed** the per-service `allow:` field (hard cut, docker-pure); the
  fabric no longer needs explicit client edges. `expose` remains the
  reachability gate (a service with no `expose` is reachable by nothing).
- Fabric splices are now **shared per exposed (service, port)** (was: one per
  consumer), so multiple consumers and scaled clients no longer collide on the
  host loopback. Each connection **round-robins across Ready replicas**.
- Scheduler fabric affinity now co-locates with exposing peers (cross-node
  splices remain unsupported).
- Exposed guest ports are **effectively unique server-wide**: each stack
  validates uniqueness at parse time, but the shared loopback splices make a
  port already bound by another stack or network report `Failed` (second
  edge). Same-port isolation across networks isn't possible (no kernel
  bridges).

### Compose-language CLI (hard cut from kubectl verbs)

- **`mc2 apply` → `mc2 up`** (no alias): publish desired state and converge —
  idempotent, same semantics.
- **`mc2 down <stack>`** (new): tear down a stack — deletes the definition and
  its instances (the node loop's GC removes the sandboxes); named volumes are
  retained (Compose semantics).
- **`mc2 rm <stack> [--volumes]`** (new): `down` plus optional named-volume
  deletion (`DELETE /v1/stacks/{name}?volumes=true`; volume root = `--volume-dir`
  or the msb default).
- **`mc2 config -f stack.yaml`** (new): validate and print the normalized
  stack config (what `mc2 up` would send) — the `compose config` equivalent.
- Server-side: `DELETE /v1/stacks/{name}` handler; store `delete_stack`
  (instances first, cascades ssh/fabric); `AppState.volume_dir` for volume
  cleanup. Internal REST stays `/v1/stacks:apply`.

### Interactive setup wizard (`mc2 setup`)

- New interactive `mc2 setup` wizard with two trees: **Server** (run on the
  VPS) and **Client** (run locally). `mc2 setup server` / `mc2 setup client`
  jump straight to a tree.
- **Server tree** collects the server options step by step (bind, data dir,
  API key on/off, public hostname, cert resolver, ingress dir), writes a
  ready-to-run default `traefik.static.yml` once (never clobbered), renders
  the runnable `mc2 server …` command, and prints a numbered finish-setup
  checklist (bootstrap token → start Traefik → DNS/ports → run the client
  tree locally).
- **Client tree** takes a context name, URL and API key (masked), saves and
  activates the context, and optionally verifies the live connection.
- Prompts use `dialoguer` (arrow-key menus); non-TTY stdin fails with a
  friendly "run it in a terminal" error instead of hanging.
- Wizard only scaffolds config and prints instructions — it never starts the
  server or Traefik (BYO Traefik, one-process philosophy).

### Local/remote client modes (context-aware CLI + self-ingress)

- New `mc2 context set|use|ls` manage `~/.mc2/config.toml` (mode 0600):
  named contexts with a URL and optional API key.
- Connection resolution precedence: `--api`/`--token` flags > `MC2_API`/
  `MC2_API_KEY` env > `--context`/`MC2_CONTEXT` > the `current` context >
  built-in loopback default. Mode (`local`/`remote`) is derived from the
  effective URL host.
- Safety rail: a remote (non-loopback) `http://` endpoint is rejected unless
  `--allow-insecure-http` / `MC2_ALLOW_INSECURE_HTTP=1` opts in (plaintext
  bearer over the network is the one footgun this feature can create).
- `mc2 status` is mode-aware: prints `local:`/`remote:` + effective URL +
  context + auth presence; `-o json` adds `mode`, `context`, `auth`, and
  `publicHostname`.
- Server flag `--public-hostname` (+ `--public-tls-cert-resolver`, default
  `le`) emits a synthetic control-plane route into the Traefik ingress
  catalog (TLS via the cert resolver, backend = the REST listener itself);
  `/v1/status` returns `publicHostname`.
- New store table `settings` (key/value) for server-level config (e.g.
  `public_hostname`); migration `006_settings.sql`.

### First-class CLI (full API parity)

- New commands covering the whole REST surface: `mc2 status` (health +
  counts), `mc2 fabric <instance>` (observed expose/allow status),
  `mc2 ingress` (desired route table), `mc2 ssh key show <name>`.
- `-o json|table` on all listing commands (`ps`, `node ls`, `secret ls`,
  `ingress`, `fabric`, `ssh ls`, `ssh key ls`) for scriptable output.
- `mc2 ps --stack/--service` client-side filters.
- Instance addressing: every instance command accepts
  `<stack>/<service>/<ordinal>` (e.g. `mc2 fabric demo/web/0`) in addition
  to the raw UUID.
- Unified API error shape: non-2xx responses surface the server's `error`
  field (`apply failed: 400 Bad Request: ...`) instead of raw JSON.
- `mc2 completions bash|zsh|fish` (clap_complete).

### Distribution (dist)

- Repository is now public at https://github.com/l3wi/mc2; release
  pipeline replaced with **dist** (cargo-dist 0.32): tag push builds
  `linux-amd64`, `linux-arm64`, `darwin-arm64` on native runners (no
  cross-compilation), ships checksummed `.tar.xz` archives and a
  `mc2-installer.sh` (rustup-style), and generates release notes from
  CHANGELOG.md.

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
- Positioning is now single-machine YAML orchestration for microsandbox;
  the earlier multi-node direction is dropped (not deferred).
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
