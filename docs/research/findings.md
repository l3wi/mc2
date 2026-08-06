# Findings — Microsandbox C2 (MCC)

**Date:** 2026-08-06  
**Scope:** Research only (no implementation).  
**Goal of eventual product:** Self-hosted command & control for microsandbox microVMs, home lab → production, K3s/microK8s-class simplicity.

---

## 1. Executive summary

Microsandbox is an excellent **execution and isolation primitive**: OCI-compatible, hardware-isolated microVMs via libkrun, strong host-brokered networking and secret injection, solid CLI/SDKs, single-host lifecycle. It is **not** a cluster orchestrator.

The industry already split these concerns:

- **Isolation:** runc / gVisor / Kata / Firecracker / libkrun  
- **Orchestration:** Swarm / Nomad / Kubernetes (and light distros)

**MCC should be the orchestration layer**, not a fork of the VMM. Best MVP inspiration:

| Take from | What |
| --------- | ---- |
| **K3s** | Install UX, single binary, embedded DB, node agent model |
| **Compose / Swarm** | Stack files, services, replicas, restart policies |
| **Nomad** | “Task driver = microsandbox” mental model |
| **Microsandbox itself** | Secrets, network policy, labels, metrics, snapshots |

Avoid early: full Kubernetes API compatibility, flat pod networks, multi-node CSI complexity.

---

## 2. Microsandbox — what we can rely on

### Strengths (build on these)

1. **True microVM isolation** — guest kernel per sandbox; host process unprivileged (needs KVM/HVF/WHP only).
2. **OCI workflow** — reuse images, registries, mental model from Docker.
3. **Embeddable local path** — no mandatory daemon for single-app use; process-per-sandbox architecture.
4. **Network policy on host** — public-by-default with private/metadata blocked; composable profiles + first-match rules; TLS/DNS controls.
5. **Secret injection** — credentials never need to enter the guest; placeholder swap at allowlisted hosts. **This is a differentiator** vs K8s Secrets.
6. **Lifecycle primitives** — create/start/stop/drain/detach/idle/max_duration/modify; enough hooks for a reconciler.
7. **Volumes & snapshots** — persistence and warm baselines on a single host.
8. **Multi-language SDKs + REST cloud API shape** — control plane can speak SDK or spawn `msb`.
9. **Observability hooks** — metrics, labels, OTLP sidecar path.
10. **Active project** — weekly releases, YC-backed, Apache-2.0 local runtime.

### Gaps (MCC must provide)

| Gap | Why it matters for “K3s-like” |
| --- | ----------------------------- |
| Multi-node awareness | Homelab fleets / HA / capacity |
| Desired-state store + reconcile | Survive reboots, agent crashes, human error |
| Scheduler / placement | CPU/RAM packing, pin to nodes, spread |
| Stack / multi-sandbox apps | Real apps = many sandboxes + volumes + ports |
| Service exposure story | Ingress / reverse proxy / stable names |
| Health + restart policies | Production resilience |
| Cluster identity & auth | Multi-user home lab / small team prod |
| Central config & secret refs | Don’t scatter host env vars |
| Upgrade / rollout of stacks | Versioned deploys |
| Capacity & quota accounting | Prevent host OOM across tenants |
| Multi-node storage plan | Later phase |

### Risks / constraints

- **Beta upstream** — pin versions; abstract SDK behind MCC interfaces.
- **Platform split** — Linux KVM vs macOS Apple Silicon vs Windows WHP; control plane may be Linux-first for “nodes”.
- **No GPU** path called out as mature — AI inference hosts may stay outside sandbox fleet initially.
- **Cloud feature lag** — design for local/self-host first; cloud API is a parallel backend, not the C2.
- **Process-per-sandbox** — large density needs careful process/cgroup accounting at node agent level.
- **Snapshots local-only** — multi-node “move sandbox” needs volume/snapshot distribution design.

---

## 3. Orchestration landscape — condensed findings

### Isolation (solved for MCC)

| Tech | Role |
| ---- | ---- |
| Containers (runc) | Shared kernel — weaker for untrusted code |
| gVisor | Syscall interception — middle ground |
| Firecracker | MicroVMM — isolation only, DIY orchestration |
| Kata | MicroVMs **under Kubernetes** |
| **Microsandbox / libkrun** | MicroVMs **with app-grade DX** |

For untrusted / multi-tenant / AI agent code, microVM class is the right choice. Microsandbox already made that call.

### Orchestration (to learn from)

| System | Lesson for MCC |
| ------ | -------------- |
| **Docker Compose** | Humans want a small YAML of services |
| **Docker Swarm** | Replicas + restart is enough for many prod cases |
| **Nomad** | Driver abstraction; simple servers/clients; not everything is a container forever |
| **Kubernetes** | Desired state, labels, probes, controllers — patterns gold, API surface heavy |
| **K3s** | Proof that “real orchestrator” can still be one-command and small |
| **MicroK8s** | Add-on UX is delightful; Snap coupling is not |
| **K0s** | Upstream purity vs batteries-included tradeoff |
| **RKE2** | Compliance features = later, not homelab MVP |

