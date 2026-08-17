# Secrets

`UNDER DEVELOPMENT`: this verifies secret configuration and agent sync, but does
not yet provide a complete guest-side command that proves the injected value.

```bash
mc2 secret set SMOKE_TOKEN --value 'lab-only-token'
mc2 up -f examples/02-secrets/stack.yaml
mc2 ps
```

Secret values are never listed by MC2. The `allowHosts` policy in the stack must
be non-empty; injection fails closed otherwise.

Secrets are a **server-wide** store (`mc2 secret set`), shared across stacks —
unlike `environment:`, which is per-service inline YAML. See
[site/content/documentation/concepts/secrets.mdx](../../site/content/documentation/concepts/secrets.mdx) for the full difference
(direct injection vs encrypted, host-gated msb secrets) and the collision
precedence rule.
