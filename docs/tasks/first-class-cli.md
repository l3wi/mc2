# Task: First-class CLI — API parity + dist packaging

Status: **DONE 2026-08-08** (commit `3ea67a7`, pushed to `dev`)
Date: 2026-08-08

## Context

Two asks: (1) the CLI should cover the whole REST surface, not a subset;
(2) MC2 should be installable like a real tool — one-command install,
completions, checksummed releases — using Rust's standard distribution
tooling.

## Part 1 — Parity audit (REST ↔ CLI)

REST surface (`crates/mc2-server/src/http.rs`) vs CLI (`crates/mc2`):

| Endpoint | Method | CLI today | Gap |
| --- | --- | --- | --- |
| `/health` | GET | — (doctor is local-only) | add `mc2 status` health check |
| `/v1/status` | GET | ❌ | **`mc2 status`** |
| `/v1/nodes` | GET | `mc2 node ls` | ✅ |
| `/v1/stacks:apply` | POST | `mc2 apply -f` | ✅ |
| `/v1/instances` | GET | `mc2 ps` | ✅ (add `-o json`, stack filter) |
| `/v1/instances/{id}/fabric` | GET | ❌ | **`mc2 fabric <id>`** |
| `/v1/ingress` | GET | ❌ | **`mc2 ingress`** |
| `/v1/secrets` | GET | `mc2 secret ls` | ✅ |
| `/v1/secrets/{name}` | PUT/DEL | `mc2 secret set/rm` | ✅ |
| `/v1/ssh/keys` | GET | `mc2 ssh key ls` | ✅ |
| `/v1/ssh/keys/{name}` | GET/PUT/DEL | add/rm | **`mc2 ssh key show`** |
| `/v1/ssh/endpoints` | GET | `mc2 ssh ls` | ✅ |
| `/v1/instances/{id}/ssh` | GET/PUT/DEL | show/open/close | ✅ |

### Planned commands (all thin wrappers over existing REST; no server changes)

```text
mc2 status              # GET /health + /v1/status → version, nodes, stacks, instances
mc2 fabric <instance>   # GET /v1/instances/{id}/fabric → expose/edge table
mc2 ingress             # GET /v1/ingress → route table (host, path, service, ports, ready)
mc2 ssh key show <name> # GET /v1/ssh/keys/{name} → fingerprint + key
```

Plus cross-cutting CLI quality:

- **`-o json` / `-o table`** on every listing command (`ps`, `node ls`,
  `secret ls`, `ingress`, `fabric`, `ssh ls`, `ssh key ls`). Table stays the
  default (human), JSON for scripts (`mc2 ps -o json | jq ...`).
- **`mc2 ps` filters**: `--stack <name>`, `--service <name>` (client-side
  filtering; REST already returns full list), and a PORTS column once
  host-port ownership lands (follow-up task; column stubbed now, hidden
  until ports are allocated).
- **Consistent errors**: non-zero exit + one-line `error: ...` on non-2xx
  (currently `bail!("ps failed: {status} {body}")` — unify shape).
- **Instance addressing**: every command taking an instance id also accepts
  `<stack>/<service>/<ordinal>` (e.g. `mc2 fabric hello/web/0`) resolved via
  `GET /v1/instances` — full UUIDs are hostile to humans.

### Tests

- `cli_smoke.rs`: help output lists the new commands; `-o json` parses.
- Integration (`mc2-tests`): new `cli_api_parity`-style coverage is NOT
  needed — commands are thin wrappers; instead extend `server_rest.rs` with
  `/v1/instances/{id}/fabric` 404 behavior if untested (it is). One new
  integration test: apply → `build_desired_set` fabric JSON shape already
  covered; skip.
- **Net test additions: smoke only** (parity is mechanical wrappers).

## Part 2 — Distribution research: Rust's `dist` (cargo-dist)

### State of the ecosystem (2026-08)

- **`dist`** (formerly cargo-dist, axodotdev) is the de-facto standard:
  v0.32.0 (May 2026), very active. v0.29.0 merged Astral's fork features
  (the fork that ships `uv`/`rye` installers), so the tool carries
  production-proven patterns. ~2k stars.
