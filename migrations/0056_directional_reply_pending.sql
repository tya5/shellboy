UPDATE receipt SET result_json=json_set(result_json,'$.ask_name','human')
 WHERE json_extract(result_json,'$.to_human')=1;
CREATE INDEX receipt_named_reply_pending ON receipt(json_extract(result_json,'$.ask_name'),target_thread_id,created_at)
 WHERE json_extract(result_json,'$.reply.state')='pending';
CREATE INDEX receipt_pending_by_time ON receipt(created_at,id)
 WHERE json_extract(result_json,'$.reply.state')='pending';
CREATE INDEX receipt_pending_owed ON receipt(target_thread_id,created_at,id)
 WHERE json_extract(result_json,'$.reply.state')='pending' AND json_extract(result_json,'$.ask_name') IS NULL;
CREATE INDEX receipt_pending_asked ON receipt(json_extract(sender_json,'$.thread_id'),created_at,id)
 WHERE json_extract(result_json,'$.reply.state')='pending' AND json_extract(sender_json,'$.kind')='model';
CREATE INDEX receipt_pending_to ON receipt(json_extract(result_json,'$.ask_name'),created_at,id)
 WHERE json_extract(result_json,'$.reply.state')='pending';
INSERT INTO schema_migration(version,name) VALUES(56,'0056_directional_reply_pending');
PRAGMA user_version=56;
