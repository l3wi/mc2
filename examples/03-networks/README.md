# Service networks

Same-node mediated networking (D13): an echo server + client reach each other
over the stack's default network via `echo.<stack>.svc.mc2` DNS.

```bash
mc2 up -f examples/03-networks/stack.yaml
mc2 network
mc2 network smoke-networks
```

After both services are running, test from the client sandbox:

```bash
mc2 exec smoke-networks/client/0 wget -qO- http://echo.smoke-networks.svc.mc2:8080/
```

Expected response: `NETWORK_OK`. Connectivity is a full mesh within a shared
network (Docker-style default allow): `echo` exposes port 8080, so `client`
reaches it by default — no allow edges needed. `mc2 network` lists every
network (default + named) with their member instances and ports;
`mc2 network <stack>/<service>/<ordinal>` shows one instance's observed
exposes/edges. Same-node only in v1.
