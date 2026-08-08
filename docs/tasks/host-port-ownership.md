# Task: MC2-owned host ports (persisted, sequential, BYO override)

Status: **APPROVED — implementing 2026-08-07**
Date: 2026-08-07

## Context

`services.<svc>.ports[].host` was a required, user-picked host port. That
breaks three ways: replicas collide on one node; the control plane cannot
know what the host already uses (lab hit Docker owning 18080); ingress
validation needed fixed ports just to name backends.

Decision (user-approved): **MC2 owns host ports.** Omit `host:` → MC2
allocates sequentially from a base (default 6100), **persists the allocation
per instance in SQLite**, and keeps it static until the instance is removed —
re-applying the stack reuses the same ports. Explicit `host:` is a BYO
override and fails closed on bind collision.

## Design (approved)

### Allocation is server-side and persisted

The allocation must survive agent restarts and re-applies, so the control
plane owns it — SQLite table keyed by instance:

```sql
port_allocations (
  instance_id TEXT, guest_port INTEGER, protocol TEXT,
  host_port INTEGER, bind TEXT, node_id TEXT,
  PRIMARY KEY (instance_id, guest_port, protocol)
)
```

Allocations happen **at schedule time** (when the instance's node is known —
ports are per-node):

```text
allocate(instance, node):
  existing row for this instance+guest port?      → reuse (static until removed)
  else scan host_port from --port-base (default 6100):
    skip: rows allocated to other instances on this node
    skip: explicit host: overrides on this node
    skip: ports the node reported occupied (heartbeat)
    first candidate wins → INSERT
  exhausted base..base+10000 → instance Failed: "no free host port"
```

The resolved port is written into the instance's `spec_json` (the runtime
contract the agent receives); the user-facing stack YAML is never rewritten.
`spec_hash` includes ports, so a changed allocation forces recreate — and
since allocations are stable, normal reconciles never churn.

### The "check the host" step

The server cannot probe node ports, so the host check travels the existing
report channel: each `HeartbeatRequest` carries `occupied_ports` (the agent's
live binds — sandbox publishes, fabric splices, ssh listeners). The allocator
skips them. External squatters between allocation and bind → msb create fails
→ instance `Failed` (fail closed, same as overrides).

### Lifecycle

- **Re-apply**: instance rows survive `reconcile_service_replicas`; their
  allocation rows survive; ports come back identical.
- **Removal**: instance row deleted ⇒ allocation rows deleted ⇒ port free.
- **Reschedule to another node** (non-sticky, node loss): allocation is
  re-issued on the new node (ports are node-local; this is the one case where
  a port can move).
- **Agent restart**: allocations live server-side; nothing to re-probe.

### Schema & CLI

```yaml
ports:
  - guest: 8000            # omit host → MC2 allocates (persisted)
  - guest: 9090
    host: 19090            # BYO override; fails closed if busy
```

`mc2 ps` / `/v1/instances` show `8000→127.0.0.1:6100` mappings. Ingress
validation drops the fixed-host requirement; catalog names backends by the
allocated port from `spec_json`.

## Changes by crate

| Crate | Change |
| --- | --- |
| `proto` | `HeartbeatRequest.occupied_ports` |
| `mc2-api` | `PortSpec.host: Option<u16>`; explicit must be non-zero; ingress backend needs only `guest` + loopback bind |
| `mc2-store` | migration `006_port_allocations.sql`; alloc CRUD; node occupied-ports; `update_instance_spec` |
| `mc2-server` | `--port-base` flag; allocator invoked at bind time and on re-apply for bound instances; heartbeat stores occupied ports; ingress route host from `spec_json` |
| `mc2-agent` | report `occupied_ports` on heartbeat |
| `mc2` CLI | `ps` prints guest→host mapping |

## Tests

- **Unit**: allocator (sequential from base; skips db/occupied/overrides;
  reuse; exhaustion error), schema (omitted host OK, explicit zero rejected,
  ingress without host OK), `spec_hash` stable under identical allocation.
- **Integration**: apply with omitted host → scheduled instance `spec_json`
  carries port ≥ base; re-apply → same port; explicit override preserved;
  heartbeat occupied ports shift the next allocation.
- **Lab**: replicas get consecutive ports; agent restart keeps ports;
  explicit port on busy port → `Failed`; `mc2 ps` shows mapping.

## Acceptance criteria

- [ ] Omitted `host:` allocates ≥ `--port-base`, sequential, persisted.
- [ ] Re-apply of the same stack returns the same ports.
- [ ] Removal frees the port for the next allocation.
- [ ] Explicit override preserved; bind collision ⇒ `Failed`.
- [ ] `/v1/instances` + `mc2 ps` show guest→host mapping.
- [ ] Existing tests green; `just check` green.

## Deferred

- Multi-backend ingress routes (`backends[]` + Traefik `servers:` list) — the
  replica/LB payoff builds on this task's per-instance ports.
- Cross-node load balancing.

## Handover notes (append as completed)

- (pending)
