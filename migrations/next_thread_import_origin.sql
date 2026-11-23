ALTER TABLE node ADD COLUMN import_origin_json TEXT;
INSERT INTO schema_migration(version,name) VALUES(57,'next_thread_import_origin');
PRAGMA user_version=57;
