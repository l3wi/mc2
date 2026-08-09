# Startup ordering (depends_on) + health checks

Compose-style `depends_on` with a `service_healthy` condition: the web service
waits for the DB to be healthy (its healthcheck passes) before starting.

```bash
export MC2_API=http://127.0.0.1:7443
mc2 up -f examples/07-startup-ordering/stack.yaml
mc2 ps
```

Until the DB's `pg_isready` probe passes, `web` stays `Pending` with a
`depends_on: waiting for db (service_healthy)` message. Once the DB reports
healthy, `web` converges to `Running`.

Notes:

- `service_healthy` requires the dependency to declare a healthcheck.
- Ordering only: `web` is not removed if the DB later dies (Compose semantics).
- `scale: 3` on `web` demonstrates per-replica published ports — each replica
  gets a distinct host port (`P`, `P+1`, `P+2`); ingress targets replica 0.
