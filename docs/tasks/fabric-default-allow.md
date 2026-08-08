# Task: Fabric default-allow (stack-scoped full mesh, Docker-style)

Status: **IMPLEMENTED — 2026-08-08**
Date: 2026-08-08

## Context

The service fabric (`expose`/`allow`) is currently **default-deny**: a client can
only reach a peer if it declares an explicit `allow: {to, port}` edge, and the
target must `expose` that port. Docker Compose networks are **default-allow** —
services sharing a network reach each other freely, scoped by the project.

Since MC2 is single-node and stacks already scope services, the operator wants
Docker semantics: **default allow within the stack**, while keeping the
"virtual networking" — the L4 splice dataplane + `*.svc.mc2` DNS.

## Design

Keep the fabric mechanism entirely (`FabricTable` splices + `/etc/hosts`
injection); change only the *edge policy*:

1. **Implicit full mesh.** `build_fabric_desired` (`crates/mc2-server/src/fabric.rs`)
   generates an `allow` edge for **every peer service × every exposed guest port**
   in the stack. The client instance gets a splice per reachable port and DNS
   names for every peer service. The YAML `allow:` field is **removed**
   (docker-pure; the mesh is the default).
2. **`expose` stays the reachability gate.** The dataplane needs the
   publish-index to know which guest ports map to host publish ports; a service
   with no `expose` is reachable via nothing (like a container that listens on
   no ports). No change to how `expose` works.
3. **Validation** in `stack.rs`: remove the `allow` field and its checks
   entirely. Nothing new is required of clients.

### Known limitation (pre-existing, surfaces more with full mesh)

The splice binds a real host port `127.0.0.1:<port>` per (client, to_service)
edge. Two peer services exposing the **same guest port** (e.g. two services
both `expose: 8080`) will collide on the client — the second splice reports
`Failed: bind … address in use`. Docker avoids this with per-network virtual
IPs; our L4 splice can't. Documented as a limitation; out of scope to fix here.

## Changes by area

| Area | Change | Size |
| --- | --- | --- |
| `mc2-api` | remove `AllowSpec` + `ServiceSpec.allow` + validation | ~40 lines |
| `mc2-server` | `fabric.rs`: implicit full-mesh edges (peer service × exposed ports) | ~35 lines |
| Tests | fabric unit: implicit mesh, dedupe, DNS names; stack validation updated | ~60 lines |
| Docs | `stack-yaml.md` fabric section, `examples/03-service-fabric`, CHANGELOG | small |

## Out of scope (deliberate)

- Fixing the same-guest-port splice collision (needs per-service virtual ports).
- Multiple networks with different allow scopes (networks remain logical-only).
- Cross-node fabric (already unsupported, single-node).

## Risks

- **Full mesh + same-port peers** → a Failed edge (documented limitation).
- More splices per client (one per reachable port). Fine at stack scale.

## Acceptance criteria

- [ ] A client reaches a peer's exposed port without any `allow:` declaration.
- [ ] Explicit `allow:` still parses/validates (backward compatible).
- [ ] DNS: all peer fqdns resolve once backends are Ready.
- [ ] Same-port collision produces a clear Failed edge + docs note.
- [ ] `just check` green.

## Open questions

1. **`allow` field**: **RESOLVED: removed entirely** (hard cut, docker-pure —
   user choice). Existing stacks that declared `allow` simply lose the
   redundant field; mesh covers them.
2. Should the implicit mesh be *opt-out* (a future `fabric: {policy: explicit}`
   switch) or is full-mesh the only mode? Recommended: full-mesh only for MVP.
   (Deferred.)

## Handover notes (append as completed)

- (pending)

## Implementation notes (2026-08-08)

- **`allow` removed** (user: hard cut): `AllowSpec` + `ServiceSpec.allow` +
  validation block deleted from `mc2-api`; `AllowSpec` re-export dropped.
- **`build_fabric_desired`** (`mc2-server/src/fabric.rs`) now synthesizes a full
  mesh: for every peer service (excluding self) it unions the exposed guest
  ports across that service's instances (parsed from `spec_json`) and emits one
  edge per port to the lowest-ordinal bound backend. No YAML input required.
- **`desired.rs`**: fabric only built when `spec.expose` non-empty.
- **Scheduler**: `fabric_affinity_scores` co-locates with peers that expose
  ports (was: explicit `allow` targets); test updated accordingly.
- **Fabric dataplane unchanged** (`fabric_serve.rs`): splices + DNS injection
  are driven by the (now mesh-derived) edge set. Cross-node edges still fail.
- **CLI**: `mc2 fabric` edge label `allow` → `reach` (they're implicit now).
- **Tests**: fabric units for full mesh / self-skip / portless-skip / lowest-
  ordinal backend / cross-node; stack validation tests updated (expose-without-
  allow parses; duplicate expose 400 via REST).
- **Docs**: `stack-yaml.md` (fabric section + validation list + example), README,
  quickstart, `examples/03-service-fabric` (allow removed, mesh noted), CHANGELOG.
- **Known limitation documented**: two services exposing the same guest port
  collide on a client's splice (second edge `Failed: bind … in use`) — Docker
  avoids this with per-network virtual IPs; our L4 splice can't. Out of scope.
- `just check` green.
