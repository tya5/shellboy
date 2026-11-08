-- The cross-Thread human inbox must not scan Thread-directed pending receipts.
CREATE INDEX receipt_human_reply_pending ON receipt(target_thread_id,created_at)
  WHERE json_extract(result_json,'$.reply.state')='pending'
    AND json_extract(result_json,'$.to_human')=1;
INSERT INTO schema_migration(version,name) VALUES(38,'0038_human_reply_pending');
PRAGMA user_version=38;
