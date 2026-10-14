-- Browser sign-in (Entra ID) and disabled users.
ALTER TABLE users ADD COLUMN disabled BOOLEAN NOT NULL DEFAULT FALSE;
CREATE TABLE identities(
    tid TEXT COLLATE "C" NOT NULL,
    oid TEXT COLLATE "C" NOT NULL,
    user_id BIGINT NOT NULL UNIQUE REFERENCES users(user_id),
    display_name TEXT,
    preferred_username TEXT,
    PRIMARY KEY (tid, oid)
);
CREATE TABLE web_sessions(
    id_digest TEXT COLLATE "C" PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(user_id),
    display TEXT NOT NULL,
    csrf TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    last_seen BIGINT NOT NULL
);
CREATE TABLE web_tokens(
    kind TEXT COLLATE "C" NOT NULL,
    digest TEXT COLLATE "C" NOT NULL,
    payload TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    PRIMARY KEY (kind, digest)
);
