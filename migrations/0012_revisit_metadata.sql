-- Extend existing derived logical-history accounting; no new state table.
DROP INDEX gc_bytes_app_meta_history;
CREATE INDEX gc_bytes_app_meta_history ON app_meta(octet_length(value)) WHERE key LIKE 'summary.part.%' OR key LIKE 'wait.pending.%' OR key LIKE 'compact.result.%' OR key LIKE 'revisit.%';
INSERT INTO schema_migration(version,name) VALUES(12,'0012_revisit_metadata');
PRAGMA user_version=12;
