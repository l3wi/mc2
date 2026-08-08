# Stack YAML reference (`mc2/v1`)

The desired-state document `mc2 apply -f <file>` sends to the control plane.
Compose-like shape: top-level declarations (`volumes`, `networks`) plus a
`services` map. All keys are camelCase.

## Minimal file

`mc2 apply` fills missing boilerplate client-side (`apiVersion`, `kind`,
`metadata.name` from the sanitized file stem), so this is a complete stack:

```yaml
services:
  web:
    image: alpine:3.20
```

Present values are never rewritten — wrong values still fail server-side
validation. Full documents pass through verbatim.

## Complete reference example

```yaml
apiVersion: mc2/v1
kind: Stack
metadata:
  name: shop                     # required (else derived from file name)
  labels:
    env: lab

volumes:                         # node-local named volumes (v1: dir only)
  data:
    kind: dir

networks:                        # logical membership groups (v1: mediated only)
  backend:
    mode: mediated

services:
  db:
    image: postgres:16
    replicas: 1
    resources:
      cpus: 1
      memoryMiB: 512
    command: ["postgres"]
    restartPolicy: on-failure    # always | on-failure | never
    networks: [backend]
    expose:
      - port: 5432
    volumes:
      - name: data
        mount: /var/lib/postgresql/data
    health:
      kind: exec                 # exec | none
      command: ["pg_isready"]
      intervalSeconds: 30
    labels:
      app: db

  web:
    image: alpine:3.20
    resources:
      cpus: 1
      memoryMiB: 256
    network:
      profiles: [public]         # public | private | host | none
    env:
      MODE: fast
    ports:
      - host: 8080               # host port
        guest: 8000              # in-guest port
        protocol: tcp            # tcp | udp
        bind: 127.0.0.1
    secrets:
      - name: SMOKE_TOKEN        # cluster secret (mc2 secret set)
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
    allow:
      - to: db
        port: 5432

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
          port: 8000             # must match a services.web.ports[].guest
  tcp:
    - name: web-ssh
      entryPoint: ssh
      service: web               # must have ssh.enabled with fixed port
```

## Top-level keys

| Key | Required | Default | Description |
| --- | --- | --- | --- |
| `apiVersion` | no* | `mc2/v1` | Schema version; must equal `mc2/v1`. |
| `kind` | no* | `Stack` | Must be `Stack`. |
| `metadata` | no* | name from file stem | Stack identity; see below. |
| `services` | yes | — | Map of service name → service spec. Must be non-empty. |
| `volumes` | no | `{}` | Declared volumes; service mounts must reference them. |
| `networks` | no | `{}` | Logical network groups (membership only in v1). |
| `ingress` | no | — | HTTP(S) rules + TCP routes for a BYO Traefik. |

\* filled by `mc2 apply` when missing.

### `metadata`

| Key | Required | Default | Description |
| --- | --- | --- | --- |
| `name` | yes* | file stem | Stack name. Must not contain `--` (volume namespace separator). |
| `labels` | no | `{}` | Free-form `key: value` metadata, stored with the stack. |

## `services.<name>`

| Key | Required | Default | Description |
| --- | --- | --- | --- |
| `image` | yes | — | Container image reference (`alpine:3.20`, `docker.io/...`). |
| `replicas` | no | `1` | Instance count; must be ≥ 1 in v1. |
| `resources` | no | 1 CPU / 512 MiB | See `resources` below. |
| `command` | no | `sleep infinity` | Guest argv. Omit to keep a shell-less image alive. |
| `restartPolicy` | no | `on-failure` | `always` \| `on-failure` \| `never`. Drives node restart/recreate on failure. |
| `env` | no | `{}` | Guest environment variables. |
| `labels` | no | `{}` | Free-form labels copied onto the sandbox. |
| `network` | no | public | See `network` below. |
| `ports` | no | `[]` | Host↔guest port forwards (north–south). |
| `secrets` | no | `[]` | Cluster secret injections. |
| `volumes` | no | `[]` | Mounts of declared stack volumes. |
| `health` | no | none | Exec health probe. |
| `ssh` | no | disabled | Host-side msb SSH front end. |
| `expose` | no | `[]` | Fabric listeners (east–west). |
| `allow` | no | `[]` | Fabric client edges (default deny). |
| `networks` | no | `[]` | Membership in declared `networks`. |
| `nodeName` | no | — | Hard pin to a node name. |
| `nodeSelector` | no | `{}` | Soft placement: node labels that must match. |

### `resources`

| Key | Default | Description |
| --- | --- | --- |
| `cpus` | `1` | vCPUs; clamped to 1–255 at create time. |
| `memoryMiB` | `512` | Guest memory in MiB. |

