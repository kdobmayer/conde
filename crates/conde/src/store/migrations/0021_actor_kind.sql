-- What a name stands for (claude, codex, human, service), shown in the lists; descriptive only:
-- routing keeps reading address_kind. Bound rows take their address kind; a mailbox row gets its
-- kind at its next publish.
ALTER TABLE agents ADD COLUMN actor_kind TEXT COLLATE "C"
    CHECK (actor_kind IN ('claude', 'codex', 'human', 'service'));
UPDATE agents SET actor_kind = address_kind WHERE address_kind IN ('claude', 'codex', 'human');
