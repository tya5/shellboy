-- Bound startup recovery to the interrupted jobs rather than the whole queue.
CREATE INDEX IF NOT EXISTS jobs_running_recovery ON embedding_job(id) WHERE state='running';
INSERT INTO schema_migration(version,name) VALUES(25,'0025_index_recovery');
PRAGMA user_version=25;
