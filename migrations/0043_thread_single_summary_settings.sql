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
-- Every candidate is validated before any JSON path mutation.  Mapping invalid
-- source bytes to '{}' makes root validation safe without relying on WHERE order.
WITH source AS (
 SELECT key,CASE WHEN json_valid(value) THEN value ELSE '{}' END AS doc
 FROM app_meta WHERE key LIKE 'summary.usage.%'
), eligible AS (
 SELECT key,doc FROM source
 WHERE json_type(doc)='object'
   AND (CASE WHEN json_type(doc,'$.map_requests') IS NULL THEN 1
             WHEN json_type(doc,'$.map_requests')='integer' AND typeof(json_extract(doc,'$.map_requests'))='integer' THEN 1 ELSE 0 END)
   AND (CASE WHEN json_type(doc,'$.reduce_requests') IS NULL THEN 1
             WHEN json_type(doc,'$.reduce_requests')='integer' AND typeof(json_extract(doc,'$.reduce_requests'))='integer' THEN 1 ELSE 0 END)
   AND (CASE WHEN json_type(doc,'$.oversized_reduce_parts') IS NULL THEN 1
             WHEN json_type(doc,'$.oversized_reduce_parts')='integer' AND typeof(json_extract(doc,'$.oversized_reduce_parts'))='integer' THEN 1 ELSE 0 END)
   AND (json_type(doc,'$.map_requests') IS NOT NULL OR json_type(doc,'$.reduce_requests') IS NOT NULL OR json_type(doc,'$.oversized_reduce_parts') IS NOT NULL)
)
INSERT INTO summary_usage_stage_archive(record_key,legacy_json)
 SELECT key,json_object('map_requests',json_extract(doc,'$.map_requests'),
   'reduce_requests',json_extract(doc,'$.reduce_requests'),
   'oversized_reduce_parts',json_extract(doc,'$.oversized_reduce_parts')) FROM eligible;

WITH source AS (
 SELECT key,CASE WHEN json_valid(value) THEN value ELSE '{}' END AS doc
 FROM app_meta WHERE key LIKE 'summary.usage.%'
), eligible AS (
 SELECT key,doc FROM source
 WHERE json_type(doc)='object'
   AND (CASE WHEN json_type(doc,'$.map_requests') IS NULL THEN 1 WHEN json_type(doc,'$.map_requests')='integer' AND typeof(json_extract(doc,'$.map_requests'))='integer' THEN 1 ELSE 0 END)
   AND (CASE WHEN json_type(doc,'$.reduce_requests') IS NULL THEN 1 WHEN json_type(doc,'$.reduce_requests')='integer' AND typeof(json_extract(doc,'$.reduce_requests'))='integer' THEN 1 ELSE 0 END)
   AND (CASE WHEN json_type(doc,'$.oversized_reduce_parts') IS NULL THEN 1 WHEN json_type(doc,'$.oversized_reduce_parts')='integer' AND typeof(json_extract(doc,'$.oversized_reduce_parts'))='integer' THEN 1 ELSE 0 END)
   AND (json_type(doc,'$.map_requests') IS NOT NULL OR json_type(doc,'$.reduce_requests') IS NOT NULL OR json_type(doc,'$.oversized_reduce_parts') IS NOT NULL)
)
UPDATE app_meta SET value=json_remove(value,'$.map_requests','$.reduce_requests','$.oversized_reduce_parts')
 WHERE key IN (SELECT key FROM eligible);

