# SSH control plane — authorized keys + open/close ports

**Status:** Implemented (control plane + **configurable** agent SSH backend)  
**Date:** 2026-08-06  
**Depends on:** agent backend choice (`auto` / `msb-cli` / `disabled` / `sdk`); msb host-side SSH when using CLI  
**Related:** [microsandbox.md](./microsandbox.md), Phase 5 secrets, Phase 6 reschedule

### Agent SSH backend (configurable)

Live SSH is **not** hard-wired to host `msb`. On each agent:

| Mode | Env / flag | Behavior |
| ---- | ---------- | -------- |
| `auto` (default) | `MCC_SSH_BACKEND=auto` | Use `msb ssh` if available; else Failed with config hint |
| `msb-cli` | `MCC_SSH_BACKEND=msb-cli` | Require `msb ssh serve` / `authorize` |
| `disabled` | `MCC_SSH_BACKEND=disabled` | Never open listeners (keys/desired still in CP) |
| `sdk` | `MCC_SSH_BACKEND=sdk` | In-process SDK (not available until upstream deps fix) |

```bash
mcc agent --server http://… --ssh-backend auto --msb-bin /path/to/msb
# or: export MCC_SSH_BACKEND=disabled
# or: export MCC_MSB_BIN=/opt/msb-new/bin/msb
```

---

## 1. Problem

Operators need:

1. A **cluster inventory of authorized public keys** (who may SSH).  
2. To **open / close SSH access** per sandbox instance (host TCP listener on the agent).  
3. The **server** to know **which keys are in force** and **which endpoints are live** (node, bind, port, status).

MSB already implements the protocol front end on the host; MCC must own **desired state, key registry, port assignment, and observed endpoints**.

---

## 2. Separation of concerns

| Layer | Owns |
| ----- | ---- |
| **Control plane (server)** | Authorized key objects; desired SSH mode per service/instance; allocated/reported ports; REST/CLI; Sync material |
| **Agent** | Bind/unbind TCP; `SshServer::serve`; host key files; enforce key set from Sync |
| **msb** | russh server, pubkey auth, shell/SFTP/forward into guest |

**Not guest `ports:`.** App publish (`host→guest`) stays separate. SSH is **host serve** (default `127.0.0.1:<port>`).

---

## 3. Object model

### 3.1 `SshAuthorizedKey` (cluster)

Public keys only (OpenSSH one-line). Never private keys.

| Field | Notes |
| ----- | ----- |
| `name` | Unique id, e.g. `lewi-laptop` |
| `public_key` | `ssh-ed25519 AAAA… comment` |
| `fingerprint` | Derived (SHA256) for list UX |
| `labels` | optional |
| `created_at` / `updated_at` | |

Stored in SQLite (`ssh_authorized_keys`). REST never treats these as secrets, but still require API auth.

**CLI**

```bash
mcc ssh-key add lewi-laptop --file ~/.ssh/id_ed25519.pub
mcc ssh-key add lewi-laptop --key 'ssh-ed25519 AAAA…'
mcc ssh-key ls
mcc ssh-key rm lewi-laptop
```

**REST**

| Method | Path | Body / notes |
| ------ | ---- | ------------ |
| `PUT` | `/v1/ssh/keys/{name}` | `{ "publicKey": "ssh-ed25519 …" }` |
| `GET` | `/v1/ssh/keys` | list name + fingerprint (+ full key optional) |
| `GET` | `/v1/ssh/keys/{name}` | full public key |
| `DELETE` | `/v1/ssh/keys/{name}` | |

Deleting a key that is still referenced by a service is either **blocked** or **triggers re-Sync** (agent drops that key from listeners). Prefer re-Sync + drop.

### 3.2 Service desired SSH (YAML)

```yaml
services:
  web:
    ssh:
      enabled: true                 # desired: agent should serve when Running
      bind: 127.0.0.1               # default
      port: 0                       # 0 = auto-allocate on agent
      # port: 2223                  # fixed host port on the node
      user: root
      sftp: true
      authorizedKeys:               # refs by name into cluster registry
        - lewi-laptop
        - ci-runner
```

