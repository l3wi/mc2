# Mediated service fabric — east–west without a real network

**Status:** Design accepted (D13) — **resolved operator defaults** — **implementation in progress**  
**Date:** 2026-08-07  
**Related:** [microsandbox.md](./microsandbox.md) §6 Networking, [decisions.md](./decisions.md) D7 + D13, [orchestration-review.md](./orchestration-review.md) §5, [ssh-control-plane.md](./ssh-control-plane.md)  
**Task / implement plan:** [docs/tasks/service-fabric.md](../tasks/service-fabric.md)

---

## 0. Resolved direction (operator decisions)

These are **accepted** before implementation. Do not re-litigate unless a spike proves a technical impossibility (see Q2).

| ID | Topic | Decision |
| -- | ----- | -------- |
| **Q1** | Synthetic destination | **DNS names only** (`db`, `db.<stack>.svc.mcc`). No VIP range in v1; no `host.microsandbox.internal` fabric path. |
| **Q2** | If narrow msb rules fail | **Block fabric** — do not ship dirty workarounds (`host`/`private` profiles, silent LAN opens). Document gap; fix via msb capability or redesign. Keep it clean. |
| **Q3** | Edge declaration | **Explicit `allow` on the client service.** Default-deny. No “network membership = mesh.” |
| **Q4** | Internal vs external listen | **Separate `expose`** (cluster-internal, loopback) vs **`ports:`** (north–south). |
| **Q5** | DNS names | **Short + FQDN:** `db` and `db.<stack>.svc.mcc` (stack-scoped short names). |
| **Q6** | Denied clients | **Allow-gated DNS:** no edge → **NXDOMAIN** (fail closed). Connect path also deny. |
| **Q7** | Multi-replica | **v1: single stable backend** (e.g. lowest ready ordinal / primary). **Note for later:** headless-style `db-0`, `db-1` (+ optional RR) — not in first ship. |
| **Q8** | Multi-node east–west | **Deferred.** Fabric v1 is **same-node only.** |
| **Q9** | Co-scheduling for fabric | **Deferred with multi-node.** Same-node fabric implies peers must land on one node for edges to work (see placement rules §8). |
| **Q10** | Cross-stack | **Same-stack only** in v1. |
| **Q11** | Dataplane | **Userspace L4 splice in the agent** (portable; auditable). Not iptables/nft as primary. |
| **Q12** | Internal publish ports | **Agent ephemeral** `127.0.0.1:0` (or equivalent). Report observed port to CP. **On bind failure: hard fail with clear message** (§7.4). |
| **Q13** | Secrets + fabric | Prefer **`allowHosts` including fabric FQDN** where msb secret injection applies. Document plain-TCP limits. |
| **Q14** | vs Ingress | **Fabric first** for multi-service apps. **Ingress (D7) later takes over north–south** HTTP(S); fabric remains east–west. Ingress does not replace `allow`/`expose`. |
| **Q15** | First milestone | **Stack-local TCP fabric** (`expose` + `allow` + DNS + agent splice), same-node — not spike-only. Spike is a **gate inside** the milestone (must pass clean path). |

### One-liner

> Same-node, DNS-named, allow-gated, agent-spliced pipes on top of msb gateways — or nothing. No dirty fallbacks, no multi-node, no mesh-by-membership.

---

## 1. Problem

Operators want Compose/K3s-like multi-service stacks (`web` talks to `db` by name) while **preserving microsandbox isolation**:

- No flat pod/container network  
- No ambient LAN / host / metadata reachability  
- No “join a network ⇒ talk to everyone on it”

MSB already **does not** provide Docker-bridge or CNI-style cross-sandbox networking. Each sandbox has its own host-side gateway. MCC adds **controlled** east–west without inventing a real inter-VM network.

---

## 2. How Docker and K3s do it (contrast)

### 2.1 Docker

| Piece | Behavior |
| ----- | -------- |
| Bridge / user-defined network | Shared L2/L3 on the host (veth ↔ bridge) |
| DNS | Container name resolution on user-defined bridges |
| Default isolation | Membership = can usually reach peers on that network |
| Multi-host | Overlay (Swarm) optional |

**UX:** excellent. **Threat model:** trusted co-located apps; weak for untrusted guests.

### 2.2 Kubernetes / K3s

