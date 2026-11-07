-- Thread tags (#222 stage 3): many per Thread, placement and cross-cutting views.
-- The primary key bounds a Thread's own tags; thread_tag_by_tag drives `thread list --tag`.
CREATE TABLE thread_tag(
 thread_id TEXT NOT NULL REFERENCES thread(id), tag TEXT NOT NULL CHECK(length(tag) BETWEEN 1 AND 128),
 PRIMARY KEY(thread_id,tag)) WITHOUT ROWID;
CREATE INDEX thread_tag_by_tag ON thread_tag(tag,thread_id);
-- `thread list --cwd` reads one working directory in list order without scanning other Threads.
CREATE INDEX threads_cwd_updated ON thread(cwd,updated_at DESC,id DESC) WHERE deleted_at IS NULL;
INSERT INTO schema_migration(version,name) VALUES(34,'0034_thread_tags');
PRAGMA user_version=34;