-- Summary node payloads are the primary durable readback record.
WITH source AS (
 SELECT id,CASE WHEN json_valid(payload_json) THEN payload_json ELSE '{}' END AS doc
 FROM node WHERE payload_json IS NOT NULL
), eligible AS (
 SELECT id,doc FROM source
 WHERE json_type(doc)='object' AND json_type(doc,'$.summary_usage')='object'
   AND (CASE WHEN json_type(doc,'$.summary_usage.map_requests') IS NULL THEN 1 WHEN json_type(doc,'$.summary_usage.map_requests')='integer' AND typeof(json_extract(doc,'$.summary_usage.map_requests'))='integer' THEN 1 ELSE 0 END)
   AND (CASE WHEN json_type(doc,'$.summary_usage.reduce_requests') IS NULL THEN 1 WHEN json_type(doc,'$.summary_usage.reduce_requests')='integer' AND typeof(json_extract(doc,'$.summary_usage.reduce_requests'))='integer' THEN 1 ELSE 0 END)
   AND (CASE WHEN json_type(doc,'$.summary_usage.oversized_reduce_parts') IS NULL THEN 1 WHEN json_type(doc,'$.summary_usage.oversized_reduce_parts')='integer' AND typeof(json_extract(doc,'$.summary_usage.oversized_reduce_parts'))='integer' THEN 1 ELSE 0 END)
   AND (json_type(doc,'$.summary_usage.map_requests') IS NOT NULL OR json_type(doc,'$.summary_usage.reduce_requests') IS NOT NULL OR json_type(doc,'$.summary_usage.oversized_reduce_parts') IS NOT NULL)
)
INSERT INTO summary_usage_stage_archive(record_key,legacy_json)
 SELECT 'node:'||id,json_object('map_requests',json_extract(doc,'$.summary_usage.map_requests'),
   'reduce_requests',json_extract(doc,'$.summary_usage.reduce_requests'),
   'oversized_reduce_parts',json_extract(doc,'$.summary_usage.oversized_reduce_parts')) FROM eligible;

WITH source AS (
 SELECT id,CASE WHEN json_valid(payload_json) THEN payload_json ELSE '{}' END AS doc
 FROM node WHERE payload_json IS NOT NULL
), eligible AS (
 SELECT id,doc FROM source
 WHERE json_type(doc)='object' AND json_type(doc,'$.summary_usage')='object'
   AND (CASE WHEN json_type(doc,'$.summary_usage.map_requests') IS NULL THEN 1 WHEN json_type(doc,'$.summary_usage.map_requests')='integer' AND typeof(json_extract(doc,'$.summary_usage.map_requests'))='integer' THEN 1 ELSE 0 END)
   AND (CASE WHEN json_type(doc,'$.summary_usage.reduce_requests') IS NULL THEN 1 WHEN json_type(doc,'$.summary_usage.reduce_requests')='integer' AND typeof(json_extract(doc,'$.summary_usage.reduce_requests'))='integer' THEN 1 ELSE 0 END)
   AND (CASE WHEN json_type(doc,'$.summary_usage.oversized_reduce_parts') IS NULL THEN 1 WHEN json_type(doc,'$.summary_usage.oversized_reduce_parts')='integer' AND typeof(json_extract(doc,'$.summary_usage.oversized_reduce_parts'))='integer' THEN 1 ELSE 0 END)
   AND (json_type(doc,'$.summary_usage.map_requests') IS NOT NULL OR json_type(doc,'$.summary_usage.reduce_requests') IS NOT NULL OR json_type(doc,'$.summary_usage.oversized_reduce_parts') IS NOT NULL)
)
UPDATE node SET payload_json=json_remove(payload_json,
 '$.summary_usage.map_requests','$.summary_usage.reduce_requests','$.summary_usage.oversized_reduce_parts')
 WHERE id IN (SELECT id FROM eligible);

