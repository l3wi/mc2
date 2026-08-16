# MicroCommandControl (MC2)

**MC2 is a Compose-shaped orchestrator for microsandbox microVMs.** Declare
services in a Docker-Compose-style stack file; MC2 schedules, runs, and
continuously reconciles them as detached, hardware-virtualized microVMs on your
machine — replicas, restart policies, health-gated startup ordering, encrypted
secrets, a default-allow service network, and Traefik ingress, without
reimplementing the VMM and without becoming Kubernetes.

One process, no daemon, no agent: `mc2 server` is the orchestrator — SQLite
state, REST API, scheduler, and a reconcile loop that drives detached microVMs
through the embedded microsandbox SDK.

## Where it fits

| | MC2 | Docker Compose | Kubernetes | Firecracker-style |
| --- | --- | --- | --- | --- |
| Isolation | microVM (kernel) per replica | container (shared kernel) | container | microVM |
| Model | desired-state reconcile | run-once | controllers | imperative VMM |
| Scope | single node, single process | single host | multi-node | no orchestrator |
| Ergonomics | Compose vocabulary | Compose-native | kubectl | n/a |

Container ergonomics on real microVM isolation, without Kubernetes.

## Why MC2

microsandbox boots a fast microVM. MC2 is the orchestration layer on top of
it — Compose-shaped stacks, lifecycle, networking, secrets, and operator
tooling, the parts msb leaves to you.

- **Compose-shaped stacks.** `services`, `scale`, `ports`, `expose`,
  `environment`, `healthcheck`, `restart`, `depends_on`, `volumes`, `networks`,
  `ingress` in a git-committable file. Unknown keys are rejected, not ignored.
- **Desired state, converged.** `mc2 up` reconciles replicas, restart
  policies, and health-gated startup ordering (`depends_on: {db: {condition:
  service_healthy}}`); `mc2 down` (alias `rm`, add `--volumes`) tears down. No run-once, no drift.
- **A local, private sandbox and app host.** The embedded SDK keeps every
  microVM on your box (or a server you control) — no account, no egress of
  your code or data. `mc2 exec` and `mc2 logs --follow` are built in.
- **Secrets that stay secret.** A server-wide, encrypted-at-rest store
  (`mc2 secret set`) that never echoes values back; the guest sees a
  placeholder and the real value only on connections to `allowHosts`.
- **A service network, not a pod network.** `expose` → default-allow east–west
  with `<service>.<network>.svc.mc2` DNS across server-wide named networks;
  `ports` + `ingress:` produce a Traefik catalog. `mc2 network` shows it all.
- **Per-replica ports, no bookkeeping.** `scale: 3` turns `8080` into
  `8080, 8081, 8082`; target-only ports get stable auto host ports.
- **One process, one CLI.** `mc2 server` = SQLite + REST + scheduler + a
  reconcile loop. The same commands work on your laptop or a remote server
  (https + token).

### What's possible

- **Launch a swarm of agents.** `scale: 20` on an agent image puts each agent
  in its own hardware-isolated microVM; `restart` keeps them alive through
  crashes, and `mc2 exec` / `mc2 logs --follow` drop you into any of them.
  Give the swarm its own network so agents can coordinate — or keep them fully
  isolated.
- **Power your existing Docker Compose with microVMs.** If you can write a
  `docker-compose.yml`, you already know MC2 — the same `services`, `ports`,
  `depends_on`, and `environment` shape now boots each service as a
  hardware-isolated microVM, with no image builds or platform teams.
- **Remote coding agents over SSH.** `ssh: true` on a dev VM, `mc2 ssh add-key`,
  and you SSH into it from anywhere over TLS — your keys and code never
  leave your box.
- **A throwaway test grid.** Bring up N identical VMs, run your suite across
  all of them, then `mc2 down --volumes` and they're gone — or keep a cache
  volume for reuse.
- **A fleet of headless browsers.** Thirty scraper VMs, each network-locked to
  only the hosts it talks to, each disposable inside its own microVM.

## Install

Prebuilt binaries for Linux (amd64/arm64) and macOS (Apple Silicon) ship with
every release:

```bash
curl -fsSL https://github.com/l3wi/mc2/releases/latest/download/mc2-installer.sh | sh
```

