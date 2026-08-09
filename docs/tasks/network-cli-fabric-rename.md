# Task: `mc2 network` + fabric→network rename

Status: **IMPLEMENTED — 2026-08-09**
Date: 2026-08-09

## Context

The east–west layer was branded **"fabric"** (custom jargon: `expose` +
L4 splices + `*.svc.mc2` DNS) while the schema already used compose vocabulary
(`expose`, `networks`). This pass drops "fabric" from the language entirely and
adds a first-class `mc2 network` command that surfaces the server-wide network
model.

## `mc2 network`

One command, three modes:

- `mc2 network` — table of every network (implicit per-stack **default**
  networks + **named** networks) with stack/service/instance/active counts.
- `mc2 network <name>` — one network's member instances, their fabric
  listeners (`expose`) and per-replica host publishes.
- `mc2 network <stack>/<service>/<ordinal>` — one instance's observed
  exposes/edges (was `mc2 fabric <instance>`).

Backing endpoints: `GET /v1/networks` (aggregates the desired set) and
`GET /v1/instances/{id}/network` (renamed from `/fabric`). `-o json` supported.

## Rename map

| Old | New |
| --- | --- |
| `mc2 fabric <instance>` | `mc2 network <stack>/<service>/<ordinal>` |
| `DesiredFabric`, `FabricAllowDesired`, `FabricExposeDesired`, `FabricObserved`, `FabricExposeStatus`, `FabricEdgeStatus`, `FabricObservedReport` | `DesiredNetwork`, `NetworkAllowDesired`, `NetworkExposeDesired`, `NetworkObserved`, `NetworkExposeStatus`, `NetworkEdgeStatus`, `NetworkObservedReport` |
| `FabricTable` (`fabric_serve.rs`) | `NetworkTable` (`network_serve.rs`) |
| `build_fabric_desired` (server `fabric.rs`) | `build_network_desired` (merged into `networks.rs`) |
| `fabric_fqdn` | `default_network_fqdn` |
| `fabric_host_allow_ports`, `fabric_expose_guest_ports` | `network_*` |
| `instance_fabric` table / `InstanceFabricRecord` / `update_instance_fabric_observed` | `instance_network` (migration 008) / `InstanceNetworkRecord` / `update_instance_network_observed` |
| `/v1/instances/{id}/fabric` | `/v1/instances/{id}/network` |
| `03-service-fabric`, `smoke-fabric`, `FABRIC_OK` | `03-networks`, `smoke-networks`, `NETWORK_OK` |
| `fabric_affinity*` (scheduler) | `network_affinity*` |

`DesiredSandbox.fabric` / `InstanceReport.fabric` fields → `.network`.

## Notes

- Migration `008_network_rename.sql` renames `instance_fabric` → `instance_network`
  (005 stays untouched, so existing data dirs migrate in place).
- Historical docs under `docs/tasks/*` keep the old name (changelog of past work).
- Same-node only; the `mc2 network` instance-ref view reports 404 until a
  hypervisor-capable host reports observed status.