-- Compact receipts retain successful and rejected summary usage.  Each present
-- path component must have its ordinary object shape, and every retired counter
-- must be an exact signed SQLite integer before either branch is touched.
WITH source AS (
 SELECT key,CASE WHEN json_valid(value) THEN value ELSE '{}' END AS doc
 FROM app_meta WHERE key LIKE 'compact.result.%'
), eligible AS (
 SELECT key,doc FROM source
 WHERE json_type(doc)='object'
   AND (json_type(doc,'$.outcome') IS NULL OR json_type(doc,'$.outcome')='object')
   AND (json_type(doc,'$.outcome.result') IS NULL OR json_type(doc,'$.outcome.result')='object')
   AND (json_type(doc,'$.outcome.result.compact') IS NULL OR json_type(doc,'$.outcome.result.compact')='object')
   AND (json_type(doc,'$.outcome.result.compact.summary_usage') IS NULL OR json_type(doc,'$.outcome.result.compact.summary_usage')='object')
   AND (json_type(doc,'$.outcome.error') IS NULL OR json_type(doc,'$.outcome.error')='object')
   AND (json_type(doc,'$.outcome.error.details') IS NULL OR json_type(doc,'$.outcome.error.details')='object')
   AND (json_type(doc,'$.outcome.error.details.summary_usage') IS NULL OR json_type(doc,'$.outcome.error.details.summary_usage')='object')
   AND (CASE WHEN json_type(doc,'$.outcome.result.compact.summary_usage.map_requests') IS NULL THEN 1 WHEN json_type(doc,'$.outcome.result.compact.summary_usage.map_requests')='integer' AND typeof(json_extract(doc,'$.outcome.result.compact.summary_usage.map_requests'))='integer' THEN 1 ELSE 0 END)
   AND (CASE WHEN json_type(doc,'$.outcome.result.compact.summary_usage.reduce_requests') IS NULL THEN 1 WHEN json_type(doc,'$.outcome.result.compact.summary_usage.reduce_requests')='integer' AND typeof(json_extract(doc,'$.outcome.result.compact.summary_usage.reduce_requests'))='integer' THEN 1 ELSE 0 END)
   AND (CASE WHEN json_type(doc,'$.outcome.result.compact.summary_usage.oversized_reduce_parts') IS NULL THEN 1 WHEN json_type(doc,'$.outcome.result.compact.summary_usage.oversized_reduce_parts')='integer' AND typeof(json_extract(doc,'$.outcome.result.compact.summary_usage.oversized_reduce_parts'))='integer' THEN 1 ELSE 0 END)
   AND (CASE WHEN json_type(doc,'$.outcome.error.details.summary_usage.map_requests') IS NULL THEN 1 WHEN json_type(doc,'$.outcome.error.details.summary_usage.map_requests')='integer' AND typeof(json_extract(doc,'$.outcome.error.details.summary_usage.map_requests'))='integer' THEN 1 ELSE 0 END)
   AND (CASE WHEN json_type(doc,'$.outcome.error.details.summary_usage.reduce_requests') IS NULL THEN 1 WHEN json_type(doc,'$.outcome.error.details.summary_usage.reduce_requests')='integer' AND typeof(json_extract(doc,'$.outcome.error.details.summary_usage.reduce_requests'))='integer' THEN 1 ELSE 0 END)
   AND (CASE WHEN json_type(doc,'$.outcome.error.details.summary_usage.oversized_reduce_parts') IS NULL THEN 1 WHEN json_type(doc,'$.outcome.error.details.summary_usage.oversized_reduce_parts')='integer' AND typeof(json_extract(doc,'$.outcome.error.details.summary_usage.oversized_reduce_parts'))='integer' THEN 1 ELSE 0 END)
   AND (json_type(doc,'$.outcome.result.compact.summary_usage.map_requests') IS NOT NULL OR json_type(doc,'$.outcome.result.compact.summary_usage.reduce_requests') IS NOT NULL OR json_type(doc,'$.outcome.result.compact.summary_usage.oversized_reduce_parts') IS NOT NULL OR json_type(doc,'$.outcome.error.details.summary_usage.map_requests') IS NOT NULL OR json_type(doc,'$.outcome.error.details.summary_usage.reduce_requests') IS NOT NULL OR json_type(doc,'$.outcome.error.details.summary_usage.oversized_reduce_parts') IS NOT NULL)
)
INSERT INTO summary_usage_stage_archive(record_key,legacy_json)
 SELECT 'compact.result:'||key,json_object(
  'result',CASE WHEN json_type(doc,'$.outcome.result.compact.summary_usage')='object'
    AND (json_type(doc,'$.outcome.result.compact.summary_usage.map_requests') IS NOT NULL OR json_type(doc,'$.outcome.result.compact.summary_usage.reduce_requests') IS NOT NULL OR json_type(doc,'$.outcome.result.compact.summary_usage.oversized_reduce_parts') IS NOT NULL)
    THEN json_object('map_requests',json_extract(doc,'$.outcome.result.compact.summary_usage.map_requests'),
      'reduce_requests',json_extract(doc,'$.outcome.result.compact.summary_usage.reduce_requests'),
      'oversized_reduce_parts',json_extract(doc,'$.outcome.result.compact.summary_usage.oversized_reduce_parts')) END,
  'error',CASE WHEN json_type(doc,'$.outcome.error.details.summary_usage')='object'
    AND (json_type(doc,'$.outcome.error.details.summary_usage.map_requests') IS NOT NULL OR json_type(doc,'$.outcome.error.details.summary_usage.reduce_requests') IS NOT NULL OR json_type(doc,'$.outcome.error.details.summary_usage.oversized_reduce_parts') IS NOT NULL)
    THEN json_object('map_requests',json_extract(doc,'$.outcome.error.details.summary_usage.map_requests'),
      'reduce_requests',json_extract(doc,'$.outcome.error.details.summary_usage.reduce_requests'),
      'oversized_reduce_parts',json_extract(doc,'$.outcome.error.details.summary_usage.oversized_reduce_parts')) END) FROM eligible;

