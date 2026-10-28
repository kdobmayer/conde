-- The realm account an invite created for the user (password workspaces) and the correlation
-- value the invite sent as its `conde_invite` attribute: a retry reuses the account, or after a
-- lost create answer adopts only the account that carries the value.
ALTER TABLE users ADD COLUMN keycloak_id TEXT COLLATE "C";
ALTER TABLE users ADD COLUMN keycloak_invite TEXT COLLATE "C";
