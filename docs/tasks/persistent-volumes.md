# Task: Node-local persistent volumes (v1, single-consumer)

Status: **COMPLETE** (implemented + lab-verified 2026-08-07)
Date: 2026-08-07

## Context

MC2 stack YAML already accepts `volumes:` (top-level `VolumeSpec { kind }`, default
`dir`) and per-service `volumes: [{name, mount}]` (`VolumeMount`), but nothing
validates them and the runtime ignores them. The scheduler already treats any
volume mount as node-sticky (`is_volume_sticky`, `may_reschedule_on_node_loss` →
false) and integration tests pin that behavior (`tests/tests/reschedule.rs::sticky_volume_stays_on_not_ready_node`).

Goal: a service can declare a named persistent directory volume that is mounted
into its microsandbox at start and survives sandbox recreate/restart. Data lives
on the node under a configurable directory.

## Constraints (decided with the user)

- **One consumer per volume.** v1 assumes one instance of a service in a stack
  consumes a volume. No cross-service sharing, no replica co-scheduling. The
  scheduler therefore needs **zero changes** — existing sticky behavior is the
  whole affinity story.
- Node-local only. Node down ⇒ volume unavailable (sticky already guarantees the
  instance stays pending on the same node). No migration, no replication.
- Directory-backed only (`kind: dir`). Disk images, quotas, snapshots, remote
  storage: deferred. `VolumeSpec.kind` stays for forward compatibility.
- Volumes are retained when services/stacks are removed. Deletion is deferred.

## SDK mapping (verified against microsandbox 0.6.8 source)

| MC2 need | SDK API | Notes |
| --- | --- | --- |
| Named dir volume, create-or-reuse | `Sandbox::builder(..).volume(mount_path, \|m\| m.named_with(msb_name, \|v\| v.ensure_exists().directory()))` | `lib/sandbox/types.rs:146-180,238` |
| Retain on sandbox delete | inherent | `create.rs:275-375`: only volumes created *in this spawn* roll back on boot failure; pre-existing volumes untouched |
| Incompatible reuse fails loud | `validate_existing_named_volume` in `ensure_named_volumes` (`spawn.rs:1058+`) | e.g. name exists as disk kind → create error |
| Volume location | `LocalConfig::volumes_dir()` = `~/.microsandbox/volumes/<name>` by default | `config/mod.rs:331-336` |
| Override location | `LocalBackend::builder().volumes_dir(path).build()` | `backend/local/mod.rs:224,336`; merges over persisted config |
| Sizing (future) | `.quota(MiB)` on named dir volumes; `.size()` is disk-only | `types.rs:172-181,428` — do not expose in v1 |

Seam in MC2: `mc2-runtime::msb_sdk::ensure_local_backend()` currently installs
`LocalBackend::new()` process-wide. Switch to builder form with the optional
`--volume-dir` override so all sandboxes in the agent process use the same root.

## Changes

### 1. Validation — `crates/mc2-api/src/stack.rs::validate_stack`

Add (inside the existing per-service loop + one pass over `doc.volumes`):

- Every `service.volumes[].name` must exist in top-level `volumes`.
- Every declared `VolumeSpec.kind` must be `dir` (case-insensitive).
- Volume names: non-empty, `[a-z0-9][a-z0-9._-]*`, and must not contain `--`.
- Stack `metadata.name` must not contain `--` (namespace separator safety).
- Mount paths: absolute (`/`-prefixed), unique per service (SDK also rejects
  duplicate guest paths: "multiple volumes cannot mount …" — validate earlier
  with a clear message).

Rationale for `--`: the resolved msb name is `mc2-{stack}--{volume}`. A bare `-`
separator collides (`a-b`+`x` vs `a`+`b-x`); `--` plus the validation rule makes
the split unambiguous. The msb local DB has `volume.name UNIQUE`, so collisions
would otherwise hard-fail at create time.

### 2. Naming + mount plan — `crates/mc2-runtime/src/naming.rs`

```rust
/// Resolved msb named-volume identity: `mc2-{stack}--{volume}` (sanitized, ≤128B).
pub fn volume_name(stack: &str, volume: &str) -> String;

/// Guest mount path → resolved msb volume name, in spec order.
pub fn volume_mount_plan(desired: &DesiredSandbox) -> Vec<(String, String)>;
```

Reuse the existing `sanitize()` + 128-byte rule from `sandbox_name`. Pure
functions ⇒ unit-testable without the SDK.

### 3. Runtime mount — `crates/mc2-runtime/src/msb_sdk.rs::create_detached`

After labels/ports, before network:

```rust
for (guest, msb_name) in volume_mount_plan(desired) {
    b = b.volume(guest, move |m| {
        m.named_with(msb_name, |v| v.ensure_exists().directory())
    });
}
```

Recreate/restart already re-derive everything from the desired spec
(`spec_hash.rs:71-73` hashes volume mounts, so mount changes trigger recreate);
no extra "preserve mounts" logic needed.

