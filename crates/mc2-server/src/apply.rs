//! Apply stack YAML: plan the whole desired state, commit it atomically, schedule.

use crate::host_metrics::dir_size_mib;
use crate::scheduler::{
    pick_node, reserved_capacity_excluding, residual_capacity, service_load_map,
};
use crate::ResourceLimits;
use anyhow::{Context, Result};
use mc2_api::{parse_stack_yaml, ServiceSpec, StackDocument};
use mc2_store::{
    ClaimKind, HostPortClaim, InstanceRecord, NodeStatus, PlannedInstance, PortProtocol, StackPlan,
    Store, StoreError,
};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

/// Apply-time classification so the REST layer can map user errors to 400
/// (validation / port allocation / capacity) instead of substring-matching messages.
#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    /// Stack YAML parse/validation failure (user error).
    #[error("{0}")]
    Validation(String),
    /// Host port allocation conflict (fixed-block / auto range).
    #[error("{0}")]
    Allocation(String),
    /// Apply refused by the configured resource budget (`--limit-*`).
    #[error("{0}")]
    Capacity(String),
    /// Store, serialization, or scheduling failure.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// What an apply may use, to enforce resource budgets. `0` = unlimited.
#[derive(Debug, Clone)]
pub struct ApplyConfig {
    pub limits: ResourceLimits,
    pub data_dir: PathBuf,
    pub volume_dir: PathBuf,
    /// Port the operator REST API binds (never claimable by `expose`); `0` = unknown.
    pub rest_port: u16,
    /// Server `--allow-host-profile`: permit `network.profiles: [host]`.
    pub allow_host_profile: bool,
    /// Host-loopback probe for `expose` ports (injectable for tests).
    pub port_probe: PortProbe,
}

/// Probes a host-loopback port for `expose`: `Ok(())` when the port is free to
/// bind, `Err(reason)` when it must be rejected (in use, privileged, …).
///
/// A function pointer (not a closure trait object) so `ApplyConfig` stays
/// `Clone`; tests substitute a no-op to stay hermetic, and the real one is
/// exercised directly against real listeners on `127.0.0.1`.
pub type PortProbe = fn(u16) -> Result<(), String>;

/// Real `expose` probe: **connect first**, then test-bind.
///
/// The connect catches a daemon bound to `*:P` (which still answers on
/// `127.0.0.1` where a `SO_REUSEADDR` bind could otherwise silently steal its
/// loopback traffic); the bind catches an address the connect can't reach
/// (e.g. a listener refusing connections) and privileged ports on Linux.
pub fn probe_host_loopback_port(port: u16) -> Result<(), String> {
    use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};

    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    if TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(250)).is_ok() {
        return Err(format!(
            "host loopback port {port} is already in use by another process (it answers on \
             127.0.0.1:{port})"
        ));
    }
    match TcpListener::bind(addr) {
        Ok(listener) => {
            drop(listener);
            Ok(())
        }
        Err(e) => Err(bind_error_message(port, &e)),
    }
}

/// Map a failed loopback bind to an operator-actionable message.
fn bind_error_message(port: u16, e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::AddrInUse => {
            format!("host loopback port {port} is already in use by another process")
        }
        std::io::ErrorKind::PermissionDenied => format!(
            "host loopback port {port} is privileged: binding it needs CAP_NET_BIND_SERVICE \
             (grant it with `sudo setcap cap_net_bind_service=+ep \"$(command -v mc2)\"`, or \
             systemd `AmbientCapabilities=CAP_NET_BIND_SERVICE`, or lower \
             net.ipv4.ip_unprivileged_port_start below {port}), or choose a port above 1023"
        ),
        _ => format!("host loopback port {port} cannot be bound: {e}"),
    }
}

impl ApplyConfig {
    /// Reserved CPU/memory for a stack document (summed over services × scale).
    fn stack_reserved(&self, doc: &StackDocument) -> (u32, u64) {
        let mut cpu = 0u32;
        let mut mem = 0u64;
        for svc in doc.services.values() {
            let scale = svc.scale.max(1) as u64;
            cpu = cpu.saturating_add(u64::from(svc.cpus).saturating_mul(scale) as u32);
            mem = mem.saturating_add(svc.mem_limit_mib.saturating_mul(scale));
        }
        (cpu, mem)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    pub stack: String,
    pub services: u32,
    pub instances: u32,
    pub scheduled: u32,
    pub pending: u32,
    /// SSH front ends declared in the stack (printed by `mc2 up`).
    #[serde(default)]
    pub ssh: Vec<ApplySshEndpoint>,
}

/// One service's declared SSH front end (desired; auto ports resolve on first
/// reconcile — see `mc2 ssh ls`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplySshEndpoint {
    pub service: String,
    pub bind: String,
    /// Fixed host port; `None` = auto-allocate on reconcile.
    pub port: Option<u16>,
    /// Traefik entrypoint from an `ingress.tcp` route, when configured.
    pub entrypoint: Option<String>,
    pub replicas: u32,
}

/// Resolve `ports` with `published: 0` (target-only / hostname sugar) and
/// `scale > 1` fixed ports to concrete, stable host ports, producing one
/// [`ServiceSpec`] per replica (index = ordinal). Persisted in the stored spec
/// so ingress routes and the desired set see fixed backend ports across restarts.
///
/// Allocation rules:
/// - Fixed `published: P`: replica `i` gets `P + i` (a contiguous block). The
///   block may move between applies — this same apply releases the service's
///   previous ports — but never onto a taken or already-assigned port.
/// - Auto (`published: 0`): replica `i` keeps the port it already had for its
///   (ordinal, target), else takes a distinct free port from the 10000+ pool.
///
/// Ports MC2 already takes are never handed out: other stacks' publishes,
/// every `expose` claim (exclusive server-wide, including this document's own),
/// the REST bind and the guest DNS interceptor. A port handed out in this pass
/// is never handed out again, so two targets can never share a host port (C5).
async fn resolve_replica_ports(
    store: &dyn Store,
    cfg: &ApplyConfig,
    stack: &str,
    service: &str,
    base: &ServiceSpec,
    additionally_taken: &[u16],
) -> Result<Vec<ServiceSpec>, ApplyError> {
    use std::collections::{BTreeMap, BTreeSet};

    let scale = base.scale.max(1);
    // Every host port that is not available to a `ports:` publish.
    let mut taken: BTreeSet<u16> = BTreeSet::new();
    let mut existing: BTreeMap<(u32, u16), u16> = BTreeMap::new(); // (ordinal, target) → published
    for inst in store.list_instances().await.map_err(anyhow::Error::from)? {
        let Ok(s) = serde_json::from_str::<ServiceSpec>(&inst.spec_json) else {
            continue;
        };
        // An exposed port is claimed exclusively server-wide; the splice binds
        // it on host loopback, so it can never double as a publish port.
        for ex in &s.expose {
            taken.insert(ex.port);
        }
        for p in s.ports {
            if p.published == 0 {
                continue;
            }
            if inst.stack != stack || inst.service != service {
                taken.insert(p.published);
            } else if p.target != 0 {
                existing.insert((inst.ordinal, p.target), p.published);
            }
        }
    }
    // This document's own `expose` claims, which are not in the store yet.
    for port in additionally_taken {
        taken.insert(*port);
    }
    if cfg.rest_port != 0 {
        taken.insert(cfg.rest_port);
    }
    // msb's DNS interceptor owns :53 on the guest's behalf.
    taken.insert(53);

    // Ports this service holds today. A *fresh* auto allocation must not steal
    // one, in case another entry keeps it; a fixed entry may take one, since
    // this same apply releases this service's previous ports.
    let held: BTreeSet<u16> = existing.values().copied().collect();
    // Ports handed out during this pass: never twice, whatever their origin.
    let mut assigned: BTreeSet<u16> = BTreeSet::new();

    let mut out = Vec::with_capacity(scale as usize);
    for ord in 0..scale {
        let mut spec = base.clone();
        for p in spec.ports.iter_mut() {
            let prev = existing.get(&(ord, p.target)).copied();
            let cand = if p.published != 0 {
                // Fixed `published: P`: replica `i` asks for `P + i`. The block
                // may move between applies; what it must never do is collide.
                let want = u32::from(p.published).checked_add(ord).ok_or_else(|| {
                    ApplyError::Allocation(format!(
                        "published host port {} + replica {ord} exceeds the host port range \
                         (0–65535) — lower the published port or the scale",
                        p.published
                    ))
                })?;
                u16::try_from(want).map_err(|_| {
                    ApplyError::Allocation(format!(
                        "published host port {} + replica {ord} exceeds the host port range \
                         (0–65535) — lower the published port or the scale",
                        p.published
                    ))
                })?
            } else {
                match prev {
                    // Stable: a target keeps the port it already had.
                    Some(prev) => prev,
                    None => (10000..=u16::MAX)
                        .find(|c| !taken.contains(c) && !assigned.contains(c) && !held.contains(c))
                        .ok_or_else(|| {
                            ApplyError::Allocation(
                                "no free host port for automatic allocation (10000–65535 \
                                 exhausted) — publish a fixed port or free one"
                                    .to_string(),
                            )
                        })?,
                }
            };

            // `taken` = other stacks'/services' publishes and every `expose`
            // claim; `assigned` = already handed out in this pass. Either is a
            // hard collision — reusing a previous allocation does not exempt a
            // port another target has already been given (C5).
            if taken.contains(&cand) {
                return Err(ApplyError::Allocation(format!(
                    "published host port {cand} for ports target {} (replica {ord}) is already \
                     taken by another expose or publish in this cluster",
                    p.target
                )));
            }
            if assigned.contains(&cand) {
                return Err(ApplyError::Allocation(format!(
                    "published host port {cand} (from {} + replica {ord}) is already assigned \
                     to another entry in this stack",
                    p.published
                )));
            }
            assigned.insert(cand);
            p.published = cand;
        }
        out.push(spec);
    }
    Ok(out)
}

