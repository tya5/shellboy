-- Derived metadata only: preserve jobs, content, current counters and pauses.
DELETE FROM app_meta
WHERE key GLOB 'index.common_failures.*'
  AND CASE WHEN json_valid(value) THEN
    json_type(value) <> 'object'
    OR COALESCE(json_type(value,'$.cause'),'') <> 'text'
    OR COALESCE(json_type(value,'$.count'),'') <> 'integer'
    OR json_extract(value,'$.count') < 0
  ELSE 1 END;
DROP INDEX IF EXISTS receipt_turn_source;
INSERT INTO schema_migration(version,name) VALUES(11,'0011_current_index_metadata');
PRAGMA user_version=11;
