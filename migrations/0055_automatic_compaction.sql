DROP INDEX automatic_compaction_turn;
CREATE TABLE automatic_compaction (
 attempt_id TEXT PRIMARY KEY NOT NULL REFERENCES model_attempt(id) ON DELETE CASCADE,
 turn_id TEXT NOT NULL REFERENCES turn(id) ON DELETE CASCADE,
 attempt_started_at TEXT NOT NULL,
 token TEXT,
 reason TEXT,
 trigger_json TEXT,
 closed_spill TEXT,
 active_spill TEXT,
 closed_summary TEXT,
 active_summary TEXT,
 failure_json TEXT,
 outcome TEXT,
 result_json TEXT,
 ended_at TEXT
);
CREATE INDEX automatic_compaction_turn ON automatic_compaction(turn_id,attempt_started_at,attempt_id);
CREATE TRIGGER logical_history_automatic_compaction_insert AFTER INSERT ON automatic_compaction BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(octet_length(NEW.token),0)+COALESCE(octet_length(NEW.reason),0)+COALESCE(octet_length(NEW.trigger_json),0)+COALESCE(octet_length(NEW.closed_spill),0)+COALESCE(octet_length(NEW.active_spill),0)+COALESCE(octet_length(NEW.closed_summary),0)+COALESCE(octet_length(NEW.active_summary),0)+COALESCE(octet_length(NEW.failure_json),0)+COALESCE(octet_length(NEW.outcome),0)+COALESCE(octet_length(NEW.result_json),0)+COALESCE(octet_length(NEW.ended_at),0)),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER logical_history_automatic_compaction_delete AFTER DELETE ON automatic_compaction BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(-(COALESCE(octet_length(OLD.token),0)+COALESCE(octet_length(OLD.reason),0)+COALESCE(octet_length(OLD.trigger_json),0)+COALESCE(octet_length(OLD.closed_spill),0)+COALESCE(octet_length(OLD.active_spill),0)+COALESCE(octet_length(OLD.closed_summary),0)+COALESCE(octet_length(OLD.active_summary),0)+COALESCE(octet_length(OLD.failure_json),0)+COALESCE(octet_length(OLD.outcome),0)+COALESCE(octet_length(OLD.result_json),0)+COALESCE(octet_length(OLD.ended_at),0))),revision=revision+1 WHERE category='logical.history';
END;
CREATE TRIGGER logical_history_automatic_compaction_update AFTER UPDATE ON automatic_compaction BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+((COALESCE(octet_length(NEW.token),0)+COALESCE(octet_length(NEW.reason),0)+COALESCE(octet_length(NEW.trigger_json),0)+COALESCE(octet_length(NEW.closed_spill),0)+COALESCE(octet_length(NEW.active_spill),0)+COALESCE(octet_length(NEW.closed_summary),0)+COALESCE(octet_length(NEW.active_summary),0)+COALESCE(octet_length(NEW.failure_json),0)+COALESCE(octet_length(NEW.outcome),0)+COALESCE(octet_length(NEW.result_json),0)+COALESCE(octet_length(NEW.ended_at),0))-(COALESCE(octet_length(OLD.token),0)+COALESCE(octet_length(OLD.reason),0)+COALESCE(octet_length(OLD.trigger_json),0)+COALESCE(octet_length(OLD.closed_spill),0)+COALESCE(octet_length(OLD.active_spill),0)+COALESCE(octet_length(OLD.closed_summary),0)+COALESCE(octet_length(OLD.active_summary),0)+COALESCE(octet_length(OLD.failure_json),0)+COALESCE(octet_length(OLD.outcome),0)+COALESCE(octet_length(OLD.result_json),0)+COALESCE(octet_length(OLD.ended_at),0))),revision=revision+1 WHERE category='logical.history';
END;

WITH legacy AS MATERIALIZED (
 SELECT id,turn_id,started_at,ended_at,
  CASE WHEN json_type(error_json,'$.automatic_trim_attempt')='object' THEN json_extract(error_json,'$.automatic_trim_attempt') END a,
  CASE WHEN json_type(error_json,'$.automatic_trim')='object' THEN json_extract(error_json,'$.automatic_trim') END r
 FROM model_attempt WHERE kind='conversation' AND CASE WHEN json_valid(error_json) THEN json_type(error_json)='object' ELSE 0 END
)
INSERT INTO automatic_compaction(attempt_id,turn_id,attempt_started_at,token,reason,trigger_json,closed_spill,active_spill,closed_summary,active_summary,failure_json,outcome,result_json,ended_at)
 SELECT id,turn_id,started_at,json_extract(a,'$.token'),json_extract(a,'$.reason'),
  a->>'$.trigger',a->>'$.committed_stages.closed_spill',a->>'$.committed_stages.active_spill',
  a->>'$.committed_stages.closed_summary',a->>'$.committed_stages.active_summary',a->>'$.compaction_outcome',
  NULLIF(a->>'$.outcome','unknown'),
  (SELECT json_group_object(key,json(body)) FROM (
    SELECT 'metrics' key, CASE WHEN a IS NULL THEN r ELSE COALESCE(
      NULLIF(a->'$.summary','null'),NULLIF(a->'$.result','null'),a->'$.error.details') END body
    UNION ALL SELECT 'error',a->'$.error' WHERE a->'$.error' IS NOT NULL
    UNION ALL SELECT 'midturn',a->'$.midturn' WHERE a->'$.midturn' IS NOT NULL
    UNION ALL SELECT 'maximum_outcome',a->'$.maximum_outcome' WHERE a->'$.maximum_outcome' IS NOT NULL
  )),
  CASE WHEN a->>'$.outcome' NOT IN ('unknown') OR a IS NULL THEN ended_at END
 FROM legacy WHERE a IS NOT NULL OR r IS NOT NULL;
UPDATE model_attempt SET error_json=json_remove(error_json,'$.automatic_trim_attempt','$.automatic_trim')
 WHERE kind='conversation' AND CASE WHEN json_valid(error_json) THEN json_type(error_json,'$.automatic_trim_attempt') IS NOT NULL OR json_type(error_json,'$.automatic_trim') IS NOT NULL ELSE 0 END;
UPDATE model_attempt SET error_json=NULL WHERE state='completed' AND error_json='{}';
INSERT INTO schema_migration(version,name) VALUES(55,'0055_automatic_compaction');
PRAGMA user_version=55;
