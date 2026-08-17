# Docs site design — MC2

- Status: draft (awaiting review)
- Date: 2026-08-09
- Task: design a public documentation site for MC2 modeled on the
  microsandbox docs site, using progressive disclosure for guides and clean
  concept explanations for MC2's surfaces and operation.

## 1. Purpose

Produce the information architecture, page inventory, and page-level design
for an MC2 docs website. This document is the plan; implementation (site
scaffold + content) happens in the **separate docs repository** after this
plan is approved.

---

## 2. Microsandbox docs — catalog

Reference: https://github.com/superradcompany/microsandbox/tree/main/docs

### 2.1 Framework

**Mintlify.** Signals:

- `docs/docs.json` with `$schema: "https://mintlify.com/docs.json"`.
- Theme `maple`, branded logo (light/dark), favicon, primary color `#BF84FE`.
- `navigation.tabs` drive the whole site structure.
- Content is **MDX** with Mintlify components: `<Tip>`, `<Note>`,
  `<Steps>/<Step>`, `<CodeGroup>` (multi-language tabs),
  `<CardGroup>/<Card>`, `<Accordion>`.
- Integrations: PostHog analytics; navbar GitHub link; footer socials.

### 2.2 Areas covered

| Area | Pages |
| --- | --- |
| Getting Started | introduction, quickstart, agents |
| Sandboxes (core object) | overview, lifecycle, commands, filesystem, secrets, tuning, ssh, volumes, snapshots, labels, logs, metrics, bootstrap |
| Networking | overview, dns, tls |
| Observability | msb-metrics, deep-dive |
| Images | overview, disk-images |
| Troubleshooting | linux, macos, windows |
| SDKs | Rust / TypeScript / Python / Go x (sandbox, execution, ssh, filesystem, volumes, networking, secrets, snapshots, images, agent-client) |
| CLI | overview, sandbox-commands, ssh-commands, volume-commands, image-commands |
| Configuration | configuration |
| Recipes | docker (3), guest systemd (1), metrics backends (5) |
| Security | overview, isolation, filesystem, network, secrets, hardening |
| Changelog | 15 dated entries |

### 2.3 Structure & navigation

- **Top-level tabs** (5): `Documentation`, `References`, `Recipes`,
  `Security`, `Changelog`.
- Tabs contain **groups**, each with an icon (`rocket`, `box`,
  `network-wired`, `gauge`, `layer-group`, `screwdriver-wrench`, `code`,
  `terminal`, `gear`, `docker`, `server`).
- The SDK group uses **collapsible sub-groups** (`expanded: false`) per
  language — reference material is hidden until needed.
- `Changelog` is a flat list of dated pages, newest first.

### 2.4 Content conventions

- Every page has `title`, `description`, `icon` frontmatter.
- Concept pages follow **why -> mechanism -> visual/example -> gotchas ->
  next steps** (Introduction uses "Why microsandbox", "What makes it
  different", "Minimal example", `<CardGroup>` next steps; Quickstart adds a
  "What just happened?" debrief).
- Tutorials use `<Steps>/<Step>`, callouts (`<Tip>`/`<Note>`), and
  multi-language `<CodeGroup>`.
- Diagrams shipped as light/dark SVG pairs under `docs/images/`.

---

## 3. MC2 surface — what the docs must explain

Source: README, `docs/guides/*`, `examples/*/README.md`,
`crates/mc2/src/cli.rs`, ADRs.

### 3.1 The orchestrator (one process)

`mc2 server` = SQLite state + REST API + scheduler + reconcile loop +
embedded microsandbox SDK runtime. No daemon, no agent, no gRPC seam.
`mc2 doctor` checks host readiness (KVM / HVF).

### 3.2 The stack (desired state)

Compose-shaped YAML: top-level `name`, `services`, `volumes`, `networks`,
`ingress`. Canonical parser — unknown keys rejected loudly (no silent
no-ops). Defaults filled by `mc2 up`.

### 3.3 Services, instances, lifecycle

