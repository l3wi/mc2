# Task: Fabric — shared-backend splices + round-robin replica routing

Status: **IMPLEMENTED — 2026-08-08**
Date: 2026-08-08

## Context

Fabric splices are **per consumer**: every client instance binds its own
`127.0.0.1:<port>` on the shared host. Two consumer instances wanting the same
exposed port collide (bind conflict), which also means scaling a consumer
breaks. The collision is inherent to per-consumer splices, not to scale.

Goal: make the fabric multi-service/multi-replica safe with **one shared
splice per exposed (service, port)**, routing each connection round-robin
across Ready replicas of the backend — no loopback-IP allocation needed.

## Design

### Shared splice registry (`fabric_serve.rs`)

- Replace `splices: HashMap<client_instance, Vec<ActiveSplice>>` with a
  **backend registry**: `service_name → Arc<Mutex<Vec<SocketAddr>>>` of Ready
  backend host publish sockets (rebuilt each reconcile from `publish_index` +
  observed phases).
- One splice task per exposed (service, guest_port), bound to
  `127.0.0.1:<guest_port>`. Per accepted connection it **round-robins** through
  the registry (reading current state per connection → always balances across
  currently-Ready replicas; no splice restart on phase churn).
- Splice lifecycle: created when a service has `expose` and ≥1 bound instance;
  aborted when the service leaves the stack / loses exposes.
- DNS injection unchanged: consumers still get `{gw} <svc>.<stack>.svc.mc2`
  for every mesh edge; they all route to the shared splice via the gateway.
- Per-consumer `FabricObserved.edges` still reported (Ready when a shared
  splice exists for that service+port and a backend is local).

### Routing

- **Default: round-robin over Ready replicas** (per connection). If no Ready
  replica, drop the connection until one appears. No routing knob in this pass.

### Validation (`mc2-api/src/stack.rs`)

- Exposed guest ports must be **distinct across the whole stack** (across
  services) — this is what makes the shared-splice model sound.
- Documented nicety: an exposed guest port must not equal any other service's
  published host port (host `127.0.0.1:<host>` vs splice `127.0.0.1:<guest>`).

## Changes by area

| Area | Change | Size |
| --- | --- | --- |
| `mc2-server/fabric_serve.rs` | shared backend registry + round-robin splice task; drop per-consumer splice map | ~140 lines |
| `mc2-server/fabric.rs` | mesh `allows` now drive DNS/status only (backend pinning moves to routing) | ~15 lines |
| `mc2-api/stack.rs` | distinct-exposed-ports validation | ~15 lines |
| Tests | round-robin selector unit, registry rebuild unit, distinct-ports validation | ~70 lines |
| Docs | stack-yaml, CHANGELOG | small |

## Out of scope

- Loopback-IP per-service allocation (superseded by shared splices).
- Routing knob (`fabric.routing`) — round-robin only for now.
- Cross-node fabric (still unsupported).

## Acceptance criteria

- [ ] Two consumer services reaching the same exposed port both work (no bind conflict).
- [ ] `scale` on a consumer or producer works; east-west traffic round-robins across Ready replicas.
- [ ] Exposed ports must be distinct within a stack (validation + docs).
- [ ] `just check` green.

## Handover notes (append as completed)

- (pending)

## Implementation notes (2026-08-08)

- **`fabric_serve.rs`**: `FabricTable.splices` is now `HashMap<(service, port),
  JoinHandle>` (one shared splice per exposed port) plus a `BackendRegistry`
  (`Arc<Mutex<HashMap<(service, port), Vec<SocketAddr>>>>`) rebuilt each
  reconcile from `publish_index` + Running phases. `reconcile_splices(desired,
  reports)` aborts stale splices, starts missing ones (bind failure →
  `failed_splices`), and swaps the registry. `splice_loop` reads the registry
  per connection and round-robins (`AtomicUsize`).
- **`node.rs`**: `fabric_table.reconcile_splices(desired.as_slice(),
  &reports)` called after the per-instance loop, before `close_missing`.
- **`fabric.rs`**: unchanged — mesh edges still drive DNS + status; routing is
  handled at the splice, not by backend pinning.
- **Validation (`stack.rs`)**: exposed guest ports must be unique across the
  stack.
- **Tests**: `shared_splice_round_robins_across_ready_replicas` (two echo
  backends, verifies both receive traffic via an ephemeral guest port) and
  `no_splice_when_no_ready_backend`.
- Docs: stack-yaml fabric section, CHANGELOG. `just check` green.
