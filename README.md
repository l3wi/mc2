# MicroCommandControl (MC2)

A **Compose-shaped orchestration system for microsandbox microVMs**. Declare
services in a Docker-Compose-style stack file; MC2 schedules, runs, and
continuously reconciles them as detached, hardware-virtualized microVMs on your
machine — with container ergonomics and VM isolation.

You write Compose vocabulary (`services`, `scale`, `ports`, `expose`,
`environment`, `command`, `healthcheck`, `restart`, `depends_on`, `volumes`,
`networks`, `ingress`) and MC2 gives you desired-state orchestration over
microsandbox: replicas, restart policies, encrypted secrets, a mediated
**service network** (default-allow east–west), Traefik ingress, health-gated
startup ordering, and OTLP metrics — without reimplementing the VMM and without
becoming Kubernetes.

One process, no daemon, no agent: `mc2 server` is the orchestrator — SQLite
state, REST API, scheduler, and the reconcile loop that fork+execs `msb
sandbox` microVMs through the embedded microsandbox SDK.

## What you get

- **Compose-shaped stack YAML.** `environment:` (map/list), `command` string
  or list, `restart`, `scale`, `mem_limit`/`cpus`, `depends_on`, full
  `healthcheck` (`interval`/`timeout`/`retries`/`start_period`/`disable`),
  `ports`, `expose`, `volumes`, `networks`, `ingress`. The parser is canonical
  — unknown keys (including the old k8s wrapper) are rejected, not ignored.
- **Startup ordering.** `depends_on: [db]` or
  `{db: {condition: service_healthy}}` — dependents stay `Pending` until their
  dependencies run (or pass health) [examples/07-startup-ordering](examples/07-startup-ordering/).
- **Real isolation, container ergonomics.** Each service replica is a
  hardware-virtualized microVM (KVM / Apple Silicon HVF). Per-replica
  published ports for `scale > 1`: fixed `8080` becomes `8080, 8081, …`;
  target-only ports get a distinct auto host port each, stable across re-applies.
- **A service network, not a pod network.** `expose` (internal listeners) →
  L4 splice + guest DNS (`svc.<network>.svc.mc2`), default-allow across
  **server-wide named networks** (stacks can share one). `mc2 network` shows
  every network with its member instances and ports.
- **Secrets that stay secret.** `mc2 secret set` stores values in a
  **server-wide** store, encrypted at rest, never echoed back. Services
  reference them by name with an `allowHosts` policy; the guest sees a
  placeholder and the real value is attached only on connections to allowed
  hosts. `environment:` overrides a colliding `secrets[].env`. See
  [docs/guides/secrets.md](docs/guides/secrets.md).
- **Exec + logs, built in.** `mc2 exec` runs commands inside a sandbox (piped
  stdin forwarded); `mc2 logs --follow` streams sandbox logs.
- **BYO Traefik ingress.** Stack `ingress:` + `ports:` → the server writes a
  Traefik file-provider catalog, including a hostname-sugar port form
  (`"mcp.example.com:3000"`) and a self-ingress route for remote control-plane
  management.
- **Local or remote, one CLI.** Named contexts (`~/.mc2/config.toml`, 0600),
  an interactive `mc2 setup` wizard, and mode-aware `mc2 status`. Remote
  installs require https + an API key; plaintext http is refused unless you opt
  in.

## Install

Prebuilt binaries for Linux (amd64/arm64) and macOS (Apple Silicon) ship with
every release:

```bash
curl -fsSL https://github.com/l3wi/mc2/releases/latest/download/mc2-installer.sh | sh
```

