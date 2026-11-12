ALTER TABLE thread ADD COLUMN auto_compact_target_percent INTEGER CHECK(auto_compact_target_percent IS NULL OR (typeof(auto_compact_target_percent)='integer' AND auto_compact_target_percent BETWEEN 1 AND 100));
ALTER TABLE thread ADD COLUMN summary_max_chars INTEGER CHECK(summary_max_chars IS NULL OR (typeof(summary_max_chars)='integer' AND summary_max_chars BETWEEN 1 AND 9223372036854775807));
-- Preserve retired per-Thread map/reduce controls as inert migration history,
-- then remove them from the runtime schema in the same transaction.
CREATE TABLE thread_retired_summary_settings(
 thread_id TEXT NOT NULL REFERENCES thread(id) ON DELETE CASCADE,
 setting_name TEXT NOT NULL CHECK(setting_name IN ('segment_chars','reduce_rounds')),
 setting_value INTEGER NOT NULL,
 PRIMARY KEY(thread_id,setting_name)
) WITHOUT ROWID;
INSERT INTO thread_retired_summary_settings(thread_id,setting_name,setting_value)
 SELECT id,'segment_chars',segment_chars FROM thread WHERE segment_chars IS NOT NULL
 UNION ALL
 SELECT id,'reduce_rounds',reduce_rounds FROM thread WHERE reduce_rounds IS NOT NULL;
ALTER TABLE thread DROP COLUMN segment_chars;
ALTER TABLE thread DROP COLUMN reduce_rounds;
-- Preserve only retired stage facts, then remove those fields from durable
-- current summaries and account records. Historical per-part cache rows remain
-- readable through history --parts; nothing in current execution consumes them.
CREATE TABLE summary_usage_stage_archive(
 record_key TEXT PRIMARY KEY NOT NULL,
 legacy_json TEXT NOT NULL CHECK(json_valid(legacy_json))
) WITHOUT ROWID;

-- Per-compaction usage ledger records flatten SummaryUsage into UsageRecord.
INSERT INTO summary_usage_stage_archive(record_key,legacy_json)
 SELECT key,json_object('map_requests',json_extract(value,'$.map_requests'),
   'reduce_requests',json_extract(value,'$.reduce_requests'),
   'oversized_reduce_parts',json_extract(value,'$.oversized_reduce_parts'))
 FROM app_meta WHERE key LIKE 'summary.usage.%' AND json_valid(value)
   AND (json_type(value,'$.map_requests') IS NOT NULL OR json_type(value,'$.reduce_requests') IS NOT NULL OR json_type(value,'$.oversized_reduce_parts') IS NOT NULL);
UPDATE app_meta SET value=json_remove(value,'$.map_requests','$.reduce_requests','$.oversized_reduce_parts')
 WHERE key LIKE 'summary.usage.%' AND json_valid(value)
   AND (json_type(value,'$.map_requests') IS NOT NULL OR json_type(value,'$.reduce_requests') IS NOT NULL OR json_type(value,'$.oversized_reduce_parts') IS NOT NULL);

-- Summary node payloads are the primary durable readback record.
INSERT INTO summary_usage_stage_archive(record_key,legacy_json)
 SELECT 'node:'||id,json_object('map_requests',json_extract(payload_json,'$.summary_usage.map_requests'),
   'reduce_requests',json_extract(payload_json,'$.summary_usage.reduce_requests'),
   'oversized_reduce_parts',json_extract(payload_json,'$.summary_usage.oversized_reduce_parts'))
 FROM node WHERE payload_json IS NOT NULL AND json_valid(payload_json)
   AND (json_type(payload_json,'$.summary_usage.map_requests') IS NOT NULL OR json_type(payload_json,'$.summary_usage.reduce_requests') IS NOT NULL OR json_type(payload_json,'$.summary_usage.oversized_reduce_parts') IS NOT NULL);
UPDATE node SET payload_json=json_remove(payload_json,
 '$.summary_usage.map_requests','$.summary_usage.reduce_requests','$.summary_usage.oversized_reduce_parts')
 WHERE payload_json IS NOT NULL AND json_valid(payload_json)
   AND (json_type(payload_json,'$.summary_usage.map_requests') IS NOT NULL OR json_type(payload_json,'$.summary_usage.reduce_requests') IS NOT NULL OR json_type(payload_json,'$.summary_usage.oversized_reduce_parts') IS NOT NULL);

