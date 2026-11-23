ALTER TABLE run ADD COLUMN instruction_warnings_json TEXT;
INSERT INTO schema_migration(version,name) VALUES(62,'next_instruction_warnings');
PRAGMA user_version=62;
