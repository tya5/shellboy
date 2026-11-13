ALTER TABLE thread ADD COLUMN max_tokens INTEGER CHECK(max_tokens IS NULL OR (typeof(max_tokens)='integer' AND max_tokens>=1));
INSERT INTO schema_migration(version,name) VALUES(44,'0044_thread_max_tokens');
PRAGMA user_version=44;
