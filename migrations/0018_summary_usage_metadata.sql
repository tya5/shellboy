-- Per-compaction summary request usage (#133) lives in app_meta; extend the
-- existing derived logical-history accounting. No new state table.
DROP INDEX gc_bytes_app_meta_history;
CREATE INDEX gc_bytes_app_meta_history ON app_meta(octet_length(value)) WHERE key LIKE 'summary.part.%' OR key LIKE 'wait.pending.%' OR key LIKE 'compact.result.%' OR key LIKE 'revisit.%' OR key LIKE 'summary.usage.%';
INSERT INTO schema_migration(version,name) VALUES(18,'0018_summary_usage_metadata');
PRAGMA user_version=18;
