-- Current conversation schema v10. Historical upgrades live in storage::migrate.
-- Keep this definition equivalent to the completed historical migrations.

CREATE TABLE app_meta(key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL);

CREATE TABLE thread(
 id TEXT PRIMARY KEY NOT NULL, parent_thread_id TEXT REFERENCES thread(id),
 title TEXT NOT NULL DEFAULT '', cwd TEXT NOT NULL, agents_path TEXT NOT NULL,
 profile_id TEXT NOT NULL, model_id TEXT NOT NULL, effort TEXT,
 revision INTEGER NOT NULL DEFAULT 0 CHECK(revision>=0), reasoning_epoch INTEGER NOT NULL DEFAULT 0 CHECK(reasoning_epoch>=0),
 cursor_node_id TEXT REFERENCES node(id), deleted_at TEXT, updated_at TEXT NOT NULL,
 selection_revision INTEGER NOT NULL DEFAULT 0 CHECK(selection_revision>=0),
 profile_revision INTEGER NOT NULL DEFAULT 0 CHECK(profile_revision>=0));

CREATE INDEX threads_updated ON thread(deleted_at,updated_at DESC,id);

CREATE TABLE thread_path(
 thread_id TEXT NOT NULL REFERENCES thread(id), ordinal INTEGER NOT NULL CHECK(ordinal>=0),
 node_id TEXT NOT NULL REFERENCES node(id), PRIMARY KEY(thread_id,ordinal), UNIQUE(thread_id,node_id));

CREATE INDEX thread_path_node ON thread_path(node_id,thread_id);

CREATE TABLE run(
 id TEXT PRIMARY KEY NOT NULL, thread_id TEXT NOT NULL REFERENCES thread(id),
 owner_run_id TEXT REFERENCES run(id), state TEXT NOT NULL
 CHECK(state IN ('live','stopping','completed','stopped','failed')),
 created_at TEXT NOT NULL, ended_at TEXT, stop_reason TEXT,
 CHECK(owner_run_id IS NULL OR owner_run_id<>id), UNIQUE(id,thread_id));

CREATE UNIQUE INDEX one_live_run ON run(thread_id) WHERE state IN ('live','stopping');

CREATE INDEX runs_by_owner ON run(owner_run_id,state);

CREATE TABLE turn(
 id TEXT PRIMARY KEY NOT NULL, thread_id TEXT NOT NULL REFERENCES thread(id), run_id TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('preparing','running','waiting','recovery_wait','completed','stopped','failed')),
 generation INTEGER NOT NULL DEFAULT 0 CHECK(generation>=0), instruction_snapshot_id TEXT REFERENCES instruction_snapshot(id),
 answer_node_id TEXT REFERENCES node(id), created_at TEXT NOT NULL, ended_at TEXT,
 recovery_reason_json TEXT CHECK(recovery_reason_json IS NULL OR json_valid(recovery_reason_json)),
 FOREIGN KEY(run_id,thread_id) REFERENCES run(id,thread_id));

CREATE UNIQUE INDEX one_open_turn ON turn(thread_id)
 WHERE state IN ('preparing','running','waiting','recovery_wait');

CREATE INDEX turns_by_run ON turn(run_id,created_at);

CREATE TABLE node(
 id TEXT PRIMARY KEY NOT NULL, turn_id TEXT REFERENCES turn(id), kind TEXT NOT NULL,
 created_at TEXT NOT NULL, payload_revision INTEGER NOT NULL DEFAULT 0 CHECK(payload_revision>=0),
 payload_json TEXT CHECK(payload_json IS NULL OR json_valid(payload_json)), payload_bytes INTEGER NOT NULL DEFAULT 0 CHECK(payload_bytes>=0),
 body_hash TEXT, deleted_reason TEXT);

CREATE TABLE edge(
 src TEXT NOT NULL REFERENCES node(id), dst TEXT NOT NULL REFERENCES node(id),
 PRIMARY KEY(src,dst), CHECK(src<>dst));

CREATE INDEX edge_reverse ON edge(dst,src);

