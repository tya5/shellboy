CREATE INDEX receipt_other_reply_pending ON receipt(target_thread_id)
 WHERE json_extract(result_json,'$.reply.state')='pending'
   AND COALESCE(json_extract(result_json,'$.to_human'),0)<>1;
CREATE INDEX threads_updated_seek ON thread(updated_at DESC,id DESC) WHERE deleted_at IS NULL;

-- Default status drives each source from pending work, never terminal history.
CREATE INDEX status_attempt_pending ON model_attempt(turn_id) WHERE state NOT IN ('completed','failed','incomplete','cancelled');
CREATE INDEX status_message_pending ON receipt(target_thread_id,run_id) WHERE state NOT IN ('applied','cancelled');
CREATE INDEX status_run_pending ON run(thread_id) WHERE state NOT IN ('completed','stopped','failed');
CREATE INDEX status_shell_pending ON shell_execution(owner_run_id) WHERE state NOT IN ('exited','failed','stopped');
CREATE INDEX status_turn_pending ON turn(thread_id,run_id) WHERE state NOT IN ('completed','stopped','failed');
INSERT INTO schema_migration(version,name) VALUES(63,'0063_thread_reply_summary');
PRAGMA user_version=63;