`scale` -> replicas. Phase model (Pending -> Running / Failed) driven by
reconcile. `restart` policies (no / on-failure / always / unless-stopped).
Health-gated startup ordering via `depends_on` + `healthcheck`.

### 3.4 Networking — three planes

1. **North-south host ports** (`ports`): loopback-bound host<->guest
   forwards; per-replica port blocks at `scale > 1`; stable auto host ports.
2. **East-west service network** (`expose` + `networks`): default-allow
   mesh, `svc.<network>.svc.mc2` DNS, server-wide named networks shared
   across stacks.
3. **Ingress** (`ingress:` + `ports`): server writes a Traefik
   file-provider catalog; BYO Traefik; HTTP(S) rules + TCP routes; TLS via
   cert resolver.

### 3.5 Secrets

Server-wide encrypted-at-rest store (`mc2 secret set`), values never echoed.
`secrets[].env` + `allowHosts` -> msb host-gated injection (placeholder in
guest, real value attached only to allowlisted hosts via TLS interception).
Distinct from plain `environment:` injection.

### 3.6 Storage, SSH, identity, observability

- **Volumes**: node-local named dir volumes, survive recreate/removal unless
  `--volumes`.
- **SSH**: host-side microsandbox SSH front end; key registry
  (`mc2 ssh add-key`), open/close endpoints, optional TCP ingress.
- **Identity/contexts**: API bearer token; `~/.mc2/config.toml` named
  contexts; local vs remote modes (https + token; insecure http refused
  unless opted in).
- **Observability**: OTLP endpoint exports `mc2.server.*` / `mc2.node.*`
  metrics; sandbox metrics stay on msb-metrics sidecar.

### 3.7 CLI surface

`server, up, down/rm, config, ps, logs, status, network, ingress, exec, ssh
(add-key/keys/show-key/rm-key/ls/show/open/close), secret (set/ls/rm),
context (set/use/ls), node ls, setup (server/client), doctor, completions,
version`. Global flags: `--api`, `--token`, `--context`,
`--allow-insecure-http`; all listing commands accept `-o json`; instance
refs accept `stack/service/ordinal`.

### 3.8 Existing content inventory

| Repo artifact | Becomes |
| --- | --- |
| `README.md` | Introduction + reference table + "What's possible" |
| `docs/guides/quickstart.md` | Quickstart tutorial + several how-tos |
| `docs/guides/stack-yaml.md` | Stack YAML reference |
| `docs/guides/secrets.md` | Secrets concept page + guide |
| `docs/guides/testing.md` | Development / contributing guide |
| `examples/*/README.md` | Recipes (copy-and-run) |
| `CHANGELOG.md` | Changelog tab entries |
| ADRs | Development section (internal) |

---

## 4. Design goals & principles

1. **Progressively disclose.** The learner's path is: concept (why) ->
   quickstart (run it) -> core how-tos (do one thing) -> reference (exhaustive,
   collapsed). Reference material never blocks the learning path.
2. **Concept pages explain, guides instruct.** Every MC2 surface has a
   concept page that establishes the mental model in plain language; guides
   then show how to operate it.
3. **Compose vocabulary as the bridge.** MC2's differentiator is "Docker
   Compose-shaped". Every concept page anchors to a Compose counterpart and
   states exactly where MC2 differs (reconciliation, microVM isolation,
   server-wide networks, host-gated secrets).
4. **One binary, one mental model.** The architecture concept page makes the
   single-process model (SQLite + REST + scheduler + reconcile + embedded
   msb runtime) obvious, so every later page can refer back to it.
5. **Parity with microsandbox.** Same five-tab skeleton, same component
   vocabulary, same icon + collapsible-group conventions, so contributors and
   users who know microsandbox feel at home.
6. **Copy-and-run examples everywhere.** Every guide and recipe is runnable
   from the `examples/` tree; `UNDER DEVELOPMENT` markers are honored.

---

## 5. Framework & tooling recommendation

**Recommendation: Mintlify**, for direct parity with microsandbox:

- `docs.json` navigation means the IA below maps 1:1 and is trivially
  reorganized.
