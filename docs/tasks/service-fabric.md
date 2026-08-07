# Task: Mediated service fabric (same-node)

**Status:** Ready for implementation after cleanliness gate  
**Date:** 2026-08-07  
**Design:** [docs/research/service-fabric.md](../research/service-fabric.md)  
**Decision:** D13 in [docs/research/decisions.md](../research/decisions.md)

---

## 1. Goal

Ship **stack-local, same-node, TCP service fabric**: `expose` + explicit `allow` + allow-gated DNS (`db` / `db.<stack>.svc.mcc`) + **agent userspace L4 splice**, without weakening msb isolation.

**Success:** Operator applies a web+db stack on one node; web reaches Postgres via `db.shop.svc.mcc:5432`; no `host`/`private` profiles; denied clients get NXDOMAIN; bind/policy failures surface clear messages.

---

## 2. Locked product choices (do not re-open)

| ID | Choice |
| -- | ------ |
| Q1 | DNS names only |
| Q2 | Clean only — block if narrow msb path impossible |
| Q3 | Explicit `allow` |
| Q4 | Separate `expose` vs `ports:` |
| Q5 | Short + FQDN |
| Q6 | NXDOMAIN if no allow |
| Q7 | Single stable backend; note ordinals later |
| Q8–Q9 | Multi-node deferred; fail clearly if split |
| Q10 | Same-stack only |
| Q11 | Agent userspace splice |
| Q12 | Agent ephemeral ports; **Failed + clear message** on block |
| Q13 | Fabric FQDN in allowHosts when applicable |
| Q14 | Fabric first; Ingress later for N–S HTTP |
| Q15 | First ship = full stack-local fabric (gate inside) |

Full text: design doc §0.

---

## 3. Implementation plan (MVP)

### 3.0 Cleanliness gate (blocker)

**Before merging product paths:**

- [ ] Prototype two sandboxes, same host process model as agent  
- [ ] Consumer resolves/connects to fabric DNS name only  
- [ ] Prove msb policy **without** `NetworkProfile::Host` or `Private`  
- [ ] Write result into design doc §15 / this file “Gate result”  
- [ ] **If fail → stop** (Q2). No dirty fallback PR.

### 3.1 Schema + server

- [ ] Extend stack YAML: `expose`, `allow`, optional `networks`  
- [ ] Apply validation (target exists, port exposed, same stack, tcp)  
- [ ] Persist desired fabric on services/instances  
- [ ] Build **FabricPlan** for agent Sync  
- [ ] Store observed fabric status (exposes, edges, phases, messages)  
- [ ] REST/CLI surface enough to debug (`ps` / instance detail)

### 3.2 Agent dataplane

- [ ] Ephemeral `127.0.0.1` publish per `expose`; report port or Failed+message  
- [ ] Userspace TCP splice for each Ready `allow`  
- [ ] Install/remove narrow msb DNS+egress rules per edge  
- [ ] NXDOMAIN / no rule for clients without allow  
- [ ] Cross-node backend → Failed + explicit message  
- [ ] Never set host/private for fabric  

### 3.3 Placement

- [ ] Prefer scheduling allow-peers on the same node  
- [ ] If split: edge Failed (message), no silent hang  

### 3.4 DNS naming

- [ ] FQDN: `<service>.<stack>.svc.mcc`  
- [ ] Short: `<service>` stack-scoped for allowed clients  
- [ ] v1 backend selection: lowest ready ordinal (single backend)

### 3.5 Tests / examples

- [ ] Unit: apply validation, plan build  
- [ ] Integration: fabric plan in Sync material (no hypervisor if possible)  
- [ ] Lab/example stack: web+db or alpine echo pair documenting fabric  
- [ ] Failure injection: bind fail → message visible  

### 3.6 Docs

- [ ] Quickstart or research note: fabric usage  
- [ ] Update README binary/features if CLI changes  
- [ ] Keep design doc status in sync  

---

## 4. Explicit non-goals (this task)

- Multi-node tunnels  
- Cross-stack allow  
- Ingress implementation  
- Multi-replica RR / headless DNS  
- VIP ranges, host-profile fabric  
- UDP  

---

## 5. Acceptance criteria

1. Cleanliness gate passed and recorded.  
2. Apply valid web→db (or equivalent) stack; same node; TCP works by FQDN and short name.  
3. Service without `allow` cannot resolve target fabric name (NXDOMAIN).  
4. `expose` only → not reachable on LAN/`0.0.0.0`.  
5. Forced bind failure → `Failed` + human-readable message in status.  
6. Split-node peers → clear unsupported message.  
7. No `private`/`host` profiles added for fabric in agent code paths.  
8. `just check` (or project gate) green.

---

## 6. Risks

| Risk | Mitigation |
| ---- | ---------- |
| msb cannot do narrow DNS allow | Gate fails → no ship (Q2) |
| Short name collision / resolver quirks | FQDN canonical; document short-name rules |
| Splice performance | Accept userspace cost for v1; measure later |
| Confusion with `ports:` | Docs + validation messages |

---

## 7. Gate result

| Date | Result | Notes |
| ---- | ------ | ----- |
| 2026-08-07 | **Pass (design)** | Narrow egress: `Destination::Group(Host)` + **TCP single port** only — **not** `NetworkProfile::Host` or `Private`. Guest reaches agent splice via gateway/host path for that port only. DNS: inject `fqdn`/`short` → gateway IP in guest `/etc/hosts`. |

---

## 8. Implementation log

### 2026-08-07 — initial ship path

- Schema: `expose`, `allow`, stack `networks` (mediated only); apply validation.
- Proto: `FabricDesired` / `FabricObserved` on Sync + ReportStatus.
- Server: `build_fabric_desired`, scheduler fabric co-location preference.
- Runtime: narrow Host:tcp:port rules at create (not Host/Private profiles).
- Agent: `fabric_serve` — ephemeral expose publish, L4 splice, `/etc/hosts` inject.
- Example: `examples/stacks/smoke-fabric.yaml`.
- Unit tests: stack parse, fabric plan, scheduler affinity, policy narrow rules.
- Gate: documented as design pass (narrow Host destination + port).
