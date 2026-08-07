# Research Index

Local research notes for **MCC** (**MicroCommandControl**) — a self-hosted orchestration layer aimed at home lab → production, similar in spirit to K3s / microK8s but for microsandbox microVMs rather than containers.

**MVP plan:** [docs/tasks/mcc-mvp.md](../tasks/mcc-mvp.md)  
**Repo shape:** MCC code repo only — docs *app/site* stays in a separate project (D12).

| Document | Purpose |
| -------- | ------- |
| [microsandbox.md](./microsandbox.md) | Architecture, APIs, security model, CLI/SDK surface of Microsandbox |
| [orchestration-review.md](./orchestration-review.md) | How Docker, Swarm, Kubernetes (full + light distros), Nomad, Kata, Firecracker, gVisor operate |
| [findings.md](./findings.md) | Synthesis: gaps in Microsandbox, what C2 must provide, recommended design stance |
| [ssh-control-plane.md](./ssh-control-plane.md) | Authorized key registry + open/close host SSH endpoints (design) |
| [service-fabric.md](./service-fabric.md) | East–west without a real network: mediated L4 fabric (D13; Q1–Q15 locked) |
| [decisions.md](./decisions.md) | Resolved design decisions D1–D13 |
| [../tasks/service-fabric.md](../tasks/service-fabric.md) | Implement plan: same-node stack-local fabric |

**Sources (primary):**

- Official docs: https://docs.microsandbox.dev (index: `/llms.txt`)
- GitHub: https://github.com/superradcompany/microsandbox
- crates: https://docs.rs/microsandbox-core
- Isolation comparison: Northflank / industry writeups on Kata, Firecracker, gVisor
- Homelab K8s distros: K3s, K0s, MicroK8s, RKE2 comparisons

**Researched:** 2026-08-06  
**Microsandbox status at research time:** beta / pre-1.0, active weekly releases through July 2026
