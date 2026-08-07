-- Phase 2: per-node credential hash after join.
ALTER TABLE nodes ADD COLUMN node_token_hash TEXT NOT NULL DEFAULT '';
