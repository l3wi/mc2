-- Phase 7: per-instance health signal (drives depends_on: service_healthy
-- startup ordering and the healthcheck state machine).

ALTER TABLE instances ADD COLUMN healthy INTEGER NOT NULL DEFAULT 0;
