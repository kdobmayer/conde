-- A second use of an authorization code revokes the grant minted from it (RFC 6749 §4.1.2):
-- each grant keeps the digest of its code.
ALTER TABLE mcp_grants ADD COLUMN code_digest TEXT COLLATE "C";
CREATE UNIQUE INDEX mcp_grants_by_code ON mcp_grants(code_digest) WHERE code_digest IS NOT NULL;
