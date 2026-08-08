# Task: Compose-faithful networking (ports north-south, expose listeners, server-wide networks)

Status: **IMPLEMENTED — 2026-08-08**
Date: 2026-08-08

## Context

MC2 moves to a Compose-faithful networking model:

- **`ports` = north-south** (host publish, compose semantics) + auto host port
  for target-only + a hostname sugar.
- **`expose` = internal listeners** (compose-faithful, load-bearing: it drives
  the fabric dataplane) in compose's bare-port list form.
- **Networks = server-wide named networks**: a network spans stacks, so
  services in different stacks that join the same named network can reach each
  other (default-allow, `*.svc.mc2` DNS). The stack gets an implicit default
  network; named networks are shared.

## Design

### Schema (`mc2-api`)

- `services.<svc>.ports` (north-south):
  - `"5000:3001"` — published:target (today).
  - `"3001"` / long `{target: 3001}` — **auto host port** (was rejected).
  - `"mcp.example.com:3000"` / long `{target, hostname}` — **hostname sugar**:
    equivalent to an ingress rule for that hostname → service:port.
  - `"ip:5000:3001"`, `/tcp` suffix, long `{target, published, protocol}` — as today.
- `services.<svc>.expose` (internal listeners): accept compose's list form
  `[5432]` / `["5432"]` **and** the existing map form `[{port, protocol, name}]`.
- `services.<svc>.networks: [name]`: server-wide membership. Absent → the
  stack's implicit default network (named `<stack>`).
- Top-level `networks:` stays optional/informational (a network need not be
  declared to be joined — it exists server-wide on first use).

### Network-scoped fabric (`mc2-server`)

- Edge building (`build_fabric_desired`) considers **all stacks' instances** on
  the node; a service gets edges to every service (any stack) that shares a
  network with it, for every `expose` port on the peer.
- DNS: default network → `svc.<stack>.svc.mc2` (unchanged); named network →
  `svc.<network>.svc.mc2`.
- Splices stay on `127.0.0.1:<port>` (shared per network+service+port),
  round-robin across Ready replicas.

### Ports dataplane + ingress

- Auto host ports are allocated at create (existing `reserve_ephemeral`
  pattern) and kept stable per instance across reconciles; `ingress` backends
  resolve the allocated host port instead of a declared `published`.
- The hostname sugar synthesizes a Traefik route (host → allocated backend).

## Known architectural constraint (flag before building)

The fabric is an L4 splice on the host loopback and guests reach the host via
the **gateway IP** (from `/etc/resolv.conf`); DNS injection maps a fqdn to that
single IP, so two services on *different* networks exposing the **same guest
port** cannot be isolated on the loopback — the second splice's bind fails
(port uniqueness is effectively **server-wide**, not per-network). Compose gets
real isolation because Docker has a kernel bridge + per-network DNS. This stays
a documented limitation; networks deliver **cross-stack reachability + DNS
scoping**, not same-port isolation across networks.

## Changes by area

| Area | Change | Size |
| --- | --- | --- |
| `mc2-api` | ports: auto-host + hostname sugar; expose list form; networks membership | ~80 lines |
| `mc2-server` | network-scoped edge building + DNS; auto host-port allocation for ports; ingress on allocated ports; splice keying | ~150 lines |
| Tests | cross-stack shared-network edge/DNS unit; ports auto-host; hostname sugar route; validation | ~120 lines |
| Docs/examples | stack-yaml, fabric/networks guide, examples, CHANGELOG | medium |

## Out of scope

- Same-port isolation across networks (loopback constraint — documented).
- `network_mode` (host/none/service) — `network.profiles` stays.
- Static IPAM / `internal` / `external` network semantics.

## Acceptance criteria

- [ ] `ports: ["3001"]` auto-allocates a host port; `"mcp.example.com:3000"`
      publishes a routed hostname.
- [ ] `expose: [5432]` list form works; services on the same network (same or
      different stack) reach each other via `svc.<network>.svc.mc2` DNS.
- [ ] `ingress` routes to auto-allocated backends.
- [ ] Default network keeps `svc.<stack>.svc.mc2` (back-compat DNS).
- [ ] `just check` green.

## Handover notes (append as completed)

- (pending)

## Implementation notes (2026-08-08)

- **Schema (`mc2-api`)**: `PortSpec` gained `hostname: Option<String>` and
  `published: 0` = auto host port. `de_ports` parses target-only `"3001"` and
  hostname sugar `"mcp.example.com:3000"` (2-part short form disambiguated by
  numeric first segment). `de_expose` accepts `[5432]` / `["5432"]` / maps.
  Removed the "networks must be declared" validation (networks are server-wide).
  Added `network_fqdn(network, service)`.
- **`mc2-server/apply.rs`**: `resolve_auto_ports` allocates concrete host ports
  for `published: 0` entries at apply (stable per stack/service/target across
  restarts + re-applies) and persists them in the stored spec_json.
- **`mc2-server/fabric.rs`**: `build_fabric_desired` is network-scoped —
  a client edges to every peer (any stack) that shares a network; same-stack
  peers keep `svc.<stack>.svc.mc2` (back-compat), cross-stack peers use the
  shared named network `svc.<network>.svc.mc2`.
- **`mc2-server/ingress.rs`**: `routes_from_port_hostnames` emits TLS (`le`)
  routes for `ports[].hostname`; `resolved_host_port` reads the auto-allocated
  backend port from the instance's stored spec (raw YAML has `published: 0`).
- **Tests**: stack.rs ports/expose parse; fabric cross-stack shared-network +
  isolation + back-compat fqdn; ingress hostname-sugar route; integration:
  target-only auto host applies (200); fabric round-robin unchanged.
- **Docs**: stack-yaml ports/expose/networks sections, CHANGELOG.
- **Verified live**: two stacks on `networks: [backend]` apply; hostname sugar
  yields `mcp.example.com → web:3000` with an auto host port (10000) in both
  `/v1/ingress` and the Traefik catalog. `just check` green.