- `enabled: false` / omit → agent must **close** any existing listener for that instance.  
- Changing `authorizedKeys` → re-Sync → agent rebuilds `SshServer` (or reloads keys) without recreating the sandbox.  
- Changing `port`/`bind` → close old listener, open new.

### 3.3 Instance observed SSH (server tracks live endpoints)

Extend instance record (or side table) with **observed** fields the agent reports:

| Field | Source |
| ----- | ------ |
| `ssh_desired` | derived from `spec_json.ssh.enabled` |
| `ssh_phase` | `Closed` \| `Opening` \| `Open` \| `Failed` |
| `ssh_bind` | actual bind addr agent used |
| `ssh_port` | actual host port (after auto-alloc) |
| `ssh_node_id` | owning node (same as instance node) |
| `ssh_message` | e.g. `bind failed: address in use` |
| `ssh_updated_at` | |

**Side table option** (cleaner migrations):

```sql
CREATE TABLE instance_ssh (
  instance_id TEXT PRIMARY KEY REFERENCES instances(id) ON DELETE CASCADE,
  desired INTEGER NOT NULL DEFAULT 0,   -- 0/1
  phase TEXT NOT NULL DEFAULT 'Closed',
  bind TEXT,
  port INTEGER,
  message TEXT,
  updated_at TEXT NOT NULL
);
```

Server is the **source of truth for inventory**; agent is source of truth for **bind success**.

---

## 4. Open / close lifecycle

```
YAML enabled=true + instance Running
        │
        ▼
  Sync → DesiredSsh { enabled, bind, port|0, keys[] }
        │
        ▼
  Agent: if enabled && Running:
           ensure listener (open or recreate if config hash changed)
         else:
           close listener
        │
        ▼
  ReportStatus → ssh_phase, ssh_bind, ssh_port
        │
        ▼
  Server stores observed endpoint → GET /v1/instances shows it
```

### Open

1. Control plane has desired `ssh.enabled=true` and resolved key material.  
2. Agent Sync includes `SshConfig` on `DesiredInstance`.  
3. Agent only opens when sandbox phase is **Running**.  
4. Port `0`: pick free port in agent range (e.g. `22000–22999`), record locally + report.  
5. Port fixed: bind or report `Failed` + message.  
6. `SshServer` built with resolved public keys; accept loop until close.

### Close

Triggers (any):

| Trigger | Behavior |
| ------- | -------- |
| `ssh.enabled: false` or key removed / section dropped | Next Sync: close |
| Scale-down / instance deleted | close + GC |
| Unbind / reschedule | close on old node; new node opens fresh (new auto port) |
| Sandbox not Running | close or leave Closed |
| Explicit API (optional) | patch desired off without full re-apply |

**Runtime open/close over API (first-class):** `PUT /v1/instances/{id}/ssh` stores an **instance override**; Sync merges **override over service YAML default**. Operators (and automation) can open/close without re-applying the whole stack. YAML remains the declarative default for greenfield stacks.

---

## 5. gRPC / Sync shape

Extend proto (sketch):

```protobuf
message DesiredInstance {
  // ... existing ...
  SshDesired ssh = 7;  // optional; absent = closed
}

message SshDesired {
  bool enabled = 1;
  string bind = 2;           // "127.0.0.1"
  uint32 port = 3;           // 0 = auto
  string user = 4;
  bool sftp = 5;
  repeated string authorized_public_keys = 6;  // full lines, resolved on server
  string config_hash = 7;    // server hash of bind/port/user/sftp/keys → agent recreates if changed
}

message InstanceStatus {
  // ... existing ...
  SshObserved ssh = 5;
}

message SshObserved {
  string phase = 1;   // Closed | Opening | Open | Failed
  string bind = 2;
  uint32 port = 3;
  string message = 4;
}
```

Server resolves `authorizedKeys: [names]` → public key strings at Sync time (same pattern as secret injection).

---

## 6. REST API (primary surface)

Same pattern as secrets / apply: **everything is available over authenticated REST**; CLI is a thin wrapper (`MCC_API` + bearer token).

