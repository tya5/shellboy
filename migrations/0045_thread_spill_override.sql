ALTER TABLE thread ADD COLUMN spill_override INTEGER CHECK(spill_override IS NULL OR (typeof(spill_override)='integer' AND spill_override IN (0,1)));
INSERT INTO schema_migration(version,name) VALUES(45,'0045_thread_spill_override');
PRAGMA user_version=45;
