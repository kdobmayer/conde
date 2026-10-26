-- owner.feed reads an agent's sent DMs newest first (store/mod.rs feed_seqs_sql): messages_by_sender
-- also holds its channel posts, so an agent that mostly posts would be walked whole for a few DMs.
CREATE INDEX messages_dm_by_sender ON messages(sender_agent_id, commit_seq) WHERE channel_id IS NULL;
