CREATE INDEX receipt_other_reply_pending ON receipt(target_thread_id)
 WHERE json_extract(result_json,'$.reply.state')='pending'
   AND COALESCE(json_extract(result_json,'$.to_human'),0)<>1;
CREATE INDEX threads_updated_seek ON thread(updated_at DESC,id DESC) WHERE deleted_at IS NULL;
INSERT INTO schema_migration(version,name) VALUES(63,'next_thread_reply_summary');
PRAGMA user_version=63;