| Piece | Behavior |
| ----- | -------- |
| CNI | Every pod gets a routable cluster IP; pod↔pod works |
| Service | Stable VIP + DNS (`*.svc.cluster.local`) → endpoints |
| NetworkPolicy | Optional deny/allow (often off by default) |
| Ingress | North–south HTTP(S) |

**UX:** multi-service and multi-node. **Default east–west is open** unless policies are strict.

### 2.3 What we refuse to copy

| Anti-goal | Why |
| --------- | --- |
| docker0 / user bridge under microVMs | Shared fabric; undoes msb isolation story |
| CNI / pod CIDR / guest-visible peer IPs | Real network plane between untrusted kernels |
| Overlay as default (VXLAN, free mesh) | Same problem multi-node |
| “Stack network membership = full mesh” | Docker ICC by another name |
| Opening msb `private` / `host` profiles for app↔app | Too broad (LAN, whole host) |
| Dirty “just use host profile” fabric | Violates Q2 cleanliness bar |

---

## 3. MSB methodology we build on

MSB network path is already **host-mediated**, not kernel-routed between guests:

```
Guest app  ──TCP/IP──►  virtio-net  ──frames──►  host process (smoltcp + policy)
                                                      │
                                                      ├─ allow / deny
                                                      ├─ DNS pin / TLS hooks
                                                      ├─ secret placeholder swap
                                                      └─ published ports (ingress)
```

Important properties (upstream security model):

- Each sandbox: own VM, own host process, **own network gateway**  
- **No** shared network namespace between sandboxes  
- Guest `localhost` ≠ host loopback  
- Default egress: public OK; private / host / metadata **denied**  
- Ingress: **published ports** only (default bind `127.0.0.1`)  
- Control channel (`virtio-console` / agentd) is **host-driven**; not a guest-initiated mesh transport for arbitrary TCP

**Cross-sandbox today:** only via mechanisms you set up (e.g. publish + connect through host, shared volume). Absent that, sandboxes cannot reach each other.

### 3.1 Two channels (do not conflate)

| Channel | Use for service fabric? |
| ------- | ------------------------ |
| **virtio-net + smoltcp** | **Yes** — apps keep speaking normal TCP; host decides path |
| **agentd control channel** | **No** for serving workloads — not a substitute for Postgres/HTTP |

MCC stitches **two msb gateways** on the trusted host. It does **not** open a real L2/L3 between guests, and does **not** tunnel app traffic over agentd RPC.

---

## 4. Decision summary (D13)

| | |
| - | - |
| **Decision** | **Host-mediated service fabric** — agent userspace L4 + allow-gated DNS |
| **Not** | CNI, bridge, overlay, guest-routable peer mesh, mesh-by-network-membership |
| **MSB role** | Per-sandbox gateway, policy, publish (unchanged threat model) |
| **MCC role** | `expose` / `allow`, DNS names, FabricPlan Sync, agent splice, observed endpoints |
| **Default** | Deny east–west until explicit `expose` + `allow` |
| **Scope v1** | **Same-stack, same-node, TCP only** |
| **North–south** | `ports:` now; **Ingress later owns public HTTP(S)** (D7) without replacing fabric |
| **Cleanliness** | If msb cannot express narrow DNS/port allows **without** `host`/`private`, **do not ship fabric** (Q2) |

One-liner:

> **msb fakes a network for one sandbox. MCC stitches those fakes together only where policy says so — still on the host — or not at all.**

---

## 5. Separation of concerns

| Layer | Owns |
| ----- | ---- |
| **Control plane (server)** | Parse/validate `expose`/`allow`; DNS name schema; FabricPlan per instance; same-node placement constraints for fabric peers; observed fabric status |
| **Agent dataplane** | Ephemeral loopback publish for `expose`; userspace splice; msb **narrow** egress + DNS for `allow`; report success/failure messages |
| **msb** | virtio-net, smoltcp, policy engine, DNS interceptor, port publish into guest |

**Not** north–south `ports:` (existing).  
**Not** SSH open/close ([ssh-control-plane.md](./ssh-control-plane.md)).  
**Not** multi-node tunnels (deferred).

---

## 6. Object model (v1)

### 6.1 Logical network (optional grouping)

Membership for **documentation / future use only** in v1 — **not** connectivity.

| Field | Notes |
| ----- | ----- |
| `name` | e.g. `backend` |
| `mode` | `mediated` only |