### `network`

| Key | Default | Description |
| --- | --- | --- |
| `profiles` | `[]` → `public` | List from `public`, `private`, `host`, `none`. `none` disables the guest network entirely. Fabric `allow` ports are always allowed via narrow rules regardless of profile. |

### `ports[]`

| Key | Required | Default | Description |
| --- | --- | --- | --- |
| `host` | yes | — | Port bound on the host. Must be non-zero for ingress backends. |
| `guest` | yes | — | Port inside the guest. |
| `protocol` | no | `tcp` | `tcp` or `udp` (`udp` not usable as ingress backend). |
| `bind` | no | `127.0.0.1` | Bind address. Ingress backends must stay loopback in v1. |

### `secrets[]`

| Key | Required | Default | Description |
| --- | --- | --- | --- |
| `name` | yes | — | Cluster secret name (`mc2 secret set <name>`). |
| `env` | yes | — | Guest env var receiving the value (placeholder until injected). |
| `allowHosts` | no | `[]` | Hosts the real value is attached to. Empty → the desired set fails closed. |

### `volumes[]` (mounts)

| Key | Required | Description |
| --- | --- | --- |
| `name` | yes | A declared top-level `volumes` key. |
| `mount` | yes | Absolute guest path; unique per service. |

Mounted as node-local named directory volumes; data survives sandbox recreate
and is retained on removal. See
[examples/06-persistent-volumes](../../examples/06-persistent-volumes/README.md).

### `health`

| Key | Required | Default | Description |
| --- | --- | --- | --- |
| `kind` | yes | — | `exec` (run `command` in the guest) or `none`. |
| `command` | for `exec` | `[]` | Probe argv; must be non-empty when `kind: exec`. |
| `intervalSeconds` | no | `30` | Probe interval. |

### `ssh`

| Key | Default | Description |
| --- | --- | --- |
| `enabled` | `false` | Turns on the host-side microsandbox SSH server. |
| `bind` | `127.0.0.1` | Agent listener address. |
| `port` | `0` | Agent backend port; `0` = auto-allocate. Ingress TCP routes need a fixed port. |
| `user` | `root` | SSH username presented to the SDK. |
| `sftp` | `true` | Enable SFTP on the session. |
| `authorizedKeys` | `[]` | Names from the cluster key registry (`mc2 ssh key add`). |

### `expose[]` (fabric)

| Key | Required | Default | Description |
| --- | --- | --- | --- |
| `port` | yes | — | Listener port; non-zero, unique per service. |
| `protocol` | no | `tcp` | Only `tcp` in v1. |
| `name` | no | — | Optional listener name. |

Reachable by other services as `<service>.<stack>.svc.mc2:<port>` once they
`allow` it. Same-node only in v1.

### `allow[]` (fabric)

| Key | Required | Default | Description |
| --- | --- | --- | --- |
| `to` | yes | — | Target service in this stack (not itself). |
| `port` | yes | — | Must match a target `expose[].port`. |
| `protocol` | no | `tcp` | Only `tcp` in v1. |

### `networks[]`

List of names declared under top-level `networks`. Membership is informational
in v1 (no connectivity semantics); unknown names are rejected.

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
| `port` | yes | — | Must equal a `ports[].guest` on that service; backend `ports` entry must be `tcp`, loopback `bind`, non-zero `host`. |

### `tcp[]`

| Key | Required | Description |
| --- | --- | --- |
| `name` | yes | Stable route name for the Traefik config. |
| `entryPoint` | yes | Traefik entrypoint accepting TCP connections. |
| `service` | yes | Backend service; must have `ssh.enabled` with a fixed (non-zero) `ssh.port`. |

## Validation summary

Apply returns `400` for: unsupported `apiVersion`/`kind`; empty services;
`--` in the stack name; undeclared volume mounts or invalid volume names;
non-`dir` volume kinds; relative or duplicate mount paths; `replicas: 0`;
unknown `restartPolicy`/`health.kind`; non-mediated networks; unknown network
membership; zero/duplicate `expose` ports; `allow` to missing/same service or
unexposed port; ingress without routes; ingress paths whose backend ports are
missing, non-tcp, non-loopback, or auto-allocated; TCP ingress targeting
services without fixed SSH ports.

## Further reading

- [Quickstart](./quickstart.md) — run a cluster and apply examples.
- [examples/README.md](../../examples/README.md) — scenario walkthroughs.
- [examples/06-persistent-volumes](../../examples/06-persistent-volumes/README.md) — volume behavior and lab runbook.
