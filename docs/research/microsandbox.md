# Microsandbox — Research Notes

> Easy, fast, local-first microVMs for untrusted workloads.  
> Project: Super Rad Company (YC X26), Apache-2.0 local runtime  
> Docs: https://docs.microsandbox.dev  
> Repo: https://github.com/superradcompany/microsandbox

This document captures architecture and capability broad strokes so MCC work does not need to re-read upstream docs constantly. Prefer linking back here; re-check upstream only when APIs change.

---

## 1. What it is

Microsandbox is a **microVM runtime + library**, not a cluster orchestrator.

It runs untrusted workloads (AI agents, user code, CI, scrapers, plugins, dev envs) inside **lightweight VMs** with:

- Their own Linux kernel
- Their own memory / vCPUs
- Host-brokered networking, secrets, and filesystem access

**Value prop vs containers:** isolation is hardware virtualization (KVM / HVF / WHP), not shared-kernel namespaces/cgroups. Container escape classes that rely on host kernel sharing do not apply the same way.

**Value prop vs full VMs / Firecracker DIY:** Docker-like OCI workflow, embeddable SDK (no required long-running daemon for the local path), multi-language SDKs, programmable network policy, host-side secret injection.

**Not:** a multi-node scheduler, service mesh, ingress controller, autoscaler, or GitOps control plane. Those are exactly the gaps MCC is meant to fill.

---

## 2. Platforms & prerequisites

| Host | Hypervisor requirement |
| ---- | ---------------------- |
| Linux | KVM (`/dev/kvm`, typically `kvm` group; **not root**) |
| macOS | Apple Silicon + Hypervisor.framework entitlement |
| Windows | Windows Hypervisor Platform (WHP); Windows 11 tested path; preview maturity |

Install locations (defaults):

- Home: `~/.microsandbox/` (Windows: `%USERPROFILE%\.microsandbox`)
- CLI binary: `~/.local/bin/msb` (or package manager)
- Config: `~/.microsandbox/config.json`
- State: sandboxes, volumes, cache, snapshots, logs, secrets under `home`

Self-check: `msb doctor` / `msb doctor --fix`

**Beta software** — expect breaking changes, missing cloud features, rough edges.

---

## 3. Architecture (broad strokes)

### 3.1 Trust model

```
┌─────────────────────────────────────────────────────────────┐
│ HOST (trusted)                                              │
│  App / msb CLI  ──spawn──►  Per-sandbox host process        │
│                             (VMM + net stack + FS broker    │
│                              + secret injection + lifecycle)│
│                                      │ virtio devices only  │
├──────────────────────────────────────┼──────────────────────┤
│ GUEST (untrusted)                    ▼                      │
│  Linux kernel (libkrunfw) + agentd (PID 1) + workload       │
└─────────────────────────────────────────────────────────────┘
```

- **Guest is untrusted.** Assume adversarial.
- **Host is trusted.** Policy, secrets, and mounts live here.
- **Boundary:** hardware hypervisor + small virtio device set.

### 3.2 Virtualization stack

| Layer | Tech |
| ----- | ---- |
| VMM | **libkrun** (library VMM; embeddable, process-local) |
| Guest kernel/firmware | **libkrunfw** |
| Host hypervisors | KVM (Linux), Hypervisor.framework (macOS), WHP (Windows) |
| Network | **smoltcp** userspace stack on host; guest sees normal virtio-net |
| Storage | virtio-blk (root/OCI upper, disk images), virtio-fs (binds/named dirs) |
| Control channel | virtio-console ↔ **agentd** (guest PID 1) |

Virtio attack surface (only host-facing devices):

| Device | Role |
| ------ | ---- |
| virtio-console | Control channel to agentd |
| virtio-net | Frames into host network stack / policy |
| virtio-fs | Explicit host directory mounts |
| virtio-blk | Rootfs + attached disks |
| virtio-rng | Entropy |

No general PCI passthrough, no ambient host sockets from guest.

### 3.3 Runtime process model

Each sandbox ≈ **one host-side process** that owns the VM:

1. App/CLI calls `create` / `msb run`
2. Host process boots microVM, mounts rootfs, starts **agentd**
3. Host ↔ agentd: framed messages (exec, fs, etc.) over virtio-console
4. **Host-driven channel:** guest answers; cannot drive host commands or open arbitrary host connections through the control path
5. Network egress: guest TCP via agentd / virtio-net → host policy → outside world

**Attached vs detached:**

- **Attached:** sandbox dies when parent process exits
- **Detached:** survives as background process; reconnect via name (`Sandbox.get` / `msb start`)

There is **no required cluster daemon** for local use. The SDK embeds/spawns runtime. State is still persisted (SQLite under home) for named sandboxes, volumes, etc.

### 3.4 Core crate modules (`microsandbox-core`)

From docs.rs architecture:

