# Phase 6 — Restart, health, reschedule

**Status:** Implemented (branch `feat/phase-6-restart-reschedule`)  
**Date:** 2026-08-06  
**Branch (proposed):** `feat/phase-6-restart-reschedule`  
**Parent:** `docs/tasks/mcc-mvp.md` §Phase 6  
**After approval:** copy this plan to `docs/tasks/phase-6-restart-health-reschedule.md` and implement.

---

## 1. Research: what sandboxes are *for* in MCC

### Intended purpose

MCC sandboxes are **desired-state microVM services** on a small self-hosted fleet (home lab → light prod):

| Use | Implication for resilience |
| --- | -------------------------- |
| Long-running processes (`httpd`, agents, tools, isolated runtimes) | Default **restart on crash**; stay up across agent ticks |
| Stacks of several sandboxes with ports + secrets | Failures are **per-instance**; control plane keeps replica count |
| Host-side secrets / network policy (msb differentiators) | Recreate must re-apply injection; Sync still owns secret resolve |
| Multi-node (Linux KVM + macOS HVF agents) | **Node death** must move *stateless* work; not freeze the fleet |
| Node-local volumes only (MVP non-goal: multi-node RWX) | **Sticky** placement when data lives on one host |

They are **not** primarily:

- Batch Jobs with strict one-shot semantics (supported via `restartPolicy: never`)
- Live-migratable VMs (no shared storage / snapshot move in MVP)
- Full K8s Deployment rolling updates (post-MVP)

### What microsandbox already does (and does not)

| msb provides | MCC must still own |
| ------------ | ------------------ |
| States: Running / Stopped / **Crashed**, start/stop/restart/remove | **Policy** when to restart vs leave Failed |
| `ping` / agentd readiness | **App-level** health (`exec` probe in guest) |
| Process-per-sandbox on one host | Multi-node desired state + placement |
| Named volumes **local to host** | Sticky schedule + “don’t reschedule away from volume” |
| Detached reattach after agent process restart | Distinct from **node** loss (another machine) |

Upstream gap (from research docs): *“Health checks + auto restart — limited (ping, idle, max_duration)”*. Phase 6 fills that gap at the orchestrator layer, Swarm-style, without inventing CSI.

### Current MCC state (gaps)

| Area | Today | Gap |
| ---- | ----- | --- |
| Crash path | Runtime **always** remove+recreate on Crashed | Ignores `restartPolicy` (`never` broken) |
| Schema | `restartPolicy`, `health` parsed | Not enforced |
| NotReady watcher | Marks node only | Instances stay bound forever |
| Scheduler | Runs on **apply only** | No re-place after unbind / late join |
| Sync | Omits Failed/Stopped | Agent may GC sandboxes that policy wants to restart |
| Volumes | YAML only | No claim / sticky filter; no msb mount required for sticky MVP |
| Tests | Single-node apply | No multi-node NotReady reschedule |

---

## 2. Goals (MVP)

1. **restartPolicy enforcement (node-local)**  
   - `on-failure` (default): recreate/start after Crashed / Failed / failed health  
   - `always`: also bring back intentional Stopped  
   - `never`: observe Failed/Stopped; **do not** recreate  

2. **Optional exec health**  
   - When `health.kind: exec` and sandbox is Running, run `health.command` via msb `exec` on `intervalSeconds`  
   - Failure → treat as local failure (subject to restartPolicy)  
   - `health: null` / omit → no probe  

3. **Node loss → reschedule (non-sticky)**  
   - After heartbeat grace → NotReady (existing)  
   - Eligible instances on that node → unbind → Pending → schedule onto other Ready nodes  
   - Old node’s agent (if it ever returns) drops them from Sync → `ensure_removed` GC  

4. **Volume sticky placement**  
   - If service has **any** volume mounts: do **not** cross-node reschedule  
   - First bind records sticky node (volume claim or instance stays pinned)  
   - Full msb volume create/mount can be thin/stub if not free; stickiness is the hard requirement  

**Exit (from MVP task):** mark node NotReady / kill agent path in harness; non-sticky instance reappears **Scheduled/Running on another Ready node**. Multi-node integration test.

---

## 3. Non-goals (Phase 6)

| Out | Why |
| --- | --- |
| Live migration / volume data move | No multi-node storage |
| HTTP/TCP probe kinds | Exec + `curl` covers lab; add kinds later |
| Configurable backoff YAML | Hardcode agent-side backoff constants |
| Rolling update / maxUnavailable | Post-MVP |
| Arch filter / preemption | Nice; not required for exit (optional drive-by) |
| Full volume provisioner + msb disk images | Sticky **placement** first; mount wiring if SDK path is already clear |
| `NodeLost` as new phase enum | Prefer reusing Pending after unbind |

