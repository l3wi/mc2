# HTTP ingress

`UNDER DEVELOPMENT`: this is a local HTTP lab, not a production TLS or ACME
setup. It requires Traefik installed separately. Traefik is the only supported
proxy for MC2 ingress.

Start the server with a catalog directory (data dir and bind stay at their
defaults). This example's Traefik static config is hardcoded to
`/tmp/mc2-ingress`:

```bash
mc2 server --no-auth --ingress-config-dir /tmp/mc2-ingress
```

Add the local hostname:

```text
127.0.0.1 smoke-ingress.local
```

Apply the stack and start Traefik:

```bash
mc2 up -f examples/04-http-ingress/stack.yaml
traefik --configFile=examples/04-http-ingress/traefik.static.yml
curl -s -H 'Host: smoke-ingress.local' http://127.0.0.1:8088/
```

MC2 writes the generated routes under `/tmp/mc2-ingress`.
