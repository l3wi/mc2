# Container & Workload Orchestration — Tooling Review

Review of how major isolation and orchestration systems operate, with an eye toward designing **MCC** (command & control for Microsandbox) for self-hosted home lab → light production.

---

## 1. Layering map (important)

People mix “Docker”, “Kubernetes”, and “Kata” as peers. They sit on different layers:

```
┌──────────────────────────────────────────────────────────┐
│ Orchestration / desired state                            │
│   Kubernetes, K3s, MicroK8s, Nomad, Docker Swarm         │
├──────────────────────────────────────────────────────────┤
│ Cluster / node agents                                    │
│   kubelet, nomad client, swarm worker                    │
├──────────────────────────────────────────────────────────┤
│ Container runtime interface                              │
│   containerd, CRI-O, Docker Engine                       │
├──────────────────────────────────────────────────────────┤
│ Low-level runtime / isolation                            │
│   runc (namespaces) │ gVisor (runsc) │ Kata │ Firecracker│
│   │ libkrun / microsandbox                               │
├──────────────────────────────────────────────────────────┤
│ Host OS + hypervisor (KVM, HVF, etc.)                    │
└──────────────────────────────────────────────────────────┘
```

**Microsandbox today occupies the low-level runtime + local lifecycle layer.**  
**MCC aims at orchestration + node agent (+ maybe a thin cluster API).**

---

## 2. Isolation technologies

### 2.1 Docker / OCI containers (runc)

**What it is:** Process isolation via Linux namespaces, cgroups, capabilities, seccomp, AppArmor/SELinux. Shared host kernel.

**How it operates:**

1. Pull OCI image → unpack layers
2. Create namespaces (pid, net, mnt, uts, ipc, user optional)
3. Apply cgroups limits
4. `runc` starts process in that jail
5. Networking: bridge/veth, CNI, or host network

**Orchestration relationship:** Docker Engine is a single-host daemon. Multi-host needs Swarm or an external orchestrator.

**Strengths:** Ubiquitous, fast, huge ecosystem, OCI standard.  
**Weaknesses:** Shared kernel; container escape history; weak default multi-tenant boundary for untrusted code.

**Relevance to MCC:** Microsandbox deliberately **does not** use this isolation model for the sandbox boundary, but still **consumes OCI images**. MCC should speak OCI (images, labels) for familiarity.

---

### 2.2 Docker Compose & Docker Swarm

**Compose:** Declarative multi-container apps on **one host** (YAML: services, networks, volumes). No scheduler across machines (unless Compose + Swarm stack).

**Swarm mode:**

- Built into Docker Engine
- Manager nodes (Raft) + workers
- Services with replicas, overlay networks, secrets, configs
- Desired state: maintain replica counts

**Strengths:** Extremely simple mental model; Compose file portability; low RAM.  
**Weaknesses:** Stagnant relative to K8s ecosystem; fewer advanced primitives (no first-class CRDs, weaker ecosystem for GitOps/service mesh); Docker Inc focus moved on.

**Homelab take:** Fine for “a few stacks on one NAS”. Weak as a long-term production control plane if you need rich scheduling, multi-cluster, operators.

**MCC takeaway:** Compose-like **stack YAML** is the right UX for human operators. Swarm’s **service + replica + restart** model is a good minimal C2 scope. Avoid copying Swarm’s full overlay networking unless needed — microsandbox already has strong per-sandbox network policy.

---

### 2.3 Kubernetes (full)

**What it is:** Declarative control plane for containerized workloads across a cluster of nodes.

**How it operates (core loop):**

1. **API server** stores objects (Pods, Deployments, Services…) in **etcd**
2. **Controllers** reconcile desired vs actual (Deployment creates ReplicaSets creates Pods)
3. **Scheduler** binds Pods to nodes (resources, affinity, taints)
4. **kubelet** on each node asks container runtime (via CRI) to run Pod sandboxes
5. **CNI** provides pod networking; **kube-proxy** / dataplane implements Services
6. Optional: Ingress, CSI storage, autoscalers, RBAC, NetworkPolicy, Operators (CRDs)

**Workload model:** Pod = smallest unit (one or more containers sharing net/IPC). Higher objects: Deployment, StatefulSet, DaemonSet, Job, CronJob.

**Strengths:** Industry standard; vast ecosystem; extensibility (CRDs/operators); multi-tenant RBAC; proven at scale.  
**Weaknesses:** Operational weight (etcd, many components); YAML explosion; overkill for small fleets; designed around **containers**, not microVM-first (though RuntimeClasses allow Kata/gVisor).