Auth: `Authorization: Bearer <api-token>` (or open cluster with `--no-auth`).

### 6.1 Authorized keys

| Method | Path | Body / response |
| ------ | ---- | --------------- |
| `PUT` | `/v1/ssh/keys/{name}` | `{ "publicKey": "ssh-ed25519 AAAA… comment" }` → meta (name, fingerprint, timestamps) |
| `GET` | `/v1/ssh/keys` | `[{ "name", "fingerprint", "publicKey", "updatedAt" }]` |
| `GET` | `/v1/ssh/keys/{name}` | single key object |
| `DELETE` | `/v1/ssh/keys/{name}` | `204` — re-Sync agents that referenced it |

```bash
# Register a key
curl -sS -X PUT "$MCC_API/v1/ssh/keys/lewi-laptop" \
  -H "Authorization: Bearer $MCC_API_KEY" \
  -H "Content-Type: application/json" \
  -d "{\"publicKey\": \"$(cat ~/.ssh/id_ed25519.pub)\"}"

# List
curl -sS "$MCC_API/v1/ssh/keys" -H "Authorization: Bearer $MCC_API_KEY"
```

### 6.2 Open / close SSH on an instance (runtime, no re-apply)

Desired SSH can come from **stack YAML** *or* an **instance override** via API. Override wins over service default when set.

| Method | Path | Body | Effect |
| ------ | ---- | ---- | ------ |
| `GET` | `/v1/instances/{id}/ssh` | — | `{ desired, phase, bind, port, user, sftp, authorizedKeyNames, message, endpoint }` |
| `PUT` | `/v1/instances/{id}/ssh` | see below | set desired SSH for this instance; agent opens/closes on next Sync |
| `DELETE` | `/v1/instances/{id}/ssh` | — | clear override → fall back to stack YAML (or close if YAML has no ssh) |

**PUT body (open / configure):**

```json
{
  "enabled": true,
  "bind": "127.0.0.1",
  "port": 0,
  "user": "root",
  "sftp": true,
  "authorizedKeys": ["lewi-laptop", "ci-runner"]
}
```

**Close without deleting override history (simple):**

```json
{ "enabled": false }
```

```bash
# Open SSH on a running instance (keys must already exist in the registry)
curl -sS -X PUT "$MCC_API/v1/instances/$ID/ssh" \
  -H "Authorization: Bearer $MCC_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"enabled":true,"port":0,"authorizedKeys":["lewi-laptop"]}'

# Close
curl -sS -X PUT "$MCC_API/v1/instances/$ID/ssh" \
  -H "Authorization: Bearer $MCC_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"enabled":false}'

# Observe
curl -sS "$MCC_API/v1/instances/$ID/ssh" \
  -H "Authorization: Bearer $MCC_API_KEY"
```

Resolution order for Sync material:

1. Instance override row (`instance_ssh` desired fields) if present  
2. Else service spec from stack YAML  
3. Else closed  

### 6.3 Inventory

| Method | Path | Purpose |
| ------ | ---- | ------- |
| `GET` | `/v1/instances` | each item includes `ssh: { desired, phase, bind, port, … }` |
| `GET` | `/v1/ssh/endpoints` | flattened open endpoints only (`?stack=&node=&phase=Open`) |

**Example `GET /v1/ssh/endpoints`**

```json
{
  "endpoints": [
    {
      "instanceId": "…",
      "stack": "demo",
      "service": "web",
      "ordinal": 0,
      "nodeId": "…",
      "nodeName": "mac-mini",
      "phase": "Open",
      "bind": "127.0.0.1",
      "port": 22017,
      "user": "root",
      "connectHint": "ssh -p 22017 root@<node-reachable-addr>"
    }
  ]
}
```

### 6.4 CLI (optional thin client)

```bash
mcc ssh-key add|ls|rm …     # → /v1/ssh/keys
mcc ssh ls                  # → /v1/ssh/endpoints
mcc ssh open <id> --keys …  # → PUT …/ssh
mcc ssh close <id>          # → PUT …/ssh {enabled:false}
mcc ssh show <id>           # → GET …/ssh
mcc ssh print <id>          # print ssh(1) command from observed endpoint
```