CREATE TRIGGER reject_edge_cycle BEFORE INSERT ON edge
 WHEN EXISTS(
 WITH RECURSIVE reached(id) AS(
 SELECT NEW.dst UNION SELECT e.dst FROM edge e JOIN reached r ON e.src=r.id)
 SELECT 1 FROM reached WHERE id=NEW.src)
 BEGIN SELECT RAISE(ABORT,'conversation edge cycle'); END;

CREATE TRIGGER reject_edge_update_cycle BEFORE UPDATE OF src,dst ON edge
 WHEN EXISTS(
 WITH RECURSIVE reached(id) AS(
 SELECT NEW.dst UNION SELECT e.dst FROM edge e JOIN reached r ON e.src=r.id
 WHERE NOT(e.src=OLD.src AND e.dst=OLD.dst))
 SELECT 1 FROM reached WHERE id=NEW.src)
 BEGIN SELECT RAISE(ABORT,'conversation edge cycle'); END;

CREATE TRIGGER reject_owner_cycle_insert BEFORE INSERT ON run
 WHEN NEW.owner_run_id IS NOT NULL AND EXISTS(
 WITH RECURSIVE owners(id) AS(
 SELECT NEW.owner_run_id UNION SELECT r.owner_run_id FROM run r JOIN owners o ON r.id=o.id
 WHERE r.owner_run_id IS NOT NULL)
 SELECT 1 FROM owners WHERE id=NEW.id)
 BEGIN SELECT RAISE(ABORT,'run ownership cycle'); END;

CREATE TRIGGER reject_owner_cycle_update BEFORE UPDATE OF owner_run_id ON run
 WHEN NEW.owner_run_id IS NOT NULL AND EXISTS(
 WITH RECURSIVE owners(id) AS(
 SELECT NEW.owner_run_id UNION SELECT r.owner_run_id FROM run r JOIN owners o ON r.id=o.id
 WHERE r.owner_run_id IS NOT NULL)
 SELECT 1 FROM owners WHERE id=NEW.id)
 BEGIN SELECT RAISE(ABORT,'run ownership cycle'); END;

CREATE TABLE instruction_snapshot(
 id TEXT PRIMARY KEY NOT NULL, source_path TEXT NOT NULL, content_hash TEXT NOT NULL,
 body TEXT NOT NULL, observed_size INTEGER, observed_mtime TEXT, created_at TEXT NOT NULL);

CREATE TABLE receipt(
 id TEXT PRIMARY KEY NOT NULL, client_operation_id TEXT NOT NULL UNIQUE,
 sender_json TEXT NOT NULL CHECK(json_valid(sender_json)), target_thread_id TEXT NOT NULL REFERENCES thread(id),
 message_node_id TEXT NOT NULL REFERENCES node(id), run_id TEXT REFERENCES run(id),
 turn_id TEXT REFERENCES turn(id), state TEXT NOT NULL CHECK(state IN ('accepted','held','applied','cancelled')),
 created_at TEXT NOT NULL, updated_at TEXT NOT NULL DEFAULT '',
 generation INTEGER NOT NULL DEFAULT 0 CHECK(generation>=0), hold_reason TEXT,
 result_json TEXT CHECK(result_json IS NULL OR json_valid(result_json)),
 CHECK(turn_id IS NULL OR run_id IS NOT NULL));

CREATE TRIGGER immutable_receipt_assignment BEFORE UPDATE OF run_id,turn_id ON receipt
 WHEN (OLD.run_id IS NOT NULL AND NEW.run_id IS NOT OLD.run_id)
 OR (OLD.turn_id IS NOT NULL AND NEW.turn_id IS NOT OLD.turn_id)
 BEGIN SELECT RAISE(ABORT,'receipt assignment is immutable'); END;

CREATE INDEX receipt_target ON receipt(target_thread_id,state,created_at);

CREATE TABLE draft(
 id TEXT PRIMARY KEY NOT NULL, thread_id TEXT REFERENCES thread(id), revision INTEGER NOT NULL CHECK(revision>=0),
 body_json TEXT NOT NULL CHECK(json_valid(body_json)), state TEXT NOT NULL, updated_at TEXT NOT NULL);

