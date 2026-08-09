# CLI surface cleanup (grouped help, global connection flags, flat ssh)

## Context

Review of MC2's CLI vs the msb CLI and Docker/docker-compose showed MC2's top-level
command count (19) is not the problem — the *presentation* was: ungrouped help,
connection flags repeated on every command, 3-deep `ssh key` nesting, and a
`down`/`rm` near-duplicate.

## Goals

1. **Group the help output** — clap `next_help_heading` + enum reorder into
   `Stacks` / `Observe` / `Access` / `Security` / `Admin`.
2. **Promote connection args to global flags** — `--api` / `--token` /
   `--context` / `--allow-insecure-http` move from a flattened `OperatorArgs`
   (on ~15 commands) to global args on the top-level `Cli`. Handlers receive a
   resolved `&Conn` instead of mutating `args.op`.
3. **Flatten `mc2 ssh key <sub>` → `mc2 ssh add-key|show-key|keys|rm-key`**.
4. **Fold `rm` into `down`** — `mc2 down [--volumes]`, `rm` kept as a hidden
   alias (`down` is docker-compose parity).
5. (deferred) split `main.rs` into `commands/` modules.
6. **Keep env-var config** — `MC2_API` / `MC2_API_KEY` / `MC2_CONTEXT` retained
   on the global flags (item 2 keeps them).

## Design notes

- `context set <name> --api <url> [--token <key>]` keeps working unchanged: the
  local `--api`/`--token` args on `SetContextArgs` are removed and the **global**
  flags become the source of the URL/key. clap forbids a global arg colliding
  with a same-named subcommand arg, so the local ones had to go.
- `status_cmd` already received `&Conn`; all other operator handlers get a
  `conn: &Conn` parameter and use `conn.url` / `conn.token` (replacing
  `args.op.api` / `args.op.token`).
- `resolve_op` drops its `&mut OperatorArgs` mutation; it now returns a `Conn`
  directly from the four global fields (flags > env > context > default).

## Files touched

- `crates/mc2/src/main.rs` — CLI definition + all handlers (the bulk).
- `crates/mc2/tests/cli_smoke.rs` — help assertions, `ssh key` help test.
- `README.md`, `docs/guides/stack-yaml.md`, `examples/05-ssh-ingress/README.md`
  — `ssh key add/ls` → `ssh add-key/keys`; `mc2 rm` → `mc2 down`.

## Verification

- `just check` (fmt + clippy + full test suite).
- `cargo run -p mc2 -- --help` shows grouped sections; `--help` for `up`/
  `ssh`/`down` shows global flags and no leftover `op` args.
- `cargo run -p mc2 -- down --help` shows `--volumes`; `rm` alias still parses.

## What changed

Implemented in `crates/mc2/src/main.rs` (+ `tests/cli_smoke.rs`, docs):

1. **Grouped top-level help** — clap has no native subcommand grouping
   (`next_help_heading` only groups *arguments*, and leaked into `up --help`
   making `--file` render under a "Stacks:" heading — reverted). Added a custom
   renderer mirroring microsandbox's PR #918: `try_show_grouped_top_level_help()`
   intercepts bare `mc2` / `mc2 -h` / `mc2 --help` via raw argv, renders
   `Cli::command().write_long_help(...)`, and splices the `Commands:` block out
   for grouped sections (`Stacks`/`Observe`/`Access`/`Security`/`Admin` + an
   `Other` fallback that includes clap's `help`). ANSI styling on TTY only,
   honoring `NO_COLOR`. Subcommand help (`mc2 up --help`) stays clap-native.
2. **Global connection flags** — `--api`/`--token`/`--context`/
   `--allow-insecure-http` are now `global = true` on `Cli`; `OperatorArgs` and
   all 15 `#[command(flatten)] op` fields were deleted. `resolve_op` takes the
   four globals and returns `Conn`; every operator handler now receives
   `conn: &Conn` and uses `conn.url`/`conn.token`. `mc2 status` already took
   `&Conn`. Accepted before or after the subcommand.
3. **Flattened `mc2 ssh key`** → `mc2 ssh add-key|show-key|keys|rm-key`
   (`keys` keeps the alias `key-ls`); `SshKeyCmd`/`SshKeyCommands` deleted.
4. **`rm` folded into `down`** — `DownArgs` gained `--volumes`; `down_cmd`
   merged `rm_cmd`; `Commands::Down` has hidden `alias = "rm"`. `RmArgs`,
   `rm_cmd`, `Commands::Rm` deleted.
6. **Env config kept** — `MC2_API`/`MC2_API_KEY`/`MC2_CONTEXT` moved onto the
   global flags unchanged. `context set <name> --api <url> [--token <key>]`
   still works: its local `--api`/`--token` were removed (they'd collide with
   the globals) and `context_set` now reads the global `api`/`token`.

Tests: updated `help_lists_core_commands` (rm is no longer a top-level
command), added `help_groups_top_level_commands`, replaced the `ssh key` help
test with `ssh_help_lists_flattened_key_commands`, updated live-server SSH
calls (`ssh add-key`/`ssh keys`/`ssh rm-key`). Docs updated: README binary
modes + bullets, `docs/guides/stack-yaml.md`, `examples/05-ssh-ingress/README.md`,
CHANGELOG (new `## Unreleased` → `CLI surface cleanup`).

Result: `just fmt` clean, `clippy --all-targets -D warnings` clean, full
`cargo test --workspace` green (22 test binaries, incl. 21 cli_smoke + live
server end-to-end). `git status` also carries the new `docs/tasks/cli-surface-cleanup.md`.

Not done (item 5, deferred): splitting `main.rs` into `commands/` modules.