| Module | Responsibility |
| ------ | --------------- |
| `vm` | MicroVM configuration and control |
| `oci` | Image pull, layers, registries |
| `management` | Sandbox lifecycle, orchestration coordination |
| `runtime` | Process supervision / monitoring |
| `models` | DB / persistence schema |
| `config` | Config types and validation |

---

## 4. Lifecycle & states

```
Creating → Running ⇄ Stopped → (removed)
              │
              ├─ Draining → Stopped   (graceful; in-flight execs finish)
              └─ Crashed              (unexpected exit)
```

| State | Meaning |
| ----- | ------- |
| Creating | Kernel load, FS mount, agentd init |
| Running | agentd ready; exec/fs/shell OK |
| Draining | No new execs; wait for in-flight; then Stopped |
| Stopped | VM down; config/state persisted; restartable |
| Crashed | Unexpected exit (panic, OOM, etc.) |

Key APIs: `create`, `start`, `stop`, `restart`, `kill`/`stop --force`, `request_drain`, `detach`, `wait_until_stopped`, `remove`, `list`, `get`, `ping`, `touch` (keepalive; ping alone does not refresh idle timer).

**Policies:** `max_duration`, `idle_timeout` (reclaim inactive sandboxes).

**Modify / tuning:** change CPUs, memory, labels, env, secrets without full recreate (some live, some need restart). Cloud lags local for modify.

---

## 5. Images

Three root sources:

1. **OCI images** (primary) — Docker Hub, GHCR, ECR, GCR, private registries  
   - CoW overlay upper layer per sandbox  
   - Shared cached base layers  
   - Default upper size ~4 GiB (`sandbox_defaults.oci.upper_size_mib`)
2. **Host directory as rootfs** — local only
3. **Disk images** — QCOW2 / raw / VMDK as block root — local only

Cloud: OCI rootfs only.

Registry auth resolution order: SDK explicit → OS keyring (`msb registry login`) → `config.json` → Docker `config.json` helpers → anonymous.

---

## 6. Networking

All traffic through **host-controlled** stack (smoltcp + policy). Guest sees a normal NIC.

### Defaults

- **Egress:** public internet allowed
- **Denied:** private ranges, loopback (host sense), link-local, cloud metadata, host machine
- **Ingress:** only **published ports** (local; bind `127.0.0.1` by default)

### Profiles (composable)

`public` | `private` | `host` (plus terminal whole-policies `none` / `all`)

Non-empty profile sets get DNS through gateway automatically.

### Custom policy

```
default_egress  : allow | deny
default_ingress : allow | deny
rules[]         : first match wins
```

Targets: groups (`public`, `private`, `host`), IPs/CIDRs, domains, ports, protocols.

**Host reach:** `host.microsandbox.internal` (requires `host` profile). Guest loopback ≠ host loopback.

**Port publish:** `-p host:guest` local-only feature; cloud has no local host to publish to.

**DNS / TLS:** host-side DNS control; optional TLS interception with auto CA (inspect HTTPS without guest cooperation).

**Secrets at network edge:** placeholders swapped only when TLS/DNS identity matches allowlisted hosts.

---

## 7. Secrets (distinctive feature)

Model:

1. Host binds secret → env var in guest becomes **placeholder** (default `$MSB_<ENV_NAME>`)
2. Real value never enters VM (when using proper host refs)
3. On egress to **allowed host** (DNS + TLS identity checked), host swaps placeholder → real credential
4. Exfil to other hosts sends only the worthless placeholder

CLI form prefers host env refs: `--secret "GITHUB_TOKEN@api.github.com"`  
Raw values in SDK are stored on disk in sandbox config until rotated to a reference — prefer refs.

---

## 8. Storage & volumes

| Type | Mechanism | Notes |
| ---- | --------- | ----- |
| Bind dir/file | virtio-fs | Host path into guest; ro/noexec/nosuid/nodev options |
| Named volume (dir) | virtio-fs under `~/.microsandbox/volumes/` | Share across sandboxes; host-accessible when unmounted (local) |
| Named volume (disk) | virtio-blk ext4 image | e.g. Docker-in-Docker data root |
| Disk image mount | virtio-blk | Attach qcow2/raw/vmdk at path |
| tmpfs | in-guest memory FS | Ephemeral scratch |

OCI root is private CoW; image cache not mutated by guest writes.

---

## 9. Snapshots (local)

- **Disk-only** capture of writable layer (+ pinned image identity)
- Sandbox must be **stopped or crashed** (not running)
- Not full memory/process snapshot
- Boot: cold boot from captured FS changes (`--from-snapshot`)
- Portable: directory artifact or `.tar.zst` save/load; optional `--with-image` for offline
- Integrity hash optional (`--integrity`)

Use cases: pre-warmed deps, portable baselines, disaster recovery, fork-by-copy of upper layer.

Cloud: use named volumes instead of disk snapshots for durable state.

