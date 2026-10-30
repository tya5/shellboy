-- Isolated evidence classification; runtime never guesses historical admission.
ALTER TABLE receipt ADD COLUMN path_admission TEXT NOT NULL DEFAULT 'unadmitted' CHECK(path_admission IN ('unadmitted','admitted','unknown'));
UPDATE receipt SET path_admission='unknown';
UPDATE receipt SET path_admission='admitted'
WHERE state='applied'
 OR EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=receipt.target_thread_id AND p.node_id=receipt.message_node_id)
 OR EXISTS(SELECT 1 FROM model_attempt a,json_each(CASE WHEN json_valid(a.input_manifest_json) THEN a.input_manifest_json ELSE '{}' END,'$.receipts') r WHERE r.value=receipt.id);
INSERT INTO schema_migration(version,name) VALUES(13,'0013_message_path_admission');
PRAGMA user_version=13;
