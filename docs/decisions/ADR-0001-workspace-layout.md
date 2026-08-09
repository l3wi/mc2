# ADR-0001: Workspace crate layout

- Status: accepted
- Date: 2026-08-09
- Deciders: maintainers
- Context: [rust-best-practice-cleanup](../tasks/rust-best-practice-cleanup.md)

## Decision

MC2 is a 7-crate workspace with one binary and six libraries:

```
crates/mc2-api/      shared REST DTOs + stack YAML schema (parse/validate)
crates/mc2-store/    Store trait + SQLite (prod) + Memory (tests) backends
crates/mc2-runtime/  NodeRuntime trait + microsandbox SDK backend
crates/mc2-server/   orchestrator: REST (api/), scheduler, node loop, ingress, ssh, network
crates/mc2-metrics/  optional OTLP metrics
crates/mc2/          operator CLI + server binary (main.rs + cli/client/help/cmd)
tests/               integration harness (mc2-tests)
```

Shared dependency versions and package metadata live in the root
`[workspace.dependencies]` / `[workspace.package]`.

## Consequences

- Crates are split on domain/capability (auth, scheduler, ingress, …), not on
  Rust construct type; there is no `models.rs`/`utils.rs` junk drawer.
- The `mc2` binary is a thin dispatcher; all logic lives behind `cmd/`
  handlers grouped along the CLI help surfaces (stacks/observe/access/security).
- The REST API is a single `api/` module tree with a shared `ApiError` type.
- Cross-crate seams are the two core traits (`Store`, `NodeRuntime`) plus the
  `mc2-api` types — few architectural commitments, easy to swap a backend.
- Integration tests live in a dedicated `tests/` package; unit tests stay next
  to production code.
- See ADR-0002 for the phase/status representation decision.
