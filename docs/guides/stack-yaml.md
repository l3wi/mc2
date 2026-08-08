# Stack YAML reference

The desired-state document `mc2 up -f <file>` sends to the MC2 server.
**Docker Compose-shaped** — a top-level `name` plus a `services` map, with
`volumes`, `networks`, and `ingress` as top-level declarations. The parser is
canonical: any unrecognized key is rejected (`unknown field …`), so the old
k8s-style `apiVersion`/`kind`/`metadata` wrapper and fields like `replicas` /
`restartPolicy` / `resources` / `health` / `mount` are errors, not silent
no-ops.

## Minimal file

`mc2 up` fills a missing top-level `name:` from the sanitized file stem, so
this is a complete stack:

```yaml
services:
  web:
    image: alpine:3.20
```

## Complete reference example

```yaml
name: shop                       # required (else derived from the file name)

volumes:                         # node-local named volumes (v1: dir only)
  data:
    kind: dir

networks:                        # logical membership groups (v1: mediated only)
  backend:
    mode: mediated

services:
  db:
    image: postgres:16
    scale: 1
    cpus: 1
    mem_limit: 512m
    command: ["postgres"]
    restart: on-failure          # no | on-failure | always | unless-stopped
    networks: [backend]
    expose:
      - port: 5432
    volumes:
      - name: data
        target: /var/lib/postgresql/data
    healthcheck:
      test: ["CMD", "pg_isready"]
      interval: 30s
    labels:
      app: db

  web:
    image: alpine:3.20
    cpus: 1
    mem_limit: 256m
    network:
      profiles: [public]         # public | private | host | none
    environment:
      MODE: fast
    ports:
      - "8080:8000"              # published:target (compose short form)
      # long form: { target: 8000, published: 8080, protocol: tcp }
    secrets:
      - name: SMOKE_TOKEN        # secret (mc2 secret set)
        env: API_TOKEN
        allowHosts: [api.example.com]
    nodeName: worker-1           # hard pin (optional)
    nodeSelector:                # soft placement by labels (optional)
      zone: a
    ssh:
      enabled: true
      bind: 127.0.0.1
      port: 2222                 # 0 = auto-allocate (not usable with ingress.tcp)
      user: root
      sftp: true
      authorizedKeys: [developer]
    networks: [backend]

ingress:                         # BYO Traefik via server file export
  tls:
    enabled: true
    certResolver: le
  rules:
    - host: shop.local
      paths:
        - path: /
          pathType: Prefix       # Prefix | Exact
          service: web
          port: 8000             # must match a services.web.ports[].target
  tcp:
    - name: web-ssh
      entryPoint: ssh
      service: web               # must have ssh.enabled with fixed port
```

## Top-level keys

| Key | Required | Default | Description |
| --- | --- | --- | --- |
| `name` | no* | file stem | Stack name. Must not contain `--` (volume namespace separator). |
| `services` | yes | — | Map of service name → service spec. Must be non-empty. |
| `volumes` | no | `{}` | Declared volumes; service mounts must reference them. |
| `networks` | no | `{}` | Logical network groups (membership only in v1). |
| `ingress` | no | — | HTTP(S) rules + TCP routes for a BYO Traefik. |

\* filled by `mc2 up` when missing.

## `services.<name>`

| Key | Required | Default | Description |
| --- | --- | --- | --- |
| `image` | yes | — | Container image reference (`alpine:3.20`, `docker.io/...`). |
| `scale` | no | `1` | Instance count; must be ≥ 1 in v1. |
| `cpus` | no | `1` | vCPUs (float, e.g. `0.5`); rounded up. |
| `mem_limit` | no | `512m` | Guest memory: `512m`, `1g`, `1.5g`, or bytes. |
| `command` | no | `sleep infinity` | Guest argv. List (`["echo", "hi"]`) or string (`echo "hi there"`, split shell-like). Omit to keep a shell-less image alive. |
| `restart` | no | `no` | `no` \| `on-failure` \| `always` \| `unless-stopped`. Drives node restart/recreate on failure. |
| `environment` | no | `{}` | Guest environment variables (map or `KEY=VALUE` list). |
| `depends_on` | no | `{}` | Startup ordering: list (`[db]`) or map (`{db: {condition: service_healthy}}`). Conditions: `service_started` (default) \| `service_healthy` (requires the dependency to declare a healthcheck). |
| `labels` | no | `{}` | Free-form labels copied onto the sandbox. |
| `network` | no | public | See `network` below. |
| `ports` | no | `[]` | Host↔guest port forwards (north-south; auto host port when `published` omitted). |
| `secrets` | no | `[]` | Cluster secret injections. |
| `volumes` | no | `[]` | Mounts of declared stack volumes. |
| `healthcheck` | no | none | Exec health probe. |
| `ssh` | no | disabled | Host-side msb SSH front end. |
| `expose` | no | `[]` | Fabric listeners (east-west; default-allow on shared networks). |
| `networks` | no | `[]` | Server-wide network membership (default = stack). |
| `nodeName` | no | — | Hard pin to a node name. |
| `nodeSelector` | no | `{}` | Soft placement: node labels that must match. |

