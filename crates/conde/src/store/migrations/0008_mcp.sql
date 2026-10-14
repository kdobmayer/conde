-- Remote MCP (docs/REMOTE-MCP.md §3): a hosted client's grants, their access tokens, each
-- grant's refresh-token family (every issued refresh token, consumed on rotation, so a replay
-- is recognised) and one-time authorization codes. Times are unix seconds; secrets digests only.
CREATE TABLE mcp_grants(
    grant_id TEXT COLLATE "C" PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(user_id),
    client_id TEXT NOT NULL,
    client_host TEXT NOT NULL,
    client_name TEXT,
    resource TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    last_used_at BIGINT,
    expires_at BIGINT NOT NULL,
    revoked_at BIGINT
);
CREATE INDEX mcp_grants_by_user ON mcp_grants(user_id);
CREATE TABLE mcp_tokens(
    token_digest TEXT COLLATE "C" PRIMARY KEY,
    grant_id TEXT COLLATE "C" NOT NULL REFERENCES mcp_grants(grant_id) ON DELETE CASCADE,
    audience TEXT NOT NULL,
    expires_at BIGINT NOT NULL
);
CREATE INDEX mcp_tokens_by_grant ON mcp_tokens(grant_id);
CREATE TABLE mcp_refresh(
    digest TEXT COLLATE "C" PRIMARY KEY,
    grant_id TEXT COLLATE "C" NOT NULL REFERENCES mcp_grants(grant_id) ON DELETE CASCADE,
    issued_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    consumed_at BIGINT
);
CREATE INDEX mcp_refresh_by_grant ON mcp_refresh(grant_id);
CREATE TABLE mcp_codes(
    code_digest TEXT COLLATE "C" PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(user_id),
    client_id TEXT NOT NULL,
    client_name TEXT,
    redirect_uri TEXT NOT NULL,
    challenge TEXT NOT NULL,
    resource TEXT NOT NULL,
    created_at BIGINT NOT NULL
);