/// Refuse the apply when the stack's reserved CPU/RAM would exceed the node's
/// advertised capacity (`--cpus`/`--memory-mib`) or the configured budget
/// (`--limit-*`), when a declared volume size is below what that volume already
/// holds, or when the disk reservation would exceed `--limit-disk-mib`.
/// `0` limits are unlimited; capacity is enforced whenever a Ready node is
/// registered (the scheduler would otherwise leave instances Pending).
///
/// Only *other* stacks' reservations count toward the ceiling: the document
/// being applied is the source of truth for this stack's own instances, so
/// re-applying at capacity is idempotent (C3).
async fn check_capacity(
    store: &dyn Store,
    cfg: &ApplyConfig,
    doc: &StackDocument,
) -> Result<(), ApplyError> {
    let limits = cfg.limits;
    let instances = store.list_instances().await.map_err(anyhow::Error::from)?;
    // An apply replaces this stack's instances wholesale, so only *other*
    // stacks' reservations count against the incoming document (C3).
    let (reserved_cpu, reserved_mem) =
        reserved_capacity_excluding(&instances, Some(doc.name.as_str()));
    let (stack_cpu, stack_mem) = cfg.stack_reserved(doc);

    // Node capacity: the advertised placement ceiling (`--cpus`/`--memory-mib`).
    let nodes = store.list_nodes().await.map_err(anyhow::Error::from)?;
    let (cap_cpu, cap_mem) = nodes
        .iter()
        .filter(|n| n.status == NodeStatus::Ready.as_str())
        .fold((0u32, 0u64), |(c, m), n| {
            (c.saturating_add(n.cpus), m.saturating_add(n.memory_mib))
        });
    if cap_cpu > 0 && reserved_cpu.saturating_add(stack_cpu) > cap_cpu {
        return Err(ApplyError::Capacity(format!(
            "refusing apply: stack would reserve {stack_cpu} CPU, exceeding the node's \
             capacity of {cap_cpu} (other stacks reserve {reserved_cpu}) — raise --cpus or scale down"
        )));
    }
    if cap_mem > 0 && reserved_mem.saturating_add(stack_mem) > cap_mem {
        return Err(ApplyError::Capacity(format!(
            "refusing apply: stack would reserve {stack_mem} MiB memory, exceeding the node's \
             capacity of {cap_mem} MiB (other stacks reserve {reserved_mem} MiB) — raise --memory-mib or scale down"
        )));
    }

    // Operator budget (`--limit-*`).
    if limits.cpus > 0 && reserved_cpu.saturating_add(stack_cpu) > limits.cpus {
        return Err(ApplyError::Capacity(format!(
            "refusing apply: stack would reserve {stack_cpu} CPU (limit {}; other stacks reserve {reserved_cpu}) \
             — raise --limit-cpus or scale down",
            limits.cpus
        )));
    }
    if limits.memory_mib > 0 && reserved_mem.saturating_add(stack_mem) > limits.memory_mib {
        return Err(ApplyError::Capacity(format!(
            "refusing apply: stack would reserve {stack_mem} MiB memory (limit {}; other stacks reserve {reserved_mem} MiB) \
             — raise --limit-memory-mib or scale down",
            limits.memory_mib
        )));
    }

    // A declared volume size below the directory's current usage would make
    // every further guest write fail — refuse before touching anything.
    check_volume_shrink(cfg, doc)?;

    // `--limit-disk-mib` is a *reservation* budget: every declared-or-default
    // volume size (each distinct volume once) plus every replica's root disk,
    // across all stacks. It is checked before the store is touched, and the
    // error shows which part of the budget went where.
    if limits.disk_mib > 0 {
        let r = disk_reservation(store, doc, &instances).await?;
        let total = r.total_mib();
        if total > limits.disk_mib {
            return Err(ApplyError::Capacity(format!(
                "refusing apply: disk reservation would reach {} MiB (limit {} MiB) — this stack \
                 needs {} MiB of volumes + {} MiB of root disks ({} replica(s)); other stacks \
                 already reserve {} MiB of volumes + {} MiB of root disks. \
                 Raise --limit-disk-mib or lower volume/root-disk sizes",
                total,
                limits.disk_mib,
                r.doc_volumes_mib,
                r.doc_root_mib,
                r.replicas,
                r.other_volumes_mib,
                r.other_root_mib,
            )));
        }
    }
    Ok(())
}

/// Disk reservation breakdown (MiB) for `--limit-disk-mib`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DiskReservation {
    /// Declared-or-default volume sizes in this document (once per volume).
    doc_volumes_mib: u64,
    /// This document's root disks × replicas (`storage_opt.size` or default).
    doc_root_mib: u64,
    /// Replicas this document will create (root-disk multiplier).
    replicas: u64,
    /// Volume sizes held by every other stored stack.
    other_volumes_mib: u64,
    /// Root disks held by every other stored stack's instances.
    other_root_mib: u64,
}

impl DiskReservation {
    /// Total reservation after this document applies.
    fn total_mib(&self) -> u64 {
        self.doc_volumes_mib
            .saturating_add(self.doc_root_mib)
            .saturating_add(self.other_volumes_mib)
            .saturating_add(self.other_root_mib)
    }
}

/// Compute the cluster-wide disk reservation: this document plus every other
/// stored stack (its declared volumes and its persisted replica specs).
///
/// The current stack's own stored instances are ignored — the document being
/// applied is the source of truth for them.
async fn disk_reservation(
    store: &dyn Store,
    doc: &StackDocument,
    instances: &[InstanceRecord],
) -> Result<DiskReservation, ApplyError> {
    let mut r = DiskReservation::default();
    for vol in doc.volumes.values() {
        r.doc_volumes_mib = r.doc_volumes_mib.saturating_add(vol.size_mib);
    }
    for svc in doc.services.values() {
        let scale = svc.scale.max(1) as u64;
        r.replicas = r.replicas.saturating_add(scale);
        r.doc_root_mib = r
            .doc_root_mib
            .saturating_add(svc.root_disk_mib().saturating_mul(scale));
    }

    for stack in store.list_stacks().await.map_err(anyhow::Error::from)? {
        if stack.name == doc.name {
            continue;
        }
        // A stack whose YAML no longer parses reserves nothing we can see; its
        // instances are still counted through their specs below.
        if let Ok(other) = parse_stack_yaml(&stack.raw_yaml) {
            for vol in other.volumes.values() {
                r.other_volumes_mib = r.other_volumes_mib.saturating_add(vol.size_mib);
            }
        }
    }
    for inst in instances {
        if inst.stack == doc.name {
            continue;
        }
        let Ok(spec) = serde_json::from_str::<ServiceSpec>(&inst.spec_json) else {
            continue;
        };
        r.other_root_mib = r.other_root_mib.saturating_add(spec.root_disk_mib());
    }
    Ok(r)
}

/// Refuse a volume `size` below that volume directory's current usage.
///
/// Shrinking by deleting data is the operator's call, but a declared size the
/// directory already exceeds would fail every subsequent guest write.
fn check_volume_shrink(cfg: &ApplyConfig, doc: &StackDocument) -> Result<(), ApplyError> {
    let root = &cfg.volume_dir;
    for (name, vol) in &doc.volumes {
        let dir = root.join(mc2_runtime::volume_name(&doc.name, name));
        if !dir.is_dir() {
            continue;
        }
        let used = dir_size_mib(&dir);
        if vol.size_mib < used {
            return Err(ApplyError::Validation(format!(
                "volume {name:?} already holds {}; `size: {}` is smaller. Free space first or \
                 choose at least {} (data is kept, the VM restarts)",
                mc2_api::disk::fmt_size(used),
                mc2_api::disk::fmt_size(vol.size_mib),
                mc2_api::disk::fmt_size(used),
            )));
        }
    }
    Ok(())
}

/// Refuse `network.profiles: [host]` unless the server enables it.
///
/// `host` hands the guest the entire host loopback — the control API and every
/// published port — so it is opt-in per server (`--allow-host-profile`).
fn check_host_profile(cfg: &ApplyConfig, doc: &StackDocument) -> Result<(), ApplyError> {
    if cfg.allow_host_profile {
        return Ok(());
    }
    for (name, svc) in &doc.services {
        if svc.network.profiles.iter().any(|p| p == "host") {
            return Err(ApplyError::Validation(format!(
                "service {name}: network.profiles [host] gives the guest the whole host loopback \
                 (the control API and every published port). Re-run the server with \
                 `--allow-host-profile` (env MC2_ALLOW_HOST_PROFILE) to accept that, or use \
                 `private`/`none`."
            )));
        }
    }
    Ok(())
}

