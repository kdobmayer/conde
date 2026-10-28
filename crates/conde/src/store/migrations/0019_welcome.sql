-- The workspace's admin-set settings (the welcome), the admin role, and when a user first
-- enrolled a host: the welcome runs once per user, so users with a host before it count as arrived.
CREATE TABLE settings(
    key TEXT COLLATE "C" PRIMARY KEY,
    value TEXT NOT NULL
);
ALTER TABLE users ADD COLUMN admin BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE users ADD COLUMN arrived_at TEXT COLLATE "C";
UPDATE users u SET arrived_at = COALESCE(
    (SELECT MIN(c.created_at) FROM device_credentials c WHERE c.user_id = u.user_id), '1970-01-01T00:00:00Z')
WHERE EXISTS (SELECT 1 FROM hosts h WHERE h.user_id = u.user_id);
