-- Observed fabric status per instance (agent-reported JSON snapshot).

CREATE TABLE IF NOT EXISTS instance_fabric (
    instance_id TEXT PRIMARY KEY,
    phase TEXT NOT NULL DEFAULT 'Pending',
    observed_json TEXT NOT NULL DEFAULT '{}',
    message TEXT,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (instance_id) REFERENCES instances(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_instance_fabric_phase ON instance_fabric(phase);