/// Record the non-`expose` host ports a service spec declares.
fn reserve_from_spec(
    reserved: &mut std::collections::BTreeMap<u16, String>,
    spec: &ServiceSpec,
    stack: &str,
    service: &str,
) {
    if let Some(ssh) = &spec.ssh {
        if ssh.enabled && ssh.port != 0 {
            reserved
                .entry(ssh.port)
                .or_insert_with(|| format!("an SSH listener declared by {stack}/{service}"));
        }
    }
    for p in &spec.ports {
        if p.published != 0 {
            reserved
                .entry(p.published)
                .or_insert_with(|| format!("a published host port of {stack}/{service}"));
        }
    }
}

/// Suggest the next port not already claimed or taken.
fn suggest_expose_port(
    port: u16,
    claims: &std::collections::BTreeMap<u16, (String, String)>,
    reserved: &std::collections::BTreeMap<u16, String>,
) -> Option<u16> {
    (port.checked_add(1)?..=u16::MAX).find(|p| !claims.contains_key(p) && !reserved.contains_key(p))
}

/// A1/A3: every `expose` port of the document must be claimable.
///
/// * A1 — a port claimed by another stack is rejected naming the owner: the
///   guest's `gateway:P` maps to host `127.0.0.1:P` with no remap, so a splice
///   can only tell services apart by port and ports are exclusive server-wide.
/// * A3 — a port MC2 already takes (REST bind, an SSH listener of any stack, a
///   published host port — including this document's, after resolution — or the
///   DNS interceptor :53) is rejected, and a port that is not free on the host
///   (something is listening, or it is privileged) is rejected too.
///
/// Ports already claimed by *this* stack are its own bound listeners: re-apply
/// is a no-op for them.
async fn check_expose_ports(
    store: &dyn Store,
    cfg: &ApplyConfig,
    stack: &str,
    resolved: &[(String, Vec<ServiceSpec>)],
) -> Result<(), ApplyError> {
    use std::collections::{BTreeMap, BTreeSet};

    let mut reserved: BTreeMap<u16, String> = BTreeMap::new();
    if cfg.rest_port != 0 {
        reserved.insert(cfg.rest_port, "the REST API listener (--bind)".into());
    }
    reserved.insert(53, "the sandbox DNS interceptor".into());

    // Server-wide `expose` claims: port → owning (stack, service).
    let mut claims: BTreeMap<u16, (String, String)> = BTreeMap::new();
    let mut own_claims: BTreeSet<u16> = BTreeSet::new();
    for inst in store.list_instances().await.map_err(anyhow::Error::from)? {
        let Ok(spec) = serde_json::from_str::<ServiceSpec>(&inst.spec_json) else {
            continue;
        };
        for ex in &spec.expose {
            claims
                .entry(ex.port)
                .or_insert_with(|| (inst.stack.clone(), inst.service.clone()));
            if inst.stack == stack {
                own_claims.insert(ex.port);
            }
        }
        reserve_from_spec(&mut reserved, &spec, &inst.stack, &inst.service);
    }
    for (svc, specs) in resolved {
        for spec in specs {
            reserve_from_spec(&mut reserved, spec, stack, svc);
        }
    }

    // Distinct expose ports declared by this document, with their service.
    let mut wanted: BTreeMap<u16, String> = BTreeMap::new();
    for (svc, specs) in resolved {
        for spec in specs {
            for ex in &spec.expose {
                wanted.entry(ex.port).or_insert_with(|| svc.clone());
            }
        }
    }

    for (port, service) in &wanted {
        if let Some((owner_stack, owner_service)) = claims.get(port) {
            if owner_stack != stack {
                let hint = suggest_expose_port(*port, &claims, &reserved)
                    .map(|p| format!(" — try {p}"))
                    .unwrap_or_default();
                return Err(ApplyError::Allocation(format!(
                    "expose port {port} ({stack}/{service}) is already claimed by \
                     {owner_stack}/{owner_service}; exposed guest ports are exclusive \
                     server-wide, so pick another port{hint}"
                )));
            }
        }
        if own_claims.contains(port) {
            // This stack already holds the claim; its listener is bound already.
            continue;
        }
        if let Some(reason) = reserved.get(port) {
            let hint = suggest_expose_port(*port, &claims, &reserved)
                .map(|p| format!(" — try {p}"))
                .unwrap_or_default();
            return Err(ApplyError::Allocation(format!(
                "expose port {port} ({stack}/{service}) conflicts with {reason}{hint}"
            )));
        }
        if let Err(reason) = (cfg.port_probe)(*port) {
            return Err(ApplyError::Allocation(format!(
                "expose port {port} ({stack}/{service}): {reason}"
            )));
        }
    }
    Ok(())
}

/// Apply a stack document from YAML text.
pub async fn apply_stack_yaml(
    store: Arc<dyn Store>,
    cfg: &ApplyConfig,
    yaml: &str,
) -> Result<ApplyResult, ApplyError> {
    let doc = parse_stack_yaml(yaml).map_err(ApplyError::Validation)?;
    apply_stack(store, cfg, &doc, yaml).await
}

/// Validate a document and resolve its stack's complete desired state —
/// capacity, `expose` claims, per-replica host ports, and the full instance set
/// — **without writing anything**.
///
/// Everything that can reject an apply is decided here, so a rejected apply
/// leaves the store byte-identical (C1). [`Store::commit_stack_plan`] then
/// publishes the whole plan in one transaction; any stored instance this plan
/// does not list (a service dropped from the document, or an ordinal past a
/// scale-down) is deleted by that commit.
async fn plan_stack(
    store: &dyn Store,
    cfg: &ApplyConfig,
    doc: &StackDocument,
    raw_yaml: &str,
) -> Result<StackPlan, ApplyError> {
    check_capacity(store, cfg, doc).await?;
    check_host_profile(cfg, doc)?;

    // Resolve per-replica host ports *before* any write so the reserved-port
    // checks below see this document's own published ports.
    let doc_exposes: Vec<u16> = doc
        .services
        .values()
        .flat_map(|s| s.expose.iter().map(|e| e.port))
        .collect();
    let mut resolved: Vec<(String, Vec<ServiceSpec>)> = Vec::with_capacity(doc.services.len());
    for (svc_name, spec) in &doc.services {
        let mut base = spec.clone();
        base.with_volume_sizes(&doc.volumes);
        let specs =
            resolve_replica_ports(store, cfg, &doc.name, svc_name, &base, &doc_exposes).await?;
        resolved.push((svc_name.clone(), specs));
    }

    // A1 (exclusive server-wide claims) + A3 (reserved/host-free ports).
    check_expose_ports(store, cfg, &doc.name, &resolved).await?;

    let mut instances = Vec::new();
    for (svc_name, specs) in &resolved {
        // Persist the resolved per-replica ports so ingress routes and the
        // desired set see stable backend ports across restarts.
        for (ordinal, spec) in specs.iter().enumerate() {
            instances.push(PlannedInstance {
                service: svc_name.clone(),
                ordinal: ordinal as u32,
                spec_json: serde_json::to_string(spec).context("serialize service spec")?,
            });
        }
    }

    // C4: the host ports this plan claims, asserted by the store's
    // `(port, protocol)` constraint inside the commit's transaction. The checks
    // above are the primary path; these rows are the backstop, and they also
    // free the stack's previous block when a `published` base moves.
    let mut host_ports: Vec<HostPortClaim> = Vec::new();
    for (svc_name, specs) in &resolved {
        for (ordinal, spec) in specs.iter().enumerate() {
            for p in &spec.ports {
                // `published: 0` was resolved to a concrete port for every
                // replica; a stored spec never carries one.
                if p.published != 0 {
                    host_ports.push(HostPortClaim {
                        port: p.published,
                        protocol: PortProtocol::from_spec(&p.protocol),
                        service: svc_name.clone(),
                        ordinal: Some(ordinal as u32),
                        kind: ClaimKind::Publish,
                    });
                }
            }
        }
        // `expose` is one exclusive server-wide claim per service+port — the
        // shared splice binds it once, for every replica — so `ordinal` is None.
        // Validation makes the port list identical across replicas.
        if let Some(spec) = specs.first() {
            for ex in &spec.expose {
                host_ports.push(HostPortClaim {
                    port: ex.port,
                    protocol: PortProtocol::from_spec(&ex.protocol),
                    service: svc_name.clone(),
                    ordinal: None,
                    kind: ClaimKind::Expose,
                });
            }
        }
    }

    Ok(StackPlan {
        stack: doc.name.clone(),
        labels_json: "{}".to_string(),
        raw_yaml: raw_yaml.to_string(),
        instances,
        host_ports,
    })
}

