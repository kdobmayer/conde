-- fleet.list's delivery health joins only the attempts that reached their agent, never a notification's retries.
CREATE INDEX attempts_reached ON attempts(notification_id) WHERE status IN ('delivered', 'accepted');
