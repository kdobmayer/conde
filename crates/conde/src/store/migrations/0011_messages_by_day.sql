-- A dated search starts at the first message of its day (first_seq_from_day): an index on the
-- day and the sequence finds it without walking every message.
CREATE INDEX messages_by_day ON messages(substr(created_at, 1, 10), commit_seq);
