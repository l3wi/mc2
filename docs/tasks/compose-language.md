# Task: Compose-language CLI (apply → up; add down / rm / config)

Status: **IMPLEMENTED — 2026-08-08**
Date: 2026-08-08

## Context

MC2 is explicitly single-node ("local/remote client modes" made the framing
clear). The CLI was imprinted on kubectl (`apply`), which drags in expectations
the project doesn't have (multi-node scheduling, rollouts, describe). The
operator wants a **hard cut to Docker Compose language**: `up` / `down` / `rm`
/ `config`, with `mc2 ps` already matching. This also closes the missing
teardown step of the VM lifecycle (there is no stack delete today).

## Design

### Command surface (hard cut, no aliases)

```text
mc2 up -f stack.yaml        # was apply: publish desired state, converge (idempotent)
mc2 down <stack>            # NEW: tear down the stack's VMs (instances + definition)
mc2 rm <stack> [--volumes]  # NEW: down + optionally delete the stack's named volumes
mc2 config -f stack.yaml    # NEW: validate + print normalized YAML (compose config)
mc2 ps                      # unchanged
```

### Teardown semantics (down / rm)

- `down <stack>` → `DELETE /v1/stacks/{name}`. Server deletes the stack row and
  its instance rows; the node loop's scale-down GC (`owned ∖ desired`) calls
  `ensure_removed` on the sandboxes next reconcile. Named **volumes are
  retained** (Compose `down` keeps volumes).
- `rm <stack> --volumes` → `DELETE /v1/stacks/{name}?volumes=true`. Additionally
  removes `<volume-root>/<volume_name(stack, vol)>` for every volume declared in
  the stack YAML (`volume_name` = `mc2-{stack}--{volume}`). Volume root = server
  `--volume-dir`, else the msb default `~/.microsandbox/volumes`. Best-effort
  removal (warns, doesn't fail the stack teardown).
- Store: new `delete_stack(name) -> bool` — deletes `instances` (cascades to
  `instance_ssh`/`instance_fabric` via FK) then `stacks` (cascades `services`).
- `AppState` gains `volume_dir: Option<PathBuf>` (from `ServerArgs`) so the
  handler can resolve volume paths.

### `mc2 config`

Client-side only: read the file, `fill_stack_defaults` (existing), validate via
`mc2_api::parse_stack_yaml`, print the normalized YAML. Errors surface the
validation message.

### Internal REST stays `apply`

`POST /v1/stacks:apply` / `apply_stack_yaml` keep their names — the HTTP
surface is internal to the CLI and already covered by server tests. Only the
user-facing CLI language changes.

## Changes by area

| Area | Change | Size |
| --- | --- | --- |
| `mc2-store` | `delete_stack` (trait + sqlite + memory); instance deletion order | ~40 lines |
| `mc2-server` | `AppState.volume_dir`; `DELETE /v1/stacks/{name}` handler (+`volumes` query); volume dir resolution | ~70 lines |
| `mc2` CLI | rename `apply`→`up` (struct/command/help); `down`/`rm` commands + delete client call; `config` command | ~120 lines |
| Tests | delete_stack unit (sqlite/memory), teardown REST test, `config` render/validate units, `up`/`down`/`rm`/`config` smoke + help | ~150 lines |
| Docs | README, quickstart, examples, CHANGELOG, task handover | small |

## Out of scope (deliberate)

- `logs` / `exec` streaming (needs runtime log plumbing — separate feature).
- Renaming the internal REST endpoint or server module names.
- `down` on a "project" (MC2 is per-stack, not per-project).
- `pull`/`build` (images are msb-pulled on the host).

## Risks

- **Hard cut breaks existing muscle memory / scripts** — intentional per the
  operator (pre-release, no aliases).
- **`rm --volumes` vs. a running sandbox**: the volume dir is removed while a
  sandbox may still have it mounted; best-effort with a warning, GC removes the
  sandbox on the next reconcile.

## Acceptance criteria

- [ ] `mc2 apply` is gone; `mc2 up -f stack.yaml` applies (fill defaults,
      POST, converge) with the same output.
- [ ] `mc2 down <stack>` tears down instances + definition; volumes retained.
- [ ] `mc2 rm <stack> --volumes` also removes the stack's named volume dirs.
- [ ] `mc2 config -f stack.yaml` validates + prints normalized YAML.
- [ ] Docs/examples use the new verbs; `just check` green.

## Handover notes (append as completed)

- (pending)

## Implementation notes (2026-08-08)

- **`mc2 up`** — `Commands::Apply`/`ApplyArgs`/`apply_cmd` renamed to
  `Up`/`UpArgs`/`up_cmd`; help text "Bring up a stack: publish desired state
  and converge (idempotent)"; error verb now "up". Still POSTs to the internal
  `/v1/stacks:apply` endpoint (kept; covered by server tests).
- **`mc2 down <stack>` / `mc2 rm <stack> [--volumes]`** — new commands DELETE
  `/v1/stacks/{name}` (+`?volumes=true`). Server handler snapshots the stack
  YAML, `delete_stack` (instances first → cascades `instance_ssh`/`instance_fabric`;
  `services` cascade via the stack FK), then best-effort removes
  `<volume-root>/mc2-{stack}--{volume}` for each declared volume when
  `volumes=true`. Volume root = `--volume-dir` else msb default
  `~/.microsandbox/volumes`; `AppState` gained `volume_dir`.
- **`mc2 config -f stack.yaml`** — client-side only: `fill_stack_defaults` →
  `parse_stack_yaml` (validation) → prints the filled YAML.
- **Store** — `delete_stack` added to the trait + sqlite + memory.
- **Tests** — sqlite `delete_stack` cascade unit; REST teardown + 404 +
  `?volumes=true` dir-removal tests; smoke: help lists `up/down/rm/config`,
  `up_without_token…`, `compose_verbs_validate_locally` (config against a real
  example; down/rm fail cleanly on a dead server).
- **Docs** — README command table, quickstart, stack-yaml/testing guides, all
  example READMEs + stack comments, CHANGELOG. Internal REST name left as
  `apply` deliberately.
- **Verified live**: `config` renders normalized YAML; `up` → `ps` shows the
  instance; `down` empties instances; `rm --volumes` removes the stack;
  `rm` on a missing stack returns a clean 404 error. `just check` green.
