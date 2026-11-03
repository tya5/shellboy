-- Revisit is an ordinary held message (#187): drop the #36 per-Run
-- reservation metadata (not moved) and index completion deliveries per Run.
-- The history-byte pattern for revisit.% stays in the #171 counters; no row
-- uses it after this delete.
DELETE FROM app_meta WHERE key GLOB 'revisit.*';
CREATE INDEX IF NOT EXISTS receipt_run_completion ON receipt(run_id) WHERE json_extract(result_json,'$.deliver')='complete';
INSERT INTO schema_migration(version,name) VALUES(24,'0024_revisit_messages');
PRAGMA user_version=24;
