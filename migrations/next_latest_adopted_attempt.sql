-- Equality on all three keys permits a reverse walk of the implicit rowid tie-breaker.
CREATE INDEX attempts_latest_adopted ON model_attempt(thread_id,profile_id,model_id)
 WHERE kind='conversation' AND state='completed' AND json_extract(input_manifest_json,'$.adoption.outcome')='adopted';
INSERT INTO schema_migration(version,name) VALUES(74,'next_latest_adopted_attempt');
PRAGMA user_version=74;
