# Environment variables vs secrets

MC2 gives services two distinct ways to get values into the guest environment.
They look similar in YAML and both end up as env vars, but they are different
mechanisms with very different security properties. This guide explains the
difference.

## `environment:` — direct injection

```yaml
services:
  web:
    image: alpine:3.20
    environment:
      MODE: fast
      DEBUG: "0"
```

- Plain literal env vars, written inline in the stack file.
- Stored **unencrypted** in the stack spec (the DB's `spec_json`), and injected
  into the sandbox unconditionally at create (`Sandbox::env`).
- Visible to every command in the guest. Anyone who can read the stack file or
  the data dir sees the values.
- A change to any value triggers a sandbox recreate.
- Constraint: keys starting with `MSB_` are rejected by the microsandbox SDK
  (reserved prefix).

**Use for**: non-sensitive configuration — ports, flags, feature toggles.

## `secrets[].env` — host-gated, encrypted secret

```yaml
services:
  web:
    image: alpine:3.20
    secrets:
      - name: SMOKE_TOKEN
        env: API_TOKEN
        allowHosts:
          - api.example.com
```

The value is **not** stored in the stack and is **not** injected like an env
var. The stack only references a cluster secret by name.

- The value lives once in the **server-wide secret store** (`mc2 secret set
  NAME --value …`), encrypted at rest. MC2 never echoes it back: `mc2 secret ls`
  and the REST API return names/metadata only.
- At reconcile time the node decrypts it (server-side, just-in-time) and passes
  it to the sandbox through the microsandbox **secret** mechanism
  (`Sandbox::secret`), not `Sandbox::env`.
- That mechanism works differently from a plain env var:
  - The guest **does not get the raw value**. It sees a generated placeholder
    (`$MSB_<env>`).
  - The sandbox's egress proxy enables **TLS interception**.
  - When the guest connects to an `allowHosts` host, the proxy injects the real
    value for that connection. Anywhere else the placeholder stays.
- `allowHosts` must be non-empty — injection **fails closed** otherwise.
- Rotation: `mc2 secret set NAME --value …` + re-apply the stack (the sandbox
  is recreated so the new value is attached).

**Use for**: credentials and tokens that must be encrypted at rest, never
listed, and scoped to specific outbound hosts.

## Scope: server-wide, not per-stack

Compose secrets are scoped to one compose project. MC2 secrets are a
**server-wide (cluster-wide) store**: there is no stack-level declaration and no
stack namespace. A secret set once is referenceable by name from any stack on
the server.

| | Compose | MC2 |
| --- | --- | --- |
| Declared | top-level `secrets:` in the project file | not in the stack at all |
| Stored | per project, file/env-derived | server-wide store, `mc2 secret set` |
| Scoped to | one project | the whole server |
| Values | plaintext on disk | encrypted at rest |
| Injected | file mount / env | msb host-gated secret (+TLS interception) |

## Precedence on collision

If `environment:` and `secrets[].env` name the **same** env var, the explicit
`environment:` value **wins**. The colliding secret is dropped at desired-set
build (never decrypted, no placeholder, no TLS-interception side effect), and
MC2 logs a warning naming the overridden var.

```yaml
environment:
  API_TOKEN: inline-value        # wins — secrets[] entry for API_TOKEN is dropped
secrets:
  - name: VAULT_TOKEN
    env: API_TOKEN
    allowHosts: [api.example.com]
```
