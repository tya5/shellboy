-- Archive: hide a Thread from the default list without deleting it (#222 stage 1). NULL = not archived.
ALTER TABLE thread ADD COLUMN archived_at TEXT;
CREATE INDEX threads_visible_updated ON thread(updated_at DESC,id DESC) WHERE deleted_at IS NULL AND archived_at IS NULL;
CREATE INDEX threads_archived_updated ON thread(updated_at DESC,id DESC) WHERE deleted_at IS NULL AND archived_at IS NOT NULL;
INSERT INTO schema_migration(version,name) VALUES(32,'0032_thread_archive');
PRAGMA user_version=32;
