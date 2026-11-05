-- A person's agent machine (one per person): the deployment the server issued, the mint, the
-- startup exchange, the sign-in state the bridge reports, the one live sign-in relay and the
-- disable route's cleanup. Times are ISO text; principal_id (the VM identity's oid) is the mint key.
CREATE TABLE machines(
    user_id BIGINT PRIMARY KEY REFERENCES users(user_id),
    host_id TEXT COLLATE "C" NOT NULL UNIQUE,
    owner_object_id TEXT NOT NULL,
    vm_size TEXT NOT NULL,
    requested_at TEXT COLLATE "C",
    deploy_state TEXT,
    deploy_checked_at TEXT COLLATE "C",
    deploy_error TEXT,
    principal_id TEXT COLLATE "C" UNIQUE,
    minted_at TEXT COLLATE "C",
    probe_id TEXT COLLATE "C",
    ready_at TEXT COLLATE "C",
    session_online_at TEXT COLLATE "C",
    sign_in TEXT,
    relay_id TEXT COLLATE "C",
    relay_flow TEXT CHECK (relay_flow IN ('claude', 'github', 'codex')),
    relay_state TEXT CHECK (relay_state IN ('starting', 'url', 'waiting', 'done', 'failed', 'cancelled')),
    relay_url TEXT,
    relay_user_code TEXT,
    relay_expires_at TEXT COLLATE "C",
    relay_reason TEXT,
    relay_at TEXT COLLATE "C",
    pending_action TEXT CHECK (pending_action IN ('start', 'code', 'cancel')),
    pending_code TEXT,
    deleting_at TEXT COLLATE "C",
    delete_error TEXT
);