- MDX + built-in components (`Steps`, `CodeGroup`, `CardGroup`, callouts)
  match the design language we are borrowing.
- Light/dark SVG diagram pairs, changelog flat lists, analytics, and SEO come
  free.
- Zero build/maintenance burden; content lives in a docs repo separate from
  the Rust code (matching the README note).

**Alternative (if the team prefers to self-host):** a Next.js (App Router)
docs app, since the global environment already standardizes on
Next.js + Tailwind. The information architecture in section 6 is framework-
agnostic; only the component mapping changes (Steps -> ordered list +
callout, CodeGroup -> tabbed code block, docs.json -> a nav config file).

Open question for review: Mintlify (recommended) vs Next.js self-hosted.

---

## 6. Information architecture

Five top-level tabs. Icons mirror microsandbox conventions.

### Tab 1: Documentation (Learn)

**Getting Started** (`rocket`)
1. `introduction` — What MC2 is; the four-way comparison table (MC2 vs
   Docker Compose vs Kubernetes vs Firecracker); "Why MC2"; "What's
   possible" (agent swarm, compose migration, remote SSH, test grid, browser
   fleet); "Minimal example" (smallest stack + `mc2 up`); next-steps cards.
2. `quickstart` — Install -> `mc2 doctor` -> `mc2 setup` (server then
   client; lab skip: `mc2 server --no-auth` with defaults) ->
   `mc2 up -f examples/01-hello-service` -> `mc2 ps` -> `curl`. Ends with
   "What just happened?" debrief (desired state published, reconcile loop,
   phase transitions) and next steps.

**Concepts** (`lightbulb`) — the clean-explanation layer, one page per
surface. See section 7 for full outlines.
1. `concepts/architecture` — one process: REST + SQLite + scheduler +
   reconcile loop + embedded msb runtime; control plane vs node; what
   `mc2 doctor` verifies.
2. `concepts/desired-state` — the operating model: publish desired state,
   converge, no drift; idempotent `mc2 up`; phases Pending -> Running /
   Failed; reconcile on interval.
3. `concepts/stacks` — the Compose-shaped document; `name`/`services`/
   `volumes`/`networks`/`ingress`; canonical parser (unknown keys rejected);
   defaults `mc2 up` fills; where k8s-style keys are rejected.
4. `concepts/services-and-instances` — `scale`, restart policies, health
   checks, `depends_on` ordering, per-replica ports, `stack/service/ordinal`
   addressing.
5. `concepts/networking` — the three planes (host ports, service networks,
   ingress) and how they relate; loopback-only host binds; DNS.
6. `concepts/secrets` — server-wide encrypted store vs `environment:`; host-
   gated injection, `allowHosts`, placeholders, TLS interception; collision
   precedence.
7. `concepts/storage` — node-local named volumes; lifecycle (survive
   recreate/removal); `--volumes` teardown.
8. `concepts/identity-and-contexts` — API token, local vs remote mode,
   `~/.mc2/config.toml`, resolution precedence, https enforcement.
9. `concepts/observability` — OTLP metrics; what is exported by the server
   vs what stays on the msb sidecar.

**Guides** (`book-open`) — the progressive how-to path. Ordered so each
builds on the last; each starts with "You will learn" + prerequisites and
links to the relevant concept page.
1. `guides/run-your-first-stack` — from `examples/01-hello-service`; verify
   with curl; `mc2 config` validate; `mc2 down`.
2. `guides/scale-and-restart` — `scale: 3`, per-replica ports, `restart`
   policies, `mc2 ps` phases.
3. `guides/connect-services` — `expose` + networks; `svc.<net>.svc.mc2`
   DNS; `mc2 network` inspection; from `examples/03-networks`.
4. `guides/publish-with-ingress` — `ports` + `ingress:` + BYO Traefik;
   `--ingress-config-dir`; from `examples/04-http-ingress`.
5. `guides/add-secrets` — `mc2 secret set` -> stack refs -> `allowHosts`;
   from `examples/02-secrets`.
