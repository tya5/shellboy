-- Derived byte indexes: SQLite maintains them atomically with the source rows.
-- No new persisted counters or body copies; existing payload_bytes is not assumed exact.
CREATE INDEX gc_bytes_node ON node(octet_length(payload_json));
CREATE INDEX gc_bytes_output_item ON output_item(octet_length(payload_json));
CREATE INDEX gc_bytes_instruction_snapshot ON instruction_snapshot(octet_length(body));
CREATE INDEX gc_bytes_draft ON draft(octet_length(body_json));
CREATE INDEX gc_bytes_model_attempt ON model_attempt(COALESCE(octet_length(input_manifest_json),0)+COALESCE(octet_length(error_json),0)+COALESCE(octet_length(usage_json),0));
CREATE INDEX gc_bytes_event ON event(octet_length(payload_ref_json)+octet_length(source_json));
CREATE INDEX gc_bytes_receipt ON receipt(octet_length(sender_json)+COALESCE(octet_length(result_json),0));
CREATE INDEX gc_bytes_operation_receipt ON operation_receipt(octet_length(origin_json)+COALESCE(octet_length(result_ref_json),0));
CREATE INDEX gc_bytes_tool_call ON tool_call(octet_length(arguments_json));
CREATE INDEX gc_bytes_app_meta_history ON app_meta(octet_length(value)) WHERE key LIKE 'summary.part.%' OR key LIKE 'wait.pending.%' OR key LIKE 'compact.result.%';
CREATE INDEX gc_bytes_output_chunk ON output_chunk(length(bytes));
CREATE INDEX gc_bytes_managed_file ON managed_file(raw_size) WHERE state<>'deleted';
CREATE INDEX gc_bytes_app_meta_index ON app_meta(octet_length(value)) WHERE key LIKE 'index.%';
-- Invalid/foreign metadata remains untouched and cannot make index creation fail.
CREATE INDEX index_bootstrap_source ON app_meta(json_extract(value,'$.witness.node_id')) WHERE key LIKE 'index.bootstrap.%' AND json_valid(value);
CREATE INDEX index_auto_source ON app_meta(json_extract(value,'$.node_id')) WHERE key LIKE 'index.auto.%' AND json_valid(value);
INSERT INTO schema_migration(version,name) VALUES(7,'0007_usage_and_source_indexes');
PRAGMA user_version=7;