### `network`

| Key | Default | Description |
| --- | --- | --- |
| `profiles` | `[]` → `public` | List from `public`, `private`, `host`, `none`. `none` disables the guest network entirely. Fabric reachable ports are always allowed via narrow rules regardless of profile. |

### `ports[]`

Compose syntax (north-south). Host ports always bind loopback; expose publicly
through `ingress` or the hostname sugar below (BYO Traefik).

```yaml
ports:
  - "5000:3001"            # published:target
  - "3001"                 # target-only → auto-allocated host port (server picks)
  - "mcp.example.com:3000" # hostname sugar → ingress route for that hostname (TLS, `le`)
  - "127.0.0.1:5000:3001"  # ip:published:target (ip advisory; always loopback)
  - "5000:3001/tcp"        # explicit protocol
```

Long form: `{ target: 3001, published: 5000, protocol: tcp, hostname: "mcp.example.com" }`.
`published: 0` / omitted → auto host port, resolved once at apply and persisted
(the port is stable across restarts and re-applies).

| Key | Required | Default | Description |
| --- | --- | --- | --- |
| `published` | no | auto | Host port. `0`/absent → server allocates. |
| `target` | yes | — | Port inside the guest. |
| `protocol` | no | `tcp` | `tcp` or `udp` (`udp` not usable as ingress backend). |
| `hostname` | no | — | Hostname sugar: route this hostname to `target` via ingress (TLS, `le`). |

### `expose[]` (fabric listeners)

Compose list form or map form:

```yaml
expose: [5432, "6379"]        # bare ports
# or map form for protocol/name:
expose:
  - port: 5432
    name: main
```

| Key | Required | Default | Description |
| --- | --- | --- | --- |
| `port` | yes | — | Listener port; non-zero, unique per service. |
| `protocol` | no | `tcp` | Only `tcp` in v1. |
| `name` | no | — | Optional listener name. |

Reachable by every other service that shares a network (same or different
stack) as `<service>.<network>.svc.mc2:<port>` (default-allow, round-robin
across Ready replicas). Same-node only in v1. Exposed guest ports are
**effectively unique server-wide**: each stack validates uniqueness at parse
time, but the L4 splices all live on the shared host loopback, so a port
already bound by another stack or network makes the later splice report
`Failed` (unlike Docker, same-port isolation across networks isn't possible).

### `networks[]`

List of **server-wide** network names this service joins. Every service is
implicitly on its stack's default network (named `<stack>`); joining a named
network also makes it reachable from services in *other* stacks on that
network (default-allow, `svc.<network>.svc.mc2` DNS). Networks need no
top-level declaration — they exist server-wide on first use.

### `secrets[]`

| Key | Required | Default | Description |
| --- | --- | --- | --- |
| `name` | yes | — | Cluster secret name (`mc2 secret set <name>`). |
| `env` | yes | — | Guest env var receiving the value (placeholder until injected). |
| `allowHosts` | no | `[]` | Hosts the real value is attached to. Empty → the desired set fails closed. |

Precedence: an explicit `environment:` entry **overrides** `secrets[].env` with
the same name — the colliding secret is dropped (not decrypted) and the env
value wins. A warning is logged per service on collision.

See [Environment variables vs secrets](./secrets.md) for how `environment:`
(direct injection) differs from `secrets[].env` (encrypted, host-gated msb
secrets), and the server-wide scoping.

### `volumes[]` (mounts)

| Key | Required | Description |
| --- | --- | --- |
| `name` | yes | A declared top-level `volumes` key. |
| `target` | yes | Absolute guest path; unique per service. |

Mounted as node-local named directory volumes; data survives sandbox recreate
and is retained on removal. See
[examples/06-persistent-volumes](../../examples/06-persistent-volumes/README.md).

### `healthcheck`

| Key | Required | Default | Description |
| --- | --- | --- | --- |
| `test` | no | — | Probe command. String (`curl -f http://localhost/`) or list (`["CMD", "curl", "-f", "http://localhost/"]`); `CMD` / `CMD-SHELL` prefix is stripped. |
| `interval` | no | `30s` | Probe interval (e.g. `30s`, `1m`). |
| `timeout` | no | `0` | Per-probe timeout (e.g. `5s`). `0` = no timeout. |
| `retries` | no | `3` | Consecutive failures before the service is marked unhealthy / restarted. |
| `start_period` | no | `0` | Grace period after start during which probe failures are ignored. |
| `disable` | no | `false` | `true` disables the healthcheck entirely. |

When running, the node `exec`s `test` every `interval`. A passing probe sets
the instance healthy (which gates `depends_on: {condition: service_healthy}`);
`retries` consecutive failures past `start_period` mark it unhealthy — with
`restart` set the sandbox is removed and recreated, otherwise it reports
`Failed`.

### `depends_on` (startup ordering)

Compose syntax. Ordering only — dependencies are started first, dependents wait.

```yaml
services:
  db:
    image: postgres:16
    expose: [5432]
    healthcheck:
      test: ["pg_isready", "-q", "-U", "postgres"]
      interval: 5s
      timeout: 3s
      retries: 5
  web:
    image: alpine
    depends_on:
      db:
        condition: service_healthy
```

- List form (`depends_on: [db]`) means `condition: service_started`.
- `service_healthy` requires the dependency to declare a (non-disabled) healthcheck.
- References must name services in the same stack; cycles are rejected at parse time.

### `scale` + published ports

When `scale > 1` and a service declares published ports, each replica gets a
distinct host port: a fixed `published: P` becomes the block `P, P+1, …, P+N-1`
(ordinal 0 keeps `P`); a target-only (`"3001"` / hostname-sugar) port gets a
distinct auto-allocated port per replica. Allocations are stable across
re-applies. Ingress routes for a scaled service target replica 0's port.

### `ssh`

| Key | Default | Description |
| --- | --- | --- |
| `enabled` | `false` | Turns on the host-side microsandbox SSH server. |
| `bind` | `127.0.0.1` | Agent listener address. |
| `port` | `0` | Agent backend port; `0` = auto-allocate. Ingress TCP routes need a fixed port. |
| `user` | `root` | SSH username presented to the SDK. |
| `sftp` | `true` | Enable SFTP on the session. |
| `authorizedKeys` | `[]` | Names from the key registry (`mc2 ssh key add`). |

## `volumes` (top-level)

| Key | Required | Default | Description |
| --- | --- | --- | --- |
| `kind` | no | `dir` | Only `dir` (named directory volume) in v1. Disk images, quotas, and snapshots are not yet supported. |

Volume names must match `[a-z0-9][a-z0-9._-]*` and must not contain `--`.
Resolved per-stack to `mc2-<stack>--<volume>` at sandbox create time; stored
under the server's named-volume root (`mc2 server --volume-dir`, default
`~/.microsandbox/volumes`).