---

## 10. Surfaces (CLI / SDK / Cloud)

### CLI (`msb`)

Familiar Docker-ish UX:

```bash
msb run python -- python -c 'print("hi")'   # ephemeral
msb create --name app python
msb exec app -- ...
msb start|stop|restart|rm|ls|ps|inspect|metrics|logs
msb pull / msb images / msb rmi
msb volume create|ls|rm
msb snapshot create|ls|load|save|...
msb ssh ...
msb install ubuntu   # install image as host command wrapper
msb doctor / msb self update|downgrade|uninstall
```

`--tree` shows full command hierarchy. TTY auto-detected (no `-it` ritual).

### SDKs

Parity-ish APIs in **Rust, TypeScript, Python, Go**:

- Builder / create / exec / fs / network / secrets / volumes / snapshots / SSH
- Low-level **agent client** for raw agentd access
- Local vs cloud backend selection

### Backends

| Backend | How selected |
| ------- | ------------ |
| Local | Default when no API key |
| Cloud | `MSB_API_KEY` set, or profiles / `MSB_BACKEND=cloud` |

Resolution order (simplified):

1. Programmatic `set_default_backend`
2. `MSB_BACKEND` / non-empty `MSB_API_KEY`
3. `MSB_PROFILE` / `active_profile`
4. Local

Cloud REST control plane: `https://api.microsandbox.dev` (org API key). REST covers lifecycle, volumes, quotas, usage, audit — **not** the in-sandbox exec channel (SDK/CLI speak that separately).

Cloud is/was private beta; feature matrix lags local (no publish ports, limited modify, no disk snapshots, etc.).

### Agent integrations

- MCP server (`microsandbox-mcp`)
- Agent Skills package
- Docker-in-sandbox recipe; systemd as guest PID 1 recipe; metrics via `msb-metrics` OTLP sidecar

---

## 11. Security model (summary)

**Defended by design:**

- Guest → host escape (via hypervisor boundary; residual: VMM/hypervisor bugs)
- Sandbox ↔ sandbox isolation (separate VM, process, upper FS, network gateway)
- SSRF / private pivot / metadata by default network policy
- Secret exfil to non-allowlisted hosts
- Host FS privacy unless mounted

**Out of scope / your job:**

- Compromised host
- Hypervisor/CPU bugs
- What allowed destinations do with data
- Image provenance beyond digest (no signature policy built-in by default)
- Guest self-DoS within its own CPU/memory limits
- In-guest root (default) — optional **restricted** security profile (`no_new_privs`, drop mount-admin, force nosuid/nodev)

---

## 12. Observability

- `msb metrics` / SDK metrics — CPU, memory, network
- Guest runtime metrics + host process metrics
- Shared-memory metrics registry; **msb-metrics** ships OTLP to Prometheus / Grafana / Datadog / Alloy / otel-collector
- Logs: host.log / guest.log split; `msb logs -f`
- Labels for bulk selection and metric attribution

---

## 13. What Microsandbox is **not** (orchestration gap list)

These are absent or only single-host / single-process scope today. MCC should treat them as product surface:

| Capability | Today in microsandbox | Typical orchestrator |
| ---------- | --------------------- | -------------------- |
| Multi-node cluster | No | K8s / Nomad / Swarm |
| Desired-state reconciliation | No (imperative lifecycle) | Controllers / operators |
| Scheduling / bin-packing | No | Scheduler |
| Service discovery / DNS of services | Partial (host names, published ports) | CoreDNS / Consul |
| Ingress / L7 routing | No | Ingress / Traefik |
| Health checks + auto restart | Limited (ping, idle, max_duration) | Probes + restart policies |
| Rolling updates / canary | No | Deployments |
| Declarative multi-sandbox apps | Sandboxfile / project config exists but not full stack | Compose / Helm / Nomad jobs |
| Multi-tenant RBAC | Cloud org API only | K8s RBAC |
| Cluster-wide secrets store | Host env + injection | Vault / K8s secrets |
| Autoscaling | No | HPA / Nomad scaling |
| Persistent multi-node storage | Named volumes local to host | CSI / Longhorn |
| GitOps | No | Argo / Flux |

---

## 14. Mental model for MCC

Think of microsandbox as **runc + containerd for microVMs**, not as Kubernetes.

| Container world | Microsandbox world |
| --------------- | ------------------ |
| runc / containerd | msb host process + libkrun |
| OCI image | OCI image (same) |
| container | sandbox (microVM) |
| docker CLI | msb CLI |
| Docker Compose | (missing → MCC app/stack layer) |
| Kubelet | (missing → MCC node agent) |
| API server / scheduler | (missing → MCC control plane) |
| Kata runtime class | Isolation already native (every sandbox is a VM) |

MCC’s job is the **control plane + node agent + desired-state** layers on top of this excellent execution primitive.
