# Changelog

All notable changes to MC2. Pre-release: entries are grouped per feature area.

## Unreleased

### CLI, docs and boundary cleanup (Wave 6)

- **SSH refs.** `mc2 ssh open|show|close` accept `stack/service/ordinal`;
  `mc2 ssh ls` shows a copyable `REF` and the full instance id.
- **CLI robustness.** Path segments are percent-encoded from UTF-8;
  `127.0.0.0/8`, `::1` and `localhost` count as loopback; every request path
  checks the HTTP status and shows the server's error (a 401 reads
  `unauthorized: missing or invalid bearer token`). `mc2 up` no longer prints
  possibly-stale disk conditions (see `mc2 ps` NOTES).
- **Setup wizard** ends with `mc2 up -f <stack.yaml>` and renders the chosen
  Traefik resolver name (validated) instead of a hard-coded `le`.
- **Runtime boundary.** The server no longer depends on the microsandbox SDK
  directly: logs, guest `/etc/hosts` injection and SSH go through
  `NodeRuntime`.
- **Disk-full hint** suggests twice the *declared* root-disk size.
- **Docs.** Contexts file reference matches the real format; quickstart and
  README work after a binary-only install; `up --help` no longer mentions
  `apiVersion`/`kind`; stale agent/gRPC and default-deny wording removed;
  superseded design decisions marked. Tautological tests removed; the
  no-token smoke test now asserts against a real listener.

### Runtime correctness (Wave 5)

- **Ingress follows the live replica.** One selector picks a Running
  (healthy-first) or lowest-ordinal replica and uses *that* replica's host
  port for hostname, HTTP and TCP routes. The catalog is rewritten only when
  routes change.
- **Capacity for restartable instances.** Failed/Stopped instances that will
  restart keep their CPU/RAM reserved; only `restart: no` releases it.
- **Crash loops back off.** Restarts microsandbox performs itself are reported
  to the controller and spaced by the restart backoff.
- **`depends_on` gates startup only.** A running dependent stays Running and
  routed when its dependency later degrades; service liveness is "any replica
  running / healthy", independent of processing order.
- **Supervised server.** The reconcile loop and REST listener are supervised:
  if one dies the process exits non-zero. `/health` returns 503 with a reason
  when the loop is dead or stale (> 3 × reconcile interval + 30 s). Metrics are
  restarted with backoff.
- **Bounded shutdown.** One cancellation token stops the reconcile loop, splice
  and SSH listeners and REST; shutdown is capped at 10 s even with open log
  streams. VMs keep running and are adopted on the next start.

### VM lifecycle & atomic apply (Wave 4 — breaking, pre-release)

- **`mc2 up` is all-or-nothing.** Apply plans the whole stack (validation,
  capacity, ports, claims) with no writes, then commits the stack and every
  instance change in one transaction. A rejected apply leaves the running
  stack untouched; services removed from the YAML, and ordinals beyond a
  lower `scale`, are deleted. `mc2 down` is transactional too.
- **Single embedded node.** The NotReady watcher, unbind/reschedule loop,
  heartbeat staleness, `--heartbeat-grace-secs` and `--reschedule-interval-secs`
  are gone. The node id is persisted, so `--node-name`/hostname changes keep
  placements; the node is registered before the API accepts requests.
- **VMs survive restarts correctly.** Every sandbox carries an install-id
  label. On startup MC2 adopts its own sandboxes and removes labelled orphans
  (foreign sandboxes are never touched); a failed removal is retried every
  pass. The applied config hash is persisted (migration 009), so a stale VM is
  recreated after a restart and a failed recreate never records the new config.
- **Isolation of bad instances.** A missing secret or SSH resolution error
  fails only that instance (it keeps its sandbox) instead of halting the node.
- **Status writes are checked.** A failed observed-state write fails the
  reconcile pass and is logged, rather than being silently dropped. The
  reconcile loop is split into lifecycle, report and GC modules.

### Correctness fixes (Wave 3 — breaking, pre-release)

- **Healthchecks.** `CMD-SHELL` runs through `/bin/sh -c`; `["NONE"]`
  disables; `disable: true` no longer makes the stored spec unreadable (which
  used to stall every reconcile pass). `timeout` defaults to 30 s and `0` is
  rejected; sub-second values round up. Probes run out of band, so a hung
  probe can't freeze the node; failures during `start_period` don't count.
- **Capacity.** Reapplying a stack no longer counts its own CPU/RAM twice.
  Reservations use the vCPUs the VM actually gets.
- **Ports.** A published port reused by another target is a 400; port-range
  overflow and auto-pool exhaustion are 400 (were 500). Long-form `ports`
  entries reject unknown keys; `cpus` must be finite and in (0, 255].
- **`mc2 exec`.** Everything after the instance ref is guest argv
  (`mc2 exec ref /bin/sh -c '…'` works without `--`); stdin, stdout and stderr
  are carried as bytes (base64 on the wire), so binary data round-trips.
