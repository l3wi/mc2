-- C4: database-level uniqueness for the host ports a stack claims at apply.
--
-- The application checks ports before writing anything (exclusive `expose`
-- claims, fixed/auto publish blocks) and is the primary, friendlier path; this
-- key is the backstop that makes the guarantee hold even if two applies race or
-- a check is missed.
--
-- `expose` claims one row per service+port with `ordinal` NULL: the shared
-- splice binds it once for the whole service. `publish` claims carry the replica
-- ordinal because the resolved host port is `published + ordinal`. Both kinds
-- share the `(port, protocol)` key space on purpose — publish, expose and the
-- ingress splice all bind host loopback, so one port can never carry two.
--
-- Rows cascade with the stack, so `mc2 down` frees its ports.

CREATE TABLE host_port_claims (
    port INTEGER NOT NULL,
    protocol TEXT NOT NULL CHECK (protocol IN ('tcp', 'udp')),
    stack TEXT NOT NULL REFERENCES stacks(name) ON DELETE CASCADE,
    service TEXT NOT NULL,
    ordinal INTEGER,
    kind TEXT NOT NULL CHECK (kind IN ('publish', 'expose')),
    PRIMARY KEY (port, protocol)
);

-- Every commit deletes and re-inserts this stack's claims; the cascade needs
-- the reverse lookup.
CREATE INDEX host_port_claims_stack ON host_port_claims (stack);