pub async fn apply_stack(
    store: Arc<dyn Store>,
    cfg: &ApplyConfig,
    doc: &StackDocument,
    raw_yaml: &str,
) -> Result<ApplyResult, ApplyError> {
    let plan = plan_stack(store.as_ref(), cfg, doc, raw_yaml).await?;

    // All-or-nothing: the stack row and every instance create/update/delete
    // land in one transaction, alone in its own commit.
    let committed = match store.commit_stack_plan(&plan).await {
        Ok(committed) => committed,
        // C4 backstop: the database constraint caught a host-port claim the
        // checks above did not (a lost race between two applies). It is the same
        // user-facing class as any other allocation conflict.
        Err(StoreError::Conflict(msg)) => return Err(ApplyError::Allocation(msg)),
        Err(e) => {
            return Err(ApplyError::Other(
                anyhow::Error::from(e).context("commit stack plan"),
            ))
        }
    };
    let total_instances = committed.len() as u32;

    let scheduled = run_scheduler(store.clone()).await?;
    let pending = store
        .list_pending_instances()
        .await
        .context("list pending")?
        .len() as u32;

    let ssh: Vec<ApplySshEndpoint> = doc
        .services
        .iter()
        .filter(|(_, s)| s.ssh.as_ref().is_some_and(|ssh| ssh.enabled))
        .map(|(name, s)| {
            let spec = s.ssh.as_ref().expect("filtered enabled");
            let entrypoint = doc
                .ingress
                .as_ref()
                .and_then(|ing| ing.tcp.iter().find(|r| r.service == *name))
                .map(|r| r.entry_point.clone());
            ApplySshEndpoint {
                service: name.clone(),
                bind: spec.bind.clone(),
                port: (spec.port != 0).then_some(spec.port),
                entrypoint,
                replicas: s.scale,
            }
        })
        .collect();

    info!(
        stack = %doc.name,
        services = doc.services.len(),
        instances = total_instances,
        scheduled,
        pending,
        ssh = ssh.len(),
        "stack applied"
    );
    mc2_metrics::record_apply();
    mc2_metrics::record_schedule_binds(u64::from(scheduled));

    Ok(ApplyResult {
        stack: doc.name.clone(),
        services: doc.services.len() as u32,
        instances: total_instances,
        scheduled,
        pending,
        ssh,
    })
}

/// Schedule all Pending instances. Returns number newly bound.
pub async fn run_scheduler(store: Arc<dyn Store>) -> Result<u32> {
    let nodes = store.list_nodes().await.context("list nodes")?;
    let all = store.list_instances().await.context("list instances")?;
    let pending = store
        .list_pending_instances()
        .await
        .context("list pending")?;

    let residual = residual_capacity(&nodes, &all);
    let mut residual_mut = residual;
    let mut scheduled = 0u32;

    // Track load as we assign within this pass
    let mut instances_snapshot = all;

    for inst in pending {
        let spec: ServiceSpec = serde_json::from_str(&inst.spec_json)
            .with_context(|| format!("parse spec for {}", inst.id))?;
        let load = service_load_map(&instances_snapshot, &inst.service);
        let Some(node_id) = pick_node(
            &inst,
            &spec,
            &nodes,
            &load,
            &residual_mut,
            &instances_snapshot,
        ) else {
            continue;
        };

        let bound = store
            .bind_instance_to_node(&inst.id, &node_id)
            .await
            .with_context(|| format!("bind {}", inst.id))?;

        if let Some(entry) = residual_mut.get_mut(&node_id) {
            entry.0 = entry.0.saturating_sub(spec.cpus);
            entry.1 = entry.1.saturating_sub(spec.mem_limit_mib);
        }
        // update snapshot
        if let Some(slot) = instances_snapshot.iter_mut().find(|i| i.id == inst.id) {
            *slot = bound.clone();
        } else {
            instances_snapshot.push(bound);
        }
        scheduled += 1;
    }

    Ok(scheduled)
}

/// List instances for operator REST/CLI.
pub async fn list_instance_views(store: Arc<dyn Store>) -> Result<Vec<InstanceRecord>> {
    Ok(store.list_instances().await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc2_api::{ExposeSpec, PortSpec};
    use mc2_store::MemoryStore;
    use std::collections::BTreeMap;

    fn spec_with_auto_port(target: u16) -> ServiceSpec {
        ServiceSpec {
            image: "alpine".into(),
            scale: 1,
            cpus: 1,
            mem_limit_mib: 512,
            ports: vec![PortSpec {
                published: 0,
                target,
                protocol: "tcp".into(),
                hostname: None,
            }],
            network: Default::default(),
            env: Default::default(),
            secrets: vec![],
            volumes: vec![],
            restart: "no".into(),
            healthcheck: None,
            labels: Default::default(),
            command: None,
            node_name: None,
            node_selector: Default::default(),
            ssh: None,
            storage_opt: None,
            expose: vec![],
            networks: vec![],
            depends_on: BTreeMap::new(),
        }
    }

    fn spec_with_fixed_port(published: u16, target: u16, scale: u32) -> ServiceSpec {
        ServiceSpec {
            image: "alpine".into(),
            scale,
            cpus: 1,
            mem_limit_mib: 512,
            ports: vec![PortSpec {
                published,
                target,
                protocol: "tcp".into(),
                hostname: None,
            }],
            network: Default::default(),
            env: Default::default(),
            secrets: vec![],
            volumes: vec![],
            restart: "no".into(),
            healthcheck: None,
            labels: Default::default(),
            command: None,
            node_name: None,
            node_selector: Default::default(),
            ssh: None,
            storage_opt: None,
            expose: vec![],
            networks: vec![],
            depends_on: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn resolve_auto_ports_allocates_and_reuses() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();

        // First apply: target-only → concrete host port from the 10000+ range.
        let specs = resolve_replica_ports(
            store.as_ref(),
            &cfg_default(),
            "demo",
            "web",
            &spec_with_auto_port(3001),
            &[],
        )
        .await
        .unwrap();
        assert_eq!(specs.len(), 1);
        let first = specs[0].ports[0].published;
        assert!(
            first >= 10000,
            "auto host port should come from the free range"
        );
        assert_ne!(first, 0);

        // Persist the resolved spec (as apply would) so re-apply reuses it.
        seed_stack(
            &store,
            "demo",
            vec![("web", vec![serde_json::to_string(&specs[0]).unwrap()])],
        )
        .await;

        // Re-apply with the same target-only port → same host port (stable).
        let again = resolve_replica_ports(
            store.as_ref(),
            &cfg_default(),
            "demo",
            "web",
            &spec_with_auto_port(3001),
            &[],
        )
        .await
        .unwrap();
        assert_eq!(
            again[0].ports[0].published, first,
            "re-apply must reuse the allocation"
        );

        // A different target gets a different free port, avoiding the used one.
        let other = resolve_replica_ports(
            store.as_ref(),
            &cfg_default(),
            "demo",
            "web",
            &spec_with_auto_port(3002),
            &[],
        )
        .await
        .unwrap();
        assert_ne!(other[0].ports[0].published, first);
    }

    #[tokio::test]
    async fn scaled_fixed_port_gets_a_distinct_block_per_replica() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();

        let specs = resolve_replica_ports(
            store.as_ref(),
            &cfg_default(),
            "demo",
            "web",
            &spec_with_fixed_port(5000, 3000, 3),
            &[],
        )
        .await
        .unwrap();
        assert_eq!(specs.len(), 3);
        let host_ports: Vec<u16> = specs.iter().map(|s| s.ports[0].published).collect();
        assert_eq!(host_ports, vec![5000, 5001, 5002]);
    }

    #[tokio::test]
    async fn scaled_auto_ports_are_distinct_and_reused_per_replica() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();

        let mut base = spec_with_auto_port(3000);
        base.scale = 3;
        let specs =
            resolve_replica_ports(store.as_ref(), &cfg_default(), "demo", "web", &base, &[])
                .await
                .unwrap();
        assert_eq!(specs.len(), 3);
        let ports: Vec<u16> = specs.iter().map(|s| s.ports[0].published).collect();
        let mut uniq = ports.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), 3, "each replica must get a distinct host port");

        // Persist and re-apply → the same per-replica ports are reused.
        let jsons: Vec<String> = specs
            .iter()
            .map(|s| serde_json::to_string(s).unwrap())
            .collect();
        store
            .commit_stack_plan(&StackPlan::replicas(
                "demo",
                "{}",
                "yaml",
                vec![("web", jsons)],
            ))
            .await
            .unwrap();
        let again =
            resolve_replica_ports(store.as_ref(), &cfg_default(), "demo", "web", &base, &[])
                .await
                .unwrap();
        assert_eq!(
            again
                .iter()
                .map(|s| s.ports[0].published)
                .collect::<Vec<_>>(),
            ports
        );
    }

    /// ApplyConfig whose host-loopback probe always succeeds (hermetic tests).
    fn cfg_stub_probe() -> ApplyConfig {
        ApplyConfig {
            port_probe: |_p| Ok(()),
            ..cfg_default()
        }
    }

    fn expose_yaml(stack: &str, service: &str, port: u16) -> String {
        format!("name: {stack}\nservices:\n  {service}:\n    image: alpine\n    expose: [{port}]\n")
    }

    #[tokio::test]
    async fn second_stack_exposing_a_claimed_port_is_rejected_naming_the_owner() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        apply_stack_yaml(
            store.clone(),
            &cfg_stub_probe(),
            &expose_yaml("shop", "db", 5432),
        )
        .await
        .expect("first claim");

        let err = apply_stack_yaml(
            store.clone(),
            &cfg_stub_probe(),
            &expose_yaml("billing", "db", 5432),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ApplyError::Allocation(_)), "{err:?}");
        let msg = err.to_string();
        assert!(msg.contains("shop/db"), "names the owner: {msg}");
        assert!(msg.contains("5432"), "{msg}");
        assert!(msg.contains("try 5433"), "suggests another port: {msg}");
        // A rejected apply writes nothing.
        assert!(store.get_stack("billing").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn reapply_keeps_this_stacks_own_expose_claim() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        let yaml = expose_yaml("shop", "db", 5432);
        apply_stack_yaml(store.clone(), &cfg_stub_probe(), &yaml)
            .await
            .unwrap();
        apply_stack_yaml(store, &cfg_stub_probe(), &yaml)
            .await
            .expect("re-apply keeps its own claim");
    }

    #[tokio::test]
    async fn expose_port_equal_to_the_rest_bind_is_rejected() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        let cfg = ApplyConfig {
            rest_port: 7443,
            ..cfg_stub_probe()
        };
        let err = apply_stack_yaml(store, &cfg, &expose_yaml("web", "api", 7443))
            .await
            .unwrap_err();
        assert!(matches!(err, ApplyError::Allocation(_)), "{err:?}");
        assert!(err.to_string().contains("REST"), "{err}");
    }

    #[tokio::test]
    async fn expose_port_conflicting_with_a_published_port_is_rejected() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        let yaml = r#"