Backend override:

```rust
// ensure_local_backend(volume_dir: Option<&Path>)
let local = match volume_dir {
    Some(p) => LocalBackend::builder().volumes_dir(p).build().await?,
    None => LocalBackend::new().await?,
};
```

`MicrosandboxRuntime::new(volume_dir: Option<PathBuf>)` stores it; callers
(`mc2-runtime::default_runtime` → agent) pass it through.

### 4. Agent flag — `crates/mc2-agent/src/lib.rs::AgentArgs`

```rust
/// Named-volume root (durable node storage; default ~/.microsandbox/volumes)
#[arg(long, env = "MC2_VOLUME_DIR")]
pub volume_dir: Option<PathBuf>,
```

Pre-release: no migration or orphaning concerns — the flag simply chooses the
root; nothing needs to warn about or preserve prior locations.

### 5. Scheduler / apply / store — **no changes**

Existing sticky semantics cover v1:

- first bind is free choice (spread scheduler);
- volume present ⇒ never unbound on node loss;
- pending instances with volumes just wait for their node.

Resolved volume identity is **not** persisted in the instance spec — the agent
derives it from `(stack, volume)` at create time. Avoids a schema/proto change
and keeps user YAML the source of truth.

### 6. Example + docs — `examples/06-persistent-volumes/`

- `stack.yaml`: one service, `alpine:3.20`, `sleep infinity`, one volume:

  ```yaml
  volumes:
    data:
      kind: dir
  services:
    keep:
      ...
      volumes:
        - name: data
          mount: /data
  ```

- `README.md`: top-level declarations; mount config; node-local + retain
  semantics; shared-mount/concurrency disclaimer (no locking); backup
  expectation; `--volume-dir`; lab runbook.
- `examples/README.md`: add 06 to "Ready-to-use".
- `examples/90-advanced/README.md`: flip persistent-volumes row out of
  UNDER DEVELOPMENT; leave deletion/migration/snapshots/quotas/remote as
  advanced TODOs. Remove `examples/90-advanced/persistent-volumes/` stub
  (superseded).
- `CHANGELOG.md`: feature entry.

## Testing harness

Follow `docs/guides/testing.md`: directed units next to code, integration via
`mc2_tests::TestCluster` without a hypervisor, real microVMs lab-only.

### A. Unit (CI) — validation matrix

`crates/mc2-api/src/stack.rs` tests, one behavior per name:

| Test | Fails when |
| --- | --- |
| `volume_mount_without_declaration_rejected` | mount name not in `volumes:` slips through |
| `volume_kind_other_than_dir_rejected` | `kind: disk` accepted |
| `volume_name_empty_or_invalid_rejected` | `""`, `Data!`, leading `-` accepted |
| `volume_name_double_dash_rejected` | `--` in volume or stack name accepted |
| `relative_mount_path_rejected` | `data/` accepted |
| `duplicate_mount_path_per_service_rejected` | two mounts at `/data` accepted |
| `valid_volume_stack_accepted` | golden path regresses |

### B. Unit (CI) — naming + mount plan

`crates/mc2-runtime/src/naming.rs` tests:

- `volume_name_is_namespaced_and_deterministic`:
  `volume_name("demo", "data") == "mc2-demo--data"`.
- `volume_name_separator_is_collision_free`: `("a-b","x")` vs `("a","b-x")`
  differ (the exact bug `--` prevents).
- `volume_name_sanitized_and_bounded`: charset + 128-byte rule.
- `volume_mount_plan_maps_spec_order`: `DesiredSandbox` with two mounts →
  pairs in order; empty volumes → empty vec.

`spec_hash.rs`: add `volume_mount_change_forces_recreate` (hash differs when a
mount is added/changed) — pins "no silent mount drift across recreate".

### C. Integration (CI, no hypervisor) — `tests/tests/volumes.rs`

Same shape as `tests/tests/secrets.rs`: real `TestCluster` (SQLite + REST +
gRPC) + fake agent over `AgentServiceClient`.

1. `apply_persists_volume_mounts_and_schedules_sticky`
   - POST `/v1/stacks:apply` with a volume stack; assert 200 and instance
     Scheduled on the joined node; `spec_json` in the store round-trips the
     mounts unchanged (user-facing names).
2. `sync_delivers_volume_material_to_agent`
   - Agent `Sync` → `desired_from_sync` → `volume_mount_plan` returns
     `[("/data", "mc2-<stack>--data")]`. This is the exact pre-`create_detached`
     path; proves the runtime contract without booting a VM.
3. `apply_rejects_undeclared_volume`
   - 400 from `/v1/stacks:apply`, error mentions the volume name (assert
     substring, not full string — testing.md anti-pattern rule).
4. Existing `tests/tests/reschedule.rs::sticky_volume_stays_on_not_ready_node`
   stays green — it is the node-loss acceptance test. No duplicate.

### D. Lab runbook (manual, hypervisor required)

