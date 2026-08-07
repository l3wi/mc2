# MicroCommandControl (MC2)

Self-hosted, K3s-shaped **command & control** for [microsandbox](https://docs.microsandbox.dev) microVMs.

MC2 adds a desired-state control plane, node agents, scheduling, Compose-like stacks, cluster secrets, port exposure, same-node **mediated service fabric** (`expose` / `allow` / `*.svc.mc2` DNS), and OTLP metrics — without reimplementing the VMM or becoming full Kubernetes.

## Quick start (dev)

Requirements: Rust **1.91+**, [just](https://github.com/casey/just). Full walkthrough: [docs/guides/quickstart.md](docs/guides/quickstart.md).

```bash
just build
DATA=/tmp/mc2-dev
./target/debug/mc2 doctor

# Terminal 1 — control plane
# Lab (no tokens): --no-auth
# Default: prints API + join tokens once on first bootstrap
./target/debug/mc2 server --data-dir "$DATA" --bind 127.0.0.1:7443 --grpc-bind 127.0.0.1:7444 --grpc-plain --no-auth

# Terminal 2 — agent (embeds microsandbox SDK; needs KVM / Apple Silicon HVF)
./target/debug/mc2 agent \
  --server http://127.0.0.1:7444 \
  --name "$(hostname)"
# With auth: add --token "<join-token>" and --tls-ca when using HTTPS

# Terminal 3 — operator (token only if server was not --no-auth)
export MC2_API=http://127.0.0.1:7443
# export MC2_API_KEY="<api-token>"   # when auth is enabled
./target/debug/mc2 node ls
./target/debug/mc2 apply -f examples/stacks/smoke.yaml
./target/debug/mc2 ps
curl -s "$MC2_API/v1/status"
# after demo is Running: curl -s http://127.0.0.1:8080/
```

Optional OTLP (server + agent):

```bash
export MC2_OTLP_ENDPOINT=http://127.0.0.1:4317
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
| `mc2 server` | Control plane (SQLite, REST, gRPC) |
| `mc2 agent` | Node worker (join, heartbeat, msb runtime) |
| `mc2 node ls` | List nodes via REST |
| `mc2 apply -f stack.yaml` | Apply desired stack (schedule + run) |
| `mc2 secret set\|ls\|rm` | Cluster secrets (encrypted; values never listed) |
| `mc2 ps` | List instances / instance phases |
| `mc2 doctor` | Host / msb readiness checks |
| `mc2 ssh key\|open\|close\|ls` | SSH keys + open/close endpoints (agent serves via microsandbox SDK) |

**Service fabric (same-node):** stack YAML `expose` + client `allow` → agent L4 splice + guest DNS (`db.<stack>.svc.mc2`). Default deny east–west; multi-node deferred. See [examples/stacks/smoke-fabric.yaml](examples/stacks/smoke-fabric.yaml).

**Ingress (same-node):** stack `ingress:` + `ports:` → agent writes Traefik/Caddy file catalog (`--ingress-config-dir`). BYO proxy for TLS/L7. See [examples/ingress/](examples/ingress/) and [examples/stacks/smoke-ingress.yaml](examples/stacks/smoke-ingress.yaml).

One dual-mode binary for operators and nodes.

## Repository layout

```text
mc2/
  crates/           # Rust workspace (mc2 bin, server, agent, api, store)
  proto/            # gRPC agent API
  docs/guides/      # Operator guides (quickstart, testing)
  examples/         # Stack YAML, ingress, OTEL samples
  justfile          # build, test, check, run-server, run-agent
```

**Docs site / Next.js app:** lives in a **separate repository** (not this one).

## License

MIT — see [LICENSE](LICENSE).