name: web
services:
  api:
    image: alpine
    ports:
      - "8080:80"
    expose: [8080]
"#;
        let err = apply_stack_yaml(store, &cfg_stub_probe(), yaml)
            .await
            .unwrap_err();
        assert!(matches!(err, ApplyError::Allocation(_)), "{err:?}");
        let msg = err.to_string();
        assert!(msg.contains("8080"), "{msg}");
        assert!(msg.contains("published host port"), "{msg}");
    }

    #[tokio::test]
    async fn expose_port_held_by_a_foreign_listener_is_rejected() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        // Something already listens on the loopback port: a real probe rejects.
        let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = squatter.local_addr().unwrap().port();

        let err = apply_stack_yaml(store, &cfg_default(), &expose_yaml("web", "api", port))
            .await
            .unwrap_err();
        assert!(matches!(err, ApplyError::Allocation(_)), "{err:?}");
        let msg = err.to_string();
        assert!(msg.contains(&port.to_string()), "{msg}");
        assert!(msg.contains("in use"), "{msg}");
    }

    #[test]
    fn probe_detects_a_held_loopback_port() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = held.local_addr().unwrap().port();
        let err = probe_host_loopback_port(port).unwrap_err();
        assert!(err.contains("in use"), "{err}");
        // (No "free after drop" check: parallel tests may reuse the port.)
    }

    #[test]
    fn privileged_bind_error_explains_cap_net_bind_service() {
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let msg = bind_error_message(80, &denied);
        assert!(msg.contains("CAP_NET_BIND_SERVICE"), "{msg}");
        assert!(msg.contains("net.ipv4.ip_unprivileged_port_start"), "{msg}");

        let in_use = std::io::Error::from(std::io::ErrorKind::AddrInUse);
        assert!(bind_error_message(80, &in_use).contains("in use"));
    }

    #[tokio::test]
    async fn host_profile_needs_the_server_flag() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        let yaml = r#"
name: browser
services:
  cdp:
    image: alpine
    network:
      profiles: [host]
"#;
        let err = apply_stack_yaml(store.clone(), &cfg_stub_probe(), yaml)
            .await
            .unwrap_err();
        assert!(matches!(err, ApplyError::Validation(_)), "{err:?}");
        assert!(err.to_string().contains("--allow-host-profile"), "{err}");

        let allowed = ApplyConfig {
            allow_host_profile: true,
            ..cfg_stub_probe()
        };
        apply_stack_yaml(store, &allowed, yaml)
            .await
            .expect("host profile accepted with the server flag");
    }

    #[tokio::test]
    async fn auto_publish_port_avoids_exclusive_expose_claims() {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        // Stored specs always carry concrete published ports; this one needs
        // none (published `0` is rejected when a stored spec is read back).
        let mut claimed = spec_with_auto_port(5432);
        claimed.ports.clear();
        claimed.expose = vec![ExposeSpec {
            port: 10000,
            protocol: "tcp".into(),
            name: None,
        }];
        seed_stack(
            &store,
            "other",
            vec![("db", vec![serde_json::to_string(&claimed).unwrap()])],
        )
        .await;

        let specs = resolve_replica_ports(
            store.as_ref(),
            &cfg_default(),
            "demo",
            "web",
            &spec_with_auto_port(3000),
            &[],
        )
        .await
        .unwrap();
        assert_ne!(
            specs[0].ports[0].published, 10000,
            "an exclusive expose claim is never handed out as a publish port"
        );
    }

    // --- C1 / C2: the apply path is plan-then-commit; a rejection writes
    // nothing and a re-apply removes what the document dropped. Each scenario
    // runs against MemoryStore and a temp-dir SqliteStore. ---

    async fn memory_store() -> Arc<dyn Store> {
        let store = MemoryStore::new();
        store.init_cluster("").await.unwrap();
        store
    }

    /// Temp-dir SQLite store; the caller keeps the `TempDir` alive.
    async fn sqlite_store() -> (Arc<dyn Store>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = mc2_store::SqliteStore::open(dir.path().join("mc2.db"))
            .await
            .unwrap();
        store.init_cluster("").await.unwrap();
        let store: Arc<dyn Store> = store;
        (store, dir)
    }

    /// Every stored byte that belongs to `stack`: the stack row and all of its
    /// instance rows, in a stable order.
    async fn state_snapshot(store: &Arc<dyn Store>, stack: &str) -> Vec<String> {
        let mut out = vec![format!(
            "stack={:?}",
            store.get_stack(stack).await.unwrap().map(|s| (
                s.labels_json,
                s.raw_yaml,
                s.created_at,
            ))
        )];
        let mut rows: Vec<String> = store
            .list_instances()
            .await
            .unwrap()
            .into_iter()
            .filter(|i| i.stack == stack)
            .map(|i| format!("{i:?}"))
            .collect();
        rows.sort();
        out.extend(rows);
        out
    }

    async fn instances_of(
        store: &Arc<dyn Store>,
        stack: &str,
        service: &str,
    ) -> Vec<InstanceRecord> {
        store
            .list_instances()
            .await
            .unwrap()
            .into_iter()
            .filter(|i| i.stack == stack && i.service == service)
            .collect()
    }

    /// C1: a port conflict for the *second* service must leave the previously
    /// applied stack byte-identical — same stack row, same instance spec,
    /// placement, phase and timestamps. The conflict is found while planning,
    /// so the commit never runs.
    async fn port_conflict_leaves_the_previous_state_untouched(store: Arc<dyn Store>) {
        let cfg = cfg_stub_probe();
        // Another stack owns host port 8080.
        store
            .commit_stack_plan(&StackPlan::replicas(
                "other",
                "{}",
                "name: other\n",
                vec![(
                    "db",
                    vec![
                        r#"{"image":"alpine","ports":[{"published":8080,"target":80}]}"#
                            .to_string(),
                    ],
                )],
            ))
            .await
            .unwrap();

        let first = "name: app\nservices:\n  a:\n    image: alpine\n  b:\n    image: alpine\n";
        apply_stack_yaml(store.clone(), &cfg, first)
            .await
            .expect("first apply");

        // Observable state the rejected apply must not disturb.
        for inst in instances_of(&store, "app", "a").await {
            store
                .bind_instance_to_node(&inst.id, "node-a")
                .await
                .unwrap();
            store
                .update_instance_status(&inst.id, "Running", Some("rt-1"), Some("up"))
                .await
                .unwrap();
            store.update_instance_health(&inst.id, true).await.unwrap();
        }
        let before = state_snapshot(&store, "app").await;

        // `a` changes image; `b` asks for a host port another stack already owns.
        let second = "name: app\nservices:\n  a:\n    image: alpine:3.20\n  b:\n    image: alpine\n    ports: [\"8080:80\"]\n";
        let err = apply_stack_yaml(store.clone(), &cfg, second)
            .await
            .unwrap_err();
        assert!(matches!(err, ApplyError::Allocation(_)), "{err:?}");

        assert_eq!(
            before,
            state_snapshot(&store, "app").await,
            "a rejected apply changed stored state"
        );
    }

    /// C2: a service removed from the document has its instances deleted — ssh
    /// and network rows included — while the surviving service keeps its rows.
    async fn reapply_removes_a_dropped_service(store: Arc<dyn Store>) {
        let cfg = cfg_stub_probe();
        let both =
            "name: app\nservices:\n  web:\n    image: alpine\n  worker:\n    image: alpine\n";
        apply_stack_yaml(store.clone(), &cfg, both).await.unwrap();

        let worker = instances_of(&store, "app", "worker").await.remove(0);
        store
            .update_instance_network_observed(&worker.id, "Ready", "{}", None)
            .await
            .unwrap();
        store
            .put_instance_ssh_desired(&mc2_store::InstanceSshRecord {
                instance_id: worker.id.clone(),
                desired: true,
                ..Default::default()
            })
            .await
            .unwrap();

        let only_web = "name: app\nservices:\n  web:\n    image: alpine\n";
        apply_stack_yaml(store.clone(), &cfg, only_web)
            .await
            .unwrap();

        assert!(store.get_instance(&worker.id).await.unwrap().is_none());
        assert!(instances_of(&store, "app", "worker").await.is_empty());
        assert!(store
            .get_instance_network(&worker.id)
            .await
            .unwrap()
            .is_none());
        assert!(!store
            .list_instance_ssh()
            .await
            .unwrap()
            .iter()
            .any(|r| r.instance_id == worker.id));
        assert_eq!(instances_of(&store, "app", "web").await.len(), 1);
    }

    /// C2: a scale-down deletes only the ordinals at or above the new scale and
    /// updates the lower ones in place, keeping their placement and phase.
    async fn reapply_scale_down_keeps_lower_ordinals(store: Arc<dyn Store>) {
        let cfg = cfg_stub_probe();
        let three = "name: app\nservices:\n  web:\n    image: alpine\n    scale: 3\n";
        apply_stack_yaml(store.clone(), &cfg, three).await.unwrap();

        let before = instances_of(&store, "app", "web").await;
        assert_eq!(before.len(), 3);
        let zero = before[0].id.clone();
        store.bind_instance_to_node(&zero, "node-a").await.unwrap();
        store
            .update_instance_status(&zero, "Running", Some("rt-1"), None)
            .await
            .unwrap();

        let one = "name: app\nservices:\n  web:\n    image: alpine:3.20\n    scale: 1\n";
        apply_stack_yaml(store.clone(), &cfg, one).await.unwrap();

        let after = instances_of(&store, "app", "web").await;
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].ordinal, 0);
        assert_eq!(after[0].id, zero, "ordinal 0 is updated, not recreated");
        assert_eq!(after[0].node_id.as_deref(), Some("node-a"));
        assert_eq!(after[0].phase, "Running");
        assert_ne!(
            after[0].spec_json, before[0].spec_json,
            "the new spec was written"
        );
    }

    #[tokio::test]
    async fn port_conflict_leaves_the_previous_state_untouched_memory() {
        port_conflict_leaves_the_previous_state_untouched(memory_store().await).await;
    }

    #[tokio::test]
    async fn port_conflict_leaves_the_previous_state_untouched_sqlite() {
        let (store, _dir) = sqlite_store().await;
        port_conflict_leaves_the_previous_state_untouched(store).await;
    }

    #[tokio::test]
    async fn reapply_removes_a_dropped_service_memory() {
        reapply_removes_a_dropped_service(memory_store().await).await;
    }

    #[tokio::test]
    async fn reapply_removes_a_dropped_service_sqlite() {
        let (store, _dir) = sqlite_store().await;
        reapply_removes_a_dropped_service(store).await;
    }

    #[tokio::test]
    async fn reapply_scale_down_keeps_lower_ordinals_memory() {
        reapply_scale_down_keeps_lower_ordinals(memory_store().await).await;
    }

    #[tokio::test]
    async fn reapply_scale_down_keeps_lower_ordinals_sqlite() {
        let (store, _dir) = sqlite_store().await;
        reapply_scale_down_keeps_lower_ordinals(store).await;
    }
}

