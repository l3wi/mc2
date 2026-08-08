# Testing MicroCommandControl

**Use clean, directed unit tests and integration tests to ensure consistency and stop regressions.**

Prefer small, obvious tests that fail if someone breaks the behavior later — over large brittle suites.

---

## Principles

| Kind | Where | Scope | Goal |
| ---- | ----- | ----- | ---- |
| **Unit** | `crates/*/src/**` next to code (`#[cfg(test)]`) | One function/module; mocks or in-memory fakes OK | Fast feedback; pin pure logic (hashing, scheduling, YAML parse, filters) |
| **Integration** | `tests/tests/*.rs` + package `crates/*/tests/*.rs` | Real boundaries: SQLite files, TCP HTTP, CLI process | Catch wiring bugs unit tests miss |
| **Lab / manual** | Operator machine with KVM/HVF | Real microsandbox | Not required in CI; document when needed |

### Directed unit tests

- One behavior per test; name says the rule (`local_node_is_registered`).
- No sleep-based flakiness; no full process when a function call is enough.
- Prefer `MemoryStore` / pure functions for combinatorial cases (scheduler scores, token verify).

### Integration tests

- Use the shared harness (`mc2_tests::TestCluster`) for control-plane HTTP + SQLite.
- Use `crates/mc2/tests/*` for **black-box CLI** (`CARGO_BIN_EXE_mc2`).
- Assert observable contracts (status codes, persisted files, CLI exit codes)—not private fields.
- Keep tests independent: each gets its own temp data dir.

### Consistency

- Public API shapes (`/v1/status`, stack YAML, CLI flags) should have at least one integration or CLI test once implemented.
- Store migrations: open DB → write → drop handle → reopen → read (see `store_persistence`).
- When you fix a bug, add the smallest test that would have failed before the fix.

---

## Layout

```text
crates/
  mc2/src/…                 # unit tests in modules
  mc2/tests/cli_smoke.rs    # CLI process integration
  mc2-server/src/…          # unit (auth, bootstrap helpers)
  mc2-store/src/…           # unit (token, memory, sqlite helpers)

tests/                      # workspace package: mc2-tests
  src/lib.rs                # TestCluster harness (not production code)
  tests/
    server_rest.rs          # HTTP + auth over real TCP
    store_persistence.rs    # SQLite reopen / data dir artifacts
  Cargo.toml
  README.md
```

---

## Commands

```bash
just test              # all workspace unit + integration tests
just test-unit         # crates only (skip mc2-tests package if needed)
just test-integration  # mc2-tests + mc2 CLI integration
just check             # fmt + clippy + full test suite
```

Or with cargo:

```bash
cargo test --workspace
cargo test -p mc2-store
cargo test -p mc2-tests
cargo test -p mc2 --test cli_smoke
```

---

## What CI runs

`.github/workflows/ci.yml`: `fmt` → `clippy -D warnings` → `cargo test --workspace` → `cargo build -p mc2`.

Integration tests must stay **free of KVM/HVF** so Linux CI stays green. Do not boot real microVMs in CI; drive the node through direct store writes and `build_desired_set`. Real microsandbox runs are lab-only.

### Secrets smoke (CI vs lab)

| Path | What it proves |
| ---- | -------------- |
| `tests/tests/secrets.rs` | `mc2 secret set` → stack apply with refs → `build_desired_set` returns `InjectedSecret` (env/value/allowHosts) (same path before SDK create). Missing secret / empty `allowHosts` → desired set errors. |
| Lab | `mc2 secret set SMOKE_TOKEN --value …` then `mc2 apply -f examples/02-secrets/stack.yaml` on a node with hypervisor; guest env shows msb placeholder, value injects only to allowlisted hosts. |

### Ingress smoke (CI vs lab)

| Path | What it proves |
| ---- | -------------- |
| `mc2-api` stack tests | YAML: `ports:` + `ingress:` accept; missing ports / non-loopback bind / empty rules → reject |
| `mc2-runtime` `ingress_render` | Catalog → Traefik strings; guest/host port mapping; pending not in proxy; multi-host; host port change |
| `mc2-server` `ingress` | Per-node plan: guest→host port, multi-path, skip other nodes, no ingress → empty |
| `mc2-server` `ingress_files` | **Ready gate:** Running + TCP accept → files with upstream; dead port / not Running → empty proxy; lifecycle port change, stop, route removed, fingerprint skip |
| `tests/tests/ingress.rs` | Apply → desired set `ingress_routes` (host/guest ports + host) → `GET /v1/ingress`; 400 validation; lifecycle update path/port, remove ingress, multi-path; no routes until scheduled |
| Lab | `mc2 apply -f examples/04-http-ingress/stack.yaml` with `--ingress-config-dir`; Traefik — [examples/04-http-ingress/README.md](../../examples/04-http-ingress/README.md) |

```bash
cargo test -p mc2-tests --test ingress
cargo test -p mc2-server --lib ingress_files
cargo test -p mc2-runtime --lib ingress_render
```

### Volumes smoke (CI vs lab)

| Path | What it proves |
| ---- | -------------- |
| `mc2-api` stack tests | YAML: declared `dir` volumes accept; undeclared mounts, non-`dir` kinds, invalid names, `--` in names, relative/duplicate mounts → reject |
| `mc2-runtime` `naming` + `spec_hash` | `volume_name` namespace/collision rules; `volume_mount_plan` order; mount change forces recreate |
| `tests/tests/volumes.rs` | Apply → instance scheduled (user-facing names persisted) → `build_desired_set` → `volume_mount_plan` resolves `mc2-<stack>--<volume>`; 400 for undeclared volume |
| Lab | Marker file under `/data` survives sandbox recreate; instance stays on its node; volume dir remains after stack removal — [examples/06-persistent-volumes/README.md](../../examples/06-persistent-volumes/README.md) |

```bash
cargo test -p mc2-tests --test volumes
cargo test -p mc2-api --lib stack
```

---

## Where to add tests

| Area | Unit | Integration |
| ---- | ---- | ----------- |
| Local node | store upsert/touch | auto node row Ready; REST list nodes |
| Apply/schedule | YAML parse, spread score | apply → instances scheduled (mock runtime) |
| msb runtime | naming, profile mapping, volume mount plan | unit only in CI; full SDK lab-only |
| Secrets | encrypt/decrypt roundtrip | set → apply → desired-set injection; missing/empty allowHosts fail closed |
| Reschedule | restartPolicy matrix; sticky/never | NotReady → unbind → recovery rebind (`tests/tests/reschedule.rs`); sticky volumes stay |
| Ingress | render, plan, file ready-gate + TCP probe | `tests/tests/ingress.rs` apply/desired-set/REST lifecycle |
| Volumes | validation matrix; naming/mount plan; recreate hash | `tests/tests/volumes.rs` apply/desired-set contract; marker persistence lab-only |

---

## Anti-patterns

- Duplicating the same assertion in unit and integration without a reason (pick one level).
- Integration tests that only call a pure function (make them unit tests).
- Shared global data dirs or fixed ports (races in parallel `cargo test`).
- Snapshots of full error strings that churn every refactor—assert stable substrings or codes.
