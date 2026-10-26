-- The server's Slack sends (bridge_instance 'slack'): lease end while sending, retry time (ms) after a failure.
ALTER TABLE attempts ADD COLUMN not_before BIGINT;
