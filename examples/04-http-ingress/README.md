# HTTP ingress

`UNDER DEVELOPMENT`: this is a local HTTP lab, not a production TLS or ACME
setup. It requires Traefik installed separately. Traefik is the only supported
proxy for MC2 ingress.

Start the server with a catalog directory:

```bash
mkdir -p /tmp/mc2-ingress
mc2 server \
  --bind 127.0.0.1:7443 \
  --no-auth \
  --ingress-config-dir /tmp/mc2-ingress
```

Add the local hostname:

```text
127.0.0.1 smoke-ingress.local
```

Apply the stack and start Traefik:

```bash
export MC2_API=http://127.0.0.1:7443
mc2 up -f examples/04-http-ingress/stack.yaml
traefik --configFile=examples/04-http-ingress/traefik.static.yml
curl -s -H 'Host: smoke-ingress.local' http://127.0.0.1:8088/
```

MC2 writes the generated routes under `/tmp/mc2-ingress`.