**MCC takeaway:** Steal the **patterns**, not necessarily the full API:

| K8s pattern | Why it works | MCC analog |
| ----------- | ------------ | ---------- |
| Desired state + reconcile | Survives crashes, restarts | MCC controller loop |
| Node agent | Local truth + report status | MCC node / agent on each host |
| Declarative objects | GitOps, audit, review | Sandbox / Stack / Volume CR-like resources |
| Labels & selectors | Bulk ops, routing | Microsandbox already has labels |
| Probes | liveness/readiness | agentd ping + app-level checks |
| RuntimeClass | Pluggable isolation | N/A if everything is microVM; maybe “profiles” |
| etcd | Strong consistency | SQLite/Postgres/NATS/Raft — pick for scale |

Building full K8s-compatible CRI for microsandbox is possible later (Kata-style) but is a **large** project; for home lab → production MVP, a **smaller API** is wiser.

---

### 2.4 Lightweight Kubernetes distros

| Distro | Character | Homelab fit |
| ------ | --------- | ----------- |
| **K3s** | Single binary-ish, SQLite or embedded etcd, opinionated defaults (Traefik, local-path), excellent ARM, huge community | **Best default** for most ARM/homelab clusters |
| **K0s** | Upstream-ish, less opinionated, embedded etcd default | Good if you want pure K8s without bundled add-ons |
| **MicroK8s** | Snap-based, excellent add-on UX, Ubuntu-centric | Great on Ubuntu mono-stack; Snap is a lifestyle choice |
| **RKE2** | Security/compliance oriented, heavier | Overkill for typical homelab; right for regulated prod |

**Resource notes (order of magnitude):** K3s server ~1GB-class on small clusters; agents much lighter. Full kubeadm stacks cost more.

**MCC takeaway:** K3s proves you can deliver “Kubernetes power” with **one install command** and **embedded datastore**. MCC should aim for **K3s-level install UX**, not kubeadm. Consider:

- Single binary control plane + agent
- Embedded SQLite for single-node / small HA later
- Sensible defaults (local storage path, simple ingress later)

---

### 2.5 HashiCorp Nomad

**What it is:** Flexible **job scheduler** for containers, VMs, Java, binaries — not only containers.

**How it operates:**

- **Servers:** Raft consensus, scheduling decisions, cluster state
- **Clients:** Run tasks via drivers (Docker, exec, QEMU, Java…)
- Job specs (HCL) declare task groups, constraints, restart policies, update strategies
- Often paired with **Consul** (service discovery/mesh) and **Vault** (secrets)

**Strengths:** Simpler than K8s for many ops; multi-workload types; multi-datacenter story; one binary.  
**Weaknesses:** Smaller ecosystem than K8s; mesh/secrets are “bring Consul/Vault”; less mindshare for operators hiring/docs.

**MCC takeaway:** Microsandbox is closer to a **Nomad task driver** than to a full K8s. A Nomad-like model:

```
Job → Group → Task(driver=microsandbox, image=..., resources=...)
```

…maps cleanly. MCC could either:

1. Implement its own Nomad-like scheduler, or  
2. Later expose a **Nomad driver** / **K8s RuntimeClass** for ecosystem leverage.

For greenfield home lab C2, (1) with Compose-like UX is simpler.

---

### 2.6 Firecracker

**What it is:** Minimal **VMM** (not an orchestrator). AWS Lambda/Fargate lineage. Rust, KVM-only historically, tiny device model.

**How it operates:** Host process runs Firecracker → boots guest kernel + rootfs → virtio net/block/vsock. Jailer for host hardening. **You** provide orchestration (kernel images, networking, rate limits, multi-tenant scheduling).

**Strengths:** Tiny TCB, fast boot (~100–200ms), proven at hyperscale.  
**Weaknesses:** Linux/KVM only; DIY everything else; no macOS first-class path like libkrun.

**vs microsandbox:** Same isolation class (microVM). Microsandbox uses **libkrun** for library/embeddable VMM + cross-platform + richer app-facing features (secrets, smoltcp policy, SDKs). Firecracker is a building block; microsandbox is a productized runtime around a different VMM.

---

### 2.7 Kata Containers

**What it is:** **OCI/K8s-integrated** framework that runs each pod/container **inside a lightweight VM**. Not a VMM itself — wraps Cloud Hypervisor (default), Firecracker, or QEMU.

**How it operates:**

