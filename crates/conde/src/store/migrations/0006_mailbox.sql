-- Design v2 (DESIGN-v2.md §4): a published mailbox is an agent row bound to
-- mailbox:<agent_id>@<host>, and a host migrated to v2 takes mailbox addresses only.
ALTER TABLE agents DROP CONSTRAINT agents_address_kind_check,
    ADD CONSTRAINT agents_address_kind_check CHECK (address_kind IN ('claude', 'codex', 'human', 'mailbox'));
ALTER TABLE hosts ADD COLUMN mailbox_only BOOLEAN NOT NULL DEFAULT false;
