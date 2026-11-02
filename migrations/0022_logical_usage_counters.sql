-- Logical usage counters and receipt-collection index (#171).
-- Exact byte counters maintained by triggers in the writing transaction, so the
-- capacity gate and GC read O(1) totals instead of O(rows) index sums. The
-- partial index lets receipt collection seek pending attachment deliveries.
-- IF NOT EXISTS / OR REPLACE keep downgrade fixtures simple; counters are
-- recomputed exactly from the saved rows.
-- Targeted Run-end event retention (#170/#171): one Run's shell events by seek.
CREATE INDEX IF NOT EXISTS event_run_shell_ended ON event(target_run_id) WHERE kind='shell_ended';
CREATE INDEX IF NOT EXISTS attachment_delivery_operation ON attachment_delivery(operation_id) WHERE status IN ('accepted','held');
INSERT OR REPLACE INTO quota_counter VALUES('logical.history',
 (SELECT COALESCE(SUM(octet_length(payload_json)),0) FROM node)
 +(SELECT COALESCE(SUM(octet_length(payload_json)),0) FROM output_item)
 +(SELECT COALESCE(SUM(octet_length(body)),0) FROM instruction_snapshot)
 +(SELECT COALESCE(SUM(octet_length(body_json)),0) FROM draft)
 +(SELECT COALESCE(SUM(COALESCE(octet_length(input_manifest_json),0)+COALESCE(octet_length(error_json),0)+COALESCE(octet_length(usage_json),0)),0) FROM model_attempt)
 +(SELECT COALESCE(SUM(octet_length(payload_ref_json)+octet_length(source_json)),0) FROM event)
 +(SELECT COALESCE(SUM(octet_length(sender_json)+COALESCE(octet_length(result_json),0)),0) FROM receipt)
 +(SELECT COALESCE(SUM(octet_length(origin_json)+COALESCE(octet_length(result_ref_json),0)),0) FROM operation_receipt)
 +(SELECT COALESCE(SUM(octet_length(arguments_json)),0) FROM tool_call)
 +(SELECT COALESCE(SUM(octet_length(value)),0) FROM app_meta WHERE key LIKE 'summary.part.%' OR key LIKE 'wait.pending.%' OR key LIKE 'compact.result.%' OR key LIKE 'revisit.%' OR key LIKE 'summary.usage.%'),1);
INSERT OR REPLACE INTO quota_counter VALUES('logical.raw',(SELECT COALESCE(SUM(length(bytes)),0) FROM output_chunk),1);
INSERT OR REPLACE INTO quota_counter VALUES('logical.managed',(SELECT COALESCE(SUM(raw_size),0) FROM managed_file WHERE state<>'deleted'),1);
INSERT OR REPLACE INTO quota_counter VALUES('logical.index',(SELECT COALESCE(SUM(s.dimension*4),0) FROM embedding_chunk c JOIN index_space s ON s.id=c.space_id)+(SELECT COALESCE(SUM(octet_length(value)),0) FROM app_meta WHERE key LIKE 'index.%'),1);
CREATE TRIGGER IF NOT EXISTS logical_history_node_insert AFTER INSERT ON node BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.payload_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_node_delete AFTER DELETE ON node BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+-(COALESCE(octet_length(OLD.payload_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_node_update AFTER UPDATE OF payload_json ON node BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.payload_json),0))-(COALESCE(octet_length(OLD.payload_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_output_item_insert AFTER INSERT ON output_item BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.payload_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_output_item_delete AFTER DELETE ON output_item BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+-(COALESCE(octet_length(OLD.payload_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_output_item_update AFTER UPDATE OF payload_json ON output_item BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.payload_json),0))-(COALESCE(octet_length(OLD.payload_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_instruction_snapshot_insert AFTER INSERT ON instruction_snapshot BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.body),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_instruction_snapshot_delete AFTER DELETE ON instruction_snapshot BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+-(COALESCE(octet_length(OLD.body),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_instruction_snapshot_update AFTER UPDATE OF body ON instruction_snapshot BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.body),0))-(COALESCE(octet_length(OLD.body),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_draft_insert AFTER INSERT ON draft BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.body_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_draft_delete AFTER DELETE ON draft BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+-(COALESCE(octet_length(OLD.body_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_draft_update AFTER UPDATE OF body_json ON draft BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.body_json),0))-(COALESCE(octet_length(OLD.body_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_model_attempt_insert AFTER INSERT ON model_attempt BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.input_manifest_json),0)+COALESCE(octet_length(NEW.error_json),0)+COALESCE(octet_length(NEW.usage_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_model_attempt_delete AFTER DELETE ON model_attempt BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+-(COALESCE(octet_length(OLD.input_manifest_json),0)+COALESCE(octet_length(OLD.error_json),0)+COALESCE(octet_length(OLD.usage_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_model_attempt_update AFTER UPDATE OF input_manifest_json,error_json,usage_json ON model_attempt BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.input_manifest_json),0)+COALESCE(octet_length(NEW.error_json),0)+COALESCE(octet_length(NEW.usage_json),0))-(COALESCE(octet_length(OLD.input_manifest_json),0)+COALESCE(octet_length(OLD.error_json),0)+COALESCE(octet_length(OLD.usage_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_event_insert AFTER INSERT ON event BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.payload_ref_json)+octet_length(NEW.source_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_event_delete AFTER DELETE ON event BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+-(COALESCE(octet_length(OLD.payload_ref_json)+octet_length(OLD.source_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_event_update AFTER UPDATE OF payload_ref_json,source_json ON event BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.payload_ref_json)+octet_length(NEW.source_json),0))-(COALESCE(octet_length(OLD.payload_ref_json)+octet_length(OLD.source_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_receipt_insert AFTER INSERT ON receipt BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.sender_json)+COALESCE(octet_length(NEW.result_json),0),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_receipt_delete AFTER DELETE ON receipt BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+-(COALESCE(octet_length(OLD.sender_json)+COALESCE(octet_length(OLD.result_json),0),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_receipt_update AFTER UPDATE OF sender_json,result_json ON receipt BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.sender_json)+COALESCE(octet_length(NEW.result_json),0),0))-(COALESCE(octet_length(OLD.sender_json)+COALESCE(octet_length(OLD.result_json),0),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_operation_receipt_insert AFTER INSERT ON operation_receipt BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.origin_json)+COALESCE(octet_length(NEW.result_ref_json),0),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_operation_receipt_delete AFTER DELETE ON operation_receipt BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+-(COALESCE(octet_length(OLD.origin_json)+COALESCE(octet_length(OLD.result_ref_json),0),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_operation_receipt_update AFTER UPDATE OF origin_json,result_ref_json ON operation_receipt BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.origin_json)+COALESCE(octet_length(NEW.result_ref_json),0),0))-(COALESCE(octet_length(OLD.origin_json)+COALESCE(octet_length(OLD.result_ref_json),0),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_tool_call_insert AFTER INSERT ON tool_call BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.arguments_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_tool_call_delete AFTER DELETE ON tool_call BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+-(COALESCE(octet_length(OLD.arguments_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_tool_call_update AFTER UPDATE OF arguments_json ON tool_call BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.arguments_json),0))-(COALESCE(octet_length(OLD.arguments_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_app_meta_insert AFTER INSERT ON app_meta BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(CASE WHEN (NEW.key LIKE 'summary.part.%' OR NEW.key LIKE 'wait.pending.%' OR NEW.key LIKE 'compact.result.%' OR NEW.key LIKE 'revisit.%' OR NEW.key LIKE 'summary.usage.%') THEN COALESCE(octet_length(NEW.value),0) ELSE 0 END),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_app_meta_delete AFTER DELETE ON app_meta BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+-(CASE WHEN (OLD.key LIKE 'summary.part.%' OR OLD.key LIKE 'wait.pending.%' OR OLD.key LIKE 'compact.result.%' OR OLD.key LIKE 'revisit.%' OR OLD.key LIKE 'summary.usage.%') THEN COALESCE(octet_length(OLD.value),0) ELSE 0 END),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_history_app_meta_update AFTER UPDATE OF key,value ON app_meta BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(CASE WHEN (NEW.key LIKE 'summary.part.%' OR NEW.key LIKE 'wait.pending.%' OR NEW.key LIKE 'compact.result.%' OR NEW.key LIKE 'revisit.%' OR NEW.key LIKE 'summary.usage.%') THEN COALESCE(octet_length(NEW.value),0) ELSE 0 END)-(CASE WHEN (OLD.key LIKE 'summary.part.%' OR OLD.key LIKE 'wait.pending.%' OR OLD.key LIKE 'compact.result.%' OR OLD.key LIKE 'revisit.%' OR OLD.key LIKE 'summary.usage.%') THEN COALESCE(octet_length(OLD.value),0) ELSE 0 END),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER IF NOT EXISTS logical_raw_output_chunk_insert AFTER INSERT ON output_chunk BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(length(NEW.bytes),0)),revision=revision+1 WHERE category='logical.raw';
END;
CREATE TRIGGER IF NOT EXISTS logical_raw_output_chunk_delete AFTER DELETE ON output_chunk BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+-(COALESCE(length(OLD.bytes),0)),revision=revision+1 WHERE category='logical.raw';
END;
CREATE TRIGGER IF NOT EXISTS logical_raw_output_chunk_update AFTER UPDATE OF bytes ON output_chunk BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(length(NEW.bytes),0))-(COALESCE(length(OLD.bytes),0)),revision=revision+1 WHERE category='logical.raw';
END;
CREATE TRIGGER IF NOT EXISTS logical_managed_managed_file_insert AFTER INSERT ON managed_file BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(CASE WHEN NEW.state<>'deleted' THEN COALESCE(NEW.raw_size,0) ELSE 0 END),revision=revision+1 WHERE category='logical.managed';
END;
CREATE TRIGGER IF NOT EXISTS logical_managed_managed_file_delete AFTER DELETE ON managed_file BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+-(CASE WHEN OLD.state<>'deleted' THEN COALESCE(OLD.raw_size,0) ELSE 0 END),revision=revision+1 WHERE category='logical.managed';
END;
CREATE TRIGGER IF NOT EXISTS logical_managed_managed_file_update AFTER UPDATE OF raw_size,state ON managed_file BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(CASE WHEN NEW.state<>'deleted' THEN COALESCE(NEW.raw_size,0) ELSE 0 END)-(CASE WHEN OLD.state<>'deleted' THEN COALESCE(OLD.raw_size,0) ELSE 0 END),revision=revision+1 WHERE category='logical.managed';
END;
CREATE TRIGGER IF NOT EXISTS logical_index_app_meta_insert AFTER INSERT ON app_meta BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(CASE WHEN NEW.key LIKE 'index.%' THEN COALESCE(octet_length(NEW.value),0) ELSE 0 END),revision=revision+1 WHERE category='logical.index';
END;
CREATE TRIGGER IF NOT EXISTS logical_index_app_meta_delete AFTER DELETE ON app_meta BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+-(CASE WHEN OLD.key LIKE 'index.%' THEN COALESCE(octet_length(OLD.value),0) ELSE 0 END),revision=revision+1 WHERE category='logical.index';
END;
CREATE TRIGGER IF NOT EXISTS logical_index_app_meta_update AFTER UPDATE OF key,value ON app_meta BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(CASE WHEN NEW.key LIKE 'index.%' THEN COALESCE(octet_length(NEW.value),0) ELSE 0 END)-(CASE WHEN OLD.key LIKE 'index.%' THEN COALESCE(octet_length(OLD.value),0) ELSE 0 END),revision=revision+1 WHERE category='logical.index';
END;
CREATE TRIGGER IF NOT EXISTS logical_index_embedding_chunk_insert AFTER INSERT ON embedding_chunk BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE((SELECT s.dimension*4 FROM index_space s WHERE s.id=NEW.space_id),0)),revision=revision+1 WHERE category='logical.index';
END;
CREATE TRIGGER IF NOT EXISTS logical_index_embedding_chunk_delete AFTER DELETE ON embedding_chunk BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+-(COALESCE((SELECT s.dimension*4 FROM index_space s WHERE s.id=OLD.space_id),0)),revision=revision+1 WHERE category='logical.index';
END;
CREATE TRIGGER IF NOT EXISTS logical_index_embedding_chunk_update AFTER UPDATE OF space_id ON embedding_chunk BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE((SELECT s.dimension*4 FROM index_space s WHERE s.id=NEW.space_id),0))-(COALESCE((SELECT s.dimension*4 FROM index_space s WHERE s.id=OLD.space_id),0)),revision=revision+1 WHERE category='logical.index';
END;
CREATE TRIGGER IF NOT EXISTS logical_index_space_insert AFTER INSERT ON index_space BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(SELECT count(*) FROM embedding_chunk c WHERE c.space_id=NEW.id)*NEW.dimension*4,revision=revision+1 WHERE category='logical.index';
END;
CREATE TRIGGER IF NOT EXISTS logical_index_space_delete AFTER DELETE ON index_space BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes-(SELECT count(*) FROM embedding_chunk c WHERE c.space_id=OLD.id)*OLD.dimension*4,revision=revision+1 WHERE category='logical.index';
END;
CREATE TRIGGER IF NOT EXISTS logical_index_space_update AFTER UPDATE OF id,dimension ON index_space BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(SELECT count(*) FROM embedding_chunk c WHERE c.space_id=NEW.id)*NEW.dimension*4-(SELECT count(*) FROM embedding_chunk c WHERE c.space_id=OLD.id)*OLD.dimension*4,revision=revision+1 WHERE category='logical.index';
END;
INSERT INTO schema_migration(version,name) VALUES(22,'0022_logical_usage_counters');
PRAGMA user_version=22;