### Anti-patterns for this project

1. **Reimplement Kubernetes** — years of work; wrong default networking for adversarial sandboxes.
2. **Ignore microsandbox secrets/network model** — folding into “just env vars” throws away the product’s best security story.
3. **Require etcd on day one** — K3s shows SQLite is fine for small clusters.
4. **Flat cluster network by default** — K8s habit; bad default for untrusted sandboxes.
5. **Orchestrate without a node agent** — pure SSH-from-controller is brittle; agent is the durable pattern.

---

## 4. Recommended architectural stance (research conclusion)

### Positioning

> **MCC is to microsandbox what K3s is to containerd/runc** — a small, self-hosted control plane and node agent for scheduling and reconciling microVM sandboxes across machines.

Not: a new sandbox runtime.  
Not: a full K8s distribution (unless a later compatibility layer).

### Suggested MVP components

```
┌─────────────────────────────────────────┐
│ mcc control plane (API + scheduler +    │
│   desired-state store)                  │
└───────────────┬─────────────────────────┘
                │ gRPC/HTTP
     ┌──────────┼──────────┐
     ▼          ▼          ▼
┌─────────┐ ┌─────────┐ ┌─────────┐
│ mcc-agent│ │ mcc-agent│ │ mcc-agent│
│ (node)  │ │ (node)  │ │ (node)  │
│  ↓      │ │  ↓      │ │  ↓      │
│ msb SDK │ │ msb SDK │ │ msb SDK │
│ sandboxes│ │sandboxes│ │sandboxes│
└─────────┘ └─────────┘ └─────────┘
```

**MVP objects (Compose-shaped):**

- `Node` — registered agent, capacity, labels
- `Sandbox` / `Service` — image, resources, network profile, secret refs, mounts
- `Stack` — multi-service app (like compose project)
- `Volume` — named volume claim → node-local provision first

**MVP behaviors:**

- Declarative apply (`mcc apply -f stack.yaml`)
- Reconcile to desired count (replicas)
- Restart policy + health (ping + optional exec probe)
- Place by resources + node selector
- Expose via host ports + optional reverse proxy later
- Preserve microsandbox network profiles + secret host allowlists in the API

**Explicit non-goals for MVP:**

- Full K8s API / CRI
- Multi-node RWX storage
- Service mesh
- GPU scheduling
- Windows nodes (optional later)

### Homelab → production path

| Phase | Capability |
| ----- | ---------- |
| 0 | Research (this doc set) |
| 1 | Single-node agent + declarative stacks (Compose power) |
| 2 | Multi-node + scheduler + simple HA control plane |
| 3 | Ingress, metrics aggregation, RBAC, upgrades |
| 4 | Optional K8s RuntimeClass / Nomad driver for ecosystem |

---

## 5. Open questions (for plan phase)

1. **Control plane language/stack** — Rust (align with msb), Go (K8s/Nomad tradition), or TypeScript (homelab DX)?
2. **API style** — custom REST/gRPC vs subset of K8s API vs Nomad job API?
3. **State store** — SQLite → Postgres, or embed NATS/Raft early?
4. **How agent talks to msb** — embed Rust SDK vs shell out to `msb` vs agentd sockets?
5. **Identity** — mTLS agent↔server, API tokens, SSO later?
6. **Network exposure** — Traefik/Caddy as first-class, or only published ports?
7. **Tenancy** — single-user lab first, or orgs from day one?
8. **Upstream coupling** — support only local backend, or also drive microsandbox cloud as a “node type”?

These should be decided in a formal plan (`docs/tasks/...`) before implementation.

---

## 6. Document map

| File | Contents |
| ---- | -------- |
| [README.md](./README.md) | Index |
| [microsandbox.md](./microsandbox.md) | Upstream architecture & API broad strokes |
| [orchestration-review.md](./orchestration-review.md) | Docker/K8s/Nomad/Kata/Firecracker/gVisor review |
| [findings.md](./findings.md) | This synthesis |

---

## 7. Sources (high signal)

- https://docs.microsandbox.dev (+ `/llms.txt` full index)
- https://docs.microsandbox.dev/security/overview.md
- https://docs.microsandbox.dev/security/isolation.md
- https://docs.microsandbox.dev/sandboxes/lifecycle.md
- https://docs.microsandbox.dev/networking/overview.md
- https://docs.microsandbox.dev/sandboxes/secrets.md
- https://docs.microsandbox.dev/configuration.md
- https://docs.rs/microsandbox-core
- https://github.com/superradcompany/microsandbox
- Kata / Firecracker / gVisor comparisons (e.g. Northflank isolation guides)
- Lightweight K8s distro comparisons (K3s, K0s, MicroK8s, RKE2)
