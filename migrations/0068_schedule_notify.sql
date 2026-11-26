-- A timed schedule's opt-in owner notices (#449): a JSON array of notice kinds,
-- empty by default. `skipped` sends one message per skipped occurrence.
ALTER TABLE schedule ADD COLUMN notify_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(notify_json) AND json_type(notify_json)='array');
INSERT INTO schema_migration(version,name) VALUES(68,'0068_schedule_notify');
PRAGMA user_version=68;
