# Persistent volumes

Named directory volumes that live on the node and survive sandbox
recreate, restart, and stack removal. Verified end to end on a hypervisor-
capable host; CI covers the apply → desired-set contract without a VM
(`tests/tests/volumes.rs`).

## Stack configuration

```yaml
volumes:
  data:
    kind: dir
    size: 1GiB        # optional; default 10 GiB

services:
  keep:
    image: alpine:3.20
    volumes:
      - name: data
        target: /data
```

Fields:

- `volumes` (top level) — declares the volumes available to this stack. Every
  service mount must reference a declared name. `kind: dir` is the only kind
  supported in v1 (directory-backed volumes; disk images and snapshots are not
  yet available).
- `volumes.<name>.size` — the volume's quota (compose byte sizes: `1GiB`,
  `512m`, or bytes), default 10 GiB. Raise it and re-apply to resize: the VM
  restarts and the data is kept. A `size` below the directory's current usage
  is rejected at apply.
- `services.<svc>.volumes[].name` — a declared volume name.
- `services.<svc>.volumes[].target` — absolute guest path; unique per service.

## Node-local behavior

Volumes are node-local: MC2 owns a plain directory per volume under its volume
root (default `~/.mc2/volumes`, override with `mc2 server --volume-dir`) and
mounts `mc2-<stack>--<volume>` into the VM as a bind mount, so user-facing YAML
names stay unchanged. Every VM start passes an explicit write quota of
`size − current usage`, which makes the declared size an absolute cap that
survives restarts. The volume root is created and canonicalized because the
underlying mount refuses to follow symlinks (e.g. macOS `/tmp` →
`/private/tmp`).

When the volume (or the VM's root disk) fills, the guest sees
`No space left on device`; `mc2 ps` shows the condition in `NOTES` and
`mc2 exec` prints the exact `stack.yaml` change to make.

Because the data lives on one node:

- The first instance that consumes a volume binds the service to that node.
  Instances are never rescheduled off it — if the node goes NotReady they stay
  Pending until the node returns.
- Node loss makes the volume unavailable, and data is lost if the node's
  storage is lost. There is no replication or migration in v1.

## Retention and backup

Volumes are retained when sandboxes, services, or stacks are removed — the
data stays on disk (except `mc2 down --volumes`, which deletes the stack's
volume directories). Back up the volume root like any other durable data
directory.

## Concurrency

One volume is consumed by one service instance. MC2 does not provide file
locking or application-level consistency for shared mounts; if a future
multi-consumer scenario mounts one directory from several sandboxes,
coordination is the application's responsibility.

## Run it (lab)

Start the server using the
[quickstart](../../site/content/documentation/quickstart.mdx). Volumes land
under the default volume root (`~/.mc2/volumes`):

```bash
mc2 up -f examples/06-persistent-volumes/stack.yaml
```

Then verify persistence:

```bash
# 1. Write a marker inside the guest.
mc2 exec smoke-volumes/keep/0 /bin/sh -c 'echo hi > /data/marker'

# 2. Tear down and re-apply; `down` keeps named volumes, so the recreate
#    reuses the same `data` directory.
mc2 down smoke-volumes
mc2 up -f examples/06-persistent-volumes/stack.yaml

# 3. Marker survives the recreate; the instance stays on the same node.
mc2 exec smoke-volumes/keep/0 cat /data/marker
mc2 ps

# 4. Volume data remains on disk after stack removal.
ls ~/.mc2/volumes/mc2-smoke-volumes--data/
```

`GET /v1/instances` shows the instance bound to its node while the node is
NotReady instead of moving.

## Future work

Migration, snapshots, and remote storage are tracked under
[90-advanced](../90-advanced/README.md).