Joining a network **does not** open east–west. Only `allow` does.

### 6.2 `expose` (service-internal listeners)

| Field | Notes |
| ----- | ----- |
| `port` | Guest listen port (e.g. 5432) |
| `protocol` | `tcp` only in v1 |
| `name` | optional |

Agent: publish **`127.0.0.1:<ephemeral> → guest port`**. Never `0.0.0.0` via `expose`.

### 6.3 `allow` (edges — the security control)

| Field | Notes |
| ----- | ----- |
| `to` | Target **service name in the same stack** |
| `port` | Must match an `expose` on the target (validate on apply) |
| `protocol` | `tcp` |

Default-deny. No cross-stack `to:` in v1.

### 6.4 Service DNS

| Form | Example | Scope |
| ---- | ------- | ----- |
| Short | `db` | Same stack only; resolved only for clients with matching `allow` |
| FQDN | `db.shop.svc.mcc` | `db.<stack-name>.svc.mcc` |

**Denied client (no `allow`):** DNS **NXDOMAIN** for those names (Q6).  
**Allowed client:** resolve to the fabric destination the agent uses for splice (implementation detail under DNS name strategy Q1 — still a name, not a guest-visible peer mesh).

### 6.5 Multi-replica (v1 behavior + later note)

| Phase | Behavior |
| ----- | -------- |
| **v1 (Q7-D)** | Name `db` / FQDN → **one** backend: prefer lowest ready ordinal (or sole replica). No client-side RR. |
| **Later (note C)** | Headless-style `db-0`, `db-1` and optional Service-level RR for stateless services. **Not in first ship.** |

### 6.6 Example YAML

```yaml
apiVersion: mcc/v1
kind: Stack
metadata:
  name: shop

networks:
  backend:
    mode: mediated   # membership only; not a mesh

services:
  db:
    image: postgres:16
    networks: [backend]
    expose:
      - port: 5432
        protocol: tcp
    # no ports: → not reachable from LAN

  web:
    image: ghcr.io/example/shop-api
    networks: [backend]
    ports:
      - host: 8080
        guest: 8080
    allow:
      - to: db
        port: 5432
        protocol: tcp
    env:
      DATABASE_URL: postgres://app@db.shop.svc.mcc:5432/shop
    # secrets example (Q13): allowHosts may include fabric FQDN when using injection
    # secrets:
    #   - name: DB_PASSWORD
    #     env: DB_PASSWORD
    #     allowHosts: [db.shop.svc.mcc]
```

### 6.7 Apply-time validation (server)

Reject apply (clear error) when:

- `allow.to` service missing in stack  
- `allow.port` not listed in target `expose`  
- `allow` targets another stack  
- protocol ≠ `tcp`  
- duplicate/conflicting expose ports on a service (as needed)

Runtime bind failures are **not** apply errors — see §7.4.

---

## 7. Data path (v1: same node)

### 7.1 Path

```
┌─ web guest ─────────────────────────────────────────┐
│  connect db.shop.svc.mcc:5432                         │
└───────────────────────────┬─────────────────────────┘
                            │ virtio-net
                            ▼
┌─ msb gateway (web) ─────────────────────────────────┐
│  narrow policy: this DNS name + port only             │
│  NO private profile, NO host profile                  │
└───────────────────────────┬─────────────────────────┘
                            ▼
┌─ mcc agent (trusted) ───────────────────────────────┐
│  DNS answer only if allow edge exists                 │
│  userspace L4 splice → db expose publish              │
└───────────────────────────┬─────────────────────────┘
                            ▼
┌─ msb gateway (db) ── 127.0.0.1:ephem → guest :5432 ─┘
```

### 7.2 Multi-node — deferred

**Out of scope for first ship (Q8).**  
If fabric peers are scheduled on **different nodes**, edges **do not work** until a future multi-node phase. Behavior: clear **Failed / Degraded** fabric status explaining cross-node not supported — not silent blackhole without message. See §8 placement.

Future options (not chosen now): agent mTLS tunnels per edge; Ingress-only for cross-node; never default full overlay.

### 7.3 Synthetic destination (Q1) + cleanliness gate (Q2)

**Preferred:** DNS name strategy only — msb domain rules + pin set for `*.svc.mcc` (and short names as aliases in-stack).

**Gate (implementation blocker):**

Same-node `web → db:5432` must work with:

