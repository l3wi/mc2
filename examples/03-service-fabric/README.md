# Service fabric

`UNDER DEVELOPMENT`: the control-plane and agent path is usable, but the example
still requires a manual sandbox-shell step to verify the client request.

```bash
export MC2_API=http://127.0.0.1:7443
./target/debug/mc2 up -f examples/03-service-fabric/stack.yaml
./target/debug/mc2 ps
```

After both services are running, test from the client sandbox:

```bash
cargo run -p mc2-runtime --example msb_shell -- smoke-fabric-client-0 \
  'wget -qO- http://echo.smoke-fabric.svc.mc2:8080/'
```

Expected response: `FABRIC_OK`. The fabric is a full mesh within the stack
(Docker-style default allow): `echo` exposes port 8080, so `client` reaches it
by default — no allow edges needed. Connectivity is same-node only in v1.
