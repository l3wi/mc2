# MicroCommandControl (MCC)

Self-hosted, K3s-shaped **command & control** for [microsandbox](https://docs.microsandbox.dev) microVMs.

MCC adds a desired-state control plane, node agents, scheduling, Compose-like stacks, cluster secrets, port exposure, and OTLP metrics — without reimplementing the VMM or becoming full Kubernetes.

> **Status:** Phases 0–4 done (including real microsandbox via `msb` CLI). See [docs/tasks/mcc-mvp.md](docs/tasks/mcc-mvp.md).

## Quick start (dev)

Requirements: Rust (1.80+), [just](https://github.com/casey/just).

```bash
just build
DATA=/tmp/mcc-dev

# Terminal 1 — control plane (prints API + join tokens once)
./target/debug/mcc server --data-dir "$DATA" --bind 127.0.0.1:7443 --grpc-bind 127.0.0.1:7444

# Terminal 2 — agent (embeds microsandbox SDK; needs KVM / Apple Silicon HVF)
./target/debug/mcc agent \
  --server https://127.0.0.1:7444 \
  --tls-ca "$DATA/tls/ca.pem" \
  --token "<join-token>" \
  --name "$(hostname)"

# Terminal 3 — operator
export MCC_API=http://127.0.0.1:7443 MCC_API_TOKEN="<api-token>"
./target/debug/mcc node ls
./target/debug/mcc apply -f examples/stacks/demo.yaml
./target/debug/mcc ps
curl -s -H "Authorization: Bearer $MCC_API_TOKEN" "$MCC_API/v1/status"
```

Plain gRPC (no TLS) for local tests: add `--grpc-plain` on the server and use `--server http://127.0.0.1:7444` on the agent.

```bash
just check              # fmt + clippy + full test suite (regression gate)
just test-unit
just test-integration
```

**Testing:** clean, directed unit tests + integration tests to stop regressions — [docs/guides/testing.md](docs/guides/testing.md).

## Binary modes

| Command | Role |
| ------- | ---- |
| `mcc server` | Control plane (SQLite, REST, gRPC) |
| `mcc agent` | Node worker (join, heartbeat, msb runtime) |
| `mcc node ls` | List nodes via REST |
| `mcc apply -f stack.yaml` | Apply desired stack (schedule + run) |
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
