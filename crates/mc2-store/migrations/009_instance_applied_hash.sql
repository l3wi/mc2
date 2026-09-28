-- Phase 9 (B3): persist the applied create-time config fingerprint per instance.
--
-- The node writes it only after `ensure_running` succeeds for that config, so a
-- server restart can tell an adopted sandbox from a stale one: a different hash
-- (or NULL while the sandbox exists) forces a recreate before adoption.
-- NULL = no sandbox has been confirmed running for this instance yet.

ALTER TABLE instances ADD COLUMN applied_hash TEXT;
