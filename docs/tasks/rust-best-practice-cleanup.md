# Rust Best-Practice Cleanup

Status: **proposed** (awaiting review)
Branch: `dev`
Owned by: review against the "Rust best practices" baseline (module boundaries, file sizes,
tooling, types-over-conventions, error handling, visibility, docs, CI).

## Context

MC2 is already a well-structured Rust workspace:

```
Cargo.toml                (workspace.dependencies, profile.release, dist metadata)
crates/
  mc2-api/                shared REST DTOs + stack YAML schema
  mc2-store/              Store trait + SQLite (prod) + Memory (tests) backends
  mc2-runtime/            NodeRuntime trait + microsandbox SDK backend
  mc2-server/             orchestrator: REST, scheduler, local node loop, ingress, ssh, network
  mc2-metrics/            optional OTLP metrics
  mc2/                    operator CLI + server binary (main.rs)
tests/                    integration harness (mc2-tests)
```

The baseline checks already pass locally:

| Check | Status |
|---|---|
| `cargo fmt --all -- --check` | ✅ clean |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | ✅ clean |
| `cargo test --workspace` | ✅ 150+ tests green |
| `cargo doc --workspace --all-features --no-deps` | ⚠️ 9 rustdoc warnings (would fail `-D warnings`) |

Many best practices are already in place and should be **preserved**:
- Workspace deps centralized via `[workspace.dependencies]`; version/edition/license via `[workspace.package]`.
- Domain-organized modules (`auth.rs`, `scheduler.rs`, `secrets.rs`, …) — no `models.rs`/`utils.rs` junk drawers.
- Unit tests next to code (`#[cfg(test)] mod tests`); black-box integration tests in the `tests/` package and `crates/mc2/tests/cli_smoke.rs`.
- Typed errors where it matters (`StoreError`, `CryptoError`).
- **Zero `unsafe`** in the entire codebase.
- `unwrap`/`expect` are essentially confined to tests; production uses `expect("reason")` only for true invariants (signal handlers, clap help rendering).
- `lib.rs` files are thin re-export/composition layers.
- Trait usage is purposeful (`Store`, `NodeRuntime`) — no Java-style `FooServiceTrait`.

The gaps are concentrated in: **file size / multiple responsibilities**, **fragile string-matching error handling**, **dead public API**, **rustdoc warnings**, and **CI coverage** (`cargo doc`, `--all-features`, `machete`/`deny`/`audit`).

## Review findings (deep)

### F1. Oversized files hiding multiple responsibilities

| File | LOC | What it mixes |
|---|---|---|
| `crates/mc2/src/main.rs` | 2123 | clap arg structs; `main` dispatch; ~15 command handlers (stacks, observe, access, security); REST client helpers; grouped-help renderer; tests. |
| `crates/mc2-api/src/stack.rs` | 1698 | schema structs + custom serde deserializers + validation + fqdn/naming helpers + large test module. |
| `crates/mc2-server/src/http.rs` | 1328 | router + all REST handlers (stacks, instances, exec/logs, secrets, ssh, ingress, nodes) + tests. |
| `crates/mc2-store/src/sqlite.rs` | 1205 | one coherent `Store` impl (see F8 — mostly OK). |
| `crates/mc2-server/src/node.rs` | 852 | reconcile loop + health-check logic + deps gating + tests; `reconcile()` is deep-nested and takes 10 params. |

`main.rs` and `http.rs` clearly violate "file represents one coherent thing". The help output already
defines the CLI domain groups (`Stacks`, `Observe`, `Access`, `Security`, `Admin`) — a natural module layout.

### F2. Fragile string-matching for error classification

- `http.rs:is_stack_client_error()` decides 400 vs 500 by substring-matching error text
  (`"must "`, `"expose"`, `"replicas"`, `"restartpolicy"`, …). Any new validation message silently
  becomes a 500.
- `put_secret`/`put_ssh_key` similarly substring-match (`"required"`, `"empty"`, `"unsupported"`, `"short"`).
- CLI `api_error()` and network/exec/ssh handlers duplicate this idea server-side.

Better: a typed `ApplyError { Validation(String) | Store(StoreError) | … }` → `ApiError` implementing
`IntoResponse`; handlers return `ApiResult<T>`.

