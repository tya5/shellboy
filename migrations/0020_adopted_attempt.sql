ALTER TABLE thread ADD COLUMN last_adopted_attempt_id TEXT
    REFERENCES model_attempt(id) ON DELETE SET NULL;
CREATE INDEX thread_last_adopted_attempt ON thread(last_adopted_attempt_id)
    WHERE last_adopted_attempt_id IS NOT NULL;
-- Completed attempts passed adoption CAS; admission is serial per Thread.
WITH latest AS MATERIALIZED (
    SELECT u.thread_id, MAX(a.rowid) AS attempt_row
    FROM model_attempt a JOIN turn u ON u.id=a.turn_id
    WHERE a.state='completed' GROUP BY u.thread_id
)
UPDATE thread SET last_adopted_attempt_id=(
    SELECT a.id FROM latest l JOIN model_attempt a ON a.rowid=l.attempt_row
    WHERE l.thread_id=thread.id
);
INSERT INTO schema_migration(version,name) VALUES(20,'0020_adopted_attempt');
PRAGMA user_version=20;
