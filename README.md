# MicroCommandControl (MCC)

Self-hosted, K3s-shaped **command & control** for [microsandbox](https://docs.microsandbox.dev) microVMs.

MCC adds a desired-state control plane, node agents, scheduling, Compose-like stacks, cluster secrets, port exposure, and OTLP metrics — without reimplementing the VMM or becoming full Kubernetes.

> **Status:** Phases 0–1 done (server + SQLite + REST). See [docs/tasks/mcc-mvp.md](docs/tasks/mcc-mvp.md).

## Quick start (dev)

Requirements: Rust (1.80+), [just](https://github.com/casey/just).

```bash
just build          # produces target/debug/mcc
./target/debug/mcc --help
./target/debug/mcc version

# First run prints API + join tokens once (save them)
just run-server -- --data-dir /tmp/mcc-dev --bind 127.0.0.1:7443

# In another shell:
curl -s http://127.0.0.1:7443/health
curl -s -H "Authorization: Bearer <api-token>" http://127.0.0.1:7443/v1/status
```

```bash
just check          # fmt + clippy + test
just init-server -- --data-dir /tmp/mcc-dev   # tokens only, no listen
```

## Binary modes

| Command | Role |
| ------- | ---- |
| `mcc server` | Control plane (SQLite, REST) — **Phase 1** |
| `mcc agent` | Node worker (join, heartbeat, microsandbox) — **Phase 2+** |
| `mcc apply -f stack.yaml` | Apply desired stack — **Phase 3+** |

One dual-mode binary for operators and nodes.

## Repository layout

```text
mcc/
  crates/           # Rust workspace (mcc bin, server, agent, api, store)
  proto/            # gRPC agent API (implemented Phase 2)
  docs/             # Engineer markdown (research, tasks, architecture)
  examples/stacks/  # Example stack YAML
  justfile          # build, test, check, run-server, run-agent
```

**Docs site / Next.js app:** lives in a **separate repository** (not this one).

## Design notes

- Research: [docs/research/](docs/research/)
- MVP plan: [docs/tasks/mcc-mvp.md](docs/tasks/mcc-mvp.md)
- Decisions: [docs/research/decisions.md](docs/research/decisions.md)

## License

MIT — see [LICENSE](LICENSE).