WITH source AS (
 SELECT key,CASE WHEN json_valid(value) THEN value ELSE '{}' END AS doc
 FROM app_meta WHERE key LIKE 'compact.result.%'
), eligible AS (
 SELECT key,doc FROM source
 WHERE json_type(doc)='object'
   AND (json_type(doc,'$.outcome') IS NULL OR json_type(doc,'$.outcome')='object')
   AND (json_type(doc,'$.outcome.result') IS NULL OR json_type(doc,'$.outcome.result')='object')
   AND (json_type(doc,'$.outcome.result.compact') IS NULL OR json_type(doc,'$.outcome.result.compact')='object')
   AND (json_type(doc,'$.outcome.result.compact.summary_usage') IS NULL OR json_type(doc,'$.outcome.result.compact.summary_usage')='object')
   AND (json_type(doc,'$.outcome.error') IS NULL OR json_type(doc,'$.outcome.error')='object')
   AND (json_type(doc,'$.outcome.error.details') IS NULL OR json_type(doc,'$.outcome.error.details')='object')
   AND (json_type(doc,'$.outcome.error.details.summary_usage') IS NULL OR json_type(doc,'$.outcome.error.details.summary_usage')='object')
   AND (CASE WHEN json_type(doc,'$.outcome.result.compact.summary_usage.map_requests') IS NULL THEN 1 WHEN json_type(doc,'$.outcome.result.compact.summary_usage.map_requests')='integer' AND typeof(json_extract(doc,'$.outcome.result.compact.summary_usage.map_requests'))='integer' THEN 1 ELSE 0 END)
   AND (CASE WHEN json_type(doc,'$.outcome.result.compact.summary_usage.reduce_requests') IS NULL THEN 1 WHEN json_type(doc,'$.outcome.result.compact.summary_usage.reduce_requests')='integer' AND typeof(json_extract(doc,'$.outcome.result.compact.summary_usage.reduce_requests'))='integer' THEN 1 ELSE 0 END)
   AND (CASE WHEN json_type(doc,'$.outcome.result.compact.summary_usage.oversized_reduce_parts') IS NULL THEN 1 WHEN json_type(doc,'$.outcome.result.compact.summary_usage.oversized_reduce_parts')='integer' AND typeof(json_extract(doc,'$.outcome.result.compact.summary_usage.oversized_reduce_parts'))='integer' THEN 1 ELSE 0 END)
   AND (CASE WHEN json_type(doc,'$.outcome.error.details.summary_usage.map_requests') IS NULL THEN 1 WHEN json_type(doc,'$.outcome.error.details.summary_usage.map_requests')='integer' AND typeof(json_extract(doc,'$.outcome.error.details.summary_usage.map_requests'))='integer' THEN 1 ELSE 0 END)
   AND (CASE WHEN json_type(doc,'$.outcome.error.details.summary_usage.reduce_requests') IS NULL THEN 1 WHEN json_type(doc,'$.outcome.error.details.summary_usage.reduce_requests')='integer' AND typeof(json_extract(doc,'$.outcome.error.details.summary_usage.reduce_requests'))='integer' THEN 1 ELSE 0 END)
   AND (CASE WHEN json_type(doc,'$.outcome.error.details.summary_usage.oversized_reduce_parts') IS NULL THEN 1 WHEN json_type(doc,'$.outcome.error.details.summary_usage.oversized_reduce_parts')='integer' AND typeof(json_extract(doc,'$.outcome.error.details.summary_usage.oversized_reduce_parts'))='integer' THEN 1 ELSE 0 END)
   AND (json_type(doc,'$.outcome.result.compact.summary_usage.map_requests') IS NOT NULL OR json_type(doc,'$.outcome.result.compact.summary_usage.reduce_requests') IS NOT NULL OR json_type(doc,'$.outcome.result.compact.summary_usage.oversized_reduce_parts') IS NOT NULL OR json_type(doc,'$.outcome.error.details.summary_usage.map_requests') IS NOT NULL OR json_type(doc,'$.outcome.error.details.summary_usage.reduce_requests') IS NOT NULL OR json_type(doc,'$.outcome.error.details.summary_usage.oversized_reduce_parts') IS NOT NULL)
)
UPDATE app_meta SET value=json_remove(value,
 '$.outcome.result.compact.summary_usage.map_requests','$.outcome.result.compact.summary_usage.reduce_requests','$.outcome.result.compact.summary_usage.oversized_reduce_parts',
 '$.outcome.error.details.summary_usage.map_requests','$.outcome.error.details.summary_usage.reduce_requests','$.outcome.error.details.summary_usage.oversized_reduce_parts')
 WHERE key IN (SELECT key FROM eligible);

