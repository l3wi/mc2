# Task: Compose-style stack config (hard cut from k8s wrapper)

Status: **IMPLEMENTED — 2026-08-08**
Date: 2026-08-08

## Context

The stack YAML is Compose-shaped in the body but still wrapped in k8s
boilerplate (`apiVersion`/`kind`/`metadata`) with k8s-renamed fields
(`replicas`, `restartPolicy`, `resources.cpus/memoryMiB`, `health`,
`volumes[].mount`). The CLI is already Compose (`up`/`down`/`rm`/`config`);
make the **config itself read as Compose** (hard cut, no aliases).

## Target shape

```yaml
name: demo                              # was metadata.name (apiVersion/kind gone)
services:
  web:
    image: python:3.12-alpine
    scale: 2                            # was replicas
    cpus: 1
    mem_limit: 512m                     # was resources.cpus / resources.memoryMiB
    restart: on-failure                 # was restartPolicy (always|on-failure|no)
    healthcheck:                        # was health
      test: ["CMD", "curl", "-f", "http://localhost:8000/"]
      interval: 30s
    ports:
      - "18080:8000"                    # compose short form (was host/guest map)
    volumes:
      - name: data
        target: /data                   # was mount
  db:
    image: postgres:16
    expose: [{ port: 5432 }]            # custom, unchanged
ingress:                                # custom, unchanged
  rules:
    - host: demo.example.com
      paths: [{ service: web, port: 8000 }]
```

## Schema changes (`mc2-api/src/stack.rs`)

| MC2 today | Compose (canonical) | Notes |
| --- | --- | --- |
| `apiVersion`/`kind`/`metadata.name` | `name:` (top-level) | apiVersion/kind removed; `name` derived from file stem when missing (`fill_stack_defaults`) |
| `replicas` | `scale` | default 1 |
| `restartPolicy` | `restart` | `no`→`never`, `unless-stopped`→`always` |
| `resources: {cpus, memoryMiB}` | `cpus:` (float→ceil) + `mem_limit:` (`512m`/bytes→MiB) | defaults 1 cpu / 512 MiB |
| `health` | `healthcheck` | `test` (string/array; CMD/CMD-SHELL stripped) + `interval` (default 30s); `timeout`/`retries`/`start_period` accepted-ignored |
| `volumes[].mount` | `volumes[].target` | named volumes only (unchanged) |
| `ports: [{host,guest,protocol,bind}]` | compose short string `"P:T"` + long `{target, published, protocol}` | `bind` dropped (always loopback, required by ingress) |
| `env` (map) | `env` map **or** list `["A=1"]` | accept both |
| top-level `metadata.labels` | removed | stack labels stored but never read — dropped |
| `expose` / `ssh` / `ingress` / `networks` / `secrets` / `nodeName` / `nodeSelector` | unchanged | custom |

### Deferred (documented, not blocked)

- **Target-only ports** (`ports: ["8000"]` → auto host) rejected with a clear
  message; auto host-port allocation is a separate feature.
- Consequence: `scale` + fixed `ports` collides at create (same host port) —
  documented, same as Docker with fixed published ports.

## Changes by area

| Area | Change | Size |
| --- | --- | --- |
| `mc2-api` | schema rework: `StackDocument`/`ServiceSpec`/`PortSpec`/`HealthSpec`/`VolumeMount`; serde for compose short/long ports + env list; validation + restart/resource parsing | ~250 lines |
| `mc2` CLI | `fill_stack_defaults` → derive `name` (drop apiVersion/kind); `config` prints compose shape | ~40 lines |
| `mc2-server` | `apply.rs` uses `doc.name` (labels gone); no other change (spec_json is internal, unchanged) | ~15 lines |
| Examples + docs | rewrite all examples to compose form; stack-yaml.md, README, quickstart, CHANGELOG | medium |
| Tests | stack.rs validation units; cli smoke config; mc2-tests YAML strings | ~150 lines |

## Out of scope

- Auto host-port allocation (`ports: ["8000"]`).
- Compose `build`/`pull`/`env_file`/`depends_on`/`profiles` etc.
- `secrets` top-level sources (MC2 uses the server secret store).

## Risks

- **Broad surface**: every example + many tests embed the old wrapper.
  Mechanical but must be thorough.
- Spec JSON stored in the DB is internal and unchanged — no data migration.

## Acceptance criteria

- [ ] `mc2 config` on a compose-form stack validates + prints the new shape.
- [ ] `mc2 up` applies compose-form stacks (name derived from file when absent).
- [ ] Old k8s-form files are rejected (hard cut).
- [ ] All examples/tests use compose form; `just check` green.

## Handover notes (append as completed)

- (pending)

## Implementation notes (2026-08-08)

### Direction change: canonical parser, no compatibility layer

Initial draft used a pre-parse transform (`normalize_compose_yaml`) mapping
compose YAML onto the old internal k8s-flavored structs. The operator rejected
that as a compatibility layer and requested a **hard cut**: the Rust structs
themselves are now the compose schema, with `#[serde(deny_unknown_fields)]`
making the parser canonical — any unknown key (old k8s keys or typos) is a
hard error. No translation, no legacy awareness.

### What shipped

- **`mc2-api/src/stack.rs`**: `StackDocument{ name, services, … }` (wrapper
  `apiVersion`/`kind`/`metadata` removed); `ServiceSpec{ scale, cpus, mem_limit,
  restart, healthcheck, … }`; `PortSpec{ published, target, protocol }` with a
  custom deserializer accepting compose short (`"18080:8000"`) and long
  (`{target, published}`) forms; `VolumeMount` serialized as `target`;
  `HealthcheckSpec{ test, interval }` (CMD/CMD-SHELL stripped); `env` map-or-list.
  `deny_unknown_fields` on the document + service + sub-structs.
- **Parser**: `parse_stack_yaml` is `serde_yaml::from_str` (strict) + validation.
- **Restart**: compose values stored verbatim; `RestartPolicy::parse` maps
  `no→Never`, `unless-stopped→Always`. Default is now `no` (compose-faithful;
  previously `on-failure`).
- **`mem_limit`**: `de_mem_limit` parses `512m`/`1g`/`1.5g`/bytes → MiB.
- **CLI**: `fill_stack_defaults` only fills top-level `name:` from the file
  stem (compose project-name semantics).
- **Server/runtime**: field renames across scheduler (`resources_from_spec_json`
  now deserializes the compose spec), reschedule (`no` not `never`), node health
  (`healthcheck.test`), ingress (`published`/`target`), fabric_serve
  (`PortSpec{ published, target }`), spec_hash (`cpus.to_bits()`, no bind),
  apply (`doc.name`, `spec.scale`, labels dropped).
- **Tests**: stack.rs units rewritten to compose form; volumes.rs assertion
  `target`; ingress.rs non-loopback case replaced with target-only-port 400;
  cli smoke config + legacy-rejection via serde's `unknown field`.
- **Examples + docs**: all examples rewritten to compose form (subagent) and
  verified via `mc2 config`; `stack-yaml.md` fully rewritten; READMEs, CHANGELOG.

### Verified

- `mc2 up` on compose-form examples applies; `mc2 config` validates + renders.
- Old k8s file (`apiVersion`/`replicas`) → `unknown field \`apiVersion\`` error.
- `just check` green (22 test binaries).

### Notes / follow-ups

- Target-only ports (`ports: ["8000"]` auto host) still rejected (deferred).
- `scale` + fixed `published` ports collide at create (documented; same as
  Docker). Auto host-port allocation would fix both.
- Stored `spec_json` shape changed → pre-release, delete the data dir.
