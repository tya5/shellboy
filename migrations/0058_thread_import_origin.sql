ALTER TABLE node ADD COLUMN import_origin_json TEXT;
INSERT INTO schema_migration(version,name) VALUES(58,'0058_thread_import_origin');
PRAGMA user_version=58;
