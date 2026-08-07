# Quickstart — MicroCommandControl

Single-node lab path for **Linux (KVM)** and **macOS Apple Silicon (HVF)**.

## Prerequisites

| | Linux | macOS |
| - | ----- | ----- |
| CPU | amd64 or arm64 | **Apple Silicon only** |
| Hypervisor | KVM (`/dev/kvm`) | Hypervisor.framework |
| Rust | 1.91+ (for build from source) | same |
| just | optional | optional |

```bash
# Host readiness
cargo run -p mc2 -- doctor
# optional: cargo run -p mc2 -- doctor --msb   # check msb CLI on PATH
```

Agent hosts need a hypervisor. The **server** can run without KVM/HVF (control plane only).

## Build

```bash
just build
# or: cargo build -p mc2
```

Release binaries (CI): `linux-amd64`, `linux-arm64`, `darwin-arm64` — see `.github/workflows/release.yml`.

## Single-node (open lab cluster)

```bash
DATA=/tmp/mc2-lab
mkdir -p "$DATA"

# Terminal 1 — control plane (no tokens)
./target/debug/mc2 server \
  --data-dir "$DATA" \
  --bind 127.0.0.1:7443 \
  --grpc-bind 127.0.0.1:7444 \
  --grpc-plain \
  --no-auth

# Terminal 2 — agent (same machine; needs hypervisor)
./target/debug/mc2 agent \
  --server http://127.0.0.1:7444 \
  --name "$(hostname -s 2>/dev/null || hostname)"

# Terminal 3 — operator
export MC2_API=http://127.0.0.1:7443
./target/debug/mc2 node ls
./target/debug/mc2 apply -f examples/01-hello-service/stack.yaml
./target/debug/mc2 ps
curl -s "$MC2_API/v1/status" | jq .
```

### Secrets (optional)

```bash
./target/debug/mc2 secret set SMOKE_TOKEN --value 'lab-only'
./target/debug/mc2 apply -f examples/02-secrets/stack.yaml
```

### Service fabric (same-node east–west)

Compose-like multi-service connectivity without a flat pod network. Stack declares `expose` (internal listeners) and client `allow` edges; the agent L4-splices and injects DNS (`db.<stack>.svc.mc2`).

```bash
./target/debug/mc2 apply -f examples/03-service-fabric/stack.yaml
./target/debug/mc2 ps
# Fabric status (agent-reported):
curl -s "$MC2_API/v1/instances/<client-instance-id>/fabric" | jq .

# From the client sandbox (lab helper):
cargo run -p mc2-runtime --example msb_shell -- smoke-fabric-client-0 \
  'wget -qO- http://echo.smoke-fabric.svc.mc2:8080/'
# expect: FABRIC_OK
```

### Ingress (same-node HTTP via Traefik files)

North–south HTTP: declare `ports:` + stack `ingress:`; the agent writes Traefik files when `--ingress-config-dir` is set. Operator guide: [examples/04-http-ingress/README.md](../../examples/04-http-ingress/README.md).

```bash
mkdir -p /tmp/mc2-ingress
# Restart agent with:
#   --ingress-config-dir /tmp/mc2-ingress

./target/debug/mc2 apply -f examples/04-http-ingress/stack.yaml
./target/debug/mc2 ps
curl -s "$MC2_API/v1/ingress" | jq .
# After instance Running and host port live:
cat /tmp/mc2-ingress/catalog.json | jq .
curl -s http://127.0.0.1:18080/ | head   # direct backend
# Point Traefik file provider at /tmp/mc2-ingress — see examples/04-http-ingress/
```

**Defaults:** deny east–west until `allow`; same-stack + same-node only; no multi-node fabric yet.

## Auth-enabled cluster

Omit `--no-auth` on first bootstrap. Save the printed **API token** and **join token**.

```bash
export MC2_API=http://127.0.0.1:7443
export MC2_API_KEY='mc2at_…'
./target/debug/mc2 apply -f examples/90-advanced/demo-reference.yaml

# Agent
./target/debug/mc2 agent \
  --server https://127.0.0.1:7444 \
  --token 'mc2jt_…' \
  --tls-ca "$DATA/tls/ca.pem" \
  --name worker-1
```

Default gRPC uses lab TLS under `<data-dir>/tls/`. Use `--grpc-plain` only for local h2c.

## Two-node sketch

1. Run `mc2 server` on the control host.  
2. On each worker: install `mc2`, run `mc2 doctor`, then `mc2 agent --server … --token …`.  
3. Apply a stack once; scheduler spreads replicas across Ready nodes.  
4. If a node goes NotReady (missed heartbeats), non-sticky instances reschedule to another Ready node.

## Metrics (OTLP)

```bash
# Terminal: collector (example)
# otelcol --config examples/90-advanced/observability/collector-config.yaml

export MC2_OTLP_ENDPOINT=http://127.0.0.1:4317
# restart server + agent so they pick up the endpoint
```

MC2 exports **control-plane / agent** metrics (`mc2.server.*`, `mc2.agent.*`). Sandbox CPU/mem/net remain on **msb-metrics** — configure that sidecar to the same collector for a unified view. See [the advanced observability example](../../examples/90-advanced/observability/).

## Platform notes

### Linux

- Ensure your user can open `/dev/kvm` (often `kvm` group).  
- Nested virt required if the agent runs inside a VM without passthrough.

### macOS (Apple Silicon)

- Agent is first-class on arm64.  
- Intel Mac is **out of scope** for MVP.  
- Server may run on the same Mac as the agent for single-node lab.

## Next reading

- [Testing](./testing.md)
- [Stack YAML reference](./stack-yaml.md)
