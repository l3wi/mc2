# Service fabric

`UNDER DEVELOPMENT`: the control-plane and agent path is usable, but the example
still requires a manual sandbox-shell step to verify the client request.

```bash
export MC2_API=http://127.0.0.1:7443
./target/debug/mc2 apply -f examples/service-fabric/stack.yaml
./target/debug/mc2 ps
```

After both services are running, test from the client sandbox:

```bash
cargo run -p mc2-runtime --example msb_shell -- smoke-fabric-client-0 \
  'wget -qO- http://echo.smoke-fabric.svc.mc2:8080/'
```

Expected response: `FABRIC_OK`. Connectivity is same-node and explicitly
allowlisted; east–west access is denied by default.