CREATE TABLE view_state(
 thread_id TEXT PRIMARY KEY NOT NULL REFERENCES thread(id), anchor_node_id TEXT,
 anchor_offset INTEGER NOT NULL DEFAULT 0 CHECK(anchor_offset>=0), follow_tail INTEGER NOT NULL DEFAULT 1 CHECK(follow_tail IN (0,1)),
 input_state_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(input_state_json)));

CREATE TABLE model_attempt(
 id TEXT PRIMARY KEY NOT NULL, turn_id TEXT NOT NULL REFERENCES turn(id),
 generation INTEGER NOT NULL CHECK(generation>=0), selection_revision INTEGER NOT NULL CHECK(selection_revision>=0),
 profile_id TEXT NOT NULL, model_id TEXT NOT NULL, effort TEXT,
 reasoning_epoch INTEGER NOT NULL CHECK(reasoning_epoch>=0), state TEXT NOT NULL CHECK(state IN ('preparing','inflight','completed','failed','incomplete','cancelled')), input_manifest_json TEXT NOT NULL CHECK(json_valid(input_manifest_json)),
 response_id TEXT, error_json TEXT, usage_json TEXT, started_at TEXT NOT NULL, ended_at TEXT);

CREATE INDEX attempts_by_turn ON model_attempt(turn_id,started_at);

CREATE TABLE output_item(
 attempt_id TEXT NOT NULL REFERENCES model_attempt(id), ordinal INTEGER NOT NULL CHECK(ordinal>=0),
 item_id TEXT, item_type TEXT NOT NULL, phase TEXT, status TEXT NOT NULL,
 payload_json TEXT NOT NULL CHECK(json_valid(payload_json)), node_id TEXT REFERENCES node(id), PRIMARY KEY(attempt_id,ordinal));

CREATE TABLE tool_group(
 id TEXT PRIMARY KEY NOT NULL, attempt_id TEXT NOT NULL REFERENCES model_attempt(id),
 turn_id TEXT NOT NULL REFERENCES turn(id), state TEXT NOT NULL, created_at TEXT NOT NULL);

CREATE TABLE tool_call(
 group_id TEXT NOT NULL REFERENCES tool_group(id), call_id TEXT NOT NULL,
 ordinal INTEGER NOT NULL CHECK(ordinal>=0), tool_name TEXT NOT NULL, arguments_json TEXT NOT NULL,
 state TEXT NOT NULL, operation_id TEXT UNIQUE, PRIMARY KEY(group_id,call_id),
 UNIQUE(group_id,ordinal));

CREATE TABLE tool_result(
 group_id TEXT NOT NULL, call_id TEXT NOT NULL, node_id TEXT NOT NULL REFERENCES node(id),
 status TEXT NOT NULL, FOREIGN KEY(group_id,call_id) REFERENCES tool_call(group_id,call_id),
 PRIMARY KEY(group_id,call_id));

CREATE TABLE shell_execution(
 id TEXT PRIMARY KEY NOT NULL, owner_run_id TEXT NOT NULL REFERENCES run(id),
 turn_id TEXT NOT NULL REFERENCES turn(id), operation_id TEXT NOT NULL UNIQUE,
 state TEXT NOT NULL CHECK(state IN ('starting','running','stopping','exited','failed','stopped')), cwd TEXT NOT NULL, shell_json TEXT NOT NULL CHECK(json_valid(shell_json)),
 input_encoding TEXT NOT NULL, output_encoding TEXT NOT NULL, stdin_state TEXT NOT NULL,
 saved_sequence INTEGER NOT NULL DEFAULT 0 CHECK(saved_sequence>=0), retention_generation INTEGER NOT NULL DEFAULT 0 CHECK(retention_generation>=0),
 exit_code INTEGER, end_reason TEXT, output_complete INTEGER CHECK(output_complete IS NULL OR output_complete IN (0,1)), started_at TEXT NOT NULL, ended_at TEXT);

