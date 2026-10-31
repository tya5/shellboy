-- Implicit trailing rowid provides a durable insertion-order range seek.
CREATE INDEX wait_run_state ON run(thread_id,state);
CREATE INDEX wait_recovery_thread ON event(json_extract(payload_ref_json,'$.thread_id')) WHERE kind='child_recovery_wait';
CREATE INDEX thread_answer_latest ON turn(thread_id) WHERE answer_node_id IS NOT NULL;
CREATE INDEX receipt_thread_order ON receipt(target_thread_id);
INSERT INTO schema_migration(version,name) VALUES(16,'0016_thread_observation');
PRAGMA user_version=16;
