-- Test-only inverse through schema 50 for fixtures made by a current owner.
DROP INDEX receipt_named_reply_pending;
DROP INDEX receipt_pending_by_time;
DROP INDEX receipt_pending_owed;
DROP INDEX receipt_pending_asked;
DROP INDEX receipt_pending_to;
DELETE FROM schema_migration WHERE version=56;
DROP INDEX threads_created;
ALTER TABLE thread DROP COLUMN created_at;
DELETE FROM schema_migration WHERE version=54;
DROP TABLE automatic_compaction;
CREATE INDEX automatic_compaction_turn ON model_attempt(turn_id,started_at,id) WHERE CASE WHEN json_valid(error_json) THEN json_type(error_json,'$.automatic_trim')='object' OR json_type(error_json,'$.automatic_trim_attempt')='object' ELSE 0 END;
CREATE TEMP TABLE saved_shells AS SELECT id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,saved_sequence,retention_generation,exit_code,end_reason,output_complete,started_at,ended_at FROM shell_execution;
CREATE TEMP TABLE saved_chunks AS SELECT * FROM output_chunk;
DROP TABLE output_chunk;
DROP TABLE shell_execution;
DROP TABLE schedule;
CREATE TABLE shell_execution(
 id TEXT PRIMARY KEY NOT NULL, owner_run_id TEXT NOT NULL REFERENCES run(id),
 turn_id TEXT NOT NULL REFERENCES turn(id), operation_id TEXT NOT NULL UNIQUE,
 state TEXT NOT NULL CHECK(state IN ('starting','running','stopping','exited','failed','stopped')), cwd TEXT NOT NULL, shell_json TEXT NOT NULL CHECK(json_valid(shell_json)),
 input_encoding TEXT NOT NULL, output_encoding TEXT NOT NULL, stdin_state TEXT NOT NULL,
 saved_sequence INTEGER NOT NULL DEFAULT 0 CHECK(saved_sequence>=0), retention_generation INTEGER NOT NULL DEFAULT 0 CHECK(retention_generation>=0),
 exit_code INTEGER, end_reason TEXT, output_complete INTEGER CHECK(output_complete IS NULL OR output_complete IN (0,1)), started_at TEXT NOT NULL, ended_at TEXT);
CREATE TABLE output_chunk(
 execution_id TEXT NOT NULL REFERENCES shell_execution(id), sequence INTEGER NOT NULL CHECK(sequence>=0),
 stream TEXT NOT NULL CHECK(stream IN ('stdout','stderr')),
 stream_offset INTEGER NOT NULL CHECK(stream_offset>=0), bytes BLOB NOT NULL,
 received_at TEXT NOT NULL, PRIMARY KEY(execution_id,sequence));
INSERT INTO shell_execution SELECT * FROM saved_shells;
INSERT INTO output_chunk SELECT * FROM saved_chunks;
DROP TABLE saved_shells;
DROP TABLE saved_chunks;
CREATE INDEX output_stream ON output_chunk(execution_id,stream,stream_offset);
CREATE INDEX gc_bytes_output_chunk ON output_chunk(length(bytes));
CREATE INDEX shell_active_owner ON shell_execution(owner_run_id,state)
 WHERE state IN ('starting','running','stopping');
CREATE INDEX shell_terminal_time ON shell_execution(COALESCE(ended_at,started_at))
 WHERE state IN ('exited','failed','stopped');
