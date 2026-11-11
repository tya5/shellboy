CREATE INDEX receipt_reply_node_lookup ON receipt(
 target_thread_id,
 (CASE WHEN json_valid(result_json) THEN
    CASE WHEN json_type(result_json,'$.reply.node_id')='text'
      THEN json_extract(result_json,'$.reply.node_id') END
  END)
);
INSERT INTO schema_migration(version,name) VALUES(41,'0041_receipt_reply_node_lookup');
PRAGMA user_version=41;
