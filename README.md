# MicroCommandControl (MCC)

Self-hosted, K3s-shaped **command & control** for [microsandbox](https://docs.microsandbox.dev) microVMs.

MCC adds a desired-state control plane, node agents, scheduling, Compose-like stacks, cluster secrets, port exposure, and OTLP metrics — without reimplementing the VMM or becoming full Kubernetes.

> **Status:** MVP phases 0–7 complete. See [docs/tasks/mcc-mvp.md](docs/tasks/mcc-mvp.md).

## Quick start (dev)

Requirements: Rust **1.91+**, [just](https://github.com/casey/just). Full walkthrough: [docs/guides/quickstart.md](docs/guides/quickstart.md).

```bash
just build
DATA=/tmp/mcc-dev
./target/debug/mcc doctor

# Terminal 1 — control plane
# Lab (no tokens): --no-auth
# Default: prints API + join tokens once on first bootstrap
./target/debug/mcc server --data-dir "$DATA" --bind 127.0.0.1:7443 --grpc-bind 127.0.0.1:7444 --grpc-plain --no-auth

# Terminal 2 — agent (embeds microsandbox SDK; needs KVM / Apple Silicon HVF)
./target/debug/mcc agent \
  --server http://127.0.0.1:7444 \
  --name "$(hostname)"
# With auth: add --token "<join-token>" and --tls-ca when using HTTPS

# Terminal 3 — operator (token only if server was not --no-auth)
export MCC_API=http://127.0.0.1:7443
# export MCC_API_KEY="<api-token>"   # when auth is enabled
./target/debug/mcc node ls
./target/debug/mcc apply -f examples/stacks/smoke.yaml
./target/debug/mcc ps
curl -s "$MCC_API/v1/status"
# after demo is Running: curl -s http://127.0.0.1:8080/
```

Optional OTLP (server + agent):

```bash
export MCC_OTLP_ENDPOINT=http://127.0.0.1:4317
# collector example: examples/otel/collector-config.yaml
```

```bash
just check              # fmt + clippy + full test suite (regression gate)
just test-unit
just test-integration
```

**Testing:** [docs/guides/testing.md](docs/guides/testing.md) · **Quickstart:** [docs/guides/quickstart.md](docs/guides/quickstart.md)

## Binary modes

| Command | Role |
| ------- | ---- |
| `mcc server` | Control plane (SQLite, REST, gRPC) |
| `mcc agent` | Node worker (join, heartbeat, msb runtime) |
| `mcc node ls` | List nodes via REST |
| `mcc apply -f stack.yaml` | Apply desired stack (schedule + run) |
| `mcc secret set\|ls\|rm` | Cluster secrets (encrypted; values never listed) |
| `mcc ps` | List instances / phases |
| `mcc doctor` | Host / msb readiness checks |

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
