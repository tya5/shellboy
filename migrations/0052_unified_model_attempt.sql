-- Foreign keys are disabled only around the checked rebuild; existing IDs and rowids are preserved.
CREATE TABLE model_attempt_new(
 id TEXT PRIMARY KEY NOT NULL, turn_id TEXT REFERENCES turn(id),
 kind TEXT NOT NULL CHECK(kind IN ('conversation','summary')) CHECK(kind='summary' OR turn_id IS NOT NULL),
 thread_id TEXT NOT NULL REFERENCES thread(id),
 generation INTEGER NOT NULL CHECK(generation>=0), selection_revision INTEGER NOT NULL CHECK(selection_revision>=0),
 profile_id TEXT NOT NULL, model_id TEXT NOT NULL, effort TEXT,
 reasoning_epoch INTEGER NOT NULL CHECK(reasoning_epoch>=0), state TEXT NOT NULL CHECK(state IN ('preparing','inflight','completed','failed','incomplete','cancelled')), input_manifest_json TEXT NOT NULL CHECK(json_valid(input_manifest_json)),
 response_id TEXT, error_json TEXT, usage_json TEXT, started_at TEXT NOT NULL, ended_at TEXT);
INSERT INTO model_attempt_new(rowid,id,turn_id,generation,selection_revision,profile_id,model_id,effort,reasoning_epoch,state,input_manifest_json,response_id,error_json,usage_json,started_at,ended_at,kind,thread_id)
 SELECT a.rowid,a.id,a.turn_id,a.generation,a.selection_revision,a.profile_id,a.model_id,a.effort,a.reasoning_epoch,a.state,a.input_manifest_json,a.response_id,a.error_json,a.usage_json,a.started_at,a.ended_at,'conversation',(SELECT thread_id FROM turn WHERE id=a.turn_id) FROM model_attempt a;
DROP TABLE model_attempt;
ALTER TABLE model_attempt_new RENAME TO model_attempt;
CREATE INDEX attempts_by_turn ON model_attempt(turn_id,started_at);
CREATE INDEX attempts_by_thread_kind ON model_attempt(thread_id,kind,started_at);
CREATE INDEX summary_attempt_group ON model_attempt(thread_id,json_extract(input_manifest_json,'$.summary.id')) WHERE kind='summary';
CREATE INDEX attempts_by_kind ON model_attempt(kind);
CREATE INDEX gc_bytes_model_attempt ON model_attempt(COALESCE(octet_length(input_manifest_json),0)+COALESCE(octet_length(error_json),0)+COALESCE(octet_length(usage_json),0));
CREATE INDEX automatic_compaction_turn ON model_attempt(turn_id,started_at,id) WHERE CASE WHEN json_valid(error_json) THEN json_type(error_json,'$.automatic_trim')='object' OR json_type(error_json,'$.automatic_trim_attempt')='object' ELSE 0 END;
CREATE INDEX attempts_completed_scope ON model_attempt(turn_id,profile_id,model_id) WHERE state='completed' AND kind='conversation' AND json_extract(input_manifest_json,'$.adoption.outcome')='adopted';
CREATE INDEX activity_attempt_turn ON model_attempt(turn_id);
CREATE INDEX model_attempt_active_recovery ON model_attempt(state) WHERE state IN ('preparing','inflight');
CREATE TRIGGER logical_history_model_attempt_insert AFTER INSERT ON model_attempt BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.input_manifest_json),0)+COALESCE(octet_length(NEW.error_json),0)+COALESCE(octet_length(NEW.usage_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER logical_history_model_attempt_delete AFTER DELETE ON model_attempt BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+-(COALESCE(octet_length(OLD.input_manifest_json),0)+COALESCE(octet_length(OLD.error_json),0)+COALESCE(octet_length(OLD.usage_json),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER logical_history_model_attempt_update AFTER UPDATE OF input_manifest_json,error_json,usage_json ON model_attempt BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.input_manifest_json),0)+COALESCE(octet_length(NEW.error_json),0)+COALESCE(octet_length(NEW.usage_json),0))-(COALESCE(octet_length(OLD.input_manifest_json),0)+COALESCE(octet_length(OLD.error_json),0)+COALESCE(octet_length(OLD.usage_json),0)),revision=revision+1 WHERE category='logical.history';
END;
-- Before separation, a completed Conversation attempt meant that Driver had adopted it.
UPDATE model_attempt SET input_manifest_json=json_set(input_manifest_json,'$.adoption',json_object('outcome','adopted'))
 WHERE kind='conversation' AND state='completed';
INSERT INTO schema_migration(version,name) VALUES(52,'0052_unified_model_attempt');
PRAGMA user_version=52;
