# MCC MVP — MicroCommandControl

**Status:** **Approved — implementing** (Phases 0–3 complete)  
**Date:** 2026-08-06  
**Name:** **MCC** = **MicroCommandControl**  
**Goal:** Self-hosted, K3s-shaped command & control for [microsandbox](https://docs.microsandbox.dev) microVMs — home lab first, expandable later.

**Inputs:**  
- [docs/research/findings.md](../research/findings.md)  
- [docs/research/decisions.md](../research/decisions.md) (D1–D11)  
- [docs/research/microsandbox.md](../research/microsandbox.md)  
- [docs/research/orchestration-review.md](../research/orchestration-review.md)

---

## 1. Problem

Microsandbox is an excellent **execution primitive** (libkrun microVMs, OCI images, host network policy, secret injection). It is **not** a multi-node orchestrator.

**MicroCommandControl (MCC)** adds:

- Desired-state control plane  
- Node agents  
- Scheduling  
- Compose-like stacks  
- Cluster secrets (refs + injection)  
- Basic exposure (ports)  
- Unified OTLP metrics with msb  

…without reimplementing the VMM or becoming full Kubernetes.

---

## 2. Success criteria (MVP done when)

An operator can:

1. Install **one dual-mode binary** `mcc` on Linux (amd64/arm64) and **macOS Apple Silicon**.  
2. Run `mcc server` on one machine (SQLite, TLS, API token).  
3. Join one or more nodes with `mcc agent --server … --token …` (join token → node credential).  
4. `mcc secret set …` and reference secrets from a stack YAML (values never in git).  
5. `mcc apply -f stack.yaml` and have sandboxes **scheduled**, **created via embedded microsandbox**, and **reconciled** to desired replicas.  
6. Publish ports and reach a service from the host/LAN as configured.  
7. `mcc status` / `mcc ps` without a metrics backend; optionally emit **OTLP** alongside msb-metrics.  
8. Survive agent restart and server restart (desired state in SQLite; agents reconverge).  
9. Local dev via **justfile** + cargo.

---

## 3. Non-goals (MVP)

| Out of scope | Why |
| ------------ | --- |
| Full Kubernetes / kubectl API | Ops *shape* only (D4) |
| Microsandbox cloud nodes | Local only; abstract runtime for later (D2) |
| Ingress implementation | Design object only (D7); ports ship |
| Multi-user RBAC / OIDC | Single-admin (D1, D6) |
| HA control plane (Raft/etcd) | SQLite single server (D5) |
| Vault / SOPS | Plugin later (D9) |
| Service mesh / flat sandbox network | Explicit exposure only |
| Multi-node shared RWX storage | Node-local volumes; sticky schedule |
| Windows agents | Later if msb WHP is solid |
| Intel Mac | Apple Silicon only for darwin |
| Agent inside Docker as happy path | Needs host hypervisor (D11) |
| Bundled Grafana | Example collector config only |

---

## 4. Architecture

```
                    ┌─────────────────────────────────────┐
                    │  Human: CLI / REST (bearer token)   │
                    │  mcc apply | secret | status | …    │
                    └─────────────────┬───────────────────┘
                                      │ HTTPS REST
                    ┌─────────────────▼───────────────────┐
                    │  mcc server  (MicroCommandControl)  │
                    │  - desired state (storage trait)    │
                    │  - SQLite default                   │
                    │  - scheduler (spread)               │
                    │  - encrypted secrets                │
                    │  - reconcile loop                   │
                    │  - OTLP metrics                     │
                    │  - gRPC API for agents (mTLS)       │
                    └─────────────────┬───────────────────┘
                                      │ gRPC + mTLS
              ┌───────────────────────┼───────────────────────┐
              ▼                       ▼                       ▼
        ┌──────────┐            ┌──────────┐            ┌──────────┐
        │mcc agent │            │mcc agent │            │mcc agent │
        │ Linux    │            │ Linux    │            │ macOS    │
        │ KVM      │            │ KVM      │            │ HVF      │
        └────┬─────┘            └────┬─────┘            └────┬─────┘
             │ embed                 │                       │
             ▼                       ▼                       ▼
        microsandbox            microsandbox            microsandbox
        sandboxes               sandboxes               sandboxes
```

**Binary:** single Rust crate workspace, dual-mode:

```bash
mcc server [flags]
mcc agent  [flags]
mcc apply -f stack.yaml
mcc …   # operator commands → REST
```

**Process split:** server holds truth; agents execute. No sandbox scheduling on the server host unless an agent also runs there.

---

## 5. Object model

### 5.1 Core types

| Object | Role |
| ------ | ---- |
| **Cluster** | Implicit single cluster per server (no multi-cluster in MVP) |
| **Node** | Registered agent: name, labels, arch, capacity, status, last_heartbeat |
| **Stack** | Named app (Compose project analog); owns services |
| **Service** | Desired sandbox template + replica count + placement + ports + secrets + network profile |
| **SandboxInstance** | Concrete scheduled unit: service + ordinal + node + msb name/id + phase |
| **Volume** | Named volume claim; **node-local** in MVP; bind schedule to node once provisioned |
| **Secret** | Name + encrypted payload + metadata (not mount-as-file semantics; msb injection) |
| **Ingress** | **Schema only** in MVP (reserved resource / YAML key documented; controller no-op or reject “not implemented”) |

### 5.2 Instance phases

```
Pending → Scheduled → Creating → Running → (Stopping) → Stopped
                ↘ Failed
NodeLost → (reschedule if policy allows)
```

### 5.3 Compose-like YAML (v1 schema draft)

Study Compose/Swarm/Nomad/K8s semantics; adapt to msb. Proposed shape:

```yaml
# stack.yaml
apiVersion: mcc/v1
kind: Stack
metadata:
  name: demo
  labels:
    env: lab

services:
  web:
    image: python:3.12
    replicas: 2
    resources:
      cpus: 1
      memoryMiB: 512
    # Placement
    nodeSelector:
      role: worker
    # nodeName: mac-mini-1   # optional hard pin
    ports:
      - host: 8080          # host port on the *node*
        guest: 8000
        protocol: tcp
        # bind: 0.0.0.0     # default 127.0.0.1 for safety? see open detail below
    network:
      profiles: [public]    # msb profiles: public | private | host | none-style
      # rules: []           # optional low-level later
    env:
      APP_ENV: production   # non-secret env only
    secrets:
      - name: GITHUB_TOKEN  # MCC secret object name
        env: GITHUB_TOKEN   # guest env placeholder binding
        allowHosts:
          - api.github.com
    volumes:
      - name: web-data      # MCC volume
        mount: /data
        # kind: dir | disk
    restartPolicy: on-failure   # always | on-failure | never
    health:
      kind: exec              # exec | none (msb ping is infrastructure)
      command: ["curl", "-sf", "http://127.0.0.1:8000/health"]
      intervalSeconds: 30
    labels:
      app: web
    command: ["python", "-m", "http.server", "8000"]  # optional

volumes:
  web-data:
    kind: dir                 # node-local named volume via msb

# ingress:                 # reserved — not implemented in MVP
#   rules: ...
```

**Mapping to microsandbox:**

| Stack field | msb concept |
| ----------- | ----------- |
| image | OCI image |
| resources.cpus/memoryMiB | cpus / memory |
| ports | publish host:guest |
| network.profiles | NetworkPolicy profiles |
| secrets + allowHosts | host-side secret injection |
| volumes | named / bind mounts |
| labels | msb labels + MCC attribution |
| command | exec / image CMD handling via create options |

**Sandbox naming:** deterministic, unique per cluster, e.g. `{stack}-{service}-{ordinal}` (msb 128-byte name limit).

---

## 6. Control plane internals

### 6.1 Storage (D5)

```rust
// Conceptual
trait Store {
  // nodes, stacks, services, instances, volumes, secrets meta, …
}
struct SqliteStore { … }
// Later: PostgresStore
```

- Migrations via `sqlx` or `refinery`  
- All server logic depends on `Store`, not SQLite types  

### 6.2 Scheduler (D8)

1. List Pending instances (or scale-up deltas).  
2. **Filter:** Ready nodes, arch match, label selector, residual CPU/memory, volume affinity if volume already on node.  
3. **Score:** prefer spread (minimize instances of same service on same node).  
4. **Pin:** `nodeName` → only that node or stay Pending.  
5. Bind instance → node; agent creates sandbox.  

**Reschedule:** heartbeat miss beyond grace → mark node NotReady → instances with `restartPolicy` / reschedule policy → new Pending (except volume-sticky without migration).

### 6.3 Reconcile loop

Server-side loop (or event-driven + periodic):

- Desired services × replicas vs instances  
- Create/delete instance rows  
- Push assignments to agents (pull model preferred: agent watches/syncs “desired for me”)  
- GC stopped instances when scale down  

**Agent pull model (recommended):** agent periodically (or stream) fetches desired instance specs for its node_id; reports status. Survives NAT/Tailscale better than server-push-only.

### 6.4 Secrets (D9)

- Algorithm proposal: **XChaCha20-Poly1305** (or AES-GCM) with key from `MCC_SECRETS_KEY` (32-byte) or key file.  
- Store ciphertext + nonce in DB.  
- REST never returns raw secret values after create.  
- gRPC `GetTaskSpec` includes injection material only to owning agent over mTLS.  
- Agent calls microsandbox secret APIs — **not** plain guest env for injected secrets.  

### 6.5 Auth (D6)

| Actor | Mechanism |
| ----- | --------- |
| Bootstrap | `mcc server init` writes API token + join token (print once / store hashed) |
| Human REST | `Authorization: Bearer <api_token>` |
| Agent join | `Join(token, node_name, labels, arch, capacity)` → node cert or signed JWT + TLS |
| Agent ongoing | mTLS client cert **or** rotating bearer bound to node_id |

Prefer **mTLS** if complexity allows in MVP; else **join → long-lived node token over TLS** is acceptable v1 (document threat model).

### 6.6 REST surface (operator)

Minimum:

| Method | Path | Purpose |
| ------ | ---- | ------- |
| POST | `/v1/stacks:apply` | Apply YAML (server-side parse) or structured JSON |
| GET | `/v1/stacks` | List |
| GET | `/v1/stacks/{name}` | Detail + status |
| DELETE | `/v1/stacks/{name}` | Tear down |
| GET | `/v1/nodes` | List nodes |
| POST | `/v1/secrets/{name}` | Set secret (body) |
| DELETE | `/v1/secrets/{name}` | Delete |
| GET | `/v1/status` | Cluster summary |

CLI wraps these (`mcc apply` can also send raw file).

### 6.7 gRPC surface (agent)

| RPC | Purpose |
| --- | ------- |
| `Join` | Register node |
| `Heartbeat` | Capacity + health |
| `Sync` / `WatchDesired` | Desired instances for this node |
| `ReportStatus` | Instance phases, ports, errors, resource usage summary |

Proto package: `mcc.agent.v1`.

---

## 7. Agent & microsandbox embed

### 7.1 Runtime trait (D2, D3)

```text
trait NodeRuntime {
  create(spec) -> id
  start/stop/remove
  status
  configure_secrets_injection
  // later: CloudRuntime
}
struct MicrosandboxRuntime { /* embed SDK */ }
```

### 7.2 Embed strategy

1. **Preferred:** depend on `microsandbox` Rust crates; pin version in `Cargo.toml`.  
2. **Fallback:** subprocess `msb` with same logical operations if embed blocks MVP.  
3. Each MCC instance ↔ one msb sandbox name; detached mode so agent restart can reattach via list/get.  

### 7.3 Platforms

| Platform | Agent | Notes |
| -------- | ----- | ----- |
| Linux + KVM | Yes | Primary workers |
| macOS Apple Silicon + HVF | Yes | First-class lab node |
| Server without hypervisor | Yes | Control plane only |

`mcc doctor` (or agent startup checks): hypervisor available, msb runtime files, disk paths.

---

## 8. Metrics (D10)

- Instrument server + agent with OpenTelemetry OTLP export (`OTEL_EXPORTER_OTLP_ENDPOINT` or `MCC_OTLP_ENDPOINT`).  
- Metrics examples: `mcc_reconcile_duration`, `mcc_schedule_attempts`, `mcc_instances`, `mcc_agent_heartbeat`, API request counts.  
- Attributes: `stack`, `service`, `node`, `phase`.  
- Document running **msb-metrics** for sandbox CPU/mem/net; example collector config accepting both.  
- No Prom scrape *required* for MVP (OTLP-first); optional Prom later.  

---

## 9. Packaging & dev (D11)

| Deliverable | Detail |
| ----------- | ------ |
| Workspace | `cargo` workspace: `mcc` bin, `mcc-server`, `mcc-agent`, `mcc-api` (proto), `mcc-store`, … (split as needed, avoid over-fragmentation) |
| justfile | `just build`, `test`, `fmt`, `lint`, `run-server`, `run-agent`, `integration` |
| CI | build/test linux; cross or native darwin-arm64 if available |
| Releases | GH Actions → binaries: `linux-amd64`, `linux-arm64`, `darwin-arm64` |
| Version pin | Document microsandbox crate/git rev per MCC release |
| Optional | Dockerfile for **server only** |

---

## 10. Implementation phases

MVP is **sliced** so each phase is demoable. Prefer working end-to-end thin vertical slices over perfect layers.

### Phase 0 — Repo skeleton

- [x] Cargo workspace, dual-mode CLI (`server` / `agent` / placeholder apply)  
- [x] Layout: `crates/`, `docs/` (markdown only), `examples/`, `proto/`  
- [x] justfile (`build`, `test`, `check`, `run-server`, `run-agent`), README (MicroCommandControl), LICENSE, `.gitignore`  
- [x] CI: Rust only (`fmt`, `clippy`, `test`)  
- [x] **No** docs app / bun workspace in this repo  

**Exit:** `just build` produces `mcc`. ✅

#### Implementation notes (Phase 0)

- **Branch:** `feat/phase-0-repo-skeleton`
- **Workspace crates:** `mcc` (bin), `mcc-server`, `mcc-agent`, `mcc-api`, `mcc-store`
- **CLI:** clap dual-mode — `server`, `agent`, `apply -f`, `version`; stubs use `--dry-run` for CI smoke
- **Store:** `Store` trait + `MemoryStore` only (SQLite in Phase 1)
- **Proto:** `proto/mcc/agent/v1/agent.proto` placeholder (no tonic codegen yet)
- **Dev:** `just build|test|fmt|lint|check|run-server|run-agent`; `.github/workflows/ci.yml`
- **Verify:** `just check` green; `target/debug/mcc` runs

### Phase 1 — Store + server process

- [x] Storage trait + SQLite implementation + migrations  
- [x] Server bootstrap: data dir, generate API token + join token  
- [x] REST: health, status stub, auth middleware  
- [x] Config: bind addr, data dir, secrets key path  

**Exit:** `mcc server` listens; `curl` with bearer hits `/v1/status`. ✅

#### Implementation notes (Phase 1)

- **Branch:** `feat/phase-1-server-store`
- **Store:** `Store` trait; `SqliteStore` (sqlx + `migrations/001_init.sql`); `MemoryStore` for unit tests
- **Tokens:** SHA-256 hashes in `cluster_meta`; plaintext printed once on first init (`mccat_*` / `mccjt_*`)
- **Secrets key:** 32-byte file at `<data_dir>/secrets.key` (mode 0600), for Phase 5
- **REST (axum, plain HTTP for now):**
  - `GET /health` — no auth
  - `GET /v1/status` — `Authorization: Bearer <api_token>`
- **Flags:** `--bind`, `--data-dir`, `--secrets-key-path`, `--init-only`, `--dry-run`
- **Verify:** `just check`; e2e curl against ephemeral data dir

### Phase 2 — Agent join + heartbeat

- [x] gRPC service + TLS (dev: generated self-signed CA in data dir)  
- [x] Join flow + node persistence  
- [x] Heartbeat + NotReady detection  
- [x] `mcc agent` flags; `mcc node ls` via REST  

**Exit:** two terminals — server + agent; node shows Ready. ✅

#### Implementation notes (Phase 2)

- **Branch:** `feat/phase-2-agent-join`
- **gRPC:** tonic `mcc.agent.v1.AgentService` (Join, Heartbeat; Sync/ReportStatus stubs)
- **TLS:** default on — rcgen CA + server cert in `<data_dir>/tls/`; agents use `--tls-ca`; `--grpc-plain` for h2c/tests
- **Auth:** join token → long-lived `mccnt_*` node token (hashed); re-join by name rotates token
- **Store:** `upsert_node_join`, `heartbeat_node`, `list_nodes`, `mark_stale_nodes` (grace default 45s, watcher 5s)
- **REST:** `GET /v1/nodes` (bearer); status counts ready/total
- **CLI:** `mcc node ls --api … --token …` (env `MCC_API`, `MCC_API_TOKEN`)
- **Tests:** unit (memory/sqlite stale); integration `tests/tests/agent_join.rs`; process e2e verified

### Phase 3 — Apply stack + schedule (no msb yet)

- [x] YAML parse → Stack/Service models  
- [x] Instance creation for replicas  
- [x] Scheduler filters + spread + pin  
- [x] Agent Sync receives **mock** runtime tasks; reports Running (fake)  

**Exit:** `mcc apply -f examples/demo.yaml` places instances on nodes; status shows Running (simulated). ✅

#### Implementation notes (Phase 3)

- **Branch:** `feat/phase-3-apply-schedule`
- **YAML:** `mcc_api::parse_stack_yaml` (`mcc/v1` Stack); rejects `ingress`
- **Store:** `instances` + stack upsert; `reconcile_service_replicas`; bind/update status
- **Scheduler:** Ready filter, `nodeName` pin, `nodeSelector`, residual CPU/mem, spread by service load
- **REST:** `POST /v1/stacks:apply` `{yaml}`, `GET /v1/instances`
- **CLI:** `mcc apply -f …`, `mcc ps`
- **Agent:** Sync + mock `ReportStatus` → Running (`mock://…` runtime id)
- **Tests:** unit parse/scheduler; integration `apply_schedule.rs`

### Phase 4 — Microsandbox runtime

- [ ] `MicrosandboxRuntime` embed (or msb CLI fallback)  
- [ ] Create/stop/remove real sandboxes from service spec  
- [ ] Ports, resources, env, network profiles  
- [ ] Labels; deterministic names  
- [ ] Reattach after agent restart  

**Exit:** real microVM runs; published port works on node.

### Phase 5 — Secrets

- [ ] Encrypted secret store  
- [ ] `mcc secret set`  
- [ ] Apply refs → agent injection config → msb secrets  
- [ ] Ensure REST/YAML never echo values  

**Exit:** sandbox calls allowlisted host with injected secret; guest env shows placeholder only.

### Phase 6 — Restart, health, reschedule

- [ ] restartPolicy enforcement  
- [ ] Optional exec health check  
- [ ] Node loss → reschedule (non-sticky)  
- [ ] Volume sticky placement  

**Exit:** kill agent process / mark node down; replacement instance on another node (multi-node test).

### Phase 7 — OTLP + polish

- [ ] OTLP metrics from server + agent  
- [ ] Example collector config (msb + mcc)  
- [ ] Ingress type stub in schema + clear error if used  
- [ ] `mcc doctor`  
- [ ] Release workflow for three targets  
- [ ] End-to-end docs: quickstart (Linux + macOS notes)  

**Exit:** MVP acceptance criteria §2 all met.

---

## 11. Suggested repo layout (MCC only)

**Decision:** Keep the **docs app/site separate** from this repository. This repo is MicroCommandControl **code + engineer markdown** (`docs/` for research, tasks, architecture, guides as plain MD). A product docs site (bun/Next/Fumadocs/etc.) lives in **another repo** when you want it — post-MVP is fine.

```text
mcc/
  README.md
  justfile                   # rust only (build, test, lint, run-server, …)
  Cargo.toml                 # Rust workspace root

  crates/                    # Rust (MicroCommandControl)
    mcc/                     # bin: CLI entry (server | agent | apply …)
    mcc-server/
    mcc-agent/
    mcc-api/                 # prost/tonic protos + REST types
    mcc-store/
    mcc-scheduler/           # optional; merge into server until needed
    mcc-runtime/             # NodeRuntime + microsandbox impl

  proto/
    mcc/agent/v1/agent.proto

  docs/                      # in-repo markdown (not a docs app)
    research/
    tasks/
    architecture/            # filled during impl
    guides/                  # quickstart as MD (filled in MVP)
    reference/               # stack YAML, CLI notes as MD

  examples/
    stacks/demo.yaml
    otel/collector.yaml

  .github/workflows/
    ci.yml                   # rust only
    release.yml              # mcc binaries
```

Keep Rust crate count minimal if it slows MVP — merge scheduler into server until it hurts.

### 11.1 Docs app (separate)

| Concern | Stance |
| ------- | ------ |
| **This repo** | Plain markdown under `docs/`; README quickstart |
| **Docs app** | **Separate repository** — not part of MCC MVP scaffold |
| **Sync later** | Copy/submodule/`docs` publish pipeline when the site exists |
| **MVP acceptance** | Does **not** require a running docs website |

**justfile** (this repo):

```text
just build        # cargo build
just test         # cargo test
just check        # fmt clippy test
just run-server
just run-agent
```---

## 12. Testing strategy

**Policy:** use clean, directed **unit tests** and **integration tests** to ensure consistency and stop regressions. See [docs/guides/testing.md](../guides/testing.md).

| Level | What | Location |
| ----- | ---- | -------- |
| Unit | Scheduler filters/scoring; YAML parse; secret encrypt/decrypt; token hash | `crates/*/src` `#[cfg(test)]` |
| Store | SQLite migrations + CRUD + reopen | unit + `tests/tests/store_persistence.rs` |
| Integration | Server HTTP/auth; later agent join; mock runtime (no KVM in CI) | `tests/` (`mcc-tests`), `crates/mcc/tests/` |
| Manual / lab | Real msb on Linux KVM + macOS Apple Silicon | outside CI |
| Optional CI | Linux nested-virt only if available; otherwise label `msb` tests ignored | — |

**Harness:** `mcc_tests::TestCluster` (temp data dir, real SQLite, real TCP). **Commands:** `just test`, `just test-unit`, `just test-integration`, `just check`.

---

## 13. Risks & mitigations

| Risk | Mitigation |
| ---- | ---------- |
| microsandbox beta API churn | Pin version; runtime trait; CLI fallback |
| Embed complexity / link issues | Phase 3 mock first; Phase 4 fallback subprocess |
| macOS signing / HVF entitlements | Follow msb docs; document `codesign` if needed |
| Secret key loss | Document backup; accept re-entry in v1 |
| Port bind defaults (127.0.0.1 vs 0.0.0.0) | Default loopback; explicit bind in YAML for LAN |
| Density (process per sandbox) | Document limits; capacity from agent |

---

## 14. Open details (resolve during implementation, not blockers)

1. **Default host port bind:** `127.0.0.1` (safer) vs `0.0.0.0` (lab convenience) — **recommend 127.0.0.1** with YAML override.  
2. **mTLS vs node token** for agent auth — prefer mTLS if bootstrap UX stays simple.  
3. **Exact OTLP attribute names** — match msb-metrics conventions when wiring.  
4. **Image pull credentials** — secret ref type `registry` vs node-local msb config (prefer CP secret ref if easy).  
5. **Scale-to-zero / idle_timeout** — pass through msb idle/max_duration as optional service fields.  

---

## 15. Documentation deliverables (with MVP)

- [ ] README: what is MicroCommandControl, quickstart  
- [ ] In-repo markdown only (`docs/architecture`, `docs/guides`, `docs/reference`) — **no docs app in this repo**  
- [ ] `docs/architecture/overview.md` — diagram + objects  
- [ ] `docs/reference/stack-spec.md` — YAML reference  
- [ ] Quickstart under `docs/guides/` (single-node + two-node; Linux + macOS notes)  
- [ ] Update [docs/research/README.md](../research/README.md) link to this task  
- [ ] Separate docs site repo: **out of MVP** (optional later)  

---

## 16. Definition of done checklist

- [ ] Name **MicroCommandControl (MCC)** used in README and binary help  
- [ ] Dual-mode binary; Rust-focused repo (`crates/` + `docs/` markdown); justfile; CI green  
- [ ] SQLite store behind trait  
- [ ] Join + heartbeat + Ready/NotReady  
- [ ] apply YAML → schedule → real msb sandboxes  
- [ ] Secrets encrypted + injection  
- [ ] Ports published  
- [ ] Restart/reschedule basics  
- [ ] OTLP from mcc; docs align with msb-metrics  
- [ ] linux-amd64, linux-arm64, darwin-arm64 release path  
- [ ] Ingress stub only  
- [ ] This task file updated with “Implementation notes” as phases complete  

---

## 17. Post-MVP roadmap (not scheduled now)

1. Ingress controller (Caddy/Traefik)  
2. Postgres store backend  
3. Multi-user tokens / RBAC  
4. Cloud `NodeRuntime`  
5. Snapshot-based warm pools  
6. Optional K8s RuntimeClass / Nomad driver experiments  

---

## Approval

**Approved** by operator (“Lets get started on the phases”) — 2026-08-06.  

Implementation in progress. Update this file’s phase checklists and Implementation notes as phases complete.

---

## Implementation log

| Phase | Status | Notes |
| ----- | ------ | ----- |
| 0 | **done** | Workspace + dual-mode CLI + justfile + CI |
| 1 | **done** | SQLite store, bootstrap tokens, axum `/health` + `/v1/status` |
| 2 | **done** | gRPC join/heartbeat, TLS lab certs, NotReady watcher, `mcc node ls` |
| 3 | **done** | apply YAML, spread scheduler, mock agent Running |
| 4 | next | real microsandbox runtime |
| 5–7 | pending | — |
| testing | **done** | `tests/` harness + CLI smoke; [docs/guides/testing.md](../guides/testing.md) |
