-- Derived access paths only; preserve rows, retention and state semantics.
CREATE INDEX shell_active_owner ON shell_execution(owner_run_id,state)
 WHERE state IN ('starting','running','stopping');
CREATE INDEX run_thread_created ON run(thread_id,created_at DESC,id DESC);
CREATE INDEX shell_terminal_time ON shell_execution(COALESCE(ended_at,started_at))
 WHERE state IN ('exited','failed','stopped');
INSERT INTO schema_migration(version,name) VALUES(10,'0010_hot_query_indexes');
PRAGMA user_version=10;
