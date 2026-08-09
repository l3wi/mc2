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
| `mc2` | on your PATH | same |

```bash
# Host readiness
mc2 doctor
# optional: mc2 doctor --msb   # check msb CLI on PATH
```

The hypervisor is required to **run sandboxes**; the REST/scheduler side still serves without one (instances fail with a runtime error instead).

## Install

Prebuilt binaries for Linux (amd64/arm64) and macOS (Apple Silicon) ship with
every release:

```bash
curl -fsSL https://github.com/l3wi/mc2/releases/latest/download/mc2-installer.sh | sh
```

Or grab the checksummed `.tar.xz` for your platform from
[releases](https://github.com/l3wi/mc2/releases). All commands below assume
`mc2` is on your PATH.

## Run it (open lab install)

```bash
DATA=/tmp/mc2-lab
mkdir -p "$DATA"

# Terminal 1 — the orchestrator (no API token)
mc2 server \
  --data-dir "$DATA" \
  --bind 127.0.0.1:7443 \
  --no-auth

# Terminal 2 — operator
export MC2_API=http://127.0.0.1:7443
mc2 node ls
mc2 up -f examples/01-hello-service/stack.yaml
mc2 ps
curl -s "$MC2_API/v1/status" | jq .
```

Useful server flags (all also env vars): `--node-name`, `--label KEY=VALUE`,
`--volume-dir` (durable volume root), `--ingress-config-dir` (Traefik files),
`--reconcile-interval-secs`.

### Secrets (optional)

```bash
mc2 secret set SMOKE_TOKEN --value 'lab-only'
mc2 up -f examples/02-secrets/stack.yaml
```

Secrets are a **server-wide** store: `mc2 secret set` writes once, any stack
references it by name. Values are encrypted at rest and never echoed back
(`mc2 secret ls` lists names only). The guest sees a placeholder and the real
value is attached only on connections to `allowHosts` hosts. See
[Environment variables vs secrets](./secrets.md) — and note that an explicit
`environment:` entry overrides a colliding `secrets[].env`.

### Compose vocabulary (environment, healthcheck, depends_on)

The stack schema is Compose-shaped. A two-service example with health-gated
startup ordering lives in [examples/07-startup-ordering/](../../examples/07-startup-ordering/):

```bash
mc2 up -f examples/07-startup-ordering/stack.yaml
mc2 ps
# web stays "Pending: depends_on: waiting for db (service_healthy)"
# until the db healthcheck passes, then converges to Running.
```

The pattern it shows:

```yaml
services:
  db:
    image: postgres:16
    expose: [5432]
    environment:                 # map or KEY=VALUE list; replaces the old env:
      POSTGRES_PASSWORD: lab
    healthcheck:                 # interval / timeout / retries / start_period / disable
      test: ["pg_isready", "-q", "-U", "postgres"]
      interval: 5s
      timeout: 3s
      retries: 5
  web:
    image: python:3.12-alpine
    scale: 3                     # per-replica published ports: 8080, 8081, 8082
    ports:
      - "8080:8000"
    depends_on:
      db:
        condition: service_healthy
    command: 'exec python -m http.server 8000'   # string or list form
```

Notes:

- `environment:` (not `env:`), `command` string or list, full `healthcheck`
  field set, and `depends_on` (`service_started` or `service_healthy`) are
  first-class. Unknown keys are rejected loudly.
- With `scale > 1` and published ports, each replica gets a distinct host port:
  fixed `8080` becomes the block `8080, 8081, 8082`; target-only ports get a
  distinct auto host port. Cross-service host-port conflicts are rejected at
  apply. Ingress routes for a scaled service target replica 0's port.

### Service networks (east–west)

Compose-like multi-service connectivity without a flat pod network. A service
declares `expose` (internal listeners); every other service that shares a
**network** reaches it by default (default-allow, Docker-style) via the node
loop's L4 splice + DNS (`svc.<network>.svc.mc2`). Named networks are
server-wide, so stacks can share one.

```bash
mc2 up -f examples/03-networks/stack.yaml
mc2 ps
# Network membership (default + named) with member instances and ports:
mc2 network
mc2 network smoke-networks

# From the client sandbox:
mc2 exec smoke-networks/client/0 wget -qO- http://echo.smoke-networks.svc.mc2:8080/
# expect: NETWORK_OK
```

`mc2 network` shows every network and its member instances; `mc2 network <name>`
drills into one; `mc2 network <stack>/<service>/<ordinal>` shows one instance's
observed exposes/edges.

### Ingress (HTTP via Traefik files)

North–south HTTP: declare `ports:` + stack `ingress:`; the server writes Traefik files when `--ingress-config-dir` is set. Operator guide: [examples/04-http-ingress/README.md](../../examples/04-http-ingress/README.md).

```bash
mkdir -p /tmp/mc2-ingress
# Start the server with:
#   --ingress-config-dir /tmp/mc2-ingress

mc2 up -f examples/04-http-ingress/stack.yaml
mc2 ps
curl -s "$MC2_API/v1/ingress" | jq .
# After instance Running and host port live:
cat /tmp/mc2-ingress/catalog.json | jq .
curl -s http://127.0.0.1:18080/ | head   # direct backend
# Point Traefik file provider at /tmp/mc2-ingress — see examples/04-http-ingress/
```

**Defaults:** default-allow within a shared network (Docker-style); networks
are server-wide. Exposed guest ports are effectively unique server-wide — the
second service to claim an already-bound port reports a `Failed` network edge.

## Auth-enabled install

Omit `--no-auth` on first bootstrap. Save the printed **API token** (operator REST bearer).

```bash
export MC2_API=http://127.0.0.1:7443
export MC2_API_KEY='mc2at_…'
mc2 up -f examples/90-advanced/demo-reference.yaml
```

## Setup wizard

`mc2 setup` is an interactive wizard with two trees that scaffolds config and
prints instructions (it never starts the server or Traefik):

```bash
mc2 setup                 # choose a tree interactively
mc2 setup server          # jump straight to the server tree
mc2 setup client          # jump straight to the client tree
```

**Server tree** (run on the VPS): answer the prompts — bind, data dir, API key
on/off, public hostname, cert resolver, ingress dir — and mc2 writes a
ready-to-run default `traefik.static.yml` (next to the ingress dir, once; it
never overwrites an existing one), prints the runnable `mc2 server …` command,
and a numbered finish-setup checklist: bootstrap (token prints once) → start
Traefik → DNS + ports 80/443 → then run the client tree on your laptop.

**Client tree** (run locally): give a context name, the control-plane URL and
the API key; mc2 saves and activates the context (`~/.mc2/config.toml`) and
optionally verifies the connection with a live status check.

Prompts use arrow keys; `Enter` accepts defaults. Non-TTY stdin (e.g. a script)
fails with a friendly "run it in a terminal" message instead of hanging.

## Remote management

The CLI is a REST client, so the same commands manage a remote install — the
server is just a URL swap. Two modes, reported by `mc2 status`:

- **local** — loopback API (`127.0.0.1` / `localhost` / `::1`), token optional.
- **remote** — any other host; requires https + an API key (plaintext `http://`
  is refused unless you opt in with `--allow-insecure-http` /
  `MC2_ALLOW_INSECURE_HTTP=1`).

### 1. On the server host

Bootstrap with auth (omit `--no-auth`; save the printed token), then start with
the ingress catalog + a public hostname:

```bash
mc2 server \
  --data-dir /srv/mc2 \
  --ingress-config-dir /srv/mc2-ingress \
  --public-hostname mc2.example.com \
  --public-tls-cert-resolver le
```

`--public-hostname` makes the server publish a synthetic control-plane route
through the Traefik catalog (TLS via the cert resolver, backend = the REST
listener). Point your existing Traefik file provider at
`/srv/mc2-ingress` and it terminates TLS for `https://mc2.example.com`. The
hostname is persisted and served at `/v1/status` as `publicHostname`.

> **Chicken-and-egg:** the client needs the route live on first setup. Get the
> hostname working once (cert resolver + DNS), then everything below is
> repeatable.

### 2. On the client machine

Save a named context once, then switch:

```bash
mc2 context set prod --api https://mc2.example.com --token 'mc2at_…'
mc2 context use prod
mc2 status                      # remote: https://mc2.example.com (context: prod)
mc2 node ls && mc2 ps
mc2 up -f examples/01-hello-service/stack.yaml
```

Contexts live in `~/.mc2/config.toml` (mode 0600). Resolution precedence:
`--api`/`--token` flags > `MC2_API`/`MC2_API_KEY` env > `--context`/`MC2_CONTEXT`
> the `current` context > the loopback default. `mc2 context ls` shows each
context with its mode.

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
- [Environment variables vs secrets](./secrets.md)
- [examples/07-startup-ordering](../../examples/07-startup-ordering/) — `depends_on` + healthcheck + per-replica ports
