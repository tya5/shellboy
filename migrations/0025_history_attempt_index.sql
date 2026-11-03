-- Preserve the existing execution-link relation while seeking groups by attempt.
CREATE INDEX IF NOT EXISTS history_tool_group_attempt ON tool_group(attempt_id);
INSERT INTO schema_migration(version,name) VALUES(25,'0025_history_attempt_index');
PRAGMA user_version=25;
