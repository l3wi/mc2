# Hello service

The smallest useful MC2 example: one service, one host port, and an HTTP
server that returns `hello world`. The stack file is deliberately minimal —
`apiVersion`, `kind`, and `metadata.name` are omitted and filled in by
`mc2 apply` (see [stack.yaml reference](../../docs/guides/stack-yaml.md)).

Start the server and agent using [the quickstart](../../docs/guides/quickstart.md),
then run:

```bash
export MC2_API=http://127.0.0.1:7443
./target/debug/mc2 apply -f examples/01-hello-service/stack.yaml
./target/debug/mc2 ps
curl http://127.0.0.1:18091/
```

Expected response:

```text
hello world
```

This example uses a direct host port. It does not require ingress, TLS,
secrets, or a second service.