- What it gives us out of the box:
  - Generated GitHub Actions `release.yml` (replaces our hand-rolled one)
  - Per-target native builds: linux-amd64, linux-arm64, darwin-arm64 (our
    exact matrix) + optional musl, macOS x64
  - **Tarballs + SHA256 checksums** (`mc2-x.y.z-aarch64-apple-darwin.tar.xz` + `SHA256SUMS`)
  - **Shell installer** (`curl -fsSL .../install.sh | sh`) — rustup-style,
    picks the right target triple
  - **Homebrew formula** (tap or core-ready), PowerShell installer, npm
    wrapper package (optional)
  - GitHub artifact attestations (supply chain)
  - Config lives in `[workspace.metadata.dist]`; `dist init` scaffolds it.
- Alternatives considered and rejected:
  - **Keep hand-rolled release.yml**: works, but no installers/checksums;
    every improvement is maintenance we own. dist generates and owns this.
  - **cargo-binstall**: only helps projects on crates.io; mc2 is a private
    workspace binary.
  - **homebrew-core only**: slow review cycle, no linux; dist's tap is ours.

### Fit check for mc2

- Binary crate `mc2` is the only published artifact → dist's simple-app
  workspace mode fits perfectly.
- **Cross-compilation caveat**: microsandbox pulls in `libkrun`/`smoltcp`
  native deps. Our current workflow already cross-builds linux-arm64 with
  `cross`; dist supports custom build steps per target, and we can keep
  the same `cross` approach via dist's CI customization. darwin-arm64 builds
  natively on macos runners.
- **Prerequisite**: `Cargo.toml` workspace `repository` is currently
  `https://github.com/placeholder/mc2`. dist installer URLs point at the
  real GitHub repo — **needs a real repo URL before installer publishing**
  (binary tarballs work regardless).

### Planned dist config

```toml
# [workspace.metadata.dist]
dist-version = "..."
targets = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "aarch64-apple-darwin"]
installers = ["shell"]        # + "homebrew" once the repo URL exists
# homebrew tap optional; shell installer is the MVP
```

Plus:
- **Shell completions**: `clap_complete` (bash/zsh/fish) generated by
  `mc2 completions <shell>` and shipped inside the tarball; the shell
  installer offers to install them.
- Keep `release.yml` replaced (not parallel) — dist generates
  `.github/workflows/release.yml`; old one deleted (pre-release: no
  legacy).

## Implementation order

1. **CLI parity commands** (`status`, `fabric`, `ingress`, `ssh key show`)
   + `-o json` plumbing + `<stack>/<svc>/<ord>` addressing + error shape.
2. `clap_complete` → `mc2 completions` + unit test that help/completions
   parse.
3. `dist init` config, replace release.yml, verify `dist plan` locally.
4. Smoke: build release binary, run completions + new commands against a
   lab server.
5. Docs: quickstart install section (installer once repo URL is set),
   CHANGELOG, README install line.

## Acceptance criteria

- [ ] Every REST endpoint reachable from `mc2 <command>` (parity table above
      all ✅).
- [ ] `-o json` on all listing commands; output is `jq`-clean.
- [ ] `mc2 fabric hello/web/0` works without knowing the UUID.
- [ ] `mc2 completions bash|zsh|fish` emits loadable completions.
- [ ] `dist plan` succeeds; release workflow builds the 3-target matrix with
      checksummed tarballs.
- [ ] `just check` green.

## Open question (resolved)

- Repo created public at **github.com/l3wi/mc2**; installer enabled in the
  same change.

## Handover notes (append as completed)

- **Done 2026-08-08.** Repo public, all commits pushed. dist 0.32 (brew
  `axodotdev/tap/cargo-dist`); config in `[workspace.metadata.dist]`;
  `dist generate` wrote `.github/workflows/release.yml` (old hand-rolled
  one deleted); `dist plan --tag=v0.1.0` verified: 3 targets on native
  runners (macos-14, ubuntu-22.04-arm, ubuntu-22.04 — no cross), tar.xz +
  sha256 + `mc2-installer.sh`. First release fires on the first `vX.Y.Z`
  tag push. Lab smoke: status / ps / ingress / fabric / ssh key show /
  completions all verified against a live server; a fabric edge failure
  (host port 8080 occupied — environmental) correctly surfaced through the
  unified error path.