Apply path remains available for declarative fleets:

```bash
curl -X POST "$MCC_API/v1/stacks:apply" -d '{"yaml":"… ssh: enabled: true …"}'
```

---

## 7. Port allocation strategy

| Policy | Behavior |
| ------ | -------- |
| **Agent auto (`port: 0`)** | Agent binds ephemeral in range; reports port. **Recommended default.** |
| **Fixed port** | YAML `port: 2223`; agent fails if busy. |
| **Server-assigned** | Server picks free port in cluster table before Sync. Avoids multi-agent collisions on same node only if server tracks per-node used ports. |

**Recommendation:** agent auto + report. Server records observed ports for inventory. Optional later: server pre-assign from `node_ssh_ports` occupancy table for sticky fixed ports across restart.

On **reschedule**, auto port is free to change; clients must re-read `mcc ssh ls` / instances.

---

## 8. Key rotation / close semantics

| Event | Agent action | Server inventory |
| ----- | ------------ | ---------------- |
| Key added to service | Recreate listener with new key set (same port if possible) | unchanged endpoint |
| Key removed from registry + still referenced | Apply fails or Sync fails closed | — |
| Key removed from registry, refs updated | re-Sync | — |
| `enabled: false` | Close listener | `phase=Closed`, clear port |
| Node NotReady | Old agent dies → port gone; CP marks Closed on unbind | clear observed |

Config hash on `SshDesired` makes “keys changed” cheap for the agent.

---

## 9. Store migration sketch

```sql
-- 00x_ssh.sql
CREATE TABLE ssh_authorized_keys (
  name TEXT PRIMARY KEY,
  public_key TEXT NOT NULL,
  fingerprint TEXT NOT NULL,
  labels_json TEXT NOT NULL DEFAULT '{}',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE instance_ssh (
  instance_id TEXT PRIMARY KEY,
  -- desired (from YAML and/or PUT /v1/instances/{id}/ssh)
  has_override INTEGER NOT NULL DEFAULT 0,  -- 1 if API override set
  desired INTEGER NOT NULL DEFAULT 0,       -- enabled 0/1
  desired_bind TEXT,
  desired_port INTEGER,                     -- 0 = auto
  desired_user TEXT,
  desired_sftp INTEGER,
  desired_key_names_json TEXT,              -- ["lewi-laptop", …]
  -- observed (from agent ReportStatus)
  phase TEXT NOT NULL DEFAULT 'Closed',
  bind TEXT,
  port INTEGER,
  message TEXT,
  updated_at TEXT NOT NULL
);
```

Host private keys: **agent-local only** (`<agent-data>/ssh/host/<runtime_id>` or msb home). Not in CP.

---

## 10. Security defaults

- Default bind `127.0.0.1`.  
- `enabled: true` with **empty** authorized key list → reject at apply or Sync (`FailedPrecondition`).  
- API auth required for key CRUD and endpoint list.  
- Public keys listed freely to operators; no private key storage.  
- Closing SSH is **desired-state**, not firewall magic: agent drops the TCP accept loop.

---

## 11. Implementation order (when built)

1. **Key registry** — store + **REST** (+ CLI wrapper) + unit tests.  
2. **Instance SSH desired/observed table** + **`PUT/GET /v1/instances/{id}/ssh`**.  
3. **Schema** — optional `ServiceSpec.ssh` for apply defaults.  
4. **gRPC** — `SshDesired` / `SshObserved`; Sync resolve key names → material.  
5. **Agent** — msb `ssh` feature; open/close serve; report observed.  
6. **Inventory** — `GET /v1/ssh/endpoints` (+ CLI).  
7. **Optional** — `mcc ssh` ProxyCommand helper.

---

## 12. Mental model (one line)

**Keys live in the control plane; listeners live on agents; the server tracks both desired SSH and observed endpoints so operators can list, open, and close access without caring about msb’s local files.**
