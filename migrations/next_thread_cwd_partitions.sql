CREATE INDEX threads_visible_cwd_updated ON thread(cwd,updated_at DESC,id DESC) WHERE deleted_at IS NULL AND archived_at IS NULL;
CREATE INDEX threads_archived_cwd_updated ON thread(cwd,updated_at DESC,id DESC) WHERE deleted_at IS NULL AND archived_at IS NOT NULL;
INSERT INTO schema_migration(version,name) VALUES(64,'next_thread_cwd_partitions');
PRAGMA user_version=64;
