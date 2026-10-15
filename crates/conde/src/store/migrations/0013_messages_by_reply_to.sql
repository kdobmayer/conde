-- owner.thread walks a reply chain down through reply_to (store/mod.rs thread_down_sql): an index
-- on the parent finds a message's replies without reading the board.
CREATE INDEX messages_by_reply_to ON messages(reply_to, commit_seq);