Not CI. `examples/06-persistent-volumes/README.md` step list (mirrors
secrets/ingress lab pattern in testing.md):

1. `mc2 agent --volume-dir /tmp/mc2-lab-volumes` (throwaway dir) + server.
2. `mc2 apply -f examples/06-persistent-volumes/stack.yaml`.
3. Write marker: `mc2` exec / msb shell → `touch /data/marker`.
4. `mc2` scale-down/delete + re-apply (or restart) to recreate the sandbox.
5. Marker still present; instance still bound to the same node
   (`GET /v1/instances`).
6. Remove the stack; `/tmp/mc2-lab-volumes/mc2-<stack>--data/` still on disk.

Optional: encode as `examples/06-persistent-volumes/smoke.sh` (guarded,
`set -euo pipefail`) so the runbook is runnable, matching the deterministic-
command preference in the repo shell guidelines.

### Out of scope (explicitly)

- Affinity-map scheduler, cross-service sharing, replica co-location tests —
  cut per user decision; revisit when multi-consumer volumes are on the table.
- Persisting resolved identity in instance specs — replaced by derivation.
- Quota/size surfacing — deferred until MC2 can report usage.

## Acceptance criteria

- [ ] All validation errors return 400 from `/v1/stacks:apply` with the stack/
      volume named; valid stacks apply.
- [ ] Sandbox created with `named_with(...ensure_exists().directory())`; mount
      change triggers recreate (spec_hash test).
- [ ] Marker file survives sandbox recreate + agent restart (lab).
- [ ] Instance stays on its node while the node is NotReady (existing test).
- [ ] Volume directory remains on disk after stack removal (lab).
- [ ] `--volume-dir` flag honored; default unchanged (`~/.microsandbox/volumes`).
- [ ] `just check` green; docs (examples README, advanced README, CHANGELOG)
      synced.

## Risks

| Risk | Mitigation |
| --- | --- |
| `--volume-dir` pointed elsewhere mid-run | none needed pre-release; volumes are keyed by directory — operator picks one location up front |
| Name collision across stacks | `--` separator + validation forbidding `--` in names; unit test pins it |
| Concurrent mounts (future multi-consumer) silently corrupt | v1 scope forbids it by construction (one instance); README disclaimer; SDK has no lock (`volume/fs.rs` verified) |
| msb SDK version drift (builder API) | pinned 0.6.8 in both Cargo.tomls; tests compile against it |

## Task breakdown

1. Validation in `mc2-api` (+ unit tests A).
2. `volume_name` / `volume_mount_plan` in `mc2-runtime` (+ unit tests B).
3. `create_detached` mount wiring + backend `volumes_dir` override.
4. Agent `--volume-dir` flag + plumbing via `MicrosandboxRuntime::new` (`default_runtime` removed).
5. Integration tests C (`tests/tests/volumes.rs`).
6. Example 06 + docs + CHANGELOG + advanced README cleanup.
7. Lab runbook pass on a hypervisor host; record results in this file.

## Handover notes (append as completed)

- **Validation** (`mc2-api/src/stack.rs`): `validate_volumes` + `valid_volume_name`; stack names may not contain `--`. 7 new unit tests. Also collapsed a clippy `collapsible_if` in `validate_ingress`.
- **Runtime**: `volume_name`/`volume_mount_plan` in `naming.rs`; `create_detached` emits `named_with(...ensure_exists().directory())` mounts; `ensure_local_backend` takes the override; **`resolve_volume_dir` canonicalizes the path** (create_dir_all + canonicalize) — found in lab: msb mounts refuse to follow symlinks, macOS `/tmp` → `/private/tmp` failed with ENOTDIR until canonicalized.
- **Agent**: `--volume-dir` (`MC2_VOLUME_DIR`); `default_runtime` deleted, agent constructs `MicrosandboxRuntime::new(volume_dir)` directly.
- **Tests**: `tests/tests/volumes.rs` (3 tests, CI-safe). Updated `tests/tests/reschedule.rs` sticky test YAML to declare the volume (new validation contract). Pre-existing fmt drift (mc2-store imports) and clippy lints (`cloned_ref_to_slice_refs` in ingress_render/ingress test) fixed to keep `just check` green.
- **Docs**: `examples/06-persistent-volumes/` (stack.yaml + README with corrected runbook — v1 has no stack-delete command, so retention is shown at sandbox level), examples README, 90-advanced README row flipped, `docs/guides/testing.md` volumes section, `CHANGELOG.md` created.
- **Lab results** (macOS Apple Silicon, HVF): server `--grpc-plain --no-auth` on 17443/17444 + agent `--volume-dir /tmp/mc2-lab-volumes`. Volume materialized at `/private/tmp/mc2-lab-volumes/mc2-smoke-volumes--data/`; marker written via `msb_shell` → sandbox removed via `msb_rm` → marker present on disk → agent recreated sandbox on the **same node** (556db05a…) → `cat /data/marker` inside the new guest returned the marker. `just check`: fmt + clippy + all 110+ tests green.