CREATE INDEX shell_started_readback ON shell_execution((CASE WHEN instr(started_at,char(0))=0 AND CAST(substr(started_at,12,2) AS INTEGER)<=23 AND strftime('%Y-%m-%dT%H:%M:%S',upper(substr(started_at,1,17)||CASE WHEN substr(started_at,18,2)='60' THEN '59' ELSE substr(started_at,18,2) END)||'Z')=upper(substr(started_at,1,17)||CASE WHEN substr(started_at,18,2)='60' THEN '59' ELSE substr(started_at,18,2) END) AND (CASE WHEN lower(substr(started_at,-1))='z' THEN 1 ELSE substr(started_at,-6) GLOB '[+-][0-2][0-9]:[0-5][0-9]' AND CAST(substr(started_at,-5,2) AS INTEGER)<=23 END) AND (CASE WHEN substr(started_at,20,1)='.' THEN length(substr(started_at,21,length(started_at)-20-(CASE WHEN lower(substr(started_at,-1))='z' THEN 1 ELSE 6 END)))>0 AND substr(started_at,21,length(started_at)-20-(CASE WHEN lower(substr(started_at,-1))='z' THEN 1 ELSE 6 END)) NOT GLOB '*[^0-9]*' ELSE length(started_at)=19+(CASE WHEN lower(substr(started_at,-1))='z' THEN 1 ELSE 6 END) END) THEN unixepoch(upper(substr(started_at,1,17)||CASE WHEN substr(started_at,18,2)='60' THEN '59' ELSE substr(started_at,18,2) END)||'Z')-(CASE WHEN lower(substr(started_at,-1))='z' THEN 0 ELSE (CASE WHEN substr(started_at,-6,1)='-' THEN -1 ELSE 1 END)*(CAST(substr(started_at,-5,2) AS INTEGER)*3600+CAST(substr(started_at,-2) AS INTEGER)*60) END) ELSE -9223372036854775808 END),(CASE WHEN instr(started_at,char(0))=0 AND CAST(substr(started_at,12,2) AS INTEGER)<=23 AND strftime('%Y-%m-%dT%H:%M:%S',upper(substr(started_at,1,17)||CASE WHEN substr(started_at,18,2)='60' THEN '59' ELSE substr(started_at,18,2) END)||'Z')=upper(substr(started_at,1,17)||CASE WHEN substr(started_at,18,2)='60' THEN '59' ELSE substr(started_at,18,2) END) AND (CASE WHEN lower(substr(started_at,-1))='z' THEN 1 ELSE substr(started_at,-6) GLOB '[+-][0-2][0-9]:[0-5][0-9]' AND CAST(substr(started_at,-5,2) AS INTEGER)<=23 END) AND (CASE WHEN substr(started_at,20,1)='.' THEN length(substr(started_at,21,length(started_at)-20-(CASE WHEN lower(substr(started_at,-1))='z' THEN 1 ELSE 6 END)))>0 AND substr(started_at,21,length(started_at)-20-(CASE WHEN lower(substr(started_at,-1))='z' THEN 1 ELSE 6 END)) NOT GLOB '*[^0-9]*' ELSE length(started_at)=19+(CASE WHEN lower(substr(started_at,-1))='z' THEN 1 ELSE 6 END) END) THEN (CASE WHEN substr(started_at,20,1)='.' THEN CAST(substr((substr(started_at,21,length(started_at)-20-(CASE WHEN lower(substr(started_at,-1))='z' THEN 1 ELSE 6 END)))||'000000000',1,9) AS INTEGER) ELSE 0 END)+(CASE WHEN substr(started_at,18,2)='60' THEN 1000000000 ELSE 0 END) ELSE 0 END),id);
CREATE INDEX shell_owner_started_readback ON shell_execution(owner_run_id,(CASE WHEN instr(started_at,char(0))=0 AND CAST(substr(started_at,12,2) AS INTEGER)<=23 AND strftime('%Y-%m-%dT%H:%M:%S',upper(substr(started_at,1,17)||CASE WHEN substr(started_at,18,2)='60' THEN '59' ELSE substr(started_at,18,2) END)||'Z')=upper(substr(started_at,1,17)||CASE WHEN substr(started_at,18,2)='60' THEN '59' ELSE substr(started_at,18,2) END) AND (CASE WHEN lower(substr(started_at,-1))='z' THEN 1 ELSE substr(started_at,-6) GLOB '[+-][0-2][0-9]:[0-5][0-9]' AND CAST(substr(started_at,-5,2) AS INTEGER)<=23 END) AND (CASE WHEN substr(started_at,20,1)='.' THEN length(substr(started_at,21,length(started_at)-20-(CASE WHEN lower(substr(started_at,-1))='z' THEN 1 ELSE 6 END)))>0 AND substr(started_at,21,length(started_at)-20-(CASE WHEN lower(substr(started_at,-1))='z' THEN 1 ELSE 6 END)) NOT GLOB '*[^0-9]*' ELSE length(started_at)=19+(CASE WHEN lower(substr(started_at,-1))='z' THEN 1 ELSE 6 END) END) THEN unixepoch(upper(substr(started_at,1,17)||CASE WHEN substr(started_at,18,2)='60' THEN '59' ELSE substr(started_at,18,2) END)||'Z')-(CASE WHEN lower(substr(started_at,-1))='z' THEN 0 ELSE (CASE WHEN substr(started_at,-6,1)='-' THEN -1 ELSE 1 END)*(CAST(substr(started_at,-5,2) AS INTEGER)*3600+CAST(substr(started_at,-2) AS INTEGER)*60) END) ELSE -9223372036854775808 END),(CASE WHEN instr(started_at,char(0))=0 AND CAST(substr(started_at,12,2) AS INTEGER)<=23 AND strftime('%Y-%m-%dT%H:%M:%S',upper(substr(started_at,1,17)||CASE WHEN substr(started_at,18,2)='60' THEN '59' ELSE substr(started_at,18,2) END)||'Z')=upper(substr(started_at,1,17)||CASE WHEN substr(started_at,18,2)='60' THEN '59' ELSE substr(started_at,18,2) END) AND (CASE WHEN lower(substr(started_at,-1))='z' THEN 1 ELSE substr(started_at,-6) GLOB '[+-][0-2][0-9]:[0-5][0-9]' AND CAST(substr(started_at,-5,2) AS INTEGER)<=23 END) AND (CASE WHEN substr(started_at,20,1)='.' THEN length(substr(started_at,21,length(started_at)-20-(CASE WHEN lower(substr(started_at,-1))='z' THEN 1 ELSE 6 END)))>0 AND substr(started_at,21,length(started_at)-20-(CASE WHEN lower(substr(started_at,-1))='z' THEN 1 ELSE 6 END)) NOT GLOB '*[^0-9]*' ELSE length(started_at)=19+(CASE WHEN lower(substr(started_at,-1))='z' THEN 1 ELSE 6 END) END) THEN (CASE WHEN substr(started_at,20,1)='.' THEN CAST(substr((substr(started_at,21,length(started_at)-20-(CASE WHEN lower(substr(started_at,-1))='z' THEN 1 ELSE 6 END)))||'000000000',1,9) AS INTEGER) ELSE 0 END)+(CASE WHEN substr(started_at,18,2)='60' THEN 1000000000 ELSE 0 END) ELSE 0 END),id);
CREATE INDEX activity_shell_spawned_turn ON shell_execution(turn_id) WHERE CASE WHEN json_valid(shell_json) THEN json_extract(shell_json,'$.spawned')=1 ELSE 1 END;
CREATE INDEX activity_shell_uncollected_owner ON shell_execution(owner_run_id) WHERE state IN ('exited','failed','stopped') AND output_complete=0;
CREATE TRIGGER logical_raw_output_chunk_insert AFTER INSERT ON output_chunk BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(length(NEW.bytes),0)),revision=revision+1 WHERE category='logical.raw';
END;
CREATE TRIGGER logical_raw_output_chunk_delete AFTER DELETE ON output_chunk BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+-(COALESCE(length(OLD.bytes),0)),revision=revision+1 WHERE category='logical.raw';
END;
CREATE TRIGGER logical_raw_output_chunk_update AFTER UPDATE OF bytes ON output_chunk BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE(length(NEW.bytes),0))-(COALESCE(length(OLD.bytes),0)),revision=revision+1 WHERE category='logical.raw';
END;
DELETE FROM schema_migration WHERE version>=51;
PRAGMA user_version=50;