#[tokio::test]
async fn apply_reports_declared_ssh_endpoints() {
    let store = mc2_store::MemoryStore::new();
    store.init_cluster("").await.unwrap();
    let yaml = r#"
name: ssh-demo
services:
  web:
    image: alpine
    ssh: true
  admin:
    image: alpine
    ssh:
      enabled: true
      port: 2222
      authorizedKeys: [dev]
  worker:
    image: alpine
ingress:
  tcp:
    - name: web-ssh
      entryPoint: ssh
      service: admin
"#;
    let result = apply_stack_yaml(store, &cfg_default(), yaml).await.unwrap();
    assert_eq!(result.ssh.len(), 2, "{:?}", result.ssh);

    let web = result.ssh.iter().find(|e| e.service == "web").unwrap();
    assert_eq!(web.bind, "127.0.0.1");
    assert!(web.port.is_none(), "short form → auto port");
    assert!(web.entrypoint.is_none(), "no tcp route for web");

    let admin = result.ssh.iter().find(|e| e.service == "admin").unwrap();
    assert_eq!(admin.port, Some(2222));
    assert_eq!(admin.entrypoint.as_deref(), Some("ssh"));
}

/// Persist a stack's desired instances directly — the same commit `apply_stack`
/// performs — for tests that need stored state without a full apply.
#[cfg(test)]
async fn seed_stack_raw(
    store: &mc2_store::MemoryStore,
    stack: &str,
    raw_yaml: &str,
    services: Vec<(&str, Vec<String>)>,
) -> Vec<InstanceRecord> {
    store
        .commit_stack_plan(&StackPlan::replicas(stack, "{}", raw_yaml, services))
        .await
        .unwrap()
}

#[cfg(test)]
async fn seed_stack(
    store: &mc2_store::MemoryStore,
    stack: &str,
    services: Vec<(&str, Vec<String>)>,
) -> Vec<InstanceRecord> {
    seed_stack_raw(store, stack, "yaml", services).await
}

/// ApplyConfig with unlimited limits and a throwaway data dir (tests only).
#[cfg(test)]
fn cfg_default() -> ApplyConfig {
    ApplyConfig {
        limits: ResourceLimits::default(),
        data_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
        volume_dir: tempfile::tempdir().unwrap().path().join("volumes"),
        rest_port: 0,
        allow_host_profile: false,
        port_probe: probe_host_loopback_port,
    }
}

#[tokio::test]
async fn refuses_apply_over_cpu_limit() {
    let store = mc2_store::MemoryStore::new();
    store.init_cluster("").await.unwrap();
    let cfg = ApplyConfig {
        limits: ResourceLimits {
            cpus: 1,
            memory_mib: 0,
            disk_mib: 0,
        },
        data_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
        volume_dir: tempfile::tempdir().unwrap().path().join("volumes"),
        rest_port: 0,
        allow_host_profile: false,
        port_probe: probe_host_loopback_port,
    };
    let yaml = "name: big\nservices:\n  a:\n    image: alpine\n    cpus: 2\n";
    let err = apply_stack_yaml(store, &cfg, yaml).await.unwrap_err();
    assert!(matches!(err, ApplyError::Capacity(_)), "{err:?}");
    assert!(err.to_string().contains("CPU"), "{err}");
}

#[tokio::test]
async fn refuses_apply_over_memory_limit() {
    let store = mc2_store::MemoryStore::new();
    store.init_cluster("").await.unwrap();
    let cfg = ApplyConfig {
        limits: ResourceLimits {
            cpus: 0,
            memory_mib: 512,
            disk_mib: 0,
        },
        data_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
        volume_dir: tempfile::tempdir().unwrap().path().join("volumes"),
        rest_port: 0,
        allow_host_profile: false,
        port_probe: probe_host_loopback_port,
    };
    let yaml = "name: big\nservices:\n  a:\n    image: alpine\n    mem_limit: 1g\n";
    let err = apply_stack_yaml(store, &cfg, yaml).await.unwrap_err();
    assert!(matches!(err, ApplyError::Capacity(_)), "{err:?}");
    assert!(err.to_string().contains("memory"), "{err}");
}

#[tokio::test]
async fn refuses_apply_when_disk_reservation_exceeded() {
    let store = mc2_store::MemoryStore::new();
    store.init_cluster("").await.unwrap();
    let cfg = ApplyConfig {
        limits: ResourceLimits {
            cpus: 0,
            memory_mib: 0,
            disk_mib: 1024,
        },
        data_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
        volume_dir: tempfile::tempdir().unwrap().path().join("volumes"),
        rest_port: 0,
        allow_host_profile: false,
        port_probe: probe_host_loopback_port,
    };
    // One replica's default root disk (4 GiB) exceeds the 1 GiB budget.
    let yaml = "name: any\nservices:\n  a:\n    image: alpine\n";
    let err = apply_stack_yaml(store, &cfg, yaml).await.unwrap_err();
    assert!(matches!(err, ApplyError::Capacity(_)), "{err:?}");
    let msg = err.to_string();
    assert!(msg.contains("volumes"), "breakdown: {msg}");
    assert!(msg.contains("root disks"), "breakdown: {msg}");
    assert!(msg.contains("limit 1024 MiB"), "breakdown: {msg}");
    assert!(msg.contains("Raise --limit-disk-mib"), "{msg}");
}

