-- SSH authorized keys registry + per-instance desired/observed SSH state.

CREATE TABLE IF NOT EXISTS ssh_authorized_keys (
    name TEXT PRIMARY KEY,
    public_key TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    labels_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS instance_ssh (
    instance_id TEXT PRIMARY KEY,
    has_override INTEGER NOT NULL DEFAULT 0,
    desired INTEGER NOT NULL DEFAULT 0,
    desired_bind TEXT,
    desired_port INTEGER,
    desired_user TEXT,
    desired_sftp INTEGER,
    desired_key_names_json TEXT,
    phase TEXT NOT NULL DEFAULT 'Closed',
    bind TEXT,
    port INTEGER,
    message TEXT,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (instance_id) REFERENCES instances(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_instance_ssh_phase ON instance_ssh(phase);
