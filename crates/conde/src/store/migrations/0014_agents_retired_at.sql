-- conde retire: a name its owner retired (unbound, kept for its history) leaves the owner's lists
-- and the directory; binding it again clears this.
ALTER TABLE agents ADD COLUMN retired_at TEXT COLLATE "C";
