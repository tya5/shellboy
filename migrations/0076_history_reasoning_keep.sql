CREATE TABLE history_reasoning_keep(
 thread_id TEXT NOT NULL REFERENCES thread(id), epoch INTEGER NOT NULL, node_id TEXT NOT NULL,
 PRIMARY KEY(thread_id,epoch,node_id)) WITHOUT ROWID;
INSERT OR IGNORE INTO history_reasoning_keep(thread_id,epoch,node_id)
 SELECT t.id,t.reasoning_epoch,j.value FROM app_meta m
 JOIN thread t ON m.key='reasoning.keep.'||t.id
 CROSS JOIN json_each(CASE WHEN json_valid(m.value) THEN m.value ELSE '{}' END,'$.nodes') j
 WHERE j.type='text' AND json_type(CASE WHEN json_valid(m.value) THEN m.value ELSE '{}' END,'$.nodes')='array'
 AND json_type(CASE WHEN json_valid(m.value) THEN m.value ELSE '{}' END,'$.epoch')='integer'
 AND json_extract(CASE WHEN json_valid(m.value) THEN m.value ELSE '{}' END,'$.epoch')=t.reasoning_epoch;
DELETE FROM app_meta WHERE key GLOB 'reasoning.keep.*';
INSERT INTO schema_migration(version,name) VALUES(76,'0076_history_reasoning_keep');
PRAGMA user_version=76;
