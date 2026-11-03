CREATE INDEX receipt_reply_pending ON receipt(target_thread_id)
  WHERE json_extract(result_json,'$.reply.state')='pending';
INSERT INTO schema_migration(version,name) VALUES(26,'0026_reply_expected');
PRAGMA user_version=26;
