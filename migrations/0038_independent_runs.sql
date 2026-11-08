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
CREATE INDEX run_thread_created ON run(thread_id,created_at DESC,id DESC);
CREATE INDEX wait_run_state ON run(thread_id,state);
UPDATE event SET kind='recovery_wait' WHERE kind='child_recovery_wait';
DROP INDEX wait_recovery_thread;
CREATE INDEX wait_recovery_thread ON event(json_extract(payload_ref_json,'$.thread_id')) WHERE kind='recovery_wait';
CREATE INDEX receipt_reply_run_pending ON receipt(run_id,state) WHERE json_extract(result_json,'$.reply.state')='pending' AND json_extract(result_json,'$.to_human') IS NOT 1;
INSERT INTO schema_migration(version,name) VALUES(38,'0038_independent_runs');
PRAGMA user_version=38;
