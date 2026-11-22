ALTER TABLE thread ADD COLUMN created_at TEXT;

-- A shared path does not make an inherited node an activity of the fork.
WITH activity(thread_id, at) AS (
    SELECT thread_id, created_at FROM turn
    UNION ALL SELECT target_thread_id, created_at FROM receipt
    UNION ALL SELECT t.thread_id, n.created_at FROM node n JOIN turn t ON t.id=n.turn_id
    UNION ALL SELECT r.target_thread_id, n.created_at FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE n.turn_id IS NULL
), earliest AS (
    SELECT thread_id, MIN(at) AS at FROM activity GROUP BY thread_id
)
UPDATE thread SET created_at=earliest.at FROM earliest WHERE thread.id=earliest.thread_id AND thread.created_at IS NULL;

CREATE INDEX IF NOT EXISTS threads_created ON thread((archived_at IS NOT NULL),created_at DESC,id DESC) WHERE deleted_at IS NULL;

INSERT INTO schema_migration(version,name) VALUES(54,'0054_thread_created_at');
PRAGMA user_version=54;