CREATE TABLE output_chunk(
 execution_id TEXT NOT NULL REFERENCES shell_execution(id), sequence INTEGER NOT NULL CHECK(sequence>=0),
 stream TEXT NOT NULL CHECK(stream IN ('stdout','stderr')),
 stream_offset INTEGER NOT NULL CHECK(stream_offset>=0), bytes BLOB NOT NULL,
 received_at TEXT NOT NULL, PRIMARY KEY(execution_id,sequence));

CREATE INDEX output_stream ON output_chunk(execution_id,stream,stream_offset);

CREATE TABLE replacement(
 id TEXT PRIMARY KEY NOT NULL, output_node_id TEXT NOT NULL REFERENCES node(id),
 input_key TEXT NOT NULL, format_version TEXT NOT NULL, prompt_version TEXT NOT NULL,
 state TEXT NOT NULL, created_at TEXT NOT NULL);

CREATE INDEX replacement_reuse ON replacement(input_key,state);

CREATE TABLE replacement_source(
 replacement_id TEXT NOT NULL REFERENCES replacement(id), ordinal INTEGER NOT NULL CHECK(ordinal>=0),
 source_node_id TEXT NOT NULL REFERENCES node(id), source_revision INTEGER NOT NULL CHECK(source_revision>=0),
 source_hash TEXT, PRIMARY KEY(replacement_id,ordinal));

CREATE INDEX replacement_source_reverse ON replacement_source(source_node_id);

CREATE TABLE thread_replacement(
 thread_id TEXT NOT NULL REFERENCES thread(id), replacement_id TEXT NOT NULL REFERENCES replacement(id),
 PRIMARY KEY(thread_id,replacement_id));

CREATE TABLE attachment_ref(
 id TEXT PRIMARY KEY NOT NULL, source_node_id TEXT REFERENCES node(id), absolute_path TEXT NOT NULL,
 input_kind TEXT NOT NULL CHECK(input_kind IN ('image','file')),
 ownership TEXT NOT NULL CHECK(ownership IN ('external','managed_clipboard')),
 created_at TEXT NOT NULL);

CREATE TABLE attachment_selection(
 thread_id TEXT NOT NULL REFERENCES thread(id), attachment_id TEXT NOT NULL REFERENCES attachment_ref(id),
 state TEXT NOT NULL CHECK(state IN ('active','excluded')), reason TEXT,
 selected_at TEXT NOT NULL, selected_order INTEGER NOT NULL CHECK(selected_order>=0),
 revision INTEGER NOT NULL DEFAULT 0 CHECK(revision>=0), protected INTEGER NOT NULL DEFAULT 1 CHECK(protected IN (0,1)), PRIMARY KEY(thread_id,attachment_id));

CREATE INDEX active_attachments ON attachment_selection(thread_id,state,selected_order);

CREATE TABLE attachment_send(
 attempt_id TEXT NOT NULL REFERENCES model_attempt(id), ordinal INTEGER NOT NULL CHECK(ordinal>=0),
 attachment_id TEXT NOT NULL REFERENCES attachment_ref(id), state TEXT NOT NULL,
 raw_size INTEGER CHECK(raw_size IS NULL OR raw_size>=0), content_hash TEXT, mime_type TEXT, error_json TEXT,
 PRIMARY KEY(attempt_id,ordinal));

CREATE TABLE managed_file(
 id TEXT PRIMARY KEY NOT NULL, absolute_path TEXT NOT NULL UNIQUE, attachment_id TEXT REFERENCES attachment_ref(id),
 state TEXT NOT NULL CHECK(state IN ('creating','ready','deleting','deleted','failed')),
 raw_size INTEGER CHECK(raw_size IS NULL OR raw_size>=0), created_at TEXT NOT NULL, deleted_reason TEXT);

CREATE TABLE event(
 id TEXT PRIMARY KEY NOT NULL, kind TEXT NOT NULL, source_json TEXT NOT NULL,
 target_thread_id TEXT NOT NULL REFERENCES thread(id),
 target_run_id TEXT REFERENCES run(id), payload_ref_json TEXT NOT NULL, created_at TEXT NOT NULL);

CREATE TABLE delivery(
 event_id TEXT NOT NULL REFERENCES event(id), recipient_id TEXT NOT NULL,
 state TEXT NOT NULL, included_attempt_id TEXT REFERENCES model_attempt(id),
 processed_attempt_id TEXT REFERENCES model_attempt(id), PRIMARY KEY(event_id,recipient_id));