---

## 4. Design decisions

### D6.1 — Split “local restart” vs “cluster reschedule”

| Concern | Owner | Trigger |
| ------- | ----- | ------- |
| Sandbox crashed / unhealthy on a live node | **Agent + runtime** | Observe status / exec fail |
| Node heartbeat stale | **Server watcher + reschedule controller** | `mark_stale_nodes` then unbind |

Do **not** overload `restartPolicy` alone for node death. Policy matrix:

| `restartPolicy` | Local crash / health fail | Node NotReady (no sticky volume) |
| --------------- | ------------------------- | -------------------------------- |
| `on-failure` | Recreate (with backoff) | Unbind → Pending → reschedule |
| `always` | Recreate even if Stopped | Same reschedule |
| `never` | Leave Failed/Stopped; no recreate | **No** auto-reschedule; leave bound or mark Failed with message |

Rationale: one-shot / debug sandboxes (`never`) should not hop nodes and re-run; services should.

### D6.2 — Sync must not fight restart

Today Sync skips Failed/Stopped → agent treats runtime as stale → `ensure_removed`. That fights local restart.

**MVP rule:** Always Sync **all non-deleted instances bound to the node** (any phase). Scale-down still removes instance rows → GC. Agent applies restartPolicy inside `ensure_running` / reconcile.

### D6.3 — Health is agent-local

- Server does not run probes (no guest access).  
- Agent: last probe time per `runtime_id`; on interval, if Running + health.exec, `Sandbox::exec`.  
- Non-zero exit / exec error → unhealthy; MVP: **1 failure** applies restartPolicy.  
- Report phase `Failed` with message `health: …` when not restarting, or `Creating`/`Running` after recreate.

### D6.4 — Reschedule controller + periodic scheduler

Extend beyond apply-only `run_scheduler`:

1. **`reschedule_loop`** (adjacent to NotReady watcher):  
   - For each NotReady node:  
     - List instances with `node_id = node`  
     - If **sticky** (service has volumes) → leave bound; optional message  
     - Else if `restartPolicy == never` → set phase Failed, message `node lost; restartPolicy=never` (keep node_id)  
     - Else → **`unbind_instance`**: `node_id=NULL`, `phase=Pending`, clear `runtime_id`  
   - Then `run_scheduler()`  

2. **`schedule_loop`** every ~5s: `run_scheduler()` so late-joining nodes pick up Pending without re-apply.

### D6.5 — Volume sticky (MVP)

- **Sticky** if `ServiceSpec.volumes` is non-empty.  
- Optional store table `volume_claims(name, stack, node_id, kind)` filled on first successful bind.  
- `pick_node`: if any mount’s claim has `node_id`, only that node (or stay Pending if that node NotReady).  
- **Do not** unbind sticky instances on NotReady.  

msb named-volume mount wiring: **best-effort** same phase; not blocking exit if stickiness + reschedule tests pass.

### D6.6 — Local restart backoff (hardcoded)

- Agent map: `runtime_id → (restart_count, next_eligible_at)`  
- Exponential backoff e.g. 2s, 5s, 15s, 30s (cap 30s); reset after continuous Running for 60s  
- Runtime today always recreates on Crashed — **gate** behind policy + backoff.

### D6.7 — Validation

`validate_stack`: `restartPolicy` ∈ `always` | `on-failure` | `never`.  
Health: if present, `kind` ∈ `exec` | `none`; `exec` requires non-empty `command`.

### D6.8 — restartPolicy semantics when “desired”

Desired instances in Sync mean **want Running**:

| Policy | Failed / Crashed | Stopped |
| ------ | ---------------- | ------- |
| `on-failure` | recreate with backoff | leave Stopped (do not force up) — *or* treat as failure if we never clean-stop desired; implement as: only restart Failed/Crashed |
| `always` | recreate | start/recreate |
| `never` | leave Failed | leave Stopped |

---

## 5. Architecture (target)

```
                    ┌──────────────────────────────────────┐
                    │ mcc server                           │
                    │  not_ready_loop (existing)           │
                    │  reschedule_loop  [NEW]              │
                    │    NotReady → unbind non-sticky      │
                    │  schedule_loop    [NEW]              │
                    │    Pending → pick_node → bind        │
                    └───────────────┬──────────────────────┘
                                    │ gRPC Sync / ReportStatus
              ┌─────────────────────┼─────────────────────┐
              ▼                     ▼                     ▼
         agent (alive)         agent (dead)          agent (other)
         ensure_running        (no heartbeat)        receives rebound
         + restartPolicy       → NotReady            instance
         + exec health
```

