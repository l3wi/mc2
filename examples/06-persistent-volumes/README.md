# Persistent volumes

Named directory volumes that live on the agent node and survive sandbox
recreate, restart, and stack removal. Verified end to end on a hypervisor-
capable agent host; CI covers the apply → agent Sync contract without a VM
(`tests/tests/volumes.rs`).

## Stack configuration

```yaml
volumes:
  data:
    kind: dir

services:
  keep:
    image: alpine:3.20
    volumes:
      - name: data
        mount: /data
```

Fields:

- `volumes` (top level) — declares the volumes available to this stack. Every
  service mount must reference a declared name. `kind: dir` is the only kind
  supported in v1 (directory-backed named volumes; disk images, quotas, and
  snapshots are not yet available).
- `services.<svc>.volumes[].name` — a declared volume name.
- `services.<svc>.volumes[].mount` — absolute guest path; unique per service.

## Node-local behavior

Volumes are node-local: the agent creates them under its named-volume root
(default `~/.microsandbox/volumes`, override with `mc2 agent --volume-dir`).
MC2 resolves each volume to a namespaced identity (`mc2-<stack>--<volume>`) at
sandbox create time, so user-facing YAML names stay unchanged. The agent
canonicalizes `--volume-dir` because the underlying mount refuses to follow
symlinks (e.g. macOS `/tmp` → `/private/tmp`).

Because the data lives on one node:

- The first instance that consumes a volume binds the service to that node.
  Instances are never rescheduled off it — if the node goes NotReady they stay
  Pending until the node returns.
- Node loss makes the volume unavailable, and data is lost if the node's
  storage is lost. There is no replication or migration in v1.

## Retention and backup

Volumes are retained when sandboxes, services, or stacks are removed — the
data stays on disk. MC2 provides no volume deletion command yet; removing data
is an operator action on the node. Back up the named-volume root like any
other durable data directory.

## Concurrency

One volume is consumed by one service instance. MC2 does not provide file
locking or application-level consistency for shared mounts; if a future
multi-consumer scenario mounts one directory from several sandboxes,
coordination is the application's responsibility.

## Run it (lab)

Start the server and agent using the
[quickstart](../../docs/guides/quickstart.md), optionally pointing the agent at
a throwaway volume root:

```bash
mc2 agent --server ... --token ... --volume-dir /tmp/mc2-lab-volumes
mc2 apply -f examples/06-persistent-volumes/stack.yaml
```

Then verify persistence:

```bash
# 1. Write a marker inside the guest.
cargo run -p mc2-runtime --example msb_shell -- smoke-volumes-keep-0 \
  'echo hi > /data/marker'

# 2. Remove the sandbox; the agent recreates it on the next reconcile
#    (MC2 has no stack-delete command yet — removal is per-sandbox).
cargo run -p mc2-runtime --example msb_rm -- smoke-volumes-keep-0

# 3. Marker survives the recreate; the instance stays on the same node.
cargo run -p mc2-runtime --example msb_shell -- smoke-volumes-keep-0 \
  'cat /data/marker'
mc2 ps

# 4. Volume data remains on disk after sandbox removal.
ls /tmp/mc2-lab-volumes/mc2-smoke-volumes--data/
```

`GET /v1/instances` shows the instance bound to its node while the node is
NotReady instead of moving.

## Future work

Deletion, migration, snapshots, quotas, and remote storage are tracked under
[90-advanced](../90-advanced/README.md).
