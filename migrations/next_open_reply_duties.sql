-- Reply duties are open while pending or awaiting_own_ask (#433). Open-duty reads
-- stay on state-qualified indexes covering both open states; Run-end settlement
-- seeks a pending duty by its owner Run: its delivery run_id, or for a duty
-- reopened after an own ask (reply.reopened_after) the Run that admitted the
-- answer (reply.owner_run_id; none until then). An outcome notice stays
-- untargeted until a Run admits it; reply_outcome_unadmitted offers it to the
-- Thread's next runnable Turn.
DROP INDEX receipt_reply_pending;
CREATE INDEX receipt_reply_pending ON receipt(target_thread_id)
 WHERE json_extract(result_json,'$.reply.state') IN ('pending','awaiting_own_ask');
DROP INDEX receipt_other_reply_pending;
CREATE INDEX receipt_other_reply_pending ON receipt(target_thread_id)
 WHERE json_extract(result_json,'$.reply.state') IN ('pending','awaiting_own_ask')
   AND COALESCE(json_extract(result_json,'$.to_human'),0)<>1;
DROP INDEX receipt_pending_by_time;
CREATE INDEX receipt_pending_by_time ON receipt(created_at,id)
 WHERE json_extract(result_json,'$.reply.state') IN ('pending','awaiting_own_ask');
DROP INDEX receipt_pending_owed;
CREATE INDEX receipt_pending_owed ON receipt(target_thread_id,created_at,id)
 WHERE json_extract(result_json,'$.reply.state') IN ('pending','awaiting_own_ask') AND json_extract(result_json,'$.ask_name') IS NULL;
DROP INDEX receipt_pending_asked;
CREATE INDEX receipt_pending_asked ON receipt(json_extract(sender_json,'$.thread_id'),created_at,id)
 WHERE json_extract(result_json,'$.reply.state') IN ('pending','awaiting_own_ask') AND json_extract(sender_json,'$.kind')='model';
DROP INDEX receipt_pending_to;
CREATE INDEX receipt_pending_to ON receipt(json_extract(result_json,'$.ask_name'),created_at,id)
 WHERE json_extract(result_json,'$.reply.state') IN ('pending','awaiting_own_ask');
DROP INDEX receipt_reply_run_pending;
CREATE INDEX receipt_reply_owner_pending ON receipt(IIF(json_extract(result_json,'$.reply.reopened_after') IS NULL,run_id,json_extract(result_json,'$.reply.owner_run_id')),state)
 WHERE json_extract(result_json,'$.reply.state')='pending' AND json_extract(result_json,'$.to_human') IS NOT 1;
CREATE INDEX reply_outcome_unadmitted ON event(target_thread_id)
 WHERE kind='reply_outcome' AND target_run_id IS NULL;
INSERT INTO schema_migration(version,name) VALUES(67,'next_open_reply_duties');
PRAGMA user_version=67;
