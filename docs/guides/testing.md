# Testing MicroCommandControl

**Use clean, directed unit tests and integration tests to ensure consistency and stop regressions.**

Every phase should leave behind tests that fail if someone breaks the behavior later. Prefer small, obvious tests over large brittle suites.

---

## Principles

| Kind | Where | Scope | Goal |
| ---- | ----- | ----- | ---- |
| **Unit** | `crates/*/src/**` next to code (`#[cfg(test)]`) | One function/module; mocks or in-memory fakes OK | Fast feedback; pin pure logic (hashing, scheduling, YAML parse, filters) |
| **Integration** | `tests/tests/*.rs` + package `crates/*/tests/*.rs` | Real boundaries: SQLite files, TCP HTTP, CLI process | Catch wiring bugs unit tests miss |
| **Lab / manual** | Operator machine with KVM/HVF | Real microsandbox | Not required in CI; document when needed |

### Directed unit tests

- One behavior per test; name says the rule (`join_token_is_not_accepted_as_api_bearer`).
- No sleep-based flakiness; no full process when a function call is enough.
- Prefer `MemoryStore` / pure functions for combinatorial cases (scheduler scores, token verify).

### Integration tests

- Use the shared harness (`mcc_tests::TestCluster`) for control-plane HTTP + SQLite.
- Use `crates/mcc/tests/*` for **black-box CLI** (`CARGO_BIN_EXE_mcc`).
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
  mcc/src/…                 # unit tests in modules
  mcc/tests/cli_smoke.rs    # CLI process integration
  mcc-server/src/…          # unit (auth, bootstrap helpers)
  mcc-store/src/…           # unit (token, memory, sqlite helpers)

tests/                      # workspace package: mcc-tests
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
just test-unit         # crates only (skip mcc-tests package if needed)
just test-integration  # mcc-tests + mcc CLI integration
just check             # fmt + clippy + full test suite
```

Or with cargo:

```bash
cargo test --workspace
cargo test -p mcc-store
cargo test -p mcc-tests
cargo test -p mcc --test cli_smoke
```

---

## What CI runs

`.github/workflows/ci.yml`: `fmt` → `clippy -D warnings` → `cargo test --workspace` → `cargo build -p mcc`.

Integration tests must stay **free of KVM/HVF** so Linux CI stays green. Do not boot real microVMs in CI; exercise control-plane + gRPC with reported status. Real microsandbox runs are lab-only.

### Secrets smoke (CI vs lab)

| Path | What it proves |
| ---- | -------------- |
| `tests/tests/secrets.rs` | `mcc secret set` → stack apply with refs → agent **Sync** returns `SecretInjection` (env/value/allowHosts) mapped via `desired_from_sync` (same path before SDK create). Missing secret / empty `allowHosts` → Sync `FailedPrecondition`. |
| Lab | `mcc secret set SMOKE_TOKEN --value …` then `mcc apply -f examples/stacks/smoke-secrets.yaml` on a node with hypervisor; guest env shows msb placeholder, value injects only to allowlisted hosts. |

---

## Adding tests with each phase

| Phase | Prefer unit | Prefer integration |
| ----- | ----------- | ------------------ |
| 2 Agent join | token/node field validation | join → node Ready in DB; REST list nodes |
| 3 Apply/schedule | YAML parse, spread score | apply → instances scheduled (mock runtime) |
| 4 msb runtime | naming, profile mapping | unit only in CI; full SDK lab-only |
| 5 Secrets | encrypt/decrypt roundtrip | set → apply → agent Sync injection; missing/empty allowHosts fail closed |
| 6 Reschedule | policy pure functions | multi-node harness with mock agents |

---

## Anti-patterns

- Duplicating the same assertion in unit and integration without a reason (pick one level).
- Integration tests that only call a pure function (make them unit tests).
- Shared global data dirs or fixed ports (races in parallel `cargo test`).
- Snapshots of full error strings that churn every refactor—assert stable substrings or codes.
