ALTER TABLE node ADD COLUMN output_limit_attempt_id TEXT REFERENCES model_attempt(id);
ALTER TABLE node ADD COLUMN output_limit_json TEXT CHECK(output_limit_json IS NULL OR (kind='assistant_response' AND output_limit_attempt_id IS NOT NULL AND octet_length(output_limit_json)<=256 AND json_valid(output_limit_json)));
INSERT INTO schema_migration(version,name) VALUES(57,'next_answer_output_limit');
PRAGMA user_version=57;
