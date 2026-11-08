DROP TRIGGER IF EXISTS reject_owner_cycle_insert;
DROP TRIGGER IF EXISTS reject_owner_cycle_update;
DROP INDEX IF EXISTS runs_by_owner;
CREATE TABLE independent_run(
 id TEXT PRIMARY KEY NOT NULL, thread_id TEXT NOT NULL REFERENCES thread(id),
 state TEXT NOT NULL CHECK(state IN ('live','stopping','completed','stopped','failed')),
 created_at TEXT NOT NULL, ended_at TEXT, stop_reason TEXT, UNIQUE(id,thread_id));
INSERT INTO independent_run(id,thread_id,state,created_at,ended_at,stop_reason)
 SELECT id,thread_id,state,created_at,ended_at,stop_reason FROM run;
DROP TABLE run;
ALTER TABLE independent_run RENAME TO run;
CREATE UNIQUE INDEX one_live_run ON run(thread_id) WHERE state IN ('live','stopping');
INSERT INTO schema_migration(version,name) VALUES(37,'0037_independent_runs');
PRAGMA user_version=37;
