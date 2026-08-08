# Task: Local/remote client modes (context-aware CLI + self-ingress)

Status: **IMPLEMENTED — 2026-08-08**
Date: 2026-08-08

## Context

MC2's CLI is a pure REST client: every command resolves `--api` (default
`http://127.0.0.1:7443`, env `MC2_API`) and an optional bearer token
(`--token` / `MC2_API_KEY`), then talks HTTP. Remote management already
*works* if you spell it out:

```bash
mc2 ps --api https://mc2.example.com --token mc2at_...   # works today
```

The gap is ergonomics + first-party TLS story:

1. No persistent client config — URL/token must be repeated or env'd.
2. The CLI doesn't distinguish or *report* local vs remote mode.
3. The server has no way to advertise itself for remote access (publish its
   own API through the BYO Traefik catalog with TLS).

Goal: two named states — **local** (loopback API, token optional) and
**remote** (https URL + API key required) — with `mc2 status` reporting the
mode and the server able to export a hostname via the ingress catalog so
Traefik terminates SSL and the CLI connects through it.

## Feasibility verdict: HIGH

- **Data path unchanged.** Same REST surface, same JSON; remote is a URL
  swap. No new protocol, no new auth scheme.
- **TLS already works client-side.** `mc2`'s reqwest is built with
  `rustls-tls`; any https endpoint with a real cert (e.g. Traefik + Let's
  Encrypt via `certResolver`) connects out of the box.
- **Auth seam already exists** (`auth.rs`): bearer enforced when the cluster
  was bootstrapped without `--no-auth`. "Start mc2 with an API key" = omit
  `--no-auth`; the token prints once at bootstrap. Nothing new server-side.
- **The ingress catalog is the right export mechanism.** The server already
  writes Traefik dynamic config under `--ingress-config-dir`; a synthetic
  self-route for the control plane fits that model exactly.

## Design

### 1. Client context config (new: `~/.mc2/config.toml`, mode 0600)

```toml
current = "local"

[contexts.local]
url = "http://127.0.0.1:7443"

[contexts.prod]
url = "https://mc2.example.com"
api_key = "mc2at_..."
```

Resolution precedence (highest first):
1. Explicit `--api` / `--token` flags
2. `MC2_API` / `MC2_API_KEY` env
3. `--context <name>` flag / `MC2_CONTEXT` env
4. `current` context from config file
5. Built-in default: `http://127.0.0.1:7443` (local)

New commands (small surface):
- `mc2 context ls` — contexts + which is current (+ mode per row)
- `mc2 context use <name>` — set `current`
- `mc2 context set <name> --api <url> [--token <key>]` — upsert

**Mode detection** is client-side and deterministic: effective URL host is
loopback (`127.0.0.1` / `localhost` / `::1`) → `local`; anything else →
`remote`.

**Safety rail (remote guard):** a `remote` context with an `http://` URL is
rejected unless an explicit opt-in flag/env is set (plaintext bearer over
the network is the one footgun this design can create). Remote + https +
token is the enforced happy path.

### 2. Server self-ingress route (new flag)

```bash
mc2 server --public-hostname mc2.example.com [--public-tls-cert-resolver le]
```

When set, the node loop's catalog writer additionally emits one synthetic
route — independent of any stack:

| field | value |
| --- | --- |
| host | `mc2.example.com` |
| path | `/` |
| backend | `127.0.0.1:<bind port>` (the REST listener itself) |
| tls | enabled, certResolver from flag (default `le`) |

Implementation notes:
- Route source is a server-level `SelfIngressRoute`, not a stack `ingress:`
  section — `IngressFileWriter.reconcile` gains a second always-available
  route list (gate: REST port accepting; no instance phase involved, since
  the control plane is not a sandbox).
- Server persists the hostname in state; `/v1/status` gains
  `public_hostname` so remote clients can display where they're connected.

### 3. Mode-aware `mc2 status`

```text
$ mc2 status
mc2 0.1.0 — remote: https://mc2.example.com (context: prod)
  auth: bearer ✓
  server api: mc2/v1, version 0.1.0
  nodes: 1/1 ready · stacks: 2 · instances: 3
```

- Reports `local`/`remote`, effective URL, context name, auth present (never
  the token), plus the existing counts.
- `-o json` includes `"mode": "remote"`, `"context": "prod"`,
  `"publicHostname"` (from the server) — scripts can branch on mode.
- `/health` stays unauthenticated (no secrets in it); status distinguishes
  "health reachable" from "auth OK" in its failure messages.

### 4. Setup story (what the operator does)

1. Server host: bootstrap with auth (`--no-auth` omitted) → save API token.
2. Start with `--ingress-config-dir` + `--public-hostname mc2.example.com`.
3. Traefik (already running for stack ingress) picks up the new route,
   provisions TLS via the cert resolver.
4. Client machine: `mc2 context set prod --api https://mc2.example.com
   --token <key>` → `mc2 context use prod` → all commands are remote.

Chicken-and-egg note: step 4 presumes the route is live; first-time setup
still needs the server reachable somehow (or Traefik configured by hand).
Document the ordering; not a blocker.

## Changes by area

| Area | Change | Size |
| --- | --- | --- |
| `mc2` CLI | config file parse + precedence + `context` commands + mode detection + remote guard | ~250 lines |
| `mc2` CLI | `-o json` mode field; TLS already handled by rustls; optional `--tls-ca`/`--insecure` for self-signed (small, optional) | ~40 lines |
| `mc2-server` | `--public-hostname` (+cert resolver) flag; synthetic self-route; always-ready route path in `IngressFileWriter`; `public_hostname` in `/v1/status` | ~120 lines |
| `mc2-api` | `ClusterStatus.public_hostname: Option<String>` | trivial |
| Tests | config precedence + mode detection units; self-route unit (ingress_files); status integration w/ mode; context smoke | ~200 lines |
| Docs | quickstart "Remote management" section; README; CHANGELOG | small |

