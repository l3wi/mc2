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
cargo run -p mcc -- doctor
# optional: cargo run -p mcc -- doctor --msb   # check msb CLI on PATH
```

Agent hosts need a hypervisor. The **server** can run without KVM/HVF (control plane only).

## Build

```bash
just build
# or: cargo build -p mcc
```

Release binaries (CI): `linux-amd64`, `linux-arm64`, `darwin-arm64` — see `.github/workflows/release.yml`.

## Single-node (open lab cluster)

```bash
DATA=/tmp/mcc-lab
mkdir -p "$DATA"

# Terminal 1 — control plane (no tokens)
./target/debug/mcc server \
  --data-dir "$DATA" \
  --bind 127.0.0.1:7443 \
  --grpc-bind 127.0.0.1:7444 \
  --grpc-plain \
  --no-auth

# Terminal 2 — agent (same machine; needs hypervisor)
./target/debug/mcc agent \
  --server http://127.0.0.1:7444 \
  --name "$(hostname -s 2>/dev/null || hostname)"

# Terminal 3 — operator
export MCC_API=http://127.0.0.1:7443
./target/debug/mcc node ls
./target/debug/mcc apply -f examples/stacks/smoke.yaml
./target/debug/mcc ps
curl -s "$MCC_API/v1/status" | jq .
```

### Secrets (optional)

```bash
./target/debug/mcc secret set SMOKE_TOKEN --value 'lab-only'
./target/debug/mcc apply -f examples/stacks/smoke-secrets.yaml
```

### Service fabric (same-node east–west)

Compose-like multi-service connectivity without a flat pod network. Stack declares `expose` (internal listeners) and client `allow` edges; the agent L4-splices and injects DNS (`db.<stack>.svc.mcc`). Design: [service-fabric.md](../research/service-fabric.md).

```bash
./target/debug/mcc apply -f examples/stacks/smoke-fabric.yaml
./target/debug/mcc ps
# Fabric status (agent-reported):
curl -s "$MCC_API/v1/instances/<client-instance-id>/fabric" | jq .

# From the client sandbox (lab helper):
cargo run -p mcc-runtime --example msb_shell -- smoke-fabric-client-0 \
  'wget -qO- http://echo.smoke-fabric.svc.mcc:8080/'
# expect: FABRIC_OK
```

**Defaults:** deny east–west until `allow`; same-stack + same-node only; no multi-node fabric yet.

## Auth-enabled cluster

Omit `--no-auth` on first bootstrap. Save the printed **API token** and **join token**.

```bash
export MCC_API=http://127.0.0.1:7443
export MCC_API_KEY='mccat_…'
./target/debug/mcc apply -f examples/stacks/demo.yaml

# Agent
./target/debug/mcc agent \
  --server https://127.0.0.1:7444 \
  --token 'mccjt_…' \
  --tls-ca "$DATA/tls/ca.pem" \
  --name worker-1
```

Default gRPC uses lab TLS under `<data-dir>/tls/`. Use `--grpc-plain` only for local h2c.

## Two-node sketch

1. Run `mcc server` on the control host.  
2. On each worker: install `mcc`, run `mcc doctor`, then `mcc agent --server … --token …`.  
3. Apply a stack once; scheduler spreads replicas across Ready nodes.  
4. If a node goes NotReady (missed heartbeats), non-sticky instances reschedule to another Ready node (Phase 6).

## Metrics (OTLP)

```bash
# Terminal: collector (example)
# otelcol --config examples/otel/collector-config.yaml

export MCC_OTLP_ENDPOINT=http://127.0.0.1:4317
# restart server + agent so they pick up the endpoint
```

MCC exports **control-plane / agent** metrics (`mcc.server.*`, `mcc.agent.*`). Sandbox CPU/mem/net remain on **msb-metrics** — configure that sidecar to the same collector for a unified view. See [examples/otel/collector-config.yaml](../../examples/otel/collector-config.yaml).

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
- [MVP task / phases](../tasks/mcc-mvp.md)  
- [Decisions](../research/decisions.md)  
