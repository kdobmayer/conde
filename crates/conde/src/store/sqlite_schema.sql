-- The board schema, SQLite dialect. Idempotent: run on every open; columns
-- added later are applied by ALTER TABLE in sqlite.rs, and a changed CHECK by
-- the table rebuild there.
CREATE TABLE IF NOT EXISTS users(
    user_id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS hosts(
    host_id TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(user_id),
    mailbox_only INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS device_credentials(
    token_digest TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(user_id),
    host_id TEXT NOT NULL REFERENCES hosts(host_id),
    revoked_at TEXT,
    label TEXT,
    created_at TEXT
);
CREATE TABLE IF NOT EXISTS agents(
    agent_id TEXT PRIMARY KEY,
    owner_id INTEGER NOT NULL REFERENCES users(user_id),
    name_lower TEXT NOT NULL,
    display TEXT,
    address_kind TEXT CHECK (address_kind IN ('claude', 'codex', 'human', 'mailbox')),
    address_target TEXT,
    host_id TEXT,
    binding_version INTEGER NOT NULL DEFAULT 0,
    pinned_address TEXT,
    UNIQUE (owner_id, name_lower)
);
CREATE INDEX IF NOT EXISTS agents_by_address ON agents(address_kind, address_target, host_id);
CREATE TABLE IF NOT EXISTS channels(
    channel_id TEXT PRIMARY KEY,
    name_lower TEXT NOT NULL UNIQUE,
    created_by TEXT NOT NULL REFERENCES agents(agent_id)
);
CREATE TABLE IF NOT EXISTS messages(
    commit_seq INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id TEXT NOT NULL UNIQUE,
    sender_agent_id TEXT NOT NULL REFERENCES agents(agent_id),
    recipient_agent_id TEXT REFERENCES agents(agent_id),
    channel_id TEXT REFERENCES channels(channel_id),
    text TEXT NOT NULL,
    reply_to TEXT,
    created_at TEXT NOT NULL,
    CHECK ((recipient_agent_id IS NULL) <> (channel_id IS NULL))
);
CREATE INDEX IF NOT EXISTS messages_by_recipient ON messages(recipient_agent_id, commit_seq);
CREATE INDEX IF NOT EXISTS messages_by_channel ON messages(channel_id, commit_seq);
CREATE INDEX IF NOT EXISTS messages_by_sender ON messages(sender_agent_id, commit_seq);
CREATE TABLE IF NOT EXISTS attachments(
    attachment_id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL REFERENCES messages(message_id),
    filename TEXT NOT NULL,
    bytes BLOB NOT NULL,
    size INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS attachments_by_message ON attachments(message_id);
CREATE TABLE IF NOT EXISTS memberships(
    channel_id TEXT NOT NULL REFERENCES channels(channel_id),
    agent_id TEXT NOT NULL REFERENCES agents(agent_id),
    read_seq INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (channel_id, agent_id)
);
CREATE TABLE IF NOT EXISTS dm_read(
    message_id TEXT NOT NULL REFERENCES messages(message_id),
    agent_id TEXT NOT NULL REFERENCES agents(agent_id),
    read_at TEXT NOT NULL,
    PRIMARY KEY (message_id, agent_id)
);
CREATE TABLE IF NOT EXISTS reactions(
    message_id TEXT NOT NULL REFERENCES messages(message_id),
    actor_id TEXT NOT NULL REFERENCES agents(agent_id),
    emoji TEXT NOT NULL,
    present INTEGER NOT NULL,
    PRIMARY KEY (message_id, actor_id, emoji)
);
CREATE TABLE IF NOT EXISTS idempotency(
    actor_id TEXT NOT NULL,
    op TEXT NOT NULL,
    key TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    result_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (actor_id, op, key)
);
CREATE TABLE IF NOT EXISTS notifications(
    notification_id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL REFERENCES messages(message_id),
    recipient_agent_id TEXT NOT NULL REFERENCES agents(agent_id),
    kind TEXT NOT NULL CHECK (kind IN ('dm', 'mention', 'post')),
    binding_version INTEGER NOT NULL,
    status TEXT NOT NULL,
    next_attempt_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS notifications_due ON notifications(status, next_attempt_at);
CREATE TABLE IF NOT EXISTS attempts(
    attempt_id TEXT PRIMARY KEY,
    notification_id TEXT NOT NULL REFERENCES notifications(notification_id),
    bridge_instance TEXT NOT NULL,
    target_binding_version INTEGER NOT NULL,
    last_event_seq INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL,
    reason TEXT,
    host_id TEXT,
    text TEXT
);
CREATE INDEX IF NOT EXISTS attempts_by_notification ON attempts(notification_id);
CREATE TABLE IF NOT EXISTS bridge_instances(
    instance TEXT PRIMARY KEY,
    host_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('active', 'fenced')),
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS bridge_instances_by_host ON bridge_instances(host_id, state);
CREATE TABLE IF NOT EXISTS endpoints(
    host_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    target TEXT NOT NULL,
    bridge_instance TEXT NOT NULL,
    seen_at TEXT NOT NULL,
    PRIMARY KEY (host_id, kind, target)
);
CREATE TABLE IF NOT EXISTS identities(
    tid TEXT NOT NULL,
    oid TEXT NOT NULL,
    user_id INTEGER NOT NULL UNIQUE REFERENCES users(user_id),
    display_name TEXT,
    preferred_username TEXT,
    PRIMARY KEY (tid, oid)
);
CREATE TABLE IF NOT EXISTS web_sessions(
    id_digest TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(user_id),
    display TEXT NOT NULL,
    csrf TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    last_seen INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS web_tokens(
    kind TEXT NOT NULL,
    digest TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (kind, digest)
);
-- Local board only (DESIGN-v2.md §4): publications of local agents on shared boards, and
-- one operation record per remote mutation, written before the first remote call.
CREATE TABLE IF NOT EXISTS publications(
    board TEXT NOT NULL,
    agent_id TEXT NOT NULL REFERENCES agents(agent_id),
    shared_name TEXT NOT NULL,
    mailbox_id TEXT,
    route_generation INTEGER,
    state TEXT NOT NULL CHECK (state IN ('publishing', 'published', 'unpublishing')),
    updated_at TEXT NOT NULL,
    PRIMARY KEY (board, agent_id),
    UNIQUE (board, mailbox_id),
    UNIQUE (board, shared_name),
    CHECK ((mailbox_id IS NULL) = (state = 'publishing')),
    CHECK ((route_generation IS NULL) = (mailbox_id IS NULL))
);
CREATE TABLE IF NOT EXISTS board_ops(
    op_id TEXT PRIMARY KEY,
    board TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('publish', 'unpublish', 'migrate')),
    agent_id TEXT,
    request_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'done', 'failed')),
    result_json TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK ((agent_id IS NULL) = (kind = 'migrate'))
);
CREATE UNIQUE INDEX IF NOT EXISTS board_ops_one_pending ON board_ops(board, agent_id) WHERE state = 'pending' AND agent_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS board_ops_one_migrate ON board_ops(board) WHERE kind = 'migrate' AND state <> 'failed';