CREATE INDEX delivery_pending ON delivery(recipient_id,state);

CREATE TABLE index_space(
 id TEXT PRIMARY KEY NOT NULL, profile_id TEXT NOT NULL, model_id TEXT NOT NULL,
 dimension INTEGER NOT NULL CHECK(dimension>0), splitter_version TEXT NOT NULL,
 state TEXT NOT NULL, vec_table_name TEXT NOT NULL UNIQUE);

CREATE TABLE embedding_job(
 id TEXT PRIMARY KEY NOT NULL, space_id TEXT NOT NULL REFERENCES index_space(id),
 source_node_id TEXT NOT NULL REFERENCES node(id), source_revision INTEGER NOT NULL CHECK(source_revision>=0),
 body_kind TEXT NOT NULL, source_hash TEXT NOT NULL, span_start INTEGER NOT NULL CHECK(span_start>=0),
 span_end INTEGER NOT NULL CHECK(span_end>=span_start), state TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts>=0),
 error_json TEXT, next_attempt_at TEXT, authorized_watermark TEXT,
 UNIQUE(space_id,source_node_id,source_revision,body_kind,span_start,span_end,source_hash));

CREATE INDEX jobs_ready ON embedding_job(space_id,state,next_attempt_at);

CREATE TABLE embedding_chunk(
 id TEXT PRIMARY KEY NOT NULL, job_id TEXT NOT NULL REFERENCES embedding_job(id),
 space_id TEXT NOT NULL REFERENCES index_space(id), source_node_id TEXT NOT NULL REFERENCES node(id),
 source_revision INTEGER NOT NULL CHECK(source_revision>=0), span_start INTEGER NOT NULL CHECK(span_start>=0), span_end INTEGER NOT NULL CHECK(span_end>=span_start),
 content_hash TEXT NOT NULL, vec_row_id INTEGER NOT NULL, UNIQUE(space_id,vec_row_id));

CREATE INDEX chunks_source ON embedding_chunk(source_node_id,space_id);

CREATE TABLE runtime_pin(
 id TEXT PRIMARY KEY NOT NULL, instance_id TEXT NOT NULL, resource_kind TEXT NOT NULL,
 resource_id TEXT NOT NULL, generation INTEGER NOT NULL CHECK(generation>=0), created_at TEXT NOT NULL);

CREATE INDEX pins_resource ON runtime_pin(resource_kind,resource_id);

CREATE TABLE gc_job(
 id TEXT PRIMARY KEY NOT NULL, kind TEXT NOT NULL, root_revision INTEGER NOT NULL CHECK(root_revision>=0),
 cursor_json TEXT, state TEXT NOT NULL, error_json TEXT);

CREATE TABLE quota_counter(
 category TEXT PRIMARY KEY NOT NULL, logical_bytes INTEGER NOT NULL CHECK(logical_bytes>=0),
 revision INTEGER NOT NULL);

CREATE TABLE operation_receipt(
 operation_id TEXT PRIMARY KEY NOT NULL, instance_id TEXT NOT NULL, origin_json TEXT NOT NULL,
 request_hash TEXT NOT NULL, state TEXT NOT NULL, result_ref_json TEXT CHECK(result_ref_json IS NULL OR json_valid(result_ref_json)), created_at TEXT NOT NULL, reserved_bytes INTEGER NOT NULL DEFAULT 0 CHECK(reserved_bytes>=0));

CREATE INDEX nodes_retention ON node(kind,created_at);

CREATE INDEX managed_retention ON managed_file(state,created_at);

CREATE INDEX receipt_created ON receipt(created_at);

CREATE INDEX operation_instance ON operation_receipt(instance_id,created_at);

