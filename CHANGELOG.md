# Changelog

All notable changes to MC2. Pre-release: entries are grouped per feature area.

## Unreleased

### Stack YAML defaults (DX)

- `mc2 apply` fills missing boilerplate before sending: `apiVersion`
  (`mc2/v1`), `kind` (`Stack`), and `metadata.name` (sanitized file stem).
  Partial files — even `services:` + `image:` only — now apply. Present values
  are never rewritten; complete files pass through verbatim (comments kept).
  Documented in `examples/README.md`.

### Persistent volumes (v1)

- Stack YAML `volumes:` are now validated: mounts must reference declared
  volumes, `kind: dir` is the only supported kind, volume names must match
  `[a-z0-9][a-z0-9._-]*` (no `--`), mount paths must be absolute and unique
  per service, and stack names must not contain `--`.
- Declared volumes mount into sandboxes as microsandbox named directory
  volumes (`ensure_exists().directory()`), resolved to
  `mc2-<stack>--<volume>` at create time. Data persists across sandbox
  recreate/restart and is retained on service/stack removal.
- New agent flag `--volume-dir` (`MC2_VOLUME_DIR`) overrides the named-volume
  root (default `~/.microsandbox/volumes`).
- New example `examples/06-persistent-volumes/` with README and lab runbook.
- Integration tests: `tests/tests/volumes.rs` (apply → Sync → mount plan,
  validation 400s). Scheduler unchanged: existing sticky-volume semantics cover
  node-local placement; instances stay Pending while their node is NotReady.