- no `NetworkProfile::Private`  
- no `NetworkProfile::Host`  
- no egress to real LAN ranges  
- allow limited to fabric DNS name + port  

**If gate fails:** stop. Record findings in this doc / task file. **Do not** ship `host`/`private` workarounds. Options then are upstream msb support or a clean redesign — not a dirty v1.

### 7.4 Port allocation and failure (Q12)

**Happy path**

1. Agent needs internal publish for each `expose` on a local instance.  
2. Bind **`127.0.0.1` with ephemeral port** (OS assign or agent free-port pick).  
3. Wire msb publish guest port ← that host port.  
4. Report to server: `fabric_expose[] = { guest_port, host_bind, host_port, phase: Ready }`.  
5. Client instances with `allow` get DNS + splice once backend expose is Ready.

**Failure path (blocked bind / publish / policy install)**

| Stage | Failure example | Instance / fabric effect | Operator-visible message (examples) |
| ----- | --------------- | ------------------------ | ------------------------------------- |
| Expose bind | Address in use, permission, msb publish error | **Provider** instance: fabric expose **Failed**; overall phase may be **Failed** or **Running with fabric Failed** (prefer explicit fabric substatus) | `fabric expose 5432: bind 127.0.0.1 failed: address already in use` |
| Policy/DNS install | msb rejects narrow domain rule | **Consumer** instance: fabric edge **Failed** | `fabric allow db:5432: msb policy install failed: …` |
| Splice listen | Agent cannot hold forwarder socket | Consumer edge **Failed** | `fabric splice db.shop.svc.mcc:5432: …` |
| Backend not Ready | db expose still pending/failed | Consumer edge **Pending** then **Failed** after grace (or stay Pending with message) | `fabric allow db:5432: waiting for backend expose` / `backend expose failed: …` |
| Cross-node peer | db scheduled elsewhere | Edge **Failed** (v1) | `fabric allow db:5432: cross-node fabric not supported (backend on node X, local node Y)` |
| Cleanliness gate | would require host/private | Feature disabled / edge **Failed** | `fabric requires narrow msb DNS allow; host/private profiles are not permitted` |

**Principles**

- **Fail loud, not soft-open.** No “best effort” open profiles.  
- Messages go to: agent logs, instance status `message` / `fabric.message`, and ideally `mcc ps` / instance API.  
- Align with SSH pattern: desired vs observed, `phase` + `message` ([ssh-control-plane.md](./ssh-control-plane.md)).  
- **Startup / reconcile:** if `expose` or required `allow` cannot be established, treat as **reconcile failure for fabric** — do not report fabric Ready. Whether that forces the whole sandbox instance to `Failed` vs `Running` + fabric Failed: **prefer sandbox still Running if workload is up, but fabric edges Failed with clear message**; if the service is *only* useful via fabric and expose is mandatory, operator sees Failed fabric until fixed. Stack apply itself already succeeded if YAML was valid.

**Retry:** transient bind errors may retry with backoff; permanent errors surface Failed until config/node changes.

---

## 8. Placement (same-node fabric)

Multi-node fabric is deferred, so connectivity requires **co-location of allow-peers**.

| Rule | v1 behavior |
| ---- | ----------- |
| Soft prefer | Scheduler **should prefer** placing instances that participate in an `allow` edge on the **same node** when capacity allows |
| Hard failure if split | If peers land on different nodes, fabric edge **Failed** with cross-node message (§7.4) — do not pretend DNS works |
| Volumes | Existing volume stickiness unchanged; may force node and thus force peers to follow or fail fabric clearly |
| Future | Multi-node tunnels or hard `placement: co-located` annotation — not required for first ship |

Exact scheduler scoring can be refined at implement time; the **product contract** is: same-node works; split fails clearly.

---

## 9. Control plane / agent contracts

### 9.1 Server

- Parse `networks`, `expose`, `allow` on apply  
- Validate edges (§6.7)  
- Build per-instance **FabricPlan**: exposes, allows, DNS names, backend service refs  
- Sync FabricPlan to agents  
- Store **observed** fabric status from agents (ports, phases, messages)  
- Placement: prefer co-locate fabric peers  

### 9.2 Agent

On Sync reconcile:

1. For each local `expose`: bind ephemeral loopback, msb publish, report Ready/Failed + message  
2. For each local `allow`: if backend expose Ready **and same node**, install narrow DNS/policy + userspace splice; else Pending/Failed + message  
3. Tear down on remove/scale/stop  
4. **Never** enable `private` or `host` solely for fabric  

