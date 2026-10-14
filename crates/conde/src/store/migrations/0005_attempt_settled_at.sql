-- When an attempt's latest outcome was recorded (the fleet's "last reached").
ALTER TABLE attempts ADD COLUMN settled_at TEXT;