6. `guides/open-ssh` — `ssh: true`, `mc2 ssh add-key`, `mc2 ssh ls`, TCP
   ingress; from `examples/05-ssh-ingress`.
7. `guides/use-persistent-volumes` — top-level `volumes`, mounts, survival
   semantics; from `examples/06-persistent-volumes`.
8. `guides/order-startup` — `healthcheck` + `depends_on` conditions; from
   `examples/07-startup-ordering`.
9. `guides/manage-a-remote-server` — contexts, `mc2 setup` (server/client
   trees), `--public-hostname`, remote https.
10. `guides/tear-down-and-clean-up` — `down`/`rm`, `--volumes`, volume
    retention; hygiene.

**Operations** (`server`) — day-2 material.
1. `operations/run-the-server` — server flags/env vars, data dir, auth
   bootstrap, OTLP endpoint, `--public-hostname`.
2. `operations/upgrade-and-migrate` — versioning, data-dir compatibility,
   release channels (from release-plz / CHANGELOG).
3. `operations/troubleshooting` — `mc2 doctor`, common failures, Linux vs
   macOS notes, CI-vs-lab testing split.

### Tab 2: References

**Stack YAML** (`file-code`) — full reference (the exhaustive reference
tables in `stack-yaml.md`, split for readability).
1. `stack/overview` — document shape, defaults, validation summary.
2. `stack/services` — every `services.<name>` key with defaults table.
3. `stack/ports-expose` — `ports[]` short/long form, auto ports,
   per-replica blocks; `expose[]` listeners.
4. `stack/networks-ingress` — `networks`, `network.profiles`, `ingress`
   rules/tcp/tls.
5. `stack/secrets-volumes` — `secrets[]` with `allowHosts`;
   `volumes[]` mounts.
6. `stack/health-depends` — `healthcheck`, `depends_on` conditions.
7. `stack/ssh-node` — `ssh`, `nodeName`, `nodeSelector`.

**CLI** (`terminal`) — collapsible sub-groups, mirroring microsandbox's CLI
group.
- `cli/overview` — command tree, global flags, connection resolution,
  instance refs, `-o json`.
- `cli/stacks` — `up`, `down`/`rm`, `config`, `ps`.
- `cli/observe` — `logs`, `status`, `network`, `ingress`.
- `cli/access` — `exec`, `ssh` subcommands.
- `cli/security` — `secret`, `context`, token.
- `cli/operate` — `server`, `doctor`, `node`, `setup`, `completions`.

**REST API** (`api`) — generated/documented from `crates/mc2-server/src/api`.
- `api/overview` — base URL, auth (bearer token), errors, `-o json` parity.
- `api/status` — `/v1/status`, health, counts, publicHostname.
- `api/stacks-instances` — apply, ps, lifecycle.
- `api/networking-ingress` — networks, ingress routes.
- `api/secrets-ssh` — secrets CRUD (values never returned), SSH endpoints.

**Configuration** (`gear`)
- `config/environment` — every env var (`MC2_API`, `MC2_API_KEY`,
  `MC2_CONTEXT`, `MC2_OTLP_ENDPOINT`, `MC2_SECRET_VALUE`, ...).
- `config/contexts-file` — `~/.mc2/config.toml`, resolution precedence,
  permissions.

### Tab 3: Recipes (`flask-conical`)

Copy-and-run scenario walkthroughs, each mapping to an example.
1. `recipes/agent-swarm` — `scale: 20` agents, per-agent microVM, restart,
   exec/logs.
2. `recipes/compose-to-mc2` — migrate a `docker-compose.yml`; mapping table
   of supported keys and intentional differences.
3. `recipes/remote-ssh-agents` — SSH dev VMs, `mc2 ssh add-key`, remote
   management over TLS.
4. `recipes/throwaway-test-grid` — N identical VMs, run suite, `down
   --volumes`; cache volume reuse.
5. `recipes/headless-browser-fleet` — per-VM network-locked scrapers.
6. `recipes/shared-network-multi-stack` — server-wide named networks across
   stacks.