- **Secrets key guard.** A missing key with stored secrets, or a key that can't
  decrypt them, refuses to start instead of silently re-keying.
  `mc2 server secrets purge --data-dir <dir> [--yes]` is the recovery path when
  the key is truly lost.

### Security & isolation hardening (Epic A — breaking, pre-release)

- **Auth is router-wide.** Every `/v1` route sits behind one auth middleware
  (only `/health` is public); `GET /v1/volumes` no longer answers without a
  token.
- **`--no-auth` is per start, loopback only.** Bootstrap always creates a
  token; `--no-auth` skips the check for that process only. A non-loopback
  `--bind` additionally needs `--allow-unauthenticated-remote`; `--no-auth`
  with `--public-hostname` always refuses to start.
- **Token recovery.** `mc2 server token rotate [--data-dir]` replaces the API
  token in place (works while the server runs; the old token is rejected
  immediately).
- **Credential files.** `secrets.key` and `~/.mc2/config.toml` are created
  0600; a world-readable or symlinked key refuses to start. Secrets are
  encrypted with their name as AEAD associated data. Without a TTY the first
  token goes to `<data-dir>/bootstrap-token` (0600), never to logs. `--help`
  no longer echoes `MC2_API_KEY` / `MC2_SECRET_VALUE` values.
- **Names.** Stack, service and network names are lowercase
  `[a-z0-9]([a-z0-9_-]*[a-z0-9])?` without `--` (stack ≤ 40, others ≤ 63);
  sandboxes are named `{stack}--{service}--{ordinal}`, so names and volume
  dirs can no longer collide across stacks.
- **Ingress.** Hosts, paths, `certResolver`, TCP `entryPoint`/`name`,
  `ports[].hostname` and `--public-hostname`/`--public-tls-cert-resolver` are
  strictly validated; the Traefik dynamic config is serialized from typed
  structs (no YAML injection); colliding router keys are rejected.
- **Network profiles** are a closed set (`public|private|host|none`, `none`
  alone); unknown values and the old `local`/`any` aliases are rejected and
  never fall back to public egress. `host` needs `--allow-host-profile`.
- **Exclusive `expose` ports.** An exposed port is claimed server-wide at
  `mc2 up`; a second claim is a 400 naming the owner. `mc2 network` lists
  claims. Applies and deletes are serialized.
- **Egress hardening.** `expose` ports that collide with the REST bind, SSH
  listeners, published ports, `53`, a port already listening on host
  loopback, or (Linux, non-root) a privileged port are rejected at apply.
  Splice listeners are held from apply until the stack is removed, and a VM
  whose allowed port has no MC2 listener is not created (fail closed).
- **Service network fixes.** Services without `expose` now reach their peers;
  replicas reach their own service name. The recreate hash covers only the
  effective policy (allowed ports + exposes), so neighbours changing no longer
  recreate consumer VMs, and the plan is deterministic.
- **Disk limits.** `volumes.<name>.size` (default 10 GiB) is enforced as a
  write quota (guest sees ENOSPC, `df` shows the cap) and is resizable with
  data kept; shrinking below usage is a 400. `services.<name>.storage_opt.size`
  (default 4 GiB) sets the VM root disk. Volumes are MC2-owned directories
  under `~/.mc2/volumes`. `--limit-disk-mib` is now an apply-time reservation
  (volume sizes + root disks × replicas) with a breakdown on refusal.
- **Disk-full is visible.** `mc2 ps` gains a `NOTES` column; `mc2 exec`,
  `mc2 ssh open`, `mc2 logs` and `mc2 up` print the condition with the exact
  stack.yaml fix. Server logs condition changes and exports
  `mc2.instance.disk_{used,limit}_mib`.
- **Listener limits.** REST: connection cap with 503 load-shedding
  (`--max-connections`, 256), header-read deadline, per-request timeout for
  ordinary routes (`--request-timeout-secs`, 60; `exec`/`logs` exempt), SSE
  keepalive, 16 MiB `exec` body limit, bounded graceful drain. SSH: global and
  per-listener session caps (`--max-ssh-sessions`,
  `--max-ssh-sessions-per-listener`), 30 s handshake deadline, TCP keepalive,
  no idle timeout.
- **Release pipeline.** Automated tags are pushed with a GitHub App token so
  they trigger releases (secrets `RELEASE_APP_ID`, `RELEASE_APP_PRIVATE_KEY`);
  all actions are SHA-pinned; cargo-dist is installed from a checksum-verified
  archive; workflows default to read-only permissions; releases carry GitHub
  artifact attestations. README install uses a version-pinned, checksum-verified
  archive.
- **CLI tables** end with a newline.

### Resource limits (opt-in; unlimited by default)

- **Configurable cluster budgets.** `--limit-cpus`, `--limit-memory-mib`,
  `--limit-disk-mib` (env `MC2_LIMIT_*`), default `0` = unlimited. `mc2 up`
  refuses an apply that would exceed the reserved CPU/RAM budget or the disk
  reservation (see *Disk limits* above).
