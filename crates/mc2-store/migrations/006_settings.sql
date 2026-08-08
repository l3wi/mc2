-- Server-level settings (key/value). First use: public hostname for remote access.
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