-- Automatic stage facts are nested under each committed stage in attempt JSON.
-- The safe CASE makes json_each see only a proven committed_stages object.  The
-- record is accepted atomically only if every traversed stage and any summary
-- usage object has its exact supported shape and all present counters fit i64.
WITH source AS (
 SELECT id,CASE WHEN json_valid(error_json) THEN error_json ELSE '{}' END AS doc
 FROM model_attempt WHERE error_json IS NOT NULL
), rooted AS (
 SELECT id,doc FROM source WHERE json_type(doc)='object'
), eligible AS (
 SELECT r.id,r.doc FROM rooted r
 WHERE json_type(r.doc,'$.automatic_trim_attempt')='object'
   AND json_type(r.doc,'$.automatic_trim_attempt.committed_stages')='object'
   AND NOT EXISTS (
    SELECT 1 FROM (
     SELECT raw_stage.key,raw_stage.type,
      CASE WHEN json_valid(raw_stage.value) THEN CASE WHEN json_type(raw_stage.value)='object' THEN raw_stage.value ELSE '{}' END ELSE '{}' END AS stage_doc
     FROM json_each(CASE WHEN json_type(r.doc,'$.automatic_trim_attempt')='object' AND json_type(r.doc,'$.automatic_trim_attempt.committed_stages')='object' THEN json_extract(r.doc,'$.automatic_trim_attempt.committed_stages') ELSE '{}' END) raw_stage
    ) stage
    WHERE stage.type!='object'
       OR (json_type(stage.stage_doc,'$.summary_usage') IS NOT NULL AND json_type(stage.stage_doc,'$.summary_usage')!='object')
       OR (CASE WHEN json_type(stage.stage_doc,'$.summary_usage.map_requests') IS NULL THEN 1 WHEN json_type(stage.stage_doc,'$.summary_usage.map_requests')='integer' AND typeof(json_extract(stage.stage_doc,'$.summary_usage.map_requests'))='integer' THEN 1 ELSE 0 END)=0
       OR (CASE WHEN json_type(stage.stage_doc,'$.summary_usage.reduce_requests') IS NULL THEN 1 WHEN json_type(stage.stage_doc,'$.summary_usage.reduce_requests')='integer' AND typeof(json_extract(stage.stage_doc,'$.summary_usage.reduce_requests'))='integer' THEN 1 ELSE 0 END)=0
       OR (CASE WHEN json_type(stage.stage_doc,'$.summary_usage.oversized_reduce_parts') IS NULL THEN 1 WHEN json_type(stage.stage_doc,'$.summary_usage.oversized_reduce_parts')='integer' AND typeof(json_extract(stage.stage_doc,'$.summary_usage.oversized_reduce_parts'))='integer' THEN 1 ELSE 0 END)=0
   )
   AND EXISTS (
    SELECT 1 FROM (
     SELECT raw_stage.key,raw_stage.type,
      CASE WHEN json_valid(raw_stage.value) THEN CASE WHEN json_type(raw_stage.value)='object' THEN raw_stage.value ELSE '{}' END ELSE '{}' END AS stage_doc
     FROM json_each(CASE WHEN json_type(r.doc,'$.automatic_trim_attempt')='object' AND json_type(r.doc,'$.automatic_trim_attempt.committed_stages')='object' THEN json_extract(r.doc,'$.automatic_trim_attempt.committed_stages') ELSE '{}' END) raw_stage
    ) stage
    WHERE json_type(stage.stage_doc,'$.summary_usage.map_requests') IS NOT NULL
       OR json_type(stage.stage_doc,'$.summary_usage.reduce_requests') IS NOT NULL
       OR json_type(stage.stage_doc,'$.summary_usage.oversized_reduce_parts') IS NOT NULL
   )
)
INSERT INTO summary_usage_stage_archive(record_key,legacy_json)
 SELECT 'attempt:'||eligible.id,json_object('committed_stages',json((
  SELECT json_group_object(stage.key,json_object('summary_usage',json_object(
    'map_requests',json_extract(stage.stage_doc,'$.summary_usage.map_requests'),
    'reduce_requests',json_extract(stage.stage_doc,'$.summary_usage.reduce_requests'),
    'oversized_reduce_parts',json_extract(stage.stage_doc,'$.summary_usage.oversized_reduce_parts'))))
  FROM (
     SELECT raw_stage.key,raw_stage.type,
      CASE WHEN json_valid(raw_stage.value) THEN CASE WHEN json_type(raw_stage.value)='object' THEN raw_stage.value ELSE '{}' END ELSE '{}' END AS stage_doc
     FROM json_each(CASE WHEN json_type(eligible.doc,'$.automatic_trim_attempt')='object' AND json_type(eligible.doc,'$.automatic_trim_attempt.committed_stages')='object' THEN json_extract(eligible.doc,'$.automatic_trim_attempt.committed_stages') ELSE '{}' END) raw_stage
    ) stage
  WHERE json_type(stage.stage_doc,'$.summary_usage.map_requests') IS NOT NULL
     OR json_type(stage.stage_doc,'$.summary_usage.reduce_requests') IS NOT NULL
     OR json_type(stage.stage_doc,'$.summary_usage.oversized_reduce_parts') IS NOT NULL))) FROM eligible;