CREATE TABLE schema_migration(
 version INTEGER PRIMARY KEY CHECK(version>0), name TEXT NOT NULL UNIQUE,
 applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')));

CREATE TRIGGER immutable_instruction_snapshot BEFORE UPDATE ON instruction_snapshot
 BEGIN SELECT RAISE(ABORT,'instruction snapshot is immutable'); END;

CREATE INDEX receipt_message_node ON receipt(message_node_id);

CREATE INDEX replacement_output_node ON replacement(output_node_id);

CREATE INDEX tool_result_node ON tool_result(node_id);

CREATE INDEX output_item_node ON output_item(node_id);

CREATE INDEX context_pending_by_thread ON event(target_thread_id,created_at,id)
 WHERE kind='context_changed' AND target_run_id IS NULL;

CREATE INDEX turns_by_thread ON turn(thread_id,created_at);

CREATE INDEX turns_by_thread_end ON turn(thread_id,ended_at) WHERE ended_at IS NOT NULL;

CREATE TABLE attachment_delivery(
 node_id TEXT NOT NULL REFERENCES node(id),
 thread_id TEXT NOT NULL REFERENCES thread(id),
 target_run_id TEXT REFERENCES run(id),
 status TEXT NOT NULL CHECK(status IN ('accepted','held','applied','cancelled')),
 operation_id TEXT NOT NULL, origin_json TEXT NOT NULL CHECK(json_valid(origin_json)),
 created_at TEXT NOT NULL, PRIMARY KEY(node_id,thread_id));

CREATE INDEX attachment_delivery_pending ON attachment_delivery(thread_id,status,created_at,node_id);

CREATE INDEX attachment_source ON attachment_ref(source_node_id);

CREATE TRIGGER attachment_source_required_insert BEFORE INSERT ON attachment_ref
 WHEN NEW.source_node_id IS NULL BEGIN SELECT RAISE(ABORT,'attachment source is required'); END;

CREATE TRIGGER attachment_source_required_update BEFORE UPDATE OF source_node_id ON attachment_ref
 WHEN NEW.source_node_id IS NULL BEGIN SELECT RAISE(ABORT,'attachment source is required'); END;

CREATE INDEX operation_receipt_pending_budget ON operation_receipt(instance_id) WHERE state='pending';

CREATE TRIGGER operation_receipt_budget_insert AFTER INSERT ON operation_receipt BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(512+length(CAST(NEW.operation_id AS BLOB))+length(CAST(NEW.instance_id AS BLOB))+length(CAST(NEW.origin_json AS BLOB))+length(CAST(NEW.request_hash AS BLOB))+length(CAST(NEW.created_at AS BLOB))+COALESCE(length(CAST(NEW.result_ref_json AS BLOB)),0)),revision=revision+1 WHERE category='operation_receipt';
 UPDATE quota_counter SET logical_bytes=logical_bytes+(CASE WHEN NEW.state='pending' THEN 512+length(CAST(NEW.operation_id AS BLOB))+length(CAST(NEW.instance_id AS BLOB))+length(CAST(NEW.origin_json AS BLOB))+length(CAST(NEW.request_hash AS BLOB))+length(CAST(NEW.created_at AS BLOB))+COALESCE(length(CAST(NEW.result_ref_json AS BLOB)),0) ELSE 0 END),revision=revision+1 WHERE category='operation_receipt.pending_actual';
END;

CREATE TRIGGER operation_receipt_budget_delete AFTER DELETE ON operation_receipt BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes-(512+length(CAST(OLD.operation_id AS BLOB))+length(CAST(OLD.instance_id AS BLOB))+length(CAST(OLD.origin_json AS BLOB))+length(CAST(OLD.request_hash AS BLOB))+length(CAST(OLD.created_at AS BLOB))+COALESCE(length(CAST(OLD.result_ref_json AS BLOB)),0)),revision=revision+1 WHERE category='operation_receipt';
 UPDATE quota_counter SET logical_bytes=logical_bytes-(CASE WHEN OLD.state='pending' THEN 512+length(CAST(OLD.operation_id AS BLOB))+length(CAST(OLD.instance_id AS BLOB))+length(CAST(OLD.origin_json AS BLOB))+length(CAST(OLD.request_hash AS BLOB))+length(CAST(OLD.created_at AS BLOB))+COALESCE(length(CAST(OLD.result_ref_json AS BLOB)),0) ELSE 0 END),revision=revision+1 WHERE category='operation_receipt.pending_actual';
END;

CREATE TRIGGER operation_receipt_budget_update AFTER UPDATE ON operation_receipt BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(512+length(CAST(NEW.operation_id AS BLOB))+length(CAST(NEW.instance_id AS BLOB))+length(CAST(NEW.origin_json AS BLOB))+length(CAST(NEW.request_hash AS BLOB))+length(CAST(NEW.created_at AS BLOB))+COALESCE(length(CAST(NEW.result_ref_json AS BLOB)),0))-(512+length(CAST(OLD.operation_id AS BLOB))+length(CAST(OLD.instance_id AS BLOB))+length(CAST(OLD.origin_json AS BLOB))+length(CAST(OLD.request_hash AS BLOB))+length(CAST(OLD.created_at AS BLOB))+COALESCE(length(CAST(OLD.result_ref_json AS BLOB)),0)),revision=revision+1 WHERE category='operation_receipt';
 UPDATE quota_counter SET logical_bytes=logical_bytes+(CASE WHEN NEW.state='pending' THEN 512+length(CAST(NEW.operation_id AS BLOB))+length(CAST(NEW.instance_id AS BLOB))+length(CAST(NEW.origin_json AS BLOB))+length(CAST(NEW.request_hash AS BLOB))+length(CAST(NEW.created_at AS BLOB))+COALESCE(length(CAST(NEW.result_ref_json AS BLOB)),0) ELSE 0 END)-(CASE WHEN OLD.state='pending' THEN 512+length(CAST(OLD.operation_id AS BLOB))+length(CAST(OLD.instance_id AS BLOB))+length(CAST(OLD.origin_json AS BLOB))+length(CAST(OLD.request_hash AS BLOB))+length(CAST(OLD.created_at AS BLOB))+COALESCE(length(CAST(OLD.result_ref_json AS BLOB)),0) ELSE 0 END),revision=revision+1 WHERE category='operation_receipt.pending_actual';
END;

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

CREATE INDEX index_bootstrap_source ON app_meta(json_extract(value,'$.witness.node_id')) WHERE key LIKE 'index.bootstrap.%' AND json_valid(value);

CREATE INDEX index_auto_source ON app_meta(json_extract(value,'$.node_id')) WHERE key LIKE 'index.auto.%' AND json_valid(value);

CREATE INDEX node_turn ON node(turn_id,id);

CREATE INDEX receipt_turn_source ON receipt(turn_id,target_thread_id,message_node_id);

CREATE INDEX shell_active_owner ON shell_execution(owner_run_id,state)
 WHERE state IN ('starting','running','stopping');

CREATE INDEX run_thread_created ON run(thread_id,created_at DESC,id DESC);

CREATE INDEX shell_terminal_time ON shell_execution(COALESCE(ended_at,started_at))
 WHERE state IN ('exited','failed','stopped');

INSERT INTO app_meta VALUES('root_revision','0');
INSERT INTO app_meta VALUES('payload_format_version','1');
INSERT INTO quota_counter VALUES('operation_receipt',0,1);
INSERT INTO quota_counter VALUES('operation_receipt.pending_actual',0,1);
INSERT INTO schema_migration(version,name) VALUES(1,'0001_initial');
INSERT INTO schema_migration(version,name) VALUES(2,'0002_gc_indexes');
INSERT INTO schema_migration(version,name) VALUES(3,'0003_context_indexes');
INSERT INTO schema_migration(version,name) VALUES(4,'0004_attachment_provenance');
INSERT INTO schema_migration(version,name) VALUES(5,'0005_origin_credentials');
INSERT INTO schema_migration(version,name) VALUES(6,'0006_receipt_budget');
INSERT INTO schema_migration(version,name) VALUES(7,'0007_usage_and_source_indexes');
INSERT INTO schema_migration(version,name) VALUES(8,'0008_receipt_accounting_compat');
INSERT INTO schema_migration(version,name) VALUES(9,'0009_compaction_turn_indexes');
INSERT INTO schema_migration(version,name) VALUES(10,'0010_hot_query_indexes');
PRAGMA user_version=10;
