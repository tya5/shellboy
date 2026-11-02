-- Exact activity membership must be seekable before any display limits.
CREATE INDEX IF NOT EXISTS activity_attempt_turn ON model_attempt(turn_id);
CREATE INDEX IF NOT EXISTS activity_turn_run ON turn(run_id,created_at,id);
CREATE INDEX IF NOT EXISTS activity_turn_run_state ON turn(run_id,state);
CREATE INDEX IF NOT EXISTS activity_group_turn_state ON tool_group(turn_id,state);
CREATE INDEX IF NOT EXISTS activity_shell_spawned_turn ON shell_execution(turn_id) WHERE CASE WHEN json_valid(shell_json) THEN json_extract(shell_json,'$.spawned')=1 ELSE 1 END;
CREATE INDEX IF NOT EXISTS activity_shell_uncollected_owner ON shell_execution(owner_run_id) WHERE state IN ('exited','failed','stopped') AND output_complete=0;
INSERT INTO schema_migration(version,name) VALUES(23,'0023_activity_bounded');
PRAGMA user_version=23;
