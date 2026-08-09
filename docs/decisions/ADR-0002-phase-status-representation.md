# ADR-0002: Phase/status representation

- Status: accepted
- Date: 2026-08-09
- Deciders: maintainers
- Context: [rust-best-practice-cleanup](../tasks/rust-best-practice-cleanup.md)

## Decision

Instance `phase`, node `status`, and SSH/network state are **persisted and
serialized as strings** (`"Running"`, `"Ready"`, `"Open"`, …) in SQLite and
over the REST API. Typed enums (`SandboxPhase`, `InstancePhase`, `NodeStatus`)
are used **at module boundaries** where comparisons and state machines live
(node reconcile, scheduler, network summary, ssh serve).

Enums are not yet applied to every store column or REST DTO because that would
touch migrations and wire formats for cosmetic value at this stage.

## Consequences

- SQLite/REST stay string-based, so the schema and API surface are stable and
  hand-editable for debugging.
- Code that reasons about lifecycle uses enums + `as_str()` / `parse()` rather
  than scattered string literals; a future task can widen enum coverage to the
  store without breaking the wire format.
- Phase literals must stay in sync between the enum `as_str()` impls and any
  remaining string comparisons (enforced by tests, not the type system).