-- Compact receipts retain both successful result facts and rejected-attempt
-- usage in their current app_meta record; archive each retired object first.
INSERT INTO summary_usage_stage_archive(record_key,legacy_json)
 SELECT 'compact.result:'||key,json_object(
  'result',json_extract(value,'$.outcome.result.compact.summary_usage'),
  'error',json_extract(value,'$.outcome.error.details.summary_usage'))
 FROM app_meta WHERE key LIKE 'compact.result.%' AND json_valid(value)
   AND (json_type(value,'$.outcome.result.compact.summary_usage.map_requests') IS NOT NULL
     OR json_type(value,'$.outcome.result.compact.summary_usage.reduce_requests') IS NOT NULL
     OR json_type(value,'$.outcome.result.compact.summary_usage.oversized_reduce_parts') IS NOT NULL
     OR json_type(value,'$.outcome.error.details.summary_usage.map_requests') IS NOT NULL
     OR json_type(value,'$.outcome.error.details.summary_usage.reduce_requests') IS NOT NULL
     OR json_type(value,'$.outcome.error.details.summary_usage.oversized_reduce_parts') IS NOT NULL);
UPDATE app_meta SET value=json_remove(value,
 '$.outcome.result.compact.summary_usage.map_requests','$.outcome.result.compact.summary_usage.reduce_requests','$.outcome.result.compact.summary_usage.oversized_reduce_parts',
 '$.outcome.error.details.summary_usage.map_requests','$.outcome.error.details.summary_usage.reduce_requests','$.outcome.error.details.summary_usage.oversized_reduce_parts')
 WHERE key LIKE 'compact.result.%' AND json_valid(value)
   AND (json_type(value,'$.outcome.result.compact.summary_usage.map_requests') IS NOT NULL
     OR json_type(value,'$.outcome.result.compact.summary_usage.reduce_requests') IS NOT NULL
     OR json_type(value,'$.outcome.result.compact.summary_usage.oversized_reduce_parts') IS NOT NULL
     OR json_type(value,'$.outcome.error.details.summary_usage.map_requests') IS NOT NULL
     OR json_type(value,'$.outcome.error.details.summary_usage.reduce_requests') IS NOT NULL
     OR json_type(value,'$.outcome.error.details.summary_usage.oversized_reduce_parts') IS NOT NULL);

-- Automatic stage facts are nested under each committed stage in attempt JSON.
INSERT INTO summary_usage_stage_archive(record_key,legacy_json)
 SELECT 'attempt:'||a.id,json_object('committed_stages',json_extract(a.error_json,'$.automatic_trim_attempt.committed_stages'))
 FROM model_attempt a WHERE a.error_json IS NOT NULL AND json_valid(a.error_json)
   AND EXISTS(SELECT 1 FROM json_each(a.error_json,'$.automatic_trim_attempt.committed_stages') stage
     WHERE json_type(stage.value,'$.summary_usage.map_requests') IS NOT NULL
        OR json_type(stage.value,'$.summary_usage.reduce_requests') IS NOT NULL
        OR json_type(stage.value,'$.summary_usage.oversized_reduce_parts') IS NOT NULL);
UPDATE model_attempt SET error_json=json_set(error_json,'$.automatic_trim_attempt.committed_stages',
 (SELECT json_group_object(stage.key,json(json_remove(stage.value,
  '$.summary_usage.map_requests','$.summary_usage.reduce_requests','$.summary_usage.oversized_reduce_parts')))
  FROM json_each(model_attempt.error_json,'$.automatic_trim_attempt.committed_stages') stage))
 WHERE error_json IS NOT NULL AND json_valid(error_json)
   AND EXISTS(SELECT 1 FROM json_each(error_json,'$.automatic_trim_attempt.committed_stages') stage
     WHERE json_type(stage.value,'$.summary_usage.map_requests') IS NOT NULL
        OR json_type(stage.value,'$.summary_usage.reduce_requests') IS NOT NULL
        OR json_type(stage.value,'$.summary_usage.oversized_reduce_parts') IS NOT NULL);

INSERT INTO schema_migration(version,name) VALUES(43,'0043_thread_single_summary_settings');
PRAGMA user_version=43;
