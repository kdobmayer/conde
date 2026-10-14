-- Design v2: a per-host route revision, bumped by every binding change that touches the host,
-- so a routes snapshot and a mailbox answer carry the causal point they were read at.
ALTER TABLE hosts ADD COLUMN route_revision BIGINT NOT NULL DEFAULT 0;
