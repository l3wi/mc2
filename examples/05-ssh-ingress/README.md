# SSH ingress

This example has been verified end to end with a real microsandbox host
and Traefik TCP forwarding. It requires a local SSH key and a hypervisor-
capable machine.

## Run it

Start the server using the [quickstart](../../site/content/documentation/quickstart.mdx),
with `--ingress-config-dir /tmp/mc2-ssh-ingress/`. Register a public
key, apply the stack, and start Traefik:

```bash
mc2 ssh add-key developer --file ~/.ssh/id_ed25519.pub
mc2 up -f examples/05-ssh-ingress/stack.yaml
traefik --configFile=examples/05-ssh-ingress/traefik.static.yml
mc2 ssh ls
ssh -p 2200 root@127.0.0.1
```

The connection should reach the service through Traefik. `mc2 ssh ls` reports
the direct MC2 listener, which is useful for distinguishing SSH or node issues
from Traefik issues.

## Stack configuration

```yaml
services:
  web:
    ssh:
      enabled: true
      bind: 127.0.0.1
      port: 2222
      user: root
      sftp: true
      authorizedKeys: [developer]

ingress:
  tcp:
    - name: web-ssh
      entryPoint: ssh
      service: web
```

Without `ingress.tcp` (direct MC2 SSH only) you can use the short form —
`ssh: true` enables SSH with all defaults and authenticates with **every
registered key** (`mc2 ssh add-key`):

```yaml
services:
  web:
    ssh: true          # auto host port, all registered keys
```

`mc2 up` prints the declared SSH port and the `ingress.tcp` entrypoint for each
service (auto ports resolve on first reconcile — check `mc2 ssh ls`).

SSH fields:

- `enabled` — turns the host-side microsandbox SSH server on.
- `bind` — address used by the host-side SSH listener. Keep this at
  `127.0.0.1` unless the listener must be reachable directly from the network.
- `port` — host-side backend port. It must be a fixed nonzero port when used
  by TCP ingress; `0` auto-allocation is not supported for this route.
- `user` — SSH username presented to the microsandbox SDK; this example uses
  `root`.
- `sftp` — enables SFTP support for the SSH session.
- `authorizedKeys` — names in MC2’s SSH-key registry, not raw public
  keys. Register each name with `mc2 ssh add-key` first.

TCP ingress fields:

- `name` — stable route name used in the generated Traefik configuration.
- `entryPoint` — Traefik entrypoint that accepts external TCP connections.
- `service` — service whose SSH listener is the backend. MC2 takes the backend
  port from that service’s `ssh.port`.

## Port relationship

The two ports are intentionally different:

```text
Traefik :2200  →  MC2 SSH listener 127.0.0.1:2222  →  microsandbox
```

The static Traefik file defines the public entrypoint and the server writes the
dynamic TCP route under `/tmp/mc2-ssh-ingress/traefik/`.