WITH source AS (
 SELECT id,CASE WHEN json_valid(error_json) THEN error_json ELSE '{}' END AS doc
 FROM model_attempt WHERE error_json IS NOT NULL
), rooted AS (
 SELECT id,doc FROM source WHERE json_type(doc)='object'
), eligible AS (
 SELECT r.id,r.doc FROM rooted r
 WHERE json_type(r.doc,'$.automatic_trim_attempt')='object'
   AND json_type(r.doc,'$.automatic_trim_attempt.committed_stages')='object'
   AND NOT EXISTS (
    SELECT 1 FROM (
     SELECT raw_stage.key,raw_stage.type,
      CASE WHEN json_valid(raw_stage.value) THEN CASE WHEN json_type(raw_stage.value)='object' THEN raw_stage.value ELSE '{}' END ELSE '{}' END AS stage_doc
     FROM json_each(CASE WHEN json_type(r.doc,'$.automatic_trim_attempt')='object' AND json_type(r.doc,'$.automatic_trim_attempt.committed_stages')='object' THEN json_extract(r.doc,'$.automatic_trim_attempt.committed_stages') ELSE '{}' END) raw_stage
    ) stage
    WHERE stage.type!='object'
       OR (json_type(stage.stage_doc,'$.summary_usage') IS NOT NULL AND json_type(stage.stage_doc,'$.summary_usage')!='object')
       OR (CASE WHEN json_type(stage.stage_doc,'$.summary_usage.map_requests') IS NULL THEN 1 WHEN json_type(stage.stage_doc,'$.summary_usage.map_requests')='integer' AND typeof(json_extract(stage.stage_doc,'$.summary_usage.map_requests'))='integer' THEN 1 ELSE 0 END)=0
       OR (CASE WHEN json_type(stage.stage_doc,'$.summary_usage.reduce_requests') IS NULL THEN 1 WHEN json_type(stage.stage_doc,'$.summary_usage.reduce_requests')='integer' AND typeof(json_extract(stage.stage_doc,'$.summary_usage.reduce_requests'))='integer' THEN 1 ELSE 0 END)=0
       OR (CASE WHEN json_type(stage.stage_doc,'$.summary_usage.oversized_reduce_parts') IS NULL THEN 1 WHEN json_type(stage.stage_doc,'$.summary_usage.oversized_reduce_parts')='integer' AND typeof(json_extract(stage.stage_doc,'$.summary_usage.oversized_reduce_parts'))='integer' THEN 1 ELSE 0 END)=0
   )
   AND EXISTS (
    SELECT 1 FROM (
     SELECT raw_stage.key,raw_stage.type,
      CASE WHEN json_valid(raw_stage.value) THEN CASE WHEN json_type(raw_stage.value)='object' THEN raw_stage.value ELSE '{}' END ELSE '{}' END AS stage_doc
     FROM json_each(CASE WHEN json_type(r.doc,'$.automatic_trim_attempt')='object' AND json_type(r.doc,'$.automatic_trim_attempt.committed_stages')='object' THEN json_extract(r.doc,'$.automatic_trim_attempt.committed_stages') ELSE '{}' END) raw_stage
    ) stage
    WHERE json_type(stage.stage_doc,'$.summary_usage.map_requests') IS NOT NULL
       OR json_type(stage.stage_doc,'$.summary_usage.reduce_requests') IS NOT NULL
       OR json_type(stage.stage_doc,'$.summary_usage.oversized_reduce_parts') IS NOT NULL
   )
)
UPDATE model_attempt SET error_json=json_set(error_json,'$.automatic_trim_attempt.committed_stages',
 (SELECT json_group_object(stage.key,json(json_remove(stage.stage_doc,
  '$.summary_usage.map_requests','$.summary_usage.reduce_requests','$.summary_usage.oversized_reduce_parts')))
  FROM (
     SELECT raw_stage.key,raw_stage.type,
      CASE WHEN json_valid(raw_stage.value) THEN CASE WHEN json_type(raw_stage.value)='object' THEN raw_stage.value ELSE '{}' END ELSE '{}' END AS stage_doc
     FROM json_each(CASE WHEN json_type(eligible.doc,'$.automatic_trim_attempt')='object' AND json_type(eligible.doc,'$.automatic_trim_attempt.committed_stages')='object' THEN json_extract(eligible.doc,'$.automatic_trim_attempt.committed_stages') ELSE '{}' END) raw_stage
    ) stage))
FROM eligible WHERE model_attempt.id=eligible.id;

INSERT INTO schema_migration(version,name) VALUES(43,'0043_thread_single_summary_settings');
PRAGMA user_version=43;
