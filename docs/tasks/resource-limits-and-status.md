# Resource limits (low defaults) + host/consumption in status

Status: **proposed** (awaiting review)
Branch: `dev`

## Context

MC2 currently has **no budget or refusal**: the scheduler only soft-places instances on nodes with
residual capacity (otherwise they sit `Pending`), node capacity defaults to host values
(`crates/mc2-server/src/lib.rs:66-73`), and **disk is not tracked at all**. There is nothing that
refuses creation when reserved usage exceeds a configurable ceiling, and `/v1/status` reports only
counts.

This task adds:
1. **Configurable resource limits with conservative low defaults** — a cluster budget for CPU/RAM/disk
   that **refuses** an apply once reserved usage would exceed it.
2. **Resource visibility in status** — host CPU/RAM/disk + MC2's reserved (CPU/RAM) and used (disk).

Single-process, single-node in v1, so the limits are effectively **cluster-wide**.

## Design

### 1. Limits (config)

New `ServerArgs` flags + env vars (existing clap `env=` pattern). **Defaults are `0` = unlimited** —
limits are opt-in; when unset nothing is refused (and `mc2 status` shows `unlimited`):

| Flag | Env | Default | Meaning |
|---|---|---|---|
| `--limit-cpus` | `MC2_LIMIT_CPUS` | `0` (unlimited) | Max reserved CPUs |
| `--limit-memory-mib` | `MC2_LIMIT_MEMORY_MIB` | `0` (unlimited) | Max reserved memory MiB |
| `--limit-disk-mib` | `MC2_LIMIT_DISK_MIB` | `0` (unlimited) | Max MC2-used disk MiB |

Carried in a small `ResourceLimits` struct in `mc2-server` (private to the server crate).

**Reserved** CPU/RAM = sum over non-`Failed`/`Stopped` instances of `spec.cpus.ceil()` and
`spec.mem_limit_mib` (same accounting as `scheduler::residual_capacity`, reused via a small
`reserved_capacity()` helper). **MC2 disk used** = recursive byte-size of `volume_dir` + `data_dir`
(volumes have no size in the spec, so this is measured, not reserved).

### 2. Refusal at apply

In `apply_stack` (after per-replica specs are resolved): compute the stack's reserved CPU/RAM, add to the
cluster's current reserved, and if `> limit` return a new
`ApplyError::Capacity(String)` → HTTP **400** with a message like:

```
refusing apply: stack would reserve 3 CPU (limit 2; currently 1 reserved) — raise --limit-cpus or scale down
```

Disk: refuse when `mc2_disk_used >= limit` (can't predict volume growth; documented limitation). All
refusal checks are skipped when the corresponding limit is `0`.

Signature change: `apply_stack_yaml(store, ctx, yaml)` where `ctx` carries the limits + data/volume dirs
(single domain struct, uniformly testable). Existing `ApplyError` gains a `Capacity` variant.

### 3. Status

Extend `mc2_api::ClusterStatus` with an optional `resources` object (serde `default` +
`skip_serializing_if = "Option::is_none"` → backward compatible):

```json
"resources": {
  "limits": { "cpus": 2, "memoryMiB": 4096, "diskMiB": 10240 },
  "host":   { "cpus": 8, "memoryMiB": 16384, "diskTotalMiB": 500000, "diskFreeMiB": 300000 },
  "reserved": { "cpus": 1, "memoryMiB": 512 },
  "mc2DiskUsedMiB": 2048
}
```

New module `crates/mc2-server/src/host_metrics.rs` with thin, `cfg`-gated host readers:

- **CPU**: `num_cpus::get()` (already a dependency).
- **RAM**: Linux `/proc/meminfo`; macOS `sysctl hw.memsize` (std + `cfg`, no new dep).
- **Disk (host)**: `statvfs`/`statfs` on the volume/data dir — adds `libc` (already transitive).
- **Disk (MC2 used)**: recursive dir size of `volume_dir` + `data_dir` (std-only walk).

`/v1/status` handler + `mc2 status` CLI (`cmd/observe.rs`) print a `Resources` section
(host cpu/ram/disk; limits; reserved cpu/ram; mc2 disk used). JSON output includes the object.

## Non-goals

- Per-service/per-stack quotas; live enforcement on the reconcile loop (apply-time only).
- Multi-node budget allocation (single cluster-wide budget for now).
- Predicting volume growth for disk refusal.
- Changing node-capacity semantics (`--cpus`/`--memory-mib` stay as host truth).

## Files touched

- `crates/mc2-server/src/lib.rs` — `ResourceLimits` + new flags/env + defaults; thread into `run()`.
- `crates/mc2-server/src/apply.rs` — `reserved_capacity()` helper; `ApplyError::Capacity`; refusal checks.
- `crates/mc2-server/src/api/meta.rs` — `/v1/status` computes + returns `resources`.
- `crates/mc2-server/src/host_metrics.rs` — **new**: host CPU/RAM/disk + dir-size readers.
- `crates/mc2-api/src/lib.rs` — `ClusterStatus.resources` DTO.
- `crates/mc2/src/cmd/observe.rs` — `mc2 status` prints resources.
- Tests: apply-refusal cases (over CPU/mem limit, disk used over), status `resources` shape, dir-size unit.

## Verification

- `just check` (fmt, clippy `-D warnings`, tests, doc, machete) + `cargo deny`/`audit` green.
- New tests: apply refused over limit returns 400 + clear message; `mc2 up` then `mc2 status` shows the
  budget/host/consumption lines; default limits apply when flags absent; `0` = unlimited.
- Backward compat: existing stacks/tests unaffected at default limits (smoke stack = 1 CPU / 512 MiB).

## Implementation log

2026-08-09 — implemented on `dev` (uncommitted):

- **Limits**: `ServerArgs` gains `--limit-cpus` / `--limit-memory-mib` / `--limit-disk-mib`
  (env `MC2_LIMIT_*`), **default `0` = unlimited** (per user: no default limits). `ResourceLimits`
  struct carried in `AppState`.
- **Refusal**: `ApplyConfig { limits, data_dir, volume_dir }` threads the budget into
  `apply_stack_yaml`/`apply_stack`; `check_capacity` refuses when a stack's reserved CPU/RAM would
  exceed the budget or when measured disk usage is already at the disk limit → new
  `ApplyError::Capacity` → HTTP 400. `scheduler::reserved_capacity` sums bound instance specs
  (mirrors `residual_capacity` accounting).
- **Status**: `/v1/status` now returns a `resources` object (limits / host / reserved /
  `mc2DiskUsedMiB`); `mc2 status` prints a `resources:` block. New `host_metrics.rs` reads host
  CPU (`num_cpus`), RAM (Linux `/proc/meminfo`, macOS `sysctl`), disk (`nix` `statvfs`, no unsafe),
  and recursive dir size (std).
- **Tests**: apply refusal (cpu/mem/disk), applies-within-limits, status `resources` shape,
  `dir_size_bytes`, `disk_usage_mib`. E2E verified: `mc2 up` over `--limit-cpus 1` → 400 with a
  clear message; `mc2 status` shows host + consumption.
- Side effect: clippy flagged `Commands::Server` as a large enum variant → `ServerArgs` is now boxed.
- Also reverted an accidental `0.1.0 → 0.1.1` version bump that had leaked into the pushed
  "docs: ADR-0003" commit (a `git add -A` swept up an unreverted release-plz run); Cargo.toml +
  Cargo.lock + CHANGELOG restored to `0.1.0` / clean Unreleased.

