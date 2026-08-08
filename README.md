# MicroCommandControl (MC2)

Self-hosted **command & control** for [microsandbox](https://docs.microsandbox.dev) microVMs — one process, MSB-embedded style.

MC2 adds a desired-state control plane with a local node, scheduling, Compose-like stacks, cluster secrets, port exposure, same-node **mediated service fabric** (`expose` / `allow` / `*.svc.mc2` DNS), and OTLP metrics — without reimplementing the VMM or becoming full Kubernetes.

The server **embeds** the microsandbox SDK directly (no daemon, no separate agent): `mc2 server` is scheduler + SQLite + REST + the local node reconcile loop that fork+execs `msb sandbox` microVMs.

## Quick start (dev)

Requirements: Rust **1.91+**, [just](https://github.com/casey/just), a hypervisor (Linux KVM / Apple Silicon HVF) for running sandboxes. Full walkthrough: [docs/guides/quickstart.md](docs/guides/quickstart.md).

```bash
just build
DATA=/tmp/mc2-dev
./target/debug/mc2 doctor

# Terminal 1 — control plane + local node (one process)
# Lab (no token): --no-auth
# Default: prints the API token once on first bootstrap
./target/debug/mc2 server --data-dir "$DATA" --bind 127.0.0.1:7443 --no-auth

# Terminal 2 — operator (token only if server was not --no-auth)
export MC2_API=http://127.0.0.1:7443
# export MC2_API_KEY="<api-token>"   # when auth is enabled
./target/debug/mc2 node ls
./target/debug/mc2 apply -f examples/01-hello-service/stack.yaml
./target/debug/mc2 ps
curl -s "$MC2_API/v1/status"
# after hello is Running: curl -s http://127.0.0.1:18091/
```

Optional OTLP:

```bash
export MC2_OTLP_ENDPOINT=http://127.0.0.1:4317
# advanced collector example: examples/90-advanced/observability/collector-config.yaml
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
| `mc2 server` | Control plane + local node (SQLite, REST, embedded msb runtime) |
| `mc2 node ls` | List nodes via REST |
| `mc2 apply -f stack.yaml` | Apply desired stack (schedule + run) |
| `mc2 secret set\|ls\|rm` | Cluster secrets (encrypted; values never listed) |
| `mc2 ps` | List instances / instance phases |
| `mc2 doctor` | Host / msb readiness checks |
| `mc2 ssh key\|open\|close\|ls` | SSH keys + open/close endpoints (served via microsandbox SDK) |

**Service fabric (same-node):** stack YAML `expose` + client `allow` → L4 splice + guest DNS (`db.<stack>.svc.mc2`). Default deny east–west; multi-node deferred. See [examples/03-service-fabric/](examples/03-service-fabric/).

**Ingress (same-node):** stack `ingress:` + `ports:` → the server writes a Traefik file-provider catalog (`--ingress-config-dir`). See [examples/04-http-ingress/](examples/04-http-ingress/).

**Examples:** start with [examples/01-hello-service/](examples/01-hello-service/); incomplete workflows are marked under [examples/90-advanced/](examples/90-advanced/).

One binary: server and operator CLI.

## Repository layout

```text
mc2/
  crates/           # Rust workspace (mc2 bin, server, api, store, runtime, metrics)
  docs/guides/      # Operator guides (quickstart, testing)
  examples/         # Stack YAML, ingress, OTEL samples
  justfile          # build, test, check, run-server
```

**Docs site / Next.js app:** lives in a **separate repository** (not this one).

## License

MIT — see [LICENSE](LICENSE).