Or grab the checksummed `.tar.xz` for your platform from
[releases](https://github.com/l3wi/mc2/releases).

## Quick start (dev)

Requirements: Rust **1.91+**, [just](https://github.com/casey/just), a
hypervisor (Linux KVM / Apple Silicon HVF) for running sandboxes. Full
walkthrough: [docs/guides/quickstart.md](docs/guides/quickstart.md).

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
./target/debug/mc2 up -f examples/01-hello-service/stack.yaml
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

**Guides:** [Quickstart](docs/guides/quickstart.md) · [Stack YAML](docs/guides/stack-yaml.md) · [Secrets](docs/guides/secrets.md) · [Testing](docs/guides/testing.md)

## Binary modes

| Command | Role |
| ------- | ---- |
| `mc2 server` | The orchestrator (SQLite, REST, scheduler, embedded msb runtime) |
| `mc2 node ls` | Show the local node (capacity, status) |
| `mc2 up -f stack.yaml` | Bring up a stack (publish desired state, converge) |
| `mc2 down <stack>` | Tear down a stack (instances + definition; volumes retained) |
| `mc2 rm <stack> [--volumes]` | Tear down + optionally delete named volumes |
| `mc2 config -f stack.yaml` | Validate and print a normalized stack config |
| `mc2 ps [--stack s] [--service s]` | List instances / phases |
| `mc2 exec <instance> <cmd…>` | Run a command inside a sandbox (piped stdin forwarded) |
| `mc2 logs <instance> [--tail N] [--follow]` | Sandbox logs; `--follow` streams |
| `mc2 status` | Health + version + counts (mode/context-aware) |
| `mc2 network [name \| inst-ref]` | Network membership summary; `<name>` detail; `<stack>/<service>/<ordinal>` per-instance connectivity |
| `mc2 ingress` | Desired ingress routes |
| `mc2 secret set\|ls\|rm` | Secrets (encrypted at rest; values never listed) |
| `mc2 ssh key\|open\|close\|ls` | SSH keys + open/close endpoints |
| `mc2 context set\|use\|ls` | Named API contexts (`~/.mc2/config.toml`, 0600) |
| `mc2 setup` | Interactive setup wizard (server / client trees) |
| `mc2 doctor` | Host / msb readiness checks |
| `mc2 completions <shell>` | Shell completions (bash/zsh/fish) |

All listing commands accept `-o json`. Instance commands accept
`<stack>/<service>/<ordinal>` in place of a UUID.

**Local/remote client modes.** The CLI resolves its connection as
`--api`/`--token` flags > `MC2_API`/`MC2_API_KEY` env > `--context`/`MC2_CONTEXT`
> the `current` context in `~/.mc2/config.toml` > the loopback default. A URL
host outside loopback is `remote` mode (`mc2 status` reports it); plaintext
`http://` for a remote endpoint is refused unless you opt in with
`--allow-insecure-http` / `MC2_ALLOW_INSECURE_HTTP=1`. Pair with the server's
`--public-hostname`, which publishes the control plane itself through the
Traefik ingress catalog so `mc2 context set prod --api https://mc2.example.com
--token mc2at_… && mc2 context use prod` manages a remote install over TLS. See
[docs/guides/quickstart.md](docs/guides/quickstart.md#remote-management).

**Interactive setup:** `mc2 setup` walks two trees — **Server** (on the VPS:
server flags, a one-time default `traefik.static.yml`, and a finish-setup
checklist) and **Client** (locally: URL + API key → saved context, optional
live verify).

**Service networks:** stack YAML `expose` (internal listeners) → L4 splice + guest DNS (`svc.<network>.svc.mc2`), default-allow across **server-wide named networks** (stacks can share a network). `mc2 network` lists every network with member instances and their ports. See [examples/03-networks/](examples/03-networks/).

**Ingress:** stack `ingress:` + `ports:` → the server writes a Traefik file-provider catalog (`--ingress-config-dir`). See [examples/04-http-ingress/](examples/04-http-ingress/).

**Examples:** start with [examples/01-hello-service/](examples/01-hello-service/), work up through
[07-startup-ordering](examples/07-startup-ordering/); incomplete workflows are marked under
[examples/90-advanced/](examples/90-advanced/).

One binary: orchestrator and operator CLI.

## Repository layout

```text
mc2/
  crates/           # Rust workspace (mc2 bin, server, api, store, runtime, metrics)
  docs/guides/      # Operator guides (quickstart, stack-yaml, secrets, testing)
  examples/         # Compose-shaped stack YAML, ingress, OTEL samples
  justfile          # build, test, check, run-server
```

**Docs site / Next.js app:** lives in a **separate repository** (not this one).

## License

MIT — see [LICENSE](LICENSE).
