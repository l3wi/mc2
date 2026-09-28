# Secrets

Stores a secret, references it from a stack, and shows the guest only ever
holds a placeholder. For an end-to-end proof against a real host (httpbin.org
echoing the substituted header), see the
[Add secrets guide](../../site/content/documentation/guides/add-secrets.mdx).

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
