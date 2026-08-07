# Ingress lab — BYO Traefik or Caddy

MC2 writes a **file catalog** of ready HTTP backends (loopback host ports from `ports:`).  
You run Traefik and/or Caddy on the **same node** as the agent.

Smoke stack: [examples/stacks/smoke-ingress.yaml](../stacks/smoke-ingress.yaml).

## Layout (agent writes)

```text
$MC2_INGRESS_CONFIG_DIR/
  catalog.json              # debug: routes + ready flags
  traefik/dynamic.yml       # Traefik file provider
  caddy/Caddyfile           # Caddy sites
```

## Agent

```bash
export MC2_INGRESS_CONFIG_DIR=/tmp/mc2-ingress
./target/debug/mc2 agent \
  --server http://127.0.0.1:7444 \
  --name "$(hostname -s 2>/dev/null || hostname)" \
  --ingress-config-dir "$MC2_INGRESS_CONFIG_DIR"
```

## Apply smoke stack

```bash
export MC2_API=http://127.0.0.1:7443
./target/debug/mc2 apply -f examples/stacks/smoke-ingress.yaml
./target/debug/mc2 ps
# Desired routes (control plane):
curl -s "$MC2_API/v1/ingress" | jq .
# After Running + port live:
cat /tmp/mc2-ingress/catalog.json | jq .
# Direct backend (bypass proxy):
curl -s http://127.0.0.1:18080/ | head
```

Lab host name:

```bash
# /etc/hosts
127.0.0.1 smoke-ingress.local
```

## Traefik (file watch — no reload)

1. Edit `directory` in [traefik.static.yml](./traefik.static.yml) if needed.  
2. For a simple lab without ACME, set `ingress.tls.enabled: false` in the stack so routers use entryPoint `web` only, **or** keep TLS and use a real cert resolver.

```bash
# Install traefik separately; then:
traefik --configFile=examples/ingress/traefik.static.yml
# HTTP lab (tls.enabled: false): curl -s -H 'Host: smoke-ingress.local' http://127.0.0.1:8088/
```

## Caddy (reload on change)

```bash
caddy run --config /tmp/mc2-ingress/caddy/Caddyfile
# After MC2 rewrites the file:
caddy reload --config /tmp/mc2-ingress/caddy/Caddyfile

curl -sk https://smoke-ingress.local/   # tls internal in smoke stack
```

Optional watcher:

```bash
fswatch -o /tmp/mc2-ingress/caddy/Caddyfile | while read; do
  caddy reload --config /tmp/mc2-ingress/caddy/Caddyfile
done
```

## Planes

| | Mechanism |
| - | --------- |
| Public/lab HTTP(S) | `ingress:` + this catalog + Traefik/Caddy |
| Raw host port | `ports:` (e.g. `curl 127.0.0.1:18080`) |
| East–west | fabric `expose` / `allow` / `*.svc.mc2` (not Ingress) |
