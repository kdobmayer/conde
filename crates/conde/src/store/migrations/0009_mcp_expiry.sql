-- The purge on every refresh and the hourly sweep delete by expiry: index what they scan.
CREATE INDEX mcp_refresh_unconsumed_by_expiry ON mcp_refresh(expires_at) WHERE consumed_at IS NULL;
CREATE INDEX mcp_tokens_by_expiry ON mcp_tokens(expires_at);