1. Kubernetes RuntimeClass → CRI → Kata shim
2. Kata starts a microVM per pod (or configuration variant)
3. Guest runs container workload with container-like semantics
4. Networking/storage plumbed to look like a normal pod from the cluster’s POV

**Strengths:** Hardware isolation + **full K8s ecosystem**; production path for multi-tenant clusters.  
**Weaknesses:** Still carries K8s operational weight; boot ~150–300ms; nested virt requirements on some clouds.

**MCC takeaway:** Kata is the industry answer to “I want K8s **and** microVMs”. Strategic options for MCC long-term:

| Path | Description |
| ---- | ----------- |
| A. Independent C2 | MCC control plane talks microsandbox SDK/CLI natively (fastest MVP) |
| B. Kata-like runtime | Implement CRI runtime so **any** K8s schedules microsandbox pods |
| C. Hybrid | MCC for sandbox-native features (secrets injection, agent exec); K8s for general containers |

For home lab → production of **sandbox fleets**, Path A is correct first. Path B is a compatibility play later if K8s ecosystem is required.

---

### 2.8 gVisor

**What it is:** User-space kernel (`runsc`) intercepting syscalls (Sentry). Not a full VM (though has KVM mode).

**Strengths:** Easy drop-in RuntimeClass; no nested virt required; stronger than plain runc.  
**Weaknesses:** Syscall compatibility gaps; I/O overhead; weaker isolation class than microVMs for adversarial multi-tenant.

**MCC takeaway:** Different point on the security spectrum. Microsandbox already chose microVMs; gVisor is a competitor isolation tech, not a control-plane model to copy.

---

### 2.9 libkrun (Microsandbox’s VMM)

**What it is:** Library-form VMM (Red Hat / containers org lineage). Process embeds VMM; often pairs with virtio-fs, TSI-style networking approaches, smoltcp in microsandbox’s design.

**Why microsandbox uses it:** Embeddable (no separate firecracker process architecture required), multi-platform (KVM/HVF/WHP path), fits “SDK spawns sandbox as child process” model.

---

## 3. Side-by-side comparison

### Isolation strength (untrusted code)

| Rank | Tech | Boundary |
| ---- | ---- | -------- |
| Strongest practical | Firecracker / Kata / libkrun-microVMs | Hardware + guest kernel |
| Middle | gVisor | User-space kernel / syscall filter |
| Weakest multi-tenant | runc containers | Shared host kernel |

### Orchestration complexity vs power

| System | Complexity | Power | Best for |
| ------ | ---------- | ----- | -------- |
| Docker Compose | Very low | Single host | Dev / simple lab |
| Docker Swarm | Low | Multi-host services | Small prod, simple ops |
| Nomad | Medium | Multi-workload cluster | Mixed workloads, simpler than K8s |
| K3s / MicroK8s | Medium | Full K8s API | Homelab wanting K8s ecosystem |
| Full Kubernetes | High | Maximum | Large teams / cloud-native shops |
| Firecracker alone | High DIY | Isolation only | Custom serverless platforms |
| Microsandbox alone | Low for single host | Isolation + DX | Local agents, single-machine sandboxes |
| **MCC (target)** | Low–medium | Multi-node microVM fleet | Homelab → small prod **sandbox** platform |

### Feature matrix (orchestration concerns)

| Concern | Compose | Swarm | Nomad | K8s/K3s | Microsandbox | MCC should |
| ------- | ------- | ----- | ----- | ------- | ------------ | ---------- |
| Multi-node | No | Yes | Yes | Yes | No | **Yes** |
| Desired state | Partial | Yes | Yes | Yes | No | **Yes** |
| Scheduling | No | Basic | Strong | Strong | No | **Yes (MVP: simple)** |
| Service discovery | Networks | Overlay DNS | Consul | CoreDNS | Manual ports | **Yes (simple)** |
| Secrets | Files | Swarm secrets | Vault | Secrets/CSI | Host inject ★ | **Integrate ★** |
| Network policy | Limited | Limited | Consul intent | NetworkPolicy | Host policy ★ | **Preserve ★** |
| Hardware isolation | No | No | Optional | Optional (Kata) | **Native ★** | **Keep ★** |
| OCI images | Yes | Yes | Yes | Yes | Yes | Yes |
| Homelab install | Easy | Easy | Easy | K3s easy | Easy single host | **K3s-easy** |

★ = Microsandbox differentiator to **preserve**, not reimplement poorly.

---

## 4. How these systems “think” (control loops)

### Imperative (Docker CLI / msb today)

```
User → command → runtime does it → done
```

State is whatever is running. Restart host → hope you remember what to start (unless compose/systemd).