## Out of scope (deliberate)

- Multi-user / RBAC / scoped tokens — one bearer token per install, as today.
- MC2 terminating TLS itself — stays BYO Traefik (consistent with the
  ingress philosophy; avoids cert lifecycle in MC2).
- Remote *node* registration — still single-node; remote is client↔server
  only.

## Risks

- **Token leakage** is the only real risk of remote exposure; mitigated by
  the https-only guard, 0600 config file, and never printing tokens.
- **Config file divergence** (user edits `current` to a dead context):
  `mc2 status` fails fast with the context name + URL, which is the
  diagnosable error we want.

## Acceptance criteria

- [ ] `mc2 context set/use/ls` manage `~/.mc2/config.toml` (0600).
- [ ] Precedence: flags > env > `--context`/`MC2_CONTEXT` > current > default.
- [ ] Remote http:// context rejected without explicit opt-in.
- [ ] `--public-hostname` emits the self-route into the catalog with TLS;
      `/v1/status` returns `publicHostname`.
- [ ] `mc2 status` reports local/remote mode, context, auth presence;
      `-o json` includes `mode`/`context`.
- [ ] Lab: full remote loop — Traefik serves https hostname, remote CLI
      applies a stack through it.
- [ ] `just check` green.

## Open questions

1. Named contexts now, or MVP = single remote URL+token (no context file)?
   Named contexts cost ~80 extra lines and match kubectl/msb muscle memory.
2. Self-signed lab certs: add `--tls-ca`/`--insecure` client flags now, or
   assume real certs via Traefik certResolver (defer)?
3. `--public-hostname` per-server flag vs a stack-level construct?
   Flag is simpler and right for a single control plane.

## Handover notes (append as completed)

- (pending)

## Implementation notes (2026-08-08)

### Decisions on open questions

1. **Named contexts — yes.** Full `context set|use|ls` per the design; the
   config file is `~/.mc2/config.toml` (0600). The `--context`/`MC2_CONTEXT`
   flag selects a context; `current` is the implicit default.
2. **`--tls-ca`/`--insecure` deferred.** TLS is already handled by reqwest's
   `rustls-tls` for real certs; self-signed lab certs were left out of scope
   (assume Traefik certResolver). Add later if the lab needs it.
3. **`--public-hostname` is a per-server flag** (not a stack-level construct),
   with a default cert resolver of `le` (`--public-tls-cert-resolver`).

### What shipped

- **mc2 CLI (`crates/mc2`)** — new `context.rs`: `ClientConfig` (`~/.mc2/config.toml`,
  serde + toml), precedence resolution (flags > env > `--context`/`MC2_CONTEXT` >
  `current` > loopback default), deterministic `local`/`remote` mode from the
  effective URL host, and the remote-plaintext guard (`--allow-insecure-http` /
  `MC2_ALLOW_INSECURE_HTTP=1`). `OperatorArgs.api` now defaults to `""`
  (`hide_default_value`) and resolution rewrites `op.api`/`op.token` before each
  handler (handlers unchanged). `mc2 status` is mode-aware; `-o json` adds
  `mode`, `context`, `auth`, and `publicHostname`. `context set` warns (does not
  block) when saving a remote `http://` endpoint.
- **mc2-server (`crates/mc2-server`)** — `--public-hostname` and
  `--public-tls-cert-resolver` flags; hostname persisted via the new `settings`
  KV; `IngressFileWriter::reconcile` gained a third `Option<&SelfIngressRoute>`
  argument rendered as a always-available route (ready gate = REST port
  accepting, TLS via the cert resolver); `/v1/status` returns `publicHostname`.
- **mc2-api** — `ClusterStatus.public_hostname` serialized as `publicHostname`
  (`#[serde(rename)]`, `skip_serializing_if` so the key is absent when unset).
  NOTE: the doc wrote `publicHostname` while sibling status fields stay
  snake_case (`nodes_ready`, `api_version`) — the rename is deliberate to match
  the doc contract.
- **mc2-store** — migration `006_settings.sql`; `Store::{get_setting,set_setting}`
  implemented in both SQLite and memory.
- **Tests** — context unit tests (mode, precedence, guard, roundtrip, 0600),
  self-route ready/pending unit tests, `publicHostname` status integration test,
  and three CLI smoke tests (`context set/use/ls` roundtrip, empty ls hint,
  missing-context fast fail).
- **Docs** — quickstart "Remote management" section, README command table +
  blurb, CHANGELOG entry, `.env.example` additions.

### Verified in the lab

- `context set/use/ls`; config written 0600; token stored, never printed.
- Remote `http://` refused without opt-in; `MC2_ALLOW_INSECURE_HTTP=1` allows it.
- Server with `--public-hostname mc2.example.com` wrote `catalog.json` +
  `traefik/dynamic.yml` with the self-route (TLS, certResolver `le`, backend
  `127.0.0.1:<bind>`); `/v1/status` returned `publicHostname`.
- `mc2 status` table + `-o json` showed `local`/`remote`, context, auth, and
  `publicHostname` (remote loop exercised via the LAN IP since real Traefik +
  certs are out of lab scope).
- `just check` (fmt + clippy + full test suite) green.

### Known gaps / follow-ups

- Full remote loop through a real Traefik + Let's Encrypt hostname was not
  exercised end-to-end in CI (needs external infra) — documented ordering in
  quickstart instead.
- Self-signed lab certs (`--tls-ca`/`--insecure`) deferred.
- `mc2 context ls -o json` not implemented (table only; no doc requirement).