- **Node capacity is host-derived.** The `--cpus` / `--memory-mib` flags are
  gone; each node now advertises the host's real CPU/RAM (detected via
  `num_cpus` and `/proc`/`sysctl`). Apply is also refused when a stack would
  exceed that capacity, so a stack that can't be placed is rejected up front
  instead of sitting `Pending`. `--limit-*` remains the operator's budget knob.
- **Resource visibility.** `mc2 status` and `/v1/status` report host CPU/RAM/disk,
  the configured limits, MC2's reserved CPU/RAM from instance specs, and MC2's
  measured disk usage.

### CLI + fixes

- **`mc2 volume ls`.** Lists the named volumes retained on the node (stack,
  volume, size MiB, path) via new `GET /v1/volumes`. Volumes are
  filesystem-only in v1, so this is the way to verify what a destructive
  `down --volumes` would remove; `-o json` mirrors the API.
- **Bordered tables.** `mc2 ps` / `node ls` / `network` / `ingress` /
  `secret ls` / `ssh keys` / `ssh endpoints` / `context ls` and the `status`
  resources block now render as bordered tables with headers via a shared
  `crate::table` wrapper over **comfy-table** (the most-downloaded Rust table
  library), replacing hand-formatted `{:<N}` strings.
- **Fix: `mem_limit` JSON round-trip.** `ServiceSpec` now serializes
  `mem_limit` as bytes (matching compose semantics), so a stored/re-read spec
  keeps the same MiB value (512 MiB no longer comes back as 1 MiB). This also
  corrects reserved-memory accounting in the scheduler and the new resource
  limits.

### Rust best-practice cleanup (internal, no behavior change)

- **Tooling baseline.** `cargo doc` is now warning-clean and enforced in CI
  (`RUSTDOCFLAGS=-D warnings`); `[workspace.lints.rust] unsafe_code = "deny"`
  codifies the zero-`unsafe` posture; `cargo clippy` runs with `--all-features`;
  CI gains `cargo machete` plus non-blocking `cargo-deny` / `cargo-audit` jobs.
  `just check` covers fmt + lint + test + doc + machete.
- **Unused deps removed** (`cargo machete`-verified): `tracing` in `mc2`,
  `thiserror` in `mc2-runtime`, `tracing` in `mc2-tests`.
- **CLI split into modules.** `mc2/src/main.rs` (2123 LOC) becomes a thin
  dispatcher; clap definitions live in `cli.rs`, REST plumbing in `client.rs`,
  the grouped-help renderer in `help.rs`, and handlers in `cmd/` grouped along
  the help surfaces (stacks / observe / access / security).
- **REST split into `api/` with typed errors.** `http.rs` (1328 LOC) becomes
  `api/` (meta / stacks / instances / secrets / ssh / ingress) with a shared
  `ApiError`; `is_stack_client_error` string-matching is replaced by a typed
  `ApplyError::{Validation,Allocation,Other}` (user errors → 400, store errors
  → 500), `SecretError` for secrets, and `StoreError::InvalidArgument` for
  invalid SSH public keys.
- **Stack schema split into `stack/`.** `stack.rs` (1698 LOC) becomes
  `schema.rs` (types) + `decode.rs` (compose-value deserializers) + `validate.rs`.
- **Node loop tightened.** `reconcile()` drops its 10-arg signature (bundled
  into `NodeRuntimeState`) and the nested health-probe block moves to
  `node/health.rs`.
- **Dead code removed**: `network_expose_guest_ports`,
  `Store::list_instance_network`, `make_route_id`, and a vestigial
  `let _ = key_names;`.
- **Typed phases at boundaries.** Scheduler / reschedule / node reconcile use
  `InstancePhase` / `NodeStatus` / `SandboxPhase` enums instead of scattered
  string literals (persistence stays string-based — see ADR-0002).
- **Network/SSH phase enums.** `NetworkPhase` (Pending/Ready/Failed/Mixed) and
  `SshPhase` (Closed/Opening/Open/Failed) now back the observed status
  constructions and comparisons in the node loop, network dataplane, and SSH
  serve layers.
- **License + advisory gates are blocking.** `cargo deny check` and
  `cargo audit` are enforced in CI (were non-blocking); the deny/audit config
  documents the one known exception (RUSTSEC-2023-0071, `rsa` via the embedded
  microsandbox SDK — no safe upgrade exists).
- **Automated releases (ADR-0003).** release-plz on `main` bumps the version
  from conventional commits (feature batches → minor, hotfixes → patch) and
  creates `vX.Y.Z` tags; cargo-dist builds the GitHub Release. Nightly
  prereleases (`vX.Y.Z-dev.<date>`) are cut from `dev` via cargo-dist. PRs into
  `dev`/`main` must update `CHANGELOG.md`.

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
- Docs: new [Environment variables vs secrets](site/content/documentation/concepts/secrets.mdx) page
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
