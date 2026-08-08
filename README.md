# MicroCommandControl (MC2)

A **YAML-driven orchestration system** for [microsandbox](https://docs.microsandbox.dev) microVMs. Declare services in a Compose-like stack file; MC2 schedules, runs, and reconciles them as detached microVMs on your machine.

MC2 gives you desired-state orchestration over microsandbox: stacks, replicas, restart policies, encrypted secrets, port exposure, a mediated **service fabric** (`expose` + `*.svc.mc2` DNS, full-mesh within a stack), ingress via a Traefik file catalog, and OTLP metrics — without reimplementing the VMM and without becoming Kubernetes.

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

**Testing:** [docs/guides/testing.md](docs/guides/testing.md) · **Quickstart:** [docs/guides/quickstart.md](docs/guides/quickstart.md) · **Secrets:** [docs/guides/secrets.md](docs/guides/secrets.md)

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
| `mc2 status` | Health + version + counts |
| `mc2 fabric <instance>` | Observed fabric status (expose/edges) |
| `mc2 ingress` | Desired ingress routes |
| `mc2 secret set\|ls\|rm` | Secrets (encrypted; values never listed) |
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

**Service fabric:** stack YAML `expose` (internal listeners) → L4 splice + guest DNS (`svc.<network>.svc.mc2`), default-allow across **server-wide named networks** (stacks can share a network). See [examples/03-service-fabric/](examples/03-service-fabric/).

**Ingress:** stack `ingress:` + `ports:` → the server writes a Traefik file-provider catalog (`--ingress-config-dir`). See [examples/04-http-ingress/](examples/04-http-ingress/).

**Examples:** start with [examples/01-hello-service/](examples/01-hello-service/); incomplete workflows are marked under [examples/90-advanced/](examples/90-advanced/).

One binary: orchestrator and operator CLI.

## Repository layout

```text
mc2/
  crates/           # Rust workspace (mc2 bin, server, api, store, runtime, metrics)
  docs/guides/      # Operator guides (quickstart, testing, secrets)
  examples/         # Stack YAML, ingress, OTEL samples
  justfile          # build, test, check, run-server
```

**Docs site / Next.js app:** lives in a **separate repository** (not this one).

## License

MIT — see [LICENSE](LICENSE).