### F3. Repetitive handler error plumbing

Every handler repeats `Result<Json<T>, (StatusCode, Json<serde_json::Value>)>` + a `store_err` helper +
`map_err` closures. A shared `ApiError` (with `store`, `not_found`, `bad_request`, `internal` constructors)
would cut ~40% of `http.rs`'s noise.

### F4. Dead / vestigial public API

Confirmed unused across the workspace (clippy can't see unused `pub` items):
- `mc2_runtime::network_expose_guest_ports` — exported, never called.
- `mc2_store::Store::list_instance_network` — trait method + both impls, never called.
- `mc2_runtime::make_route_id` / `normalize_path` — thin re-wraps of `mc2_api` fns, only used internally/test.
- `crates/mc2-server/src/ssh.rs:95` `let _ = key_names;` — vestigial no-op.
- `mc2_api::default_network_fqdn` — only used in a test.

### F5. Rustdoc warnings (CI-breaking for `cargo doc`)

`cargo doc --no-deps` emits 9 warnings: `<stack>/<service>/<ordinal>` in `main.rs` doc comments
(lines 227, 236, 299) parsed as unclosed HTML tags. `RUSTDOCFLAGS="-D warnings"` + a doc job would fail today.

### F6. CI vs the recommended gate

Current `ci.yml`: fmt, clippy (`--all-targets`, **no `--all-features`**), `cargo test --workspace`, build bin.
Missing vs baseline: `cargo doc` check, `--all-features`, `cargo machete`, `cargo deny`, `cargo audit`,
`cargo nextest` (optional). No `[workspace.lints]` table exists.

### F7. `#[allow]` smell: `reconcile` takes 10 args

`node.rs:163` `#[allow(clippy::too_many_arguments)]` on `reconcile(store, secrets_key, node_id, runtime,
&mut owned, &mut rt_state, &mut ssh_table, &mut network_table, ingress_writer, self_route)`. This is the
textbook "introduce a domain struct" case: bundle the mutable reconcile state into one `ReconcileState`
(or `NodeContext`). The health-probe block inside the loop is ~100 lines of nested `if let` and should be
extracted to a `health.rs`.

### F8. `sqlite.rs` (1205 LOC) — deliberate non-change

`impl Store for SqliteStore` must live in one file (Rust forbids splitting a trait impl). The file is one
coherent thing (the SQLite backend). Recommendation: keep as-is; optionally extract row-mapping helpers to
trim ~80 lines. Splitting further would harm cohesion.

### F9. Magic phase/status strings ("types over conventions")

Records store `phase: String` / `status: String` and code compares against literals (`"Running"`,
`"Pending"`, `"Ready"`, `"Closed"`, `"Open"`, …) throughout server + runtime + CLI. Enums exist
(`SandboxPhase`, `InstancePhase`, `NodeStatus`) but are used inconsistently; `InstancePhase::parse`/
`as_str` exist but records bypass them. Full enum typing is a large cross-cutting refactor (DB + REST
boundaries). MVP: enforce the existing enums at the module boundaries (node reconcile, scheduler, network
summary) and centralize phase literals, leaving SQLite/REST as string boundaries.

### F10. Testing/tooling niceties (low priority)

- `tests/tests/*.rs` nesting is fine (documented harness pattern).
- `MSB_HOME` mutation in `http.rs` log test + `HOME_LOCK` in `context.rs` are process-global workarounds — acceptable, but worth a comment.
- No `docs/architecture/` directory exists yet (global convention expects it kept in sync with PRs).

## Goals

1. Get the repo clean against the recommended tooling baseline (fmt, clippy all-targets+all-features,
   test, doc, machete/deny/audit).
2. Split the three oversized files on **semantic boundaries** (not line counts), keeping public API and
   behavior identical (pure moves first; refinements second).
3. Replace string-matched error classification with typed errors.
4. Remove dead public API and vestigial code.
5. Tighten the node loop (remove the `too_many_arguments` allow; extract health logic).
6. Enforce a couple of workspace lints that match the codebase's actual posture (`unsafe_code = "deny"`).
7. Scope the typed-phase refactor as a clearly-bounded MVP so it stays reviewable.

## Non-goals

- No behavior changes, no API/schema changes, no feature additions.
- No wholesale enum-typing of every store column (F9 kept to module boundaries).
- No new crate boundaries (keep the current 7-crate workspace shape).
- No `sqlite.rs` re-split (F8).
- Do not enable blanket pedantic lints (`clippy::pedantic`, `missing_docs` everywhere) without sign-off.

## Plan

### Phase 0 — Tooling baseline (P0, small, independent)
1. **Fix rustdoc warnings**: wrap `<stack>/<service>/<ordinal>` in backticks in `main.rs` (3 sites).
2. **Add `[workspace.lints.rust]`**: `unsafe_code = "deny"` (codifies the current zero-unsafe posture).
3. **Harden CI** (`ci.yml`):
   - clippy: add `--all-features`.
   - add `cargo doc --workspace --all-features --no-deps` step with `RUSTDOCFLAGS="-D warnings"`.
   - add `cargo machete` step (unused-deps gate).
   - add `cargo deny check` (licenses/advisories/sources; start with a permissive `deny.toml`).
   - add `cargo audit` step (advisories) — can be non-blocking initially.
4. **Add `just` recipes** mirroring the new gates (`just check` grows doc + machete + deny).
5. Verify: `just check` green; `cargo doc -D warnings` green.

### Phase 1 — Split `crates/mc2/src/main.rs` (2123 → thin) (P0)
Pure moves; behavior identical. Map modules onto the existing help groups:
```
crates/mc2/src/
  main.rs            # tracing init, main() dispatch, doctor_cmd, context_* cmds (~150-250)
  cli.rs             # Cli, Commands, all *Args, OutputFormat, ListArgs (~370)
  client.rs          # operator_get/post, api_error, resolve_instance_id, urlencoding_simple (~200)
  help.rs            # CommandGroup/HelpStyles/grouped-help renderer (~200)
  context.rs, prompt.rs, setup.rs   # (existing)
  cmd/
    mod.rs           # re-exports + OutputFormat helpers
    stacks.rs        # up, down, config + fill_stack_defaults + stack_name_from_path + sanitize + print_ssh_endpoints
    observe.rs       # ps, status, node ls, network family + printers, ingress
    access.rs        # exec, logs + print_log_entry
    security.rs      # secret set/ls/rm, ssh keys + endpoints + instance ssh
```
Tests move with their code. `crate::tests` keep compiling via `cmd::*` imports in `main.rs`.
Verify: `cargo test --workspace` green; `cargo clippy -D warnings` green; `cargo fmt` green.

### Phase 2 — Split `crates/mc2-server/src/http.rs` (1328 → api/) (P0)
Pure-ish moves + one refinement (typed errors).
```
crates/mc2-server/src/api/
  mod.rs        # router() assembly + ApiError (IntoResponse) + ApiResult<T> + store_err/not_found helpers
  status.rs     # health, status, list_nodes
  stacks.rs     # apply_stack (+ typed ApplyError), delete_stack
  instances.rs  # list_instances, get_instance_network, exec_instance, get_instance_logs (+ SSE helpers)
  secrets.rs    # list_secrets, put_secret, delete_secret
  ssh.rs        # list/get/put/delete_ssh_key, list_ssh_endpoints, get/put/delete_instance_ssh, ssh_view
  ingress.rs    # list_ingress
```
Refinement (same phase, small): add `ApiError` and convert handlers to `ApiResult<T>`;
replace `is_stack_client_error`/substring checks with a typed `ApplyError::Validation`.
`lib.rs` re-export: `pub use http::router` → `pub use api::router`.
Verify: green workspace tests; REST handler unit tests still pass; `cargo clippy -D warnings`.

### Phase 3 — Split `crates/mc2-api/src/stack.rs` (1698) (P1)
```
crates/mc2-api/src/stack/
  mod.rs       # parse_stack_yaml, validate_stack entry, fqdn/naming helpers, re-exports
  schema.rs    # StackDocument, ServiceSpec, Ingress*, PortSpec, ExposeSpec, SshSpec, Healthcheck*, DependsOn*, Network*, SecretRef, Volume* (~350)
  decode.rs    # de_env, de_command, de_ports, de_expose, de_ssh, de_depends_on, de_mem_limit, de_duration, de_interval, de_healthcheck_test, split_command_string (~300)
  validate.rs  # validate_stack + validate_depends_on + validate_volumes + validate_ingress + valid_volume_name (~300)
```
Tests move with their code. Public re-exports from `mc2-api::lib` unchanged.
Verify: `cargo test -p mc2-api` green; external imports unchanged.

### Phase 4 — Tighten `node.rs`: reconcile state struct + health extraction (P1)
- Introduce `ReconcileState { owned, rt_state, ssh_table, network_table, ingress_writer, self_route }`
  (or `NodeContext` grouping store/secrets/runtime/node_id). Drop `#[allow(clippy::too_many_arguments)]`.
- Extract the ~100-line health-probe block into `health.rs`: `HealthRunner`/`run_health_probe` +
  `handle_health_failure` + `health_active` (moved from `node.rs`).
- Keep `run()` loop and `build_desired_set` unchanged.
Verify: `cargo test -p mc2-server` green; clippy clean **without** the `too_many_arguments` allow.

### Phase 5 — Dead code + unused dependency sweep (P1)
- Remove `mc2_runtime::network_expose_guest_ports` (and its re-export).
- Remove `Store::list_instance_network` (trait + SQLite + Memory impls).
- Remove `mc2_runtime::make_route_id`/`normalize_path` re-exports if no external use (keep private if used internally).
- Remove `let _ = key_names;` in `ssh.rs`.
- Run `cargo machete` and remove any unused deps it flags (e.g. re-check `mc2-runtime` dev-deps `tempfile`, `tokio` rt-multi-thread).
- Decide/keep `mc2_api::default_network_fqdn` (used only by tests — keep if considered part of the public fqdn surface, else drop).
Verify: `cargo machete` clean; `cargo test --workspace` green.

### Phase 6 — Workspace lints + docs hygiene (P1)
- `[workspace.lints.rust] unsafe_code = "deny"` propagated via `lints.workspace = true`
  (done in Phase 0).
- Optional, with sign-off: `#![warn(missing_docs)]` on `mc2-api`/`mc2-runtime`/`mc2-store` — **not
  enabled** (large doc-comment sweep; most public items are already documented).
- Create `docs/decisions/ADR-0001-workspace-layout.md` and
  `docs/decisions/ADR-0002-phase-status-representation.md` documenting the workspace shape and the
  string-phase-vs-enum-boundary decision (F9).
Verify: `cargo doc` with `-D warnings` green; docs render.

### Phase 7 — Typed phases MVP (P2, optional, clearly bounded)
Scope: apply existing enums at module boundaries; do **not** change DB/REST string columns.
- Done: `scheduler.rs` / `reschedule.rs` / `node/mod.rs` now parse to and match on
  `InstancePhase` / `NodeStatus` / `SandboxPhase` variants instead of scattered string literals
  (e.g. `InstancePhase::parse(&inst.phase)` in capacity/load maps; report phases built from
  `InstancePhase::X.as_str()`).
- Deferred (documented in ADR-0002): `network_serve.rs`/`networks.rs`/`ingress_files.rs`
  (`"Ready"/"Pending"/"Failed"`) and `ssh_serve.rs`/`ssh.rs` (`"Closed"/"Open"/"Opening"/"Failed"`)
  keep string literals; introducing new enums there adds API surface without a behavior payoff.
Verify: workspace tests green; clippy clean.

### Phase 8 — Verification & release prep (P0 gate)
- Full `just check` (fmt, clippy `--workspace --all-targets --all-features -D warnings`, `cargo test
  --workspace`, `cargo doc`).
- `cargo machete`, `cargo deny check`, `cargo audit` clean (or documented exceptions).
- Update `CHANGELOG.md`; sync `README.md` if commands/docs changed (CI-only + internal refactors → minor).
- PR from `dev` as separate conventional commits per phase (`refactor(cli): …`, `refactor(server): …`,
  `chore(ci): …`), single release PR at the end.

## Implementation log

2026-08-09 — all phases landed on `dev` (7 commits, tree green):

- **Phase 0** (in `chore(ci): tooling baseline…`): rustdoc warnings fixed; `[workspace.lints.rust]
  unsafe_code = "deny"` + per-crate `lints.workspace = true`; unused deps removed (`mc2:tracing`,
  `mc2-runtime:thiserror`, `mc2-tests:tracing`); CI hardened (clippy `--all-features`, doc job with
  `-D warnings`, machete, non-blocking deny/audit); `just` recipes + `deny.toml`.
- **Phase 1** (in the same commit, index was pre-staged): `main.rs` → `cli.rs` / `client.rs` /
  `help.rs` / `cmd/{stacks,observe,access,security}` (+ `cmd/mod.rs`). Pure moves; 22 handler fns made
  `pub(crate)`. `main.rs` is now a ~280-line dispatcher.
- **Phase 2** (same commit): `http.rs` → `api/` with shared `ApiError`/`ApiResult` + `testing` harness;
  typed `ApplyError::{Validation,Allocation,Other}` (replaces `is_stack_client_error`),
  `SecretError::{Validation,Other}`, `StoreError::InvalidArgument`. Router + lib re-export updated.
  All prior http.rs tests preserved/moved.
- **Phase 3** (`refactor(api)`): `stack.rs` → `stack/{mod,schema,decode,validate}.rs`; decode fns
  `pub(crate)`, referenced from schema via `use crate::stack::decode::…`; tests moved to `mod.rs`.
- **Phase 4** (`refactor(server)`): `node.rs` → `node/{mod,state,health}.rs`; `reconcile()` takes
  `&mut NodeRuntimeState` (drops the `too_many_arguments` allow); `run_healthcheck()` extracted.
- **Phase 5** (`chore(cleanup)`): removed dead public API (`network_expose_guest_ports`,
  `Store::list_instance_network`, `make_route_id`), privatized `normalize_path`, removed
  `let _ = key_names;`. `cargo machete` clean.
- **Phase 6** (`docs`): ADR-0001 (workspace layout) + ADR-0002 (phase/status representation) under
  `docs/decisions/`. `missing_docs` lint intentionally not enabled (large doc sweep, low value).
- **Phase 7** (`refactor(server)`): scheduler/reschedule/node use `InstancePhase`/`NodeStatus`/
  `SandboxPhase` enums (parse-to-enum + variant matches, `as_str()` for report phases). Network/ssh
  phase strings deferred per ADR-0002 scope.
- **Phase 8**: full `just check` green (fmt, clippy all-targets+all-features `-D warnings`, 194 tests,
  `cargo doc -D warnings`, machete); CHANGELOG updated; ADR-0001 added under `docs/decisions/`.

Notes / deviations from plan:
- Phases 0–2 landed as a single commit because the git index was pre-staged; the commit message describes
  only Phase 0 but the content is complete and verified. Future commits are per-phase.
- `missing_docs` lint skipped (optional + needs sign-off).
- Phase 7 network/ssh phase centralization deferred (bounded MVP; see ADR-0002).
- `default_network_fqdn` kept (public fqdn surface, symmetric with `network_fqdn`).
- `docs/tasks` plan lives at `docs/tasks/rust-best-practice-cleanup.md`; ADRs under `docs/decisions/`.


## Verification / acceptance criteria

- `cargo fmt --all -- --check` clean.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean — **no new `#[allow]`**.
- `cargo test --workspace` green (150+ tests).
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps` clean.
- `cargo machete` clean; `cargo deny check` / `cargo audit` green or with documented exceptions.
- Public API + CLI surface unchanged (black-box `cli_smoke.rs` + integration tests are the guard).
- No `unsafe` introduced; `unsafe_code = "deny"` enforced.

## Risks / decisions

- **Split risk**: pure moves keep `git blame` locality; each phase lands as its own commit so regressions
  bisect cleanly. Integration + cli_smoke tests are the safety net.
- **Typed errors**: `is_stack_client_error` string table is behavior today; the typed `ApplyError` must
  reproduce the same 400/500 classification (covered by existing `stack_client_errors_map_to_400_keywords`
  test).
- **F9 typing**: deliberately scoped; full enum store would touch migrations/REST and is deferred.
- **`sqlite.rs`**: explicit non-change (F8) to avoid splitting a coherent trait impl.
- **CI additions** (`deny`/`audit`): start advisory checks non-blocking to avoid CI breakage from the
  dependency graph; tighten once the initial report is reviewed.
