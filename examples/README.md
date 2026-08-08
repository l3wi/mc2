# MC2 examples

The examples are arranged by user-facing scenario. Start with
[hello-service](./01-hello-service/).

## Minimal configuration

`mc2 up` fills in missing boilerplate, so partial files work. A valid stack
file can be as small as:

```yaml
services:
  web:
    image: alpine:3.20
```

Defaults applied by the CLI when fields are absent:

| Field | Default |
| --- | --- |
| `name` | the file name (lowercased, sanitized) |
| `services.<svc>.scale` | `1` |
| `services.<svc>.cpus` | `1` |
| `services.<svc>.mem_limit` | `512m` |
| `services.<svc>.restart` | `no` |
| `services.<svc>.network.profiles` | `public` |
| `services.<svc>.command` | `sleep infinity` (keeps the VM alive) |
| `services.<svc>.healthcheck` | none |

Volumes still require an explicit top-level declaration and absolute `target`
paths. The parser is canonical — unknown keys (including the old k8s-style
`apiVersion`/`kind`/`metadata`, `replicas`, `resources`, `restartPolicy`,
`health`, `mount`) are rejected loudly rather than silently ignored. Note that
filling re-serializes the document, so YAML comments in partial files are
dropped; complete files pass through verbatim. Full key reference:
[stack.yaml guide](../docs/guides/stack-yaml.md).

## Ready-to-use

- [Hello service](./01-hello-service/) — run one HTTP service and verify it with `curl`.
- [Secrets](./02-secrets/) — inject an encrypted secret with an `allowHosts` policy.
- [Service fabric](./03-service-fabric/) — connect two services with `expose` (full mesh).
- [HTTP ingress](./04-http-ingress/) — route a local hostname through a BYO Traefik instance.
- [SSH ingress](./05-ssh-ingress/) — forward a service's host-side SSH endpoint through Traefik TCP ingress.
- [Persistent volumes](./06-persistent-volumes/) — mount a node-local named volume that survives sandbox recreate.
- [Startup ordering](./07-startup-ordering/) — `depends_on` with `service_healthy`, healthcheck tuning, and per-replica published ports at `scale > 1`.

## Advanced

Deferred or incomplete workflows live under [90-advanced](./90-advanced/). Each one is
marked `UNDER DEVELOPMENT` where the repository does not yet provide a complete,
copy-and-run scenario.
