-- MicroCommandControl cluster bootstrap + placeholder tables for later phases.

CREATE TABLE IF NOT EXISTS cluster_meta (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    initialized INTEGER NOT NULL DEFAULT 0,
    api_token_hash TEXT NOT NULL,
    created_at TEXT NOT NULL
);

-- Nodes (Phase 2 fills these)
CREATE TABLE IF NOT EXISTS nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    labels_json TEXT NOT NULL DEFAULT '{}',
    arch TEXT NOT NULL DEFAULT '',
    cpus INTEGER NOT NULL DEFAULT 0,
    memory_mib INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'Unknown',
    last_heartbeat TEXT,
    created_at TEXT NOT NULL
);

-- Stacks / services / instances (Phase 3)
CREATE TABLE IF NOT EXISTS stacks (
    name TEXT PRIMARY KEY,
    labels_json TEXT NOT NULL DEFAULT '{}',
    raw_yaml TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS secrets_meta (
    name TEXT PRIMARY KEY,
    nonce BLOB,
    ciphertext BLOB,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