## `networks` (top-level)

| Key | Required | Default | Description |
| --- | --- | --- | --- |
| `mode` | no | `mediated` | Only `mediated` is valid in v1. |

## `ingress`

Requires at least one of `rules` or `tcp`. Consumed by the server's Traefik
file export (`mc2 server --ingress-config-dir`).

### `tls`

| Key | Default | Description |
| --- | --- | --- |
| `enabled` | `false` | Mark routes TLS in the catalog. Certs/ACME live in the proxy. |
| `certResolver` | — | Traefik certificate resolver name from the proxy static config. |

### `rules[]`

| Key | Required | Default | Description |
| --- | --- | --- | --- |
| `host` | yes | — | Hostname to route. |
| `paths` | yes (≥1) | — | Path → backend entries. |

### `rules[].paths[]`

| Key | Required | Default | Description |
| --- | --- | --- | --- |
| `path` | no | `/` | Route path prefix/exact match. |
| `pathType` | no | `Prefix` | `Prefix` or `Exact`. |
| `service` | yes | — | Backend service in this stack. |
| `port` | yes | — | Must equal a `ports[].target` on that service; backend `ports` entry must be `tcp` with a non-zero `published`. |

### `tcp[]`

| Key | Required | Description |
| --- | --- | --- |
| `name` | yes | Stable route name for the Traefik config. |
| `entryPoint` | yes | Traefik entrypoint accepting TCP connections. |
| `service` | yes | Backend service; must have `ssh.enabled` with a fixed (non-zero) `ssh.port`. |

## Validation summary

Apply returns `400` for: unknown keys (canonical parser — k8s-style keys such
as `apiVersion`, `kind`, `metadata`, `replicas`, `resources`, `restartPolicy`,
`health`, `allow`, `mount`, `env`, and `host`/`guest` ports are rejected);
empty services; a missing `name`; `--` in the stack name; undeclared volume
mounts or invalid volume names; non-`dir` volume kinds; relative or duplicate
target paths; `scale: 0`; unknown `restart` values; non-mediated networks;
zero/duplicate `expose` ports; non-unique expose ports across the stack;
ingress without routes; ingress paths whose backend ports are missing or
non-tcp; TCP ingress targeting services without fixed SSH ports;
`depends_on` references to services outside the stack, unknown conditions, or
cycles; `service_healthy` dependencies without a declared healthcheck.

## Further reading

- [Quickstart](./quickstart.md) — run MC2 and apply examples.
- [examples/README.md](../../examples/README.md) — scenario walkthroughs.
- [examples/06-persistent-volumes](../../examples/06-persistent-volumes/README.md) — volume behavior and lab runbook.
