-- The fleet's delivery health reads notifications per recipient.
CREATE INDEX notifications_by_recipient ON notifications(recipient_agent_id);
