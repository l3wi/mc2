# MicroCommandControl (MC2)

A **YAML-driven orchestration system** for [microsandbox](https://docs.microsandbox.dev) microVMs. Declare services in a Compose-like stack file; MC2 schedules, runs, and reconciles them as detached microVMs on your machine.

MC2 gives you desired-state orchestration over microsandbox: stacks, replicas, restart policies, encrypted secrets, port exposure, a mediated **service fabric** (`expose` / `allow` / `*.svc.mc2` DNS), ingress via a Traefik file catalog, and OTLP metrics — without reimplementing the VMM and without becoming Kubernetes.

The server **embeds** the microsandbox SDK directly (no daemon, no separate worker process — MSB-embedded style): `mc2 server` is the orchestrator — SQLite state, REST API, scheduler, and the reconcile loop that fork+execs `msb sandbox` microVMs.

## Install

Prebuilt binaries for Linux (amd64/arm64) and macOS (Apple Silicon) ship with every release:

```bash
curl -fsSL https://github.com/l3wi/mc2/releases/latest/download/mc2-installer.sh | sh
```

Or grab the checksummed `.tar.xz` for your platform from [releases](https://github.com/l3wi/mc2/releases).

## Quick start (dev)

Requirements: Rust **1.91+**, [just](https://github.com/casey/just), a hypervisor (Linux KVM / Apple Silicon HVF) for running sandboxes. Full walkthrough: [docs/guides/quickstart.md](docs/guides/quickstart.md).

```bash
just build
DATA=/tmp/mc2-dev
./target/debug/mc2 doctor

# Terminal 1 — the orchestrator (one process)
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
| `mc2 server` | The orchestrator (SQLite, REST, scheduler, embedded msb runtime) |
| `mc2 node ls` | Show the local node (capacity, status) |
| `mc2 apply -f stack.yaml` | Apply desired stack (schedule + run) |
| `mc2 ps [--stack s] [--service s]` | List instances / phases |
| `mc2 status` | Health + version + counts |
| `mc2 fabric <instance>` | Observed fabric status (expose/allow) |
| `mc2 ingress` | Desired ingress routes |
| `mc2 secret set\|ls\|rm` | Secrets (encrypted; values never listed) |
| `mc2 ssh key\|open\|close\|ls` | SSH keys + open/close endpoints |
| `mc2 doctor` | Host / msb readiness checks |
| `mc2 completions <shell>` | Shell completions (bash/zsh/fish) |

All listing commands accept `-o json`. Instance commands accept
`<stack>/<service>/<ordinal>` in place of a UUID.

**Service fabric:** stack YAML `expose` + client `allow` → L4 splice + guest DNS (`db.<stack>.svc.mc2`). Default deny east–west. See [examples/03-service-fabric/](examples/03-service-fabric/).

**Ingress:** stack `ingress:` + `ports:` → the server writes a Traefik file-provider catalog (`--ingress-config-dir`). See [examples/04-http-ingress/](examples/04-http-ingress/).

**Examples:** start with [examples/01-hello-service/](examples/01-hello-service/); incomplete workflows are marked under [examples/90-advanced/](examples/90-advanced/).

One binary: orchestrator and operator CLI.

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
