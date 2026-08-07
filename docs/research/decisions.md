# MCC Design Decisions (Interview)

**MCC** = **MicroCommandControl**

Resolved from open questions in [findings.md](./findings.md).  
**Date:** 2026-08-06  
**Style:** Single-admin home lab → expand later; **K3s-like ops shape**, not full Kubernetes API.  
**MVP plan:** [docs/tasks/mcc-mvp.md](../tasks/mcc-mvp.md)

---

## D1 — Tenancy & audience

| | |
| - | - |
| **Decision** | Single-admin / single-trust-domain for v1 |
| **Expand later** | Multi-user, RBAC, orgs — without rewriting the core object model |
| **Not v1** | Multi-tenant platform (orgs/projects/billing) |

---

## D2 — What is a node?

| | |
| - | - |
| **Decision** | **Local only** — machines you control running local microsandbox |
| **Expand later** | Microsandbox cloud (or other) as a pluggable **runtime / node type** |
| **Not v1** | Hybrid scheduling to cloud |

Interface hint: abstract `NodeRuntime` so cloud can plug in without changing Stack/Sandbox objects.

---

## D3 — Language & runtime coupling

| | |
| - | - |
| **Decision** | **Rust** for control plane and agent |
| **msb integration** | Prefer **embed / link** microsandbox (SDK/crates); single-binary or dual-mode binary (`server` / `agent`) is desired |
| **Fallback** | Thin spawn of `msb` only if embed is painful during upstream beta |
| **Rationale** | Stay close to msb; one stack; embed path for single-binary flow |

---

## D4 — API & UX

| | |
| - | - |
| **Human** | **Compose-like YAML** — e.g. `mcc apply -f stack.yaml` |
| **Operator API** | Custom **REST** (CLI/UI/automation) |
| **Agent protocol** | Custom **gRPC** (typed reconcile, status) |
| **Not v1** | kubectl / full Kubernetes API compatibility |

**Design note:** Study structure and semantics from Compose, Swarm, Nomad, and Kubernetes (services, replicas, restarts, health, volumes, labels, desired state). **Adapt** to microsandbox boundaries and quirks:

- MicroVM per sandbox (not shared-kernel containers)
- Host-brokered network policy (no default flat pod network)
- Host-side secret placeholders / allowlisted injection
- Local volumes, snapshots, process-per-sandbox density
- Publish-port model vs ClusterIP

Do not copy foreign networking or secret models blindly.

---

## D5 — State store

| | |
| - | - |
| **Default v1** | **SQLite** embedded in control plane |
| **Architecture** | **Storage-agnostic** persistence layer (traits) so backends can be swapped later (e.g. Postgres) |
| **Not v1** | Raft/etcd multi-server HA control plane |

Agents: local cache + report status; **desired state lives on the control plane**.

---

## D6 — Identity & trust

| | |
| - | - |
| **Agents** | Join with **join token** → long-lived **node credential**; **TLS** always on agent↔server |
| **Humans / CLI** | **API bearer token(s)** for REST |
| **Deployment** | Prefer LAN / Tailscale / localhost bind; not “open on the internet unauthenticated” |
| **Later** | Token scopes, RBAC, OIDC/SSO |
| **Not v1** | Per-user RBAC, SSO, SPIFFE-style identity mesh |

---

## D7 — Workload network exposure

| | |
| - | - |
| **v1** | **Published ports only** (north–south) — MCC stores desired host↔guest mappings and node placement |
| **Soon after** | **Ingress-shaped object** designed early; implement with Caddy/Traefik (or BYO proxy) |
| **East–west** | See **D13** — mediated service fabric (not CNI / not flat mesh) |
| **Not v1** | Service mesh, cluster-wide overlay, flat all-to-all sandbox network |

Preserve microsandbox egress defaults and policies; exposure is explicit.

---

## Implied architecture (from decisions)

```
mcc (Rust, dual-mode binary — linux + darwin-arm64)
├── server  — REST + gRPC, SQLite (storage trait), reconcile, spread scheduler,
│             encrypted secrets, OTLP metrics
└── agent   — gRPC/TLS to server, embed microsandbox (KVM or macOS HVF),
              secret injection config, OTLP (+ msb-metrics alignment)

Human:  msb-like DX → mcc apply -f stack.yaml  + bearer token
Agent:  join token → node creds → desired sandboxes + status
Node:   local msb only (cloud runtime driver later)
Expose: ports v1 (N–S); mediated fabric D13 (E–W, post-MVP phases);
        Ingress object designed for proxy next
Tele:   unified OTLP path (mcc + msb-metrics)
Dev:    justfile + cargo; GH release binaries for install
```

---

## D8 — Scheduler (v1)

| | |
| - | - |
| **Filters** | Capacity (CPU/memory), `nodeSelector` / labels, architecture |
| **Default score** | **Spread** across eligible nodes |
| **Pin** | `nodeName` always wins |
| **Reschedule** | On node Lost/NotReady after grace period; node-local volumes stay sticky |
| **Not v1** | Affinity DSL, taints/tolerations zoo, preemption |

---

## D9 — Secrets

| | |
| - | - |
| **YAML** | **References only** (secret name + injection allow_hosts); never raw values in stack files |
| **Store** | Control-plane secret objects; values **encrypted at rest** via storage layer |
| **Runtime** | Server delivers injection config to agent over mTLS gRPC; agent configures **msb host-side injection** (placeholders in guest) |
| **Bootstrap** | `mcc secret set`; master key from env/file (`MCC_SECRETS_KEY` / key file) |
| **Recovery v1** | Lose key ⇒ re-enter secrets (accepted) |
| **Not v1** | Vault / SOPS (design `SecretSource` plugin later) |

