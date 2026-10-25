-- Consumed observations leave this partial index; ordinary requests do not scan
-- the complete history of context changes.
CREATE INDEX context_pending_by_thread ON event(target_thread_id,created_at,id)
 WHERE kind='context_changed' AND target_run_id IS NULL;
CREATE INDEX turns_by_thread ON turn(thread_id,created_at);
CREATE INDEX turns_by_thread_end ON turn(thread_id,ended_at) WHERE ended_at IS NOT NULL;
INSERT INTO schema_migration(version,name) VALUES(3,'0003_context_indexes');
PRAGMA user_version=3;
