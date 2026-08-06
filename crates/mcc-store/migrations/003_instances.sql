-- Phase 3: service specs + scheduled sandbox instances.

CREATE TABLE IF NOT EXISTS services (
    stack TEXT NOT NULL,
    name TEXT NOT NULL,
    spec_json TEXT NOT NULL,
    replicas INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (stack, name),
    FOREIGN KEY (stack) REFERENCES stacks(name) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS instances (
    id TEXT PRIMARY KEY,
    stack TEXT NOT NULL,
    service TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    node_id TEXT,
    phase TEXT NOT NULL DEFAULT 'Pending',
    runtime_id TEXT,
    message TEXT,
    spec_json TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (stack, service, ordinal)
);

CREATE INDEX IF NOT EXISTS idx_instances_node ON instances(node_id);
CREATE INDEX IF NOT EXISTS idx_instances_phase ON instances(phase);
