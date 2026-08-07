# Hello service

The smallest useful MC2 example: one service, one host port, and a deterministic
HTTP response.

Start the server and agent using [the quickstart](../../docs/guides/quickstart.md),
then run:

```bash
export MC2_API=http://127.0.0.1:7443
./target/debug/mc2 apply -f examples/hello-service/stack.yaml
./target/debug/mc2 ps
curl http://127.0.0.1:18080/
```

Expected response:

```text
MC2_OK
```

This example uses a direct host port. It does not require ingress, TLS, secrets,
or a second service.