---

## D10 — Metrics

| | |
| - | - |
| **Primary path** | **OTLP**, aligned with **msb-metrics** / microsandbox observability |
| **MCC series** | Control plane + agent (reconcile, schedule, API, node health, counts, errors) on the **same OTLP pipeline** |
| **Sandbox series** | Stay on msb / `msb-metrics`; do not invent a parallel resource model |
| **Labels** | Propagate stack / service / node (and msb labels where applicable) |
| **Without collector** | CLI/API status still useful |
| **Not v1** | Bundled Grafana/TSDB; ship example collector config for msb + mcc |

---

## D11 — Packaging & platforms

| | |
| - | - |
| **Primary distribute** | GitHub **release binaries**; dual-mode `mcc` (`server` / `agent`) |
| **Arch** | **linux-amd64**, **linux-arm64**, **darwin-arm64** (Apple Silicon) |
| **msb** | Pin/vendor known-good microsandbox per MCC release |
| **Server optional** | Container image for control plane only (no KVM in that container) |
| **Agent** | Bare metal / host VM with hypervisor (KVM or macOS HVF) — not “agent in Docker” as happy path |
| **Local dev** | **justfile** (`just build`, `just test`, `just run-server`, …) + `cargo` |
| **Not v1 primary** | deb/rpm/nix/Homebrew (add when needed) |
| **Upgrade** | Re-run install / release binary; self-update later |

### macOS

- **In scope for v1** where microsandbox supports it: **Apple Silicon + Hypervisor.framework**.
- Control plane runs on macOS or Linux.
- Agent on Mac is a first-class lab node (dev laptop / Mac mini), subject to msb platform limits.
- **Not promised v1:** Intel Mac, Windows agents (revisit if msb WHP path is solid enough later).

---

## D12 — Repository shape

| | |
| - | - |
| **Decision** | **MCC repo stays focused** — Rust workspace + in-repo markdown (`docs/`) + examples |
| **Docs app / site** | **Separate repository** (or separate project later); not scaffolded inside MCC for MVP |
| **In this repo** | Engineer-facing markdown: research, tasks, architecture notes, guides as plain MD in `docs/` |
| **Rationale** | Clear boundary: control plane code vs product docs site; no bun/Next coupling in the MCC tree |

See [mcc-mvp.md §11](../tasks/mcc-mvp.md).

---

## D13 — East–west connectivity (mediated service fabric)

| | |
| - | - |
| **Decision** | **Host-mediated service fabric** — agent **userspace L4 splice** + **allow-gated DNS**, on msb per-sandbox gateway (smoltcp + policy + publish) |
| **Methodology** | Same as msb: guest speaks normal TCP; **no real inter-VM network** (no bridge, CNI, pod CIDR, overlay) |
| **Default** | **Deny east–west** until `expose` + explicit client `allow` |
| **YAML** | `expose` (internal loopback) separate from `ports:` (north–south); optional `networks` = membership only, **not** mesh |
| **DNS** | Short `db` + FQDN `db.<stack>.svc.mcc`; **NXDOMAIN** if client has no allow |
| **Replicas v1** | Single stable backend (e.g. lowest ready ordinal); headless/RR **later** |
| **Scope v1** | **Same-stack, same-node, TCP only**; multi-node fabric **deferred** (split peers → clear Failed message) |
| **Ports** | Agent **ephemeral** `127.0.0.1`; report observed; **bind/policy failure → Failed + clear message** (no silent degrade) |
| **msb profiles** | **Hard gate:** no `private` / `host` for fabric. If narrow DNS allow impossible → **do not ship** (no dirty fallback) |
| **Secrets** | Prefer fabric FQDN in `allowHosts` when injection applies |
| **vs Ingress** | Fabric first (E–W). Ingress later (D7) owns public HTTP(S) N–S; does not replace fabric |
| **First ship** | Full stack-local fabric (not spike-only); cleanliness gate must pass inside the work |
| **Not v1** | Multi-node tunnels, cross-stack allow, mesh-by-membership, VIP fabric, app traffic over agentd |
| **Design / task** | [service-fabric.md](./service-fabric.md) · [docs/tasks/service-fabric.md](../tasks/service-fabric.md) |

**Rationale:** Preserve msb’s untrusted-guest threat model while enabling Compose-like multi-service stacks. Prefer **no feature** over a feature that opens host/LAN.

**Operator decisions:** Q1–Q15 recorded in design doc §0 (2026-08-07).

**Phasing:** cleanliness gate → same-node stack fabric → (later) ordinals/RR → (later) multi-node → (later) Ingress N–S.

---

## Still open (plan-phase detail)

1. Exact Compose-like schema details beyond D13 locks (health probe shape, etc.)
2. Encryption algorithm / key rotation for secret store
3. gRPC service definitions and REST resource layout (fabric status fields)
4. Control-plane HA timeline (still “later”)
5. Exact OTLP attribute naming vs msb-metrics conventions
6. When/where to stand up the separate docs site repo (post-MVP OK)
7. **D13 gate result:** msb narrow DNS-only allow without host/private (technical, not product fork)
8. Multi-node fabric design when un-deferred
9. Ingress implementation (D7) when fabric N–S handoff is needed

---

## Source

Interview over open questions in findings §5 and follow-up (scheduler, secrets, metrics, packaging); research in [microsandbox.md](./microsandbox.md), [orchestration-review.md](./orchestration-review.md), [service-fabric.md](./service-fabric.md) (D13 + Q1–Q15).
