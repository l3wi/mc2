# Secrets

`UNDER DEVELOPMENT`: this verifies secret configuration and agent sync, but does
not yet provide a complete guest-side command that proves the injected value.

```bash
export MC2_API=http://127.0.0.1:7443
./target/debug/mc2 secret set SMOKE_TOKEN --value 'lab-only-token'
./target/debug/mc2 apply -f examples/secrets/stack.yaml
./target/debug/mc2 ps
```

Secret values are never listed by MC2. The `allowHosts` policy in the stack must
be non-empty; injection fails closed otherwise.