### 9.3 Observed status

| Field | Notes |
| ----- | ----- |
| `fabric.exposes[]` | guest_port, host_port, phase, message |
| `fabric.edges[]` | to, port, phase, message, backend_instance? |
| phases | `Pending` \| `Ready` \| `Failed` |

---

## 10. Security properties (acceptance)

| Property | Required |
| -------- | -------- |
| Default east–west deny | Yes |
| Edge requires explicit `allow` | Yes |
| No guest-to-guest L2/L3 | Yes |
| No blanket private/host for fabric | Yes — **hard gate** |
| Same-stack only | Yes (v1) |
| Same-node only | Yes (v1) |
| Allow-gated DNS (NXDOMAIN if denied) | Yes |
| North–south still explicit `ports:` / later Ingress | Yes |
| Untrusted guest cannot widen policy | Yes (Sync-only) |
| Failures visible | Yes (§7.4) |

---

## 11. Relationship to Ingress (Q14)

| Plane | Mechanism | Owner over time |
| ----- | --------- | --------------- |
| **East–west** | `expose` + `allow` + fabric DNS + agent splice | Service fabric (this doc) |
| **North–south today** | `ports:` host publish | Existing MCC |
| **North–south later** | Ingress object (Caddy/Traefik/BYO) | D7 — **takes over public HTTP(S)** exposure story |

Ingress **does not** replace fabric. It sits in front of services that already have (or gain) a publish/Ingress backend. Internal `db` stays `expose`-only with no Ingress rule.

---

## 12. Phased delivery

| Phase | Scope | Status |
| ----- | ----- | ------ |
| **Gate** | Prove narrow DNS allow + splice **without** host/private | Required before product merge |
| **1 — First ship (Q15-B)** | Stack-local TCP fabric: schema, Sync, agent dataplane, status, same-node, single-backend DNS | Not started |
| **2 — Later** | Headless ordinals / optional RR (Q7-C note) | Deferred |
| **3 — Later** | Multi-node east–west | Deferred (Q8) |
| **4 — Later** | Ingress north–south (D7) | Deferred; designed to take over public HTTP |

---

## 13. Non-goals (v1)

- CNI / bridge / overlay under microVMs  
- Multi-node fabric tunnels  
- Cross-stack allows  
- Mesh-by-network-membership  
- VIP ranges (unless forced by msb and still DNS-fronted — prefer pure DNS; re-open only if gate requires)  
- `host` / `private` profile workarounds  
- UDP / multicast  
- App traffic over agentd  
- Full multi-replica Service RR  
- Replacing msb policy engine  

---

## 14. Mapping cheat sheet

| Docker / K3s | MCC mediated fabric (v1) |
| ------------ | ------------------------ |
| User bridge | Logical `networks:` only (no ICC) |
| Container DNS | Allow-gated `db` + `db.<stack>.svc.mcc` |
| `-p` publish | `ports:` north–south |
| Internal only | `expose` + loopback ephemeral |
| NetworkPolicy | `allow:` default-deny |
| ClusterIP / kube-proxy | Agent userspace L4 splice |
| CNI | **None** |
| Multi-node pod net | **Deferred** |

---

## 15. Remaining technical unknowns (not product forks)

Product options Q1–Q15 are **resolved**. Left for implement / gate:

1. **msb API details** for domain-only egress allow + DNS pin that agents can install per sandbox without host/private  
2. How short-name `db` is presented to the guest resolver (msb alias vs injected search domain) while keeping FQDN canonical  
3. Userspace splice: idle timeouts, half-close, concurrency limits  
4. Exact scheduler scoring for “prefer co-locate allow peers”  
5. Whether whole instance phase stays `Running` when only fabric substatus is `Failed` (lean yes — §7.4)  
6. Secret injection to fabric FQDN for non-TLS protocols (document msb limits)

If (1) fails the cleanliness gate → **stop and document**; do not ship.

---

## 16. Source

- Operator decisions Q1–Q15 (2026-08-07 session)  
- microsandbox networking & security docs  
- Docker bridge / K8s Service+CNI contrast  
- MCC D7 / D13; SSH observed-port + message pattern  
- Prior design: host-mediated pipes, not a real network  