**Phases:**

```
Pending → Scheduled → Creating → Running
                ↘ Failed  (local never / health / create error)
Node NotReady → unbind → Pending → …   (non-sticky only)
```

---

## 6. Implementation tasks (MVP-ordered)

### Task A — Store: unbind + sticky helpers

- [ ] `Store::unbind_instance(id) → Pending, node_id=None, runtime_id=None`  
- [ ] Memory + SQLite implementations  
- [ ] Unit tests: unbind then `list_pending`  
- [ ] (Optional same PR) `volume_claims` table migration + put/get by name  

### Task B — Scheduler hygiene + sticky filter

- [ ] `service_load_map`: only count phases that consume capacity (not Failed/Stopped)  
- [ ] `pick_node`: sticky claims / volumes pin to node  
- [ ] Unit: sticky pin; two-node spread without volumes  

### Task C — Server loops: reschedule + periodic schedule

- [ ] `reschedule_not_ready_instances(store)` + loop  
- [ ] Wire `schedule_loop` + reschedule in `mcc-server` run  
- [ ] Config: reuse heartbeat grace; `--reschedule-interval-secs` default 5  
- [ ] Integration: two agents → apply → NotReady node A → instance on B  

### Task D — Agent/runtime restartPolicy

- [ ] Honor `desired.spec.restart_policy` in `ensure_running`  
- [ ] Gate unconditional crash recreate  
- [ ] Backoff map in agent reconcile  
- [ ] Unit: policy matrix as pure helper where possible  

### Task E — Exec health

- [ ] Agent: Running + health.exec + interval → exec command  
- [ ] Failure → crash/restart path  
- [ ] Validate health in `parse_stack_yaml`  

### Task F — Sync include Failed/Stopped when bound

- [ ] Change gRPC Sync to all bound instances (any phase)  
- [ ] Scale-down still deletes rows → GC  
- [ ] Regression: secrets/apply tests  

### Task G — Docs + examples

- [ ] `docs/tasks/mcc-mvp.md` Phase 6 notes  
- [ ] `docs/guides/testing.md`  
- [ ] Example restartPolicy + health on smoke/demo  

### Task H — Validation

- [ ] `just check`  
- [ ] Multi-node reschedule integration (exit criteria)  

---

## 7. Testing plan

| Kind | Case |
| ---- | ---- |
| Unit | unbind; sticky pick_node; residual/load; restart policy matrix |
| Unit | HealthSpec validation |
| Integration | Two-node → NotReady A → instance on B |
| Integration | Sticky volumes → NotReady → no hop to B |
| Integration | `never` + NotReady → no rebind |
| Integration | secrets/apply still pass after Sync change |
| Lab (optional) | Real crash recreate; exec health |

CI remains **hypervisor-free**.

---

## 8. Risks

| Risk | Mitigation |
| ---- | ---------- |
| Double-create / name on revive | Unbind first; revived node Sync empty → ensure_removed |
| Thrash | Keep 45s heartbeat grace |
| Health exec needs agentd | Only when Running |
| Sticky without real mounts | Document follow-up for msb volume wiring |

---

## 9. Commit slice (single branch)

1. `feat(store): unbind_instance for reschedule`  
2. `feat(server): reschedule NotReady + periodic scheduler`  
3. `feat(agent): honor restartPolicy + Sync failed instances`  
4. `feat(agent): optional exec health checks`  
5. `feat(scheduler): volume sticky placement`  
6. `test: multi-node reschedule harness`  
7. `docs: Phase 6 notes`

---

## 10. Success criteria

- [ ] `restartPolicy: never` does not recreate after Failed  
- [ ] `on-failure` recreates after crash path  
- [ ] Exec health failure triggers policy path  
- [ ] NotReady → non-sticky instance on another Ready node (automated)  
- [ ] Volume-bearing service does not hop on NotReady  
- [ ] `just check` green  
- [ ] `mcc-mvp.md` Phase 6 done + implementation notes  

---

## 11. Defaults if unapproved open points

| Topic | Default |
| ----- | ------- |
| Sticky definition | Non-empty `service.volumes` |
| `never` + node loss | No reschedule; Failed message on dead node |
| Health consecutive fails | 1 failure = unhealthy |
| msb volume mount in create | Best-effort; stickiness not blocked |

---

## Approval

**Please review.** Implementation starts only after explicit approval.
After approval: write plan to `docs/tasks/phase-6-restart-health-reschedule.md` and begin Task A.
