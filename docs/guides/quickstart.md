# Quickstart — MicroCommandControl

Single-machine path for **Linux (KVM)** and **macOS Apple Silicon (HVF)**.

MC2 is a **YAML-driven orchestration system for microsandbox**: one process
(`mc2 server`) that keeps state in SQLite, serves REST, and drives sandboxes
through the embedded microsandbox SDK — no separate agent, no gRPC seam.

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

The hypervisor is required to **run sandboxes**; the REST/scheduler side still serves without one (instances fail with a runtime error instead).

## Build

```bash
just build
# or: cargo build -p mc2
```

Release binaries (CI): `linux-amd64`, `linux-arm64`, `darwin-arm64` — see `.github/workflows/release.yml`.

## Run it (open lab install)

```bash
DATA=/tmp/mc2-lab
mkdir -p "$DATA"

# Terminal 1 — the orchestrator (no API token)
./target/debug/mc2 server \
  --data-dir "$DATA" \
  --bind 127.0.0.1:7443 \
  --no-auth

# Terminal 2 — operator
export MC2_API=http://127.0.0.1:7443
./target/debug/mc2 node ls
./target/debug/mc2 apply -f examples/01-hello-service/stack.yaml
./target/debug/mc2 ps
curl -s "$MC2_API/v1/status" | jq .
```

Useful server flags (all also env vars): `--node-name`, `--label KEY=VALUE`,
`--volume-dir` (durable volume root), `--ingress-config-dir` (Traefik files),
`--reconcile-interval-secs`.

### Secrets (optional)

```bash
./target/debug/mc2 secret set SMOKE_TOKEN --value 'lab-only'
./target/debug/mc2 apply -f examples/02-secrets/stack.yaml
```

### Service fabric (east–west)

Compose-like multi-service connectivity without a flat pod network. Stack declares `expose` (internal listeners) and client `allow` edges; the node loop L4-splices and injects DNS (`db.<stack>.svc.mc2`).

```bash
./target/debug/mc2 apply -f examples/03-service-fabric/stack.yaml
./target/debug/mc2 ps
# Fabric status (node-reported):
curl -s "$MC2_API/v1/instances/<client-instance-id>/fabric" | jq .

# From the client sandbox (lab helper):
cargo run -p mc2-runtime --example msb_shell -- smoke-fabric-client-0 \
  'wget -qO- http://echo.smoke-fabric.svc.mc2:8080/'
# expect: FABRIC_OK
```

### Ingress (HTTP via Traefik files)

North–south HTTP: declare `ports:` + stack `ingress:`; the server writes Traefik files when `--ingress-config-dir` is set. Operator guide: [examples/04-http-ingress/README.md](../../examples/04-http-ingress/README.md).

```bash
mkdir -p /tmp/mc2-ingress
# Start the server with:
#   --ingress-config-dir /tmp/mc2-ingress

./target/debug/mc2 apply -f examples/04-http-ingress/stack.yaml
./target/debug/mc2 ps
curl -s "$MC2_API/v1/ingress" | jq .
# After instance Running and host port live:
cat /tmp/mc2-ingress/catalog.json | jq .
curl -s http://127.0.0.1:18080/ | head   # direct backend
# Point Traefik file provider at /tmp/mc2-ingress — see examples/04-http-ingress/
```

**Defaults:** deny east–west until `allow`; same-stack only.

## Auth-enabled install

Omit `--no-auth` on first bootstrap. Save the printed **API token** (operator REST bearer).

```bash
export MC2_API=http://127.0.0.1:7443
export MC2_API_KEY='mc2at_…'
./target/debug/mc2 apply -f examples/90-advanced/demo-reference.yaml
```

## Metrics (OTLP)

```bash
# Terminal: collector (example)
# otelcol --config examples/90-advanced/observability/collector-config.yaml

export MC2_OTLP_ENDPOINT=http://127.0.0.1:4317
# restart the server so it picks up the endpoint
```

MC2 exports orchestrator + node metrics (`mc2.server.*`, `mc2.node.*`). Sandbox CPU/mem/net remain on **msb-metrics** — configure that sidecar to the same collector for a unified view. See [the advanced observability example](../../examples/90-advanced/observability/).

## Platform notes

### Linux

- Ensure your user can open `/dev/kvm` (often `kvm` group).
- Nested virt required if MC2 runs inside a VM without passthrough.

### macOS (Apple Silicon)

- arm64 is first-class.
- Intel Mac is **out of scope** for MVP.

## Next reading

- [Testing](./testing.md)
- [Stack YAML reference](./stack-yaml.md)
