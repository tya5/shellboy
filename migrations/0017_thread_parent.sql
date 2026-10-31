CREATE INDEX thread_parent ON thread(parent_thread_id,id);
INSERT INTO schema_migration(version,name) VALUES(17,'0017_thread_parent');
PRAGMA user_version=17;
