-- Limit startup interruption updates to rows that can actually be interrupted.
CREATE INDEX model_attempt_active_recovery
ON model_attempt(state)
WHERE state IN ('preparing','inflight');
INSERT INTO schema_migration(version,name) VALUES(29,'0029_active_attempt_recovery_index');
PRAGMA user_version=29;
