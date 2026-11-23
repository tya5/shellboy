ALTER TABLE node ADD COLUMN output_limit_attempt_id TEXT REFERENCES model_attempt(id);
INSERT INTO schema_migration(version,name) VALUES(57,'0057_answer_output_limit');
PRAGMA user_version=57;