Or grab the checksummed `.tar.xz` for your platform from
[releases](https://github.com/l3wi/mc2/releases).

## Quick start (dev)

Requirements: `mc2` on your PATH (see [Install](#install)) and a hypervisor
(Linux KVM / Apple Silicon HVF) for running sandboxes. Full walkthrough:
[site/content/documentation/quickstart.mdx](site/content/documentation/quickstart.mdx).

```bash
DATA=/tmp/mc2-dev
mc2 doctor

# Terminal 1 — the orchestrator (one process)
# Lab (no token): --no-auth
# Default: prints the API token once on first bootstrap
mc2 server --data-dir "$DATA" --bind 127.0.0.1:7443 --no-auth

# Terminal 2 — operator (token only if server was not --no-auth)
export MC2_API=http://127.0.0.1:7443
# export MC2_API_KEY="<api-token>"   # when auth is enabled
mc2 node ls
mc2 up -f examples/01-hello-service/stack.yaml
mc2 ps
curl -s "$MC2_API/v1/status"
# after hello is Running: curl -s http://127.0.0.1:18091/
```

Optional OTLP:

```bash
export MC2_OTLP_ENDPOINT=http://127.0.0.1:4317
# advanced collector example: examples/90-advanced/observability/collector-config.yaml
```

**Guides:** [Quickstart](site/content/documentation/quickstart.mdx) · [Stack YAML](site/content/references/stack/overview.mdx) · [Secrets](site/content/documentation/concepts/secrets.mdx) · [Testing](site/content/documentation/operations/troubleshooting.mdx)

## Binary modes

| Command | Role |
| ------- | ---- |
| `mc2 server` | The orchestrator (SQLite, REST, scheduler, embedded msb runtime) |
| `mc2 node ls` | Show the local node (capacity, status) |
| `mc2 up -f stack.yaml` | Bring up a stack (publish desired state, converge) |
| `mc2 down <stack> [--volumes]` | Tear down a stack (instances + definition; volumes retained unless `--volumes`; `rm` is an alias) |
| `mc2 config -f stack.yaml` | Validate and print a normalized stack config |
| `mc2 ps [--stack s] [--service s]` | List instances / phases |
| `mc2 exec <instance> <cmd…>` | Run a command inside a sandbox (piped stdin forwarded) |
| `mc2 logs <instance> [--tail N] [--follow]` | Sandbox logs; `--follow` streams |
| `mc2 status` | Health + version + counts (mode/context-aware) |
| `mc2 network [name \| inst-ref]` | Network membership summary; `<name>` detail; `<stack>/<service>/<ordinal>` per-instance connectivity |
| `mc2 ingress` | Desired ingress routes |
| `mc2 volume ls` | Named volumes retained on the node (stack, size, path) |
| `mc2 secret set\|ls\|rm` | Secrets (encrypted at rest; values never listed) |
| `mc2 ssh add-key\|keys\|show-key\|rm-key\|ls\|open\|close` | SSH keys + open/close endpoints |
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
[site/content/documentation/quickstart.mdx](site/content/documentation/quickstart.mdx#remote-management).

**Interactive setup:** `mc2 setup` walks two trees — **Server** (on the VPS:
server flags, a one-time default `traefik.static.yml`, and a finish-setup
checklist) and **Client** (locally: URL + API key → saved context, optional
live verify).

**Service networks:** `expose` → default-allow east–west with `<service>.<network>.svc.mc2` DNS (server-wide — stacks can share a network); `mc2 network` lists networks, members, and ports. See [examples/03-networks/](examples/03-networks/).

**Ingress:** stack `ingress:` + `ports:` → the server writes a Traefik file-provider catalog (`--ingress-config-dir`). See [examples/04-http-ingress/](examples/04-http-ingress/).

**Examples:** start with [examples/01-hello-service/](examples/01-hello-service/), work up through
[07-startup-ordering](examples/07-startup-ordering/); incomplete workflows are marked under
[examples/90-advanced/](examples/90-advanced/).

## Repository layout

```text
mc2/
  crates/           # Rust workspace (mc2 bin, server, api, store, runtime, metrics)
  site/             # Docs site (Next.js + MDX; `cd site && bun run dev`)
  examples/         # Compose-shaped stack YAML, ingress, OTLP samples
  justfile          # build, test, check, run-server
```

**Docs site:** a Next.js + MDX app lives in [`site/`](site/) — the public
documentation (concepts, guides, references, recipes, security). Run
`cd site && bun install && bun run dev` and open http://localhost:3000.

## License

MIT — see [LICENSE](LICENSE).
