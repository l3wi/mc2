-- Phase 8: rename instance_fabric → instance_network (fabric→network language).
-- The old table was created by migration 005; renaming keeps its FK/index.

ALTER TABLE instance_fabric RENAME TO instance_network;
