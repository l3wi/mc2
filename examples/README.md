# MC2 examples

The examples are arranged by user-facing scenario. Start with
[hello-service](./01-hello-service/).

## Minimal configuration

`mc2 apply` fills in missing boilerplate, so partial files work. A valid stack
file can be as small as:

```yaml
services:
  web:
    image: alpine:3.20
```

Defaults applied by the CLI when fields are absent:

| Field | Default |
| --- | --- |
| `apiVersion` | `mc2/v1` |
| `kind` | `Stack` |
| `metadata.name` | the file name (lowercased, sanitized) |
| `services.<svc>.replicas` | `1` |
| `services.<svc>.resources` | 1 CPU / 512 MiB |
| `services.<svc>.restartPolicy` | `on-failure` |
| `services.<svc>.network.profiles` | `public` |
| `services.<svc>.command` | `sleep infinity` (keeps the VM alive) |
| `services.<svc>.health` | none |

Volumes still require an explicit top-level declaration and absolute mount
paths. Present values are never rewritten — a wrong `apiVersion` still fails
server-side validation loudly. Note that filling re-serializes the document,
so YAML comments in partial files are dropped; complete files pass through
verbatim. Full key reference: [stack.yaml guide](../docs/guides/stack-yaml.md).

## Ready-to-use

- [Hello service](./01-hello-service/) — run one HTTP service and verify it with `curl`.
- [Secrets](./02-secrets/) — inject an encrypted secret with an `allowHosts` policy.
- [Service fabric](./03-service-fabric/) — connect two services with `expose` and `allow`.
- [HTTP ingress](./04-http-ingress/) — route a local hostname through a BYO Traefik instance.
- [SSH ingress](./05-ssh-ingress/) — forward a service's host-side SSH endpoint through Traefik TCP ingress.
- [Persistent volumes](./06-persistent-volumes/) — mount a node-local named volume that survives sandbox recreate.

## Advanced

Deferred or incomplete workflows live under [90-advanced](./90-advanced/). Each one is
marked `UNDER DEVELOPMENT` where the repository does not yet provide a complete,
copy-and-run scenario.