7. `recipes/metrics-backends` — OTLP collector + Grafana (map to advanced
   observability example).

### Tab 4: Security (`shield-half`)

Concept-first, cross-cutting. Maps to MC2's surfaces.
1. `security/overview` — trust boundary: control plane vs sandbox vs guest;
   what MC2 guarantees and what it delegates (BYO Traefik TLS, host trust).
2. `security/isolation` — microVM isolation per replica; host ports bind
   loopback; network profiles (`public`/`private`/`host`/`none`).
3. `security/secrets` — encrypted at rest, never listed, host-gated
   injection, `allowHosts` fail-closed, collision precedence.
4. `security/networking` — default-allow east-west, unique server-wide
   expose ports, remote https enforcement, `--allow-insecure-http`.
5. `security/authentication` — API token, remote mode requirements, context
   file permissions.
6. `security/hardening` — production checklist (TLS, token rotation, data
   dir permissions, no `--no-auth`, dedicated user).

### Tab 5: Changelog (`clock`)

Flat, newest-first, derived from `CHANGELOG.md` (one entry per release).

---

## 7. Concept page design (the "clean explanations")

Common template, reused for every page in the Concepts group:

1. **Plain-English summary** (2-3 sentences, no jargon) — what this surface
   is and the one job it does.
2. **The analogy** — anchor to Docker Compose / containers / k8s and state
   the exact difference (e.g. "a service is like a compose service, but each
   replica is a separate microVM; MC2 converges, it does not run once").
3. **How it works** — the mechanism, with a light/dark SVG diagram where a
   diagram helps.
4. **In practice** — the smallest YAML or CLI that exercises it.
5. **Key behaviors & gotchas** — bullets: what you can rely on, what fails
   closed, current v1 limits (single-node, mediated-only networks, dir-only
   volumes).
6. **Deeper** — links to the relevant guide, reference pages, and example.

### 7.1 Concepts/architecture — outline

- Summary: "MC2 is one process. `mc2 server` is the whole orchestrator: it
  stores state, serves the REST API, schedules work, and reconciles real
  microVMs toward what you declared. There is no daemon to install and no
  agent to run on the host."
- Diagram: control plane (REST + SQLite + scheduler + reconcile loop) -> node
  (embedded msb runtime) -> microVMs.
- Key behaviors: single-node v1; `mc2 doctor` verifies hypervisor/msb/paths
  before boot; REST/scheduler serve without a hypervisor, instances fail with
  a runtime error; `--node-name`, `--label`, `--volume-dir`,
  `--ingress-config-dir` are server-level knobs.
- Deeper: quickstart, operations/run-the-server, REST API overview.

### 7.2 Concepts/desired-state — outline

- Summary: "You write what you want; MC2 makes it true and keeps it true.
  `mc2 up` publishes a stack as the desired state and the server converges
  to it on an interval. Nothing is run-once and nothing drifts."
- Mechanism: apply -> persisted spec (spec_json) -> build desired set ->
  reconcile loop compares and drives instances through phases
  (Pending -> Running / Failed); restart policies re-create failed replicas.
- Key behaviors: idempotent re-apply; `mc2 down` removes instances + def;
  volumes retained unless `--volumes`; `depends_on` gates startup, not
  teardown.
- Deeper: services-and-instances, guides/scale-and-restart,
  guides/tear-down-and-clean-up.

### 7.3 Concepts/networking — outline

- Summary: "MC2 has three network planes: host ports for outside-in access,
  service networks for inside-in communication, and ingress routes for
  hostnames. They are independent and compose together."
- The three planes (diagram): `ports` (north-south, loopback host binds) /
  `expose` + `networks` (east-west, default-allow, DNS) / `ingress:` (Traefik
  file catalog).
- Key behaviors: host ports always bind loopback, expose through ingress;
  per-replica port blocks at scale; exposed ports effectively unique
  server-wide (collisions report `Failed` edge); networks server-wide and
  shareable across stacks; `svc.<network>.svc.mc2` DNS.
- Deeper: guides/connect-services, guides/publish-with-ingress, security/
  networking, stack/ports-expose, stack/networks-ingress.

### 7.4 Concepts/secrets — outline

- Summary: "MC2 secrets are encrypted at rest, never listed back, and the
  real value is attached to outbound connections to allowlisted hosts only —
  not copied into the guest. This is different from `environment:`, which is
  plaintext injected directly."
- Mechanism diagram: server-wide store -> reconcile -> msb `secret`
  mechanism (placeholder in guest + TLS interception on egress to
  `allowHosts`).
- Key behaviors: fail-closed (empty `allowHosts` errors); rotation = re-set +
  re-apply; `environment:` wins collisions; `MSB_` prefix reserved.
- Deeper: guides/add-secrets, security/secrets, stack/secrets-volumes,
  examples/02-secrets.

### 7.5 Concepts/storage — outline

- Summary: "A volume is a named directory on the node that survives sandbox
  recreation, restarts, and stack removal. It is the way state outlives a
  microVM."
- Key behaviors: must be declared top-level and referenced; `dir` kind only
  in v1; name constraints (`--` forbidden); resolved to
  `mc2-<stack>--<volume>`; retained unless `down --volumes`.
- Deeper: guides/use-persistent-volumes, stack/secrets-volumes,
  examples/06-persistent-volumes.

### 7.6 Concepts/identity-and-contexts — outline

- Summary: "The CLI is a REST client. A context names a server (URL + token)
  so the same commands work on your laptop or a remote VPS; local and remote
  modes differ in trust."
- Mechanism: resolution precedence (`--api`/`--token` > env > context >
  current > loopback default); remote requires https + token; plaintext http
  refused unless opted in; `~/.mc2/config.toml` mode 0600.
- Key behaviors: `mc2 setup` scaffolds server and client trees; context ls
  reports mode; `--public-hostname` publishes the control plane through the
  ingress catalog.
- Deeper: guides/manage-a-remote-server, config/contexts-file,
  security/authentication.

---

## 8. Progressive disclosure map

| Level | Where | Reader | Exit criteria |
| --- | --- | --- | --- |
| 1 — Orient | introduction, concepts | newcomer | Can explain what MC2 is and its model in one sentence |
| 2 — Run | quickstart | newcomer | Has a running hello service |
| 3 — Operate | guides (ordered) | operator | Can stand up a scaled, networked, secret-bearing stack |
| 4 — Reference | stack/CLI/API/config | experienced operator | Looks up exact keys/flags/endpoints |
| 5 — Extend | recipes, operations | power user / admin | Composes custom scenarios, runs remote, hardens |

Disclosure mechanics:
- Concepts first in nav; guides after; references collapsed into sub-groups.
- Each guide states prerequisites and links its concept page (no orphan
  how-tos).
- Reference pages are tables-first (readable top to bottom, skimmable).
- Recipes are entirely optional; nothing in levels 1-3 depends on them.

---

## 9. Implementation notes

- Docs repo is separate (matches README). If Mintlify: one `docs.json`,
  folders mirroring groups, MDX per page, images/ SVG light+dark pairs.
- Content migration: port existing guides 1:1 (they are already
  well-written), then split `stack-yaml.md` into the reference set, then
  write the new Concepts + operations pages.
- New writing required: concepts (9), operations (3), REST API (5), recipes
  (7), security (6), introduction. Reuse README + examples heavily.
- Diagrams needed (light/dark pairs): architecture, desired-state loop,
  network planes, secrets flow, remote contexts.
- `CHANGELOG.md` -> one entry per release under the Changelog tab.

## 10. Open questions for review

1. Framework: **Mintlify (recommended)** or Next.js self-hosted?
2. Should the site live in a new `mc2-docs` repo now, or start in-repo under
   `site/` until content settles?
3. Scope of the first ship: all five tabs, or Documentation + References
   first (Recipes/Security/Changelog as fast-follow)?
4. Logo/branding: reuse mc2 name + color, or mint a docs-specific mark?