### Declarative reconciliation (K8s, Nomad, Swarm services)

```
User writes desired state
     ↓
Controller: while true {
  observe actual
  if actual ≠ desired → act (create/kill/migrate)
}
```

**This is the core of any serious C2.** MCC needs a store of desired state and agents that converge.

### Two-level scheduling (K8s)

1. Cluster scheduler: which node?
2. Node runtime: how to run?

MCC should mirror this even if the “scheduler” is naive (spread / fill / pin labels) at first.

---

## 5. Networking models (contrast)

| System | Model |
| ------ | ----- |
| Docker bridge | Per-host bridge, NAT egress |
| Swarm overlay | Multi-host VXLAN-ish overlay |
| K8s | Flat pod network (CNI plugins vary), Services as VIP |
| Nomad | Host ports or CNI; discovery often Consul |
| Microsandbox | **No shared flat network by default**; host gateway + policy per VM; publish ports to host loopback |

**Implication for MCC:** Do **not** assume every sandbox needs a cluster-wide flat L3. Prefer:

1. Default isolated egress policy (inherit microsandbox defaults)
2. Explicit service exposure (host port maps, reverse proxy, optional mesh later)
3. Optional “private profile” between sandboxes when users opt in

This is more secure for untrusted workloads than K8s’s default “all pods can talk”.

---

## 6. Storage models (contrast)

| System | Model |
| ------ | ----- |
| Docker volumes | Local or plugin |
| K8s | PV/PVC + CSI (Longhorn, NFS, cloud disks) |
| Nomad | host_volume, CSI |
| Microsandbox | bind, named vol, disk image, tmpfs, snapshots (local) |

**MCC:** Start with **node-local** named volumes + explicit placement (like local-path provisioner). Multi-node shared storage is phase 2 (NFS/Longhorn-like) — expensive to get right.

---

## 7. Secrets models (contrast)

| System | Model |
| ------ | ----- |
| K8s Secrets | etcd (base64); often encrypted at rest; mounted as env/files — **values enter the container** |
| Vault | Dynamic secrets, agents inject — values still often land in process env |
| Swarm secrets | tmpfs mounts in container |
| Microsandbox | **Placeholder in guest; real value only at allowed network edge** |

**MCC must not dumb this down to “env var in the VM”.** Control plane should store secret *references* and configure microsandbox injection; never require putting long-lived raw secrets only inside guests.

---

## 8. What “good” looks like for MCC (drawn from above)

### Steal from K3s

- One-command install
- Single binary / small footprint
- Embedded DB for small deployments
- Agent on every node
- Sensible defaults

### Steal from Compose / Swarm

- Human-writable stack files
- Services, replicas, restart policies
- Simple mental model for home lab users

### Steal from Nomad

- Explicit job/task driver abstraction (`driver = microsandbox`)
- Easy multi-workload future (maybe later: container tasks too)
- Constraints / affinities without full K8s scheduler complexity

### Steal from Kata (conceptually, later)

- Optional “looks like a runtime under a bigger orchestrator” for ecosystem

### Preserve from Microsandbox

- Hardware isolation
- Host-side network policy
- Host-side secret injection
- OCI workflow
- SDK/CLI ergonomics
- Labels, metrics, snapshots

### Explicitly **do not** copy early

- Full K8s API surface / CRD zoo
- Flat all-to-all pod networking as default
- etcd operational burden for single-node labs
- Swarm overlay as mandatory substrate

---

## 9. Competitive / adjacent products (sandbox platforms)

For context (not full deep dive):

| Product | Model |
| ------- | ----- |
| E2B | Cloud Firecracker sandboxes, managed |
| Daytona | Dev environments (often container-based) |
| Modal | Serverless compute, GPUs |
| Docker Sandboxes | microVM + private dockerd for agents |
| Microsandbox Cloud | Hosted twin of local runtime (beta) |

MCC differentiates as **self-hosted control plane for microsandbox**, not a new isolation runtime.

---

## 10. Bottom line for architecture decisions

1. **Isolation is solved** by microsandbox (libkrun microVMs). Don’t rebuild VMM.
2. **Orchestration is missing.** That’s MCC.
3. **Closest spiritual models:** K3s (ops UX) + Nomad (driver model) + Compose (app UX) + Swarm (replica simplicity).
4. **Closest isolation peer in K8s world:** Kata — but full K8s integration is optional phase, not MVP.
5. **Security features unique to microsandbox** (secrets, egress policy) must remain first-class in the C2 API, not papered over with vanilla “env secrets”.
