# MC2 examples

The examples are arranged by user-facing scenario. Start with
[hello-service](./hello-service/).

## Ready-to-use

- [Hello service](./hello-service/) — run one HTTP service and verify it with `curl`.
- [Secrets](./secrets/) — inject a cluster secret with an `allowHosts` policy.
- [Service fabric](./service-fabric/) — connect two same-node services with `expose` and `allow`.
- [HTTP ingress](./04-http-ingress/) — route a local hostname through a BYO Traefik instance.
- [SSH ingress](./05-ssh-ingress/) — forward a service's host-side SSH endpoint through Traefik TCP ingress.
- [Persistent volumes](./06-persistent-volumes/) — mount a node-local named volume that survives sandbox recreate.

## Advanced

Deferred or incomplete workflows live under [90-advanced](./90-advanced/). Each one is
marked `UNDER DEVELOPMENT` where the repository does not yet provide a complete,
copy-and-run scenario.