#[tokio::test]
async fn disk_reservation_counts_volumes_root_disks_and_other_stacks() {
    let store = mc2_store::MemoryStore::new();
    store.init_cluster("").await.unwrap();

    // Another stack: 1 GiB volume + one replica with a 2 GiB root disk.
    let other_yaml = "name: other\nvolumes:\n  data:\n    kind: dir\n    size: 1GiB\nservices:\n  a:\n    image: alpine\n    volumes:\n      - name: data\n        target: /data\n";
    seed_stack_raw(
        &store,
        "other",
        other_yaml,
        vec![(
            "a",
            vec![r#"{"image":"alpine","storage_opt":{"size":"2GiB"}}"#.to_string()],
        )],
    )
    .await;

    // This stack's own stored instance must not be double-counted.
    let doc = parse_stack_yaml(
        "name: mine\nvolumes:\n  d1:\n    kind: dir\n    size: 512m\n  d2:\n    kind: dir\n    size: 256m\nservices:\n  w:\n    image: alpine\n    scale: 3\n    storage_opt:\n      size: 1GiB\n",
    )
    .unwrap();
    seed_stack(
        &store,
        "mine",
        vec![("w", vec![r#"{"image":"alpine"}"#.to_string(); 3])],
    )
    .await;

    let instances = store.list_instances().await.unwrap();
    let r = disk_reservation(store.as_ref(), &doc, &instances)
        .await
        .unwrap();
    assert_eq!(r.doc_volumes_mib, 512 + 256);
    assert_eq!(r.doc_root_mib, 3 * 1024, "root disk × replicas");
    assert_eq!(r.replicas, 3);
    assert_eq!(r.other_volumes_mib, 1024);
    assert_eq!(
        r.other_root_mib, 2048,
        "other stacks' stored specs, not this stack's"
    );
    assert_eq!(r.total_mib(), 512 + 256 + 3 * 1024 + 1024 + 2048);
}

#[tokio::test]
async fn refuses_apply_when_a_volume_shrinks_below_its_usage() {
    let store = mc2_store::MemoryStore::new();
    store.init_cluster("").await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let vol_dir = root.path().join("mc2-demo--data");
    std::fs::create_dir_all(&vol_dir).unwrap();
    std::fs::write(vol_dir.join("payload"), vec![0u8; 2 * 1024 * 1024]).unwrap();

    let cfg = ApplyConfig {
        limits: ResourceLimits::default(),
        data_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
        volume_dir: root.path().to_path_buf(),
        rest_port: 0,
        allow_host_profile: false,
        port_probe: probe_host_loopback_port,
    };
    let yaml = "name: demo\nvolumes:\n  data:\n    kind: dir\n    size: 1MiB\nservices:\n  web:\n    image: alpine\n    volumes:\n      - name: data\n        target: /data\n";
    let err = apply_stack_yaml(store.clone(), &cfg, yaml)
        .await
        .unwrap_err();
    assert!(matches!(err, ApplyError::Validation(_)), "{err:?}");
    let msg = err.to_string();
    assert!(msg.contains("already holds"), "{msg}");
    assert!(msg.contains("is smaller"), "{msg}");
    assert!(msg.contains("Free space first"), "{msg}");

    // A size at or above the usage applies.
    let ok = "name: demo\nvolumes:\n  data:\n    kind: dir\n    size: 2MiB\nservices:\n  web:\n    image: alpine\n    volumes:\n      - name: data\n        target: /data\n";
    apply_stack_yaml(store, &cfg, ok)
        .await
        .expect("size == usage");
}

#[tokio::test]
async fn applies_within_limits() {
    let store = mc2_store::MemoryStore::new();
    store.init_cluster("").await.unwrap();
    let cfg = ApplyConfig {
        limits: ResourceLimits {
            cpus: 2,
            memory_mib: 1024,
            disk_mib: 0,
        },
        data_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
        volume_dir: tempfile::tempdir().unwrap().path().join("volumes"),
        rest_port: 0,
        allow_host_profile: false,
        port_probe: probe_host_loopback_port,
    };
    let yaml = "name: ok\nservices:\n  a:\n    image: alpine\n    cpus: 1\n    mem_limit: 512m\n";
    apply_stack_yaml(store, &cfg, yaml)
        .await
        .expect("within limits");
}

#[tokio::test]
async fn refuses_apply_over_node_cpu_capacity() {
    let store = mc2_store::MemoryStore::new();
    store.init_cluster("").await.unwrap();
    store
        .upsert_local_node(mc2_store::NodeJoin {
            name: "n1".into(),
            labels_json: "{}".into(),
            arch: "aarch64".into(),
            cpus: 2,
            memory_mib: 8192,
        })
        .await
        .unwrap();
    // No --limit-* set; capacity alone must refuse.
    let yaml = "name: big\nservices:\n  a:\n    image: alpine\n    cpus: 4\n";
    let err = apply_stack_yaml(store, &cfg_default(), yaml)
        .await
        .unwrap_err();
    assert!(matches!(err, ApplyError::Capacity(_)), "{err:?}");
    assert!(err.to_string().contains("capacity"), "{err}");
}

#[tokio::test]
async fn refuses_apply_over_node_memory_capacity() {
    let store = mc2_store::MemoryStore::new();
    store.init_cluster("").await.unwrap();
    store
        .upsert_local_node(mc2_store::NodeJoin {
            name: "n1".into(),
            labels_json: "{}".into(),
            arch: "aarch64".into(),
            cpus: 8,
            memory_mib: 1024,
        })
        .await
        .unwrap();
    // Two 512 MiB services (1024 total) fit; a third pushes past 1024 MiB.
    let ok = "name: ok\nservices:\n  a:\n    image: alpine\n  b:\n    image: alpine\n";
    apply_stack_yaml(store.clone(), &cfg_default(), ok)
        .await
        .expect("two 512 MiB services fit 1024 MiB capacity");
    let over = "name: over\nservices:\n  c:\n    image: alpine\n";
    let err = apply_stack_yaml(store, &cfg_default(), over)
        .await
        .unwrap_err();
    assert!(matches!(err, ApplyError::Capacity(_)), "{err:?}");
    assert!(err.to_string().contains("memory"), "{err}");
}

/// B12: a Failed but still-desired instance whose restart policy will bring it
/// back keeps its reservation, so a second apply that needs that capacity is
/// rejected; `restart: no` releases it and the apply succeeds.
#[cfg(test)]
async fn apply_onto_a_full_node_with_a_failed_instance(
    restart: &str,
) -> Result<ApplyResult, ApplyError> {
    let store = mc2_store::MemoryStore::new();
    store.init_cluster("").await.unwrap();
    store
        .upsert_local_node(mc2_store::NodeJoin {
            name: "n1".into(),
            labels_json: "{}".into(),
            arch: "aarch64".into(),
            cpus: 2,
            memory_mib: 8192,
        })
        .await
        .unwrap();
    let cfg = cfg_default();

    let app = format!(
        "name: app\nservices:\n  web:\n    image: alpine\n    cpus: 2\n    restart: {restart}\n"
    );
    apply_stack_yaml(store.clone(), &cfg, &app)
        .await
        .expect("2 CPUs fit a 2-CPU node");

    let failed = store
        .list_instances()
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.stack == "app")
        .expect("app instance");
    assert!(
        failed.node_id.is_some(),
        "apply must bind the instance, or it would not reserve capacity"
    );
    store
        .update_instance_status(&failed.id, "Failed", None, None)
        .await
        .unwrap();

    // A different stack now wants the node's only spare capacity.
    let other = "name: other\nservices:\n  w:\n    image: alpine\n";
    apply_stack_yaml(store, &cfg, other).await
}

#[tokio::test]
async fn failed_instance_keeps_capacity_while_it_will_restart() {
    let err = apply_onto_a_full_node_with_a_failed_instance("always")
        .await
        .unwrap_err();
    assert!(matches!(err, ApplyError::Capacity(_)), "{err:?}");

    let err = apply_onto_a_full_node_with_a_failed_instance("on-failure")
        .await
        .unwrap_err();
    assert!(matches!(err, ApplyError::Capacity(_)), "{err:?}");

    apply_onto_a_full_node_with_a_failed_instance("no")
        .await
        .expect("restart: no releases the Failed instance's capacity");
}

/// C3: a 3-CPU stack on a 4-CPU node must re-apply unchanged, scale down at
/// capacity, and leave the leftover capacity to *other* stacks only.
#[tokio::test]
async fn reapply_at_node_capacity_is_idempotent() {
    let store = mc2_store::MemoryStore::new();
    store.init_cluster("").await.unwrap();
    store
        .upsert_local_node(mc2_store::NodeJoin {
            name: "n1".into(),
            labels_json: "{}".into(),
            arch: "aarch64".into(),
            cpus: 4,
            memory_mib: 8192,
        })
        .await
        .unwrap();
    let cfg = cfg_default();

    let three = "name: app\nservices:\n  web:\n    image: alpine\n    cpus: 3\n";
    apply_stack_yaml(store.clone(), &cfg, three)
        .await
        .expect("3 CPUs fit a 4-CPU node");
    // The stack's own 3 CPUs must not be counted a second time.
    apply_stack_yaml(store.clone(), &cfg, three)
        .await
        .expect("re-applying at capacity is idempotent");

    let two = "name: app\nservices:\n  web:\n    image: alpine\n    cpus: 2\n";
    apply_stack_yaml(store.clone(), &cfg, two)
        .await
        .expect("scale-down at capacity");

    // Another stack only gets the remaining 2 CPUs.
    let other = "name: other\nservices:\n  w:\n    image: alpine\n    cpus: 2\n";
    apply_stack_yaml(store.clone(), &cfg, other)
        .await
        .expect("remaining 2 CPUs fit");
    let over = "name: over\nservices:\n  w:\n    image: alpine\n    cpus: 3\n";
    let err = apply_stack_yaml(store, &cfg, over).await.unwrap_err();
    assert!(matches!(err, ApplyError::Capacity(_)), "{err:?}");
}

/// C3: the same must hold for the `--limit-*` budgets.
#[tokio::test]
async fn reapply_within_limits_is_idempotent() {
    let store = mc2_store::MemoryStore::new();
    store.init_cluster("").await.unwrap();
    // A big node so only the operator limits can refuse.
    store
        .upsert_local_node(mc2_store::NodeJoin {
            name: "n1".into(),
            labels_json: "{}".into(),
            arch: "aarch64".into(),
            cpus: 16,
            memory_mib: 32768,
        })
        .await
        .unwrap();
    let cfg = ApplyConfig {
        limits: ResourceLimits {
            cpus: 3,
            memory_mib: 1024,
            disk_mib: 0,
        },
        ..cfg_default()
    };

    let app = "name: app\nservices:\n  web:\n    image: alpine\n    cpus: 2\n    mem_limit: 1g\n";
    apply_stack_yaml(store.clone(), &cfg, app)
        .await
        .expect("within the CPU and memory limits");
    apply_stack_yaml(store.clone(), &cfg, app)
        .await
        .expect("re-apply at the limit is idempotent");

    // Only 1 CPU of the limit is left for another stack.
    let over_cpu = "name: over\nservices:\n  w:\n    image: alpine\n    cpus: 2\n";
    let err = apply_stack_yaml(store.clone(), &cfg, over_cpu)
        .await
        .unwrap_err();
    assert!(matches!(err, ApplyError::Capacity(_)), "{err:?}");
    assert!(err.to_string().contains("limit 3"), "{err}");

    // CPU fits (2 + 1 = 3) but memory does not (1024 + 512 > 1024).
    let over_mem =
        "name: mem\nservices:\n  w:\n    image: alpine\n    cpus: 1\n    mem_limit: 512m\n";
    let err = apply_stack_yaml(store, &cfg, over_mem).await.unwrap_err();
    assert!(matches!(err, ApplyError::Capacity(_)), "{err:?}");
    assert!(err.to_string().contains("memory"), "{err}");
}

/// C5: a stored allocation for `8081:81` must not license 8081 (nor 8080) for a
/// different target — `["8080:80","8081:81"]` → `["8080:80","8080:81"]` used to
/// hand 8080 to two targets.
#[tokio::test]
async fn published_port_reuse_must_be_the_exact_previous_allocation() {
    let store = mc2_store::MemoryStore::new();
    store.init_cluster("").await.unwrap();
    seed_stack(
        &store,
        "app",
        vec![(
            "web",
            vec![r#"{"image":"alpine","ports":[{"published":8080,"target":80},{"published":8081,"target":81}]}"#.to_string()],
        )],
    )
    .await;

    let yaml =
        "name: app\nservices:\n  web:\n    image: alpine\n    ports: [\"8080:80\",\"8080:81\"]\n";
    let err = apply_stack_yaml(store, &cfg_default(), yaml)
        .await
        .unwrap_err();
    assert!(matches!(err, ApplyError::Allocation(_)), "{err:?}");
    assert!(err.to_string().contains("8080"), "{err}");
}

/// C5: `published + ordinal` overflowing `u16` is a user error (400), not a 500.
#[tokio::test]
async fn published_port_overflow_is_an_allocation_error() {
    let store = mc2_store::MemoryStore::new();
    store.init_cluster("").await.unwrap();
    let yaml =
        "name: app\nservices:\n  web:\n    image: alpine\n    scale: 2\n    ports: [\"65535:80\"]\n";
    let err = apply_stack_yaml(store, &cfg_default(), yaml)
        .await
        .unwrap_err();
    assert!(matches!(err, ApplyError::Allocation(_)), "{err:?}");
    assert!(err.to_string().contains("65535"), "{err}");
}

/// C5: exhausting the auto-allocation pool is a user error (400), not a 500.
#[tokio::test]
async fn auto_port_exhaustion_is_an_allocation_error() {
    let store = mc2_store::MemoryStore::new();
    store.init_cluster("").await.unwrap();
    let doc =
        parse_stack_yaml("name: app\nservices:\n  web:\n    image: alpine\n    ports: [\"80\"]\n")
            .unwrap();
    let base = doc.services["web"].clone();
    // Pretend every auto-pool port is already claimed.
    let all: Vec<u16> = (10000..=u16::MAX).collect();
    let err = resolve_replica_ports(store.as_ref(), &cfg_default(), "app", "web", &base, &all)
        .await
        .unwrap_err();
    assert!(matches!(err, ApplyError::Allocation(_)), "{err:?}");
}

/// C5: a fixed block is stable across re-applies and may move (the same apply
/// releases the old block), but two entries never share a host port.
#[tokio::test]
async fn fixed_port_block_is_stable_and_may_move() {
    fn yaml(published: u16) -> String {
        format!(
            "name: app\nservices:\n  web:\n    image: alpine\n    scale: 2\n    ports: [\"{published}:80\"]\n"
        )
    }
    async fn published_ports(store: &mc2_store::MemoryStore) -> Vec<u16> {
        let mut ports: Vec<u16> = store
            .list_instances()
            .await
            .unwrap()
            .iter()
            .map(|i| {
                serde_json::from_str::<ServiceSpec>(&i.spec_json)
                    .unwrap()
                    .ports[0]
                    .published
            })
            .collect();
        ports.sort_unstable();
        ports
    }

    let store = mc2_store::MemoryStore::new();
    store.init_cluster("").await.unwrap();
    apply_stack_yaml(store.clone(), &cfg_default(), &yaml(5000))
        .await
        .unwrap();
    assert_eq!(published_ports(&store).await, vec![5000, 5001]);

    apply_stack_yaml(store.clone(), &cfg_default(), &yaml(5000))
        .await
        .expect("unchanged re-apply");
    assert_eq!(
        published_ports(&store).await,
        vec![5000, 5001],
        "the block is stable across re-applies"
    );

    apply_stack_yaml(store.clone(), &cfg_default(), &yaml(5001))
        .await
        .expect("a shifted block does not collide with the old one");
    assert_eq!(published_ports(&store).await, vec![5001, 5002]);

    let collide =
        "name: app\nservices:\n  web:\n    image: alpine\n    ports: [\"5001:80\",\"5001:81\"]\n";
    let err = apply_stack_yaml(store, &cfg_default(), collide)
        .await
        .unwrap_err();
    assert!(matches!(err, ApplyError::Allocation(_)), "{err:?}");
}

/// C4: a commit rejected by the store's host-port constraint is an allocation
/// conflict (HTTP 400), not a 500 — and the rejected apply writes nothing.
///
/// Only a lost race reaches this path: the checks in `plan_stack` read the
/// stored instance specs, so a claim whose owning stack is gone from that scan
/// (as after a concurrent apply that won the race) is exactly what the
/// database backstop is for.
#[tokio::test]
async fn store_host_port_conflict_is_an_allocation_error() {
    let store = mc2_store::MemoryStore::new();
    store.init_cluster("").await.unwrap();
    store
        .commit_stack_plan(
            &StackPlan::replicas("ghost", "{}", "yaml", []).with_host_ports(vec![HostPortClaim {
                port: 8080,
                protocol: PortProtocol::Tcp,
                service: "web".into(),
                ordinal: Some(0),
                kind: ClaimKind::Publish,
            }]),
        )
        .await
        .unwrap();

    let yaml = "name: app\nservices:\n  web:\n    image: alpine\n    ports: [\"8080:80\"]\n";
    let err = apply_stack_yaml(store.clone(), &cfg_default(), yaml)
        .await
        .unwrap_err();
    assert!(matches!(err, ApplyError::Allocation(_)), "{err:?}");
    assert!(err.to_string().contains("8080/tcp"), "{err}");
    assert!(
        store.get_stack("app").await.unwrap().is_none(),
        "the rejected apply wrote nothing"
    );
}

/// Apply records the plan's host ports, for both `ports[]` (per replica) and
/// `expose[]` (once per service) — the store then refuses them to any other
/// stack even when the application-level scan sees no owner in the specs.
#[tokio::test]
async fn apply_records_host_port_claims() {
    let store = mc2_store::MemoryStore::new();
    store.init_cluster("").await.unwrap();
    let yaml = "name: shop\nservices:\n  web:\n    image: alpine\n    ports: [\"8080:80\"]\n  db:\n    image: postgres\n    expose: [15432]\n";
    // A probe that always succeeds: the `expose` port is this test's subject,
    // not whether the host happens to listen on it.
    let cfg = ApplyConfig {
        port_probe: |_p| Ok(()),
        ..cfg_default()
    };
    apply_stack_yaml(store.clone(), &cfg, yaml).await.unwrap();

    for (port, kind) in [(8080u16, ClaimKind::Publish), (15432, ClaimKind::Expose)] {
        let err = store
            .commit_stack_plan(
                &StackPlan::replicas("other", "{}", "yaml", []).with_host_ports(vec![
                    HostPortClaim {
                        port,
                        protocol: PortProtocol::Tcp,
                        service: "api".into(),
                        ordinal: None,
                        kind,
                    },
                ]),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Conflict(_)),
            "port {port}: {err:?}"
        );
        assert!(err.to_string().contains(&format!("{port}/tcp")), "{err}");
        assert!(err.to_string().contains("shop/"), "{err}");
    }
}
