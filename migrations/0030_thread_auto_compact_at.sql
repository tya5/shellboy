-- Per-Thread override of the proactive compaction threshold (#229). NULL uses the connection/[compaction] value; 0 disables.
ALTER TABLE thread ADD COLUMN auto_compact_at INTEGER CHECK(auto_compact_at IS NULL OR auto_compact_at>=0);
INSERT INTO schema_migration(version,name) VALUES(30,'0030_thread_auto_compact_at');
PRAGMA user_version=30;
