# Task: Compose parity round 2 — `environment`, string `command`, full `healthcheck`, `depends_on`, per-replica published ports

Status: **IMPLEMENTED — 2026-08-09**
Date: 2026-08-09

## Context

The first compose-faithful pass delivered the canonical schema + CLI. This pass
closes the closest compose-vocabulary gaps and unblocks real multi-service
graphs on one host:

- `environment` (was `env`) — compose vocabulary.
- `command` string form — compose accepts a string that it splits shell-like.
- `healthcheck` full field set — `timeout` / `retries` / `start_period` /
  `disable` (was only `test` + `interval`).
- `depends_on` — compose startup ordering with `service_started` and
  `service_healthy` conditions.
- `scale` + published ports — per-replica host-port allocation so `scale > 1`
  no longer collides on a single published port.

## Design

### Schema (`mc2-api/src/stack.rs`)

- `ServiceSpec.env` renamed to the YAML/JSON key **`environment`** (map or
  `KEY=VALUE` list). The internal Rust field stays `env` to avoid churn in the
  runtime consumers. Old `env:` key is rejected by the canonical parser.
- `command` accepts a string (split by `split_command_string`, shell-like
  single/double quotes + backslash escapes, not run via a shell) or a list.
- `healthcheck` gains `timeout` (duration, `0` = none), `retries` (default 3),
  `start_period` (duration, default 0), `disable`. New `de_duration` parser.
- `depends_on` accepts list form (`[db]` → `service_started`) and map form
  (`{db: {condition: service_healthy}}`). Validation: refs must name services
  in the same stack; conditions limited to `service_started` | `service_healthy`
  (`service_completed_successfully` rejected — VMs don't complete);
  `service_healthy` requires the dependency to declare a healthcheck; DFS cycle
  detection.
- `command`/`depends_on`/`environment` are excluded from `desired_recreate_hash`
  where appropriate (orchestration/runtime, not create-time config).

### Store (`mc2-store`)

- Migration `007_healthy.sql`: `instances.healthy` (default 0).
- `InstanceRecord.healthy`; `update_instance_health`; `reconcile_service_replicas_multi`
  (per-ordinal spec JSON) — single-spec variant delegates to it.

### Runtime (`mc2-server/src/node.rs`)

- Health state machine: `interval` (unchanged), `timeout` (tokio timeout around
  the exec probe), `retries` consecutive failures before marking unhealthy
  (default 3), `start_period` grace during which failures are ignored,
  `disable` skips probing entirely. Passing probes set `state.health_ok`,
  persisted to the store each cycle.
- `depends_on` startup gate: a per-(stack,service) liveness map is seeded from
  the store and updated live as the cycle reports phases/health. An instance
  whose dependencies are not Running (`service_started`) or Running+healthy
  (`service_healthy`) reports `Pending` with `depends_on: waiting for …` and
  does not call `ensure_running`. Ordering only — no removal when a dependency
  later dies (compose semantics). Converges over reconcile cycles.

### Ports (`mc2-server/src/apply.rs`)

`resolve_replica_ports` returns one `ServiceSpec` per replica:
- fixed `published: P` → `P + ordinal` (contiguous block; ordinal 0 keeps `P`);
- target-only / hostname-sugar → a distinct free 10000+ port per replica;
- previous per-(ordinal, target) allocations are reused (stable across re-applies);
- cross-service host-port conflicts are rejected at apply (400), which is a new
  hard constraint: published host ports are effectively unique server-wide.

## Verification

- Unit tests: `environment` map/list + old-key rejection; `command` string
  splitting (quotes/escapes); `healthcheck` full field set + defaults;
  `depends_on` list/map, unknown ref, bad condition, cycle, missing healthcheck
  for `service_healthy`; `resolve_replica_ports` fixed-block/auto-distinct/reuse;
  `waiting_deps`/`apply_live` gating; store `healthy` + multi-spec reconcile.
- Integration: `apply_schedule` scaled-ports-per-replica + `environment` round
  trip through the REST apply; `ingress` lifecycle updated for the
  server-wide-port-conflict constraint.
- `just check` green (fmt, clippy -D warnings, all unit + integration tests,
  CLI smoke, live-server smoke).

## Handover notes

- The `environment` rename and the `healthy` column are breaking for existing
  data dirs (pre-release: delete `~/.mc2/data` and re-apply). Old stored specs
  with `env:` fail to parse in the scheduler — expected per the documented
  breaking-change policy.
- Cross-stack published-port conflicts are now apply-time 400s. The ingress
  integration test previously reused host ports across stacks; it now uses
  disjoint ports.
- Ingress routes for a scaled service target replica 0's port (documented
  limitation; east-west fabric already round-robins across Ready replicas).
- `service_completed_successfully` is intentionally unsupported.
