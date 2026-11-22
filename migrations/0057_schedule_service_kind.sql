CREATE TABLE service_schedule(
 id TEXT PRIMARY KEY NOT NULL, thread_id TEXT NOT NULL REFERENCES thread(id) ON DELETE CASCADE,
 kind TEXT NOT NULL DEFAULT 'time' CHECK(kind IN ('time','service')),
 cron TEXT, once_at INTEGER, next_at INTEGER, enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
 cwd TEXT NOT NULL, command TEXT NOT NULL, description TEXT, last_skipped_at INTEGER,
 CHECK((kind='time' AND ((cron IS NULL) <> (once_at IS NULL)))
    OR (kind='service' AND cron IS NULL AND once_at IS NULL)));
INSERT INTO service_schedule(id,thread_id,cron,once_at,next_at,enabled,cwd,command,description,last_skipped_at)
 SELECT id,thread_id,cron,once_at,next_at,enabled,cwd,command,description,last_skipped_at FROM schedule;
DROP TABLE schedule;
ALTER TABLE service_schedule RENAME TO schedule;
CREATE INDEX schedule_thread ON schedule(thread_id,id);
CREATE INDEX schedule_due ON schedule(next_at,id) WHERE enabled=1 AND next_at IS NOT NULL;
INSERT INTO schema_migration(version,name) VALUES(57,'0057_schedule_service_kind');
PRAGMA user_version=57;
