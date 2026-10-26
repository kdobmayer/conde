-- Human paging: the owner's sign-in email, the Slack member id with where it came from
-- (env import, lookup by email, or the admin), whether the human takes "work done" pages, and
-- a page's kind on its message.
ALTER TABLE users ADD COLUMN email TEXT COLLATE "C";
ALTER TABLE users ADD COLUMN slack_member_id TEXT COLLATE "C";
ALTER TABLE users ADD COLUMN slack_source TEXT COLLATE "C" CHECK (slack_source IN ('env', 'lookup', 'admin'));
ALTER TABLE users ADD COLUMN done_pages BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE messages ADD COLUMN page TEXT COLLATE "C" CHECK (page IN ('blocked', 'done'));
