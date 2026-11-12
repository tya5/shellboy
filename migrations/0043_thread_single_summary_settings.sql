ALTER TABLE thread ADD COLUMN auto_compact_target_percent INTEGER CHECK(auto_compact_target_percent IS NULL OR (typeof(auto_compact_target_percent)='integer' AND auto_compact_target_percent BETWEEN 1 AND 100));
ALTER TABLE thread ADD COLUMN summary_max_chars INTEGER CHECK(summary_max_chars IS NULL OR (typeof(summary_max_chars)='integer' AND summary_max_chars BETWEEN 1 AND 9223372036854775807));
INSERT INTO schema_migration(version,name) VALUES(43,'0043_thread_single_summary_settings');
PRAGMA user_version=43;
