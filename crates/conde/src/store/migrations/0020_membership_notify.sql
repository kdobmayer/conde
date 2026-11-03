-- Per membership: whether the member is pushed every post of the channel ('all') or only DMs,
-- mentions and humans' posts ('mentions', the behaviour before this setting).
ALTER TABLE memberships ADD COLUMN notify TEXT COLLATE "C" NOT NULL DEFAULT 'mentions'
    CHECK (notify IN ('mentions', 'all'));
