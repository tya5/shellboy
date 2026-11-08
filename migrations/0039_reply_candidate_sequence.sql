-- Keep reply-duty candidate enumeration seekable across snapshot watermarks.
ALTER TABLE receipt ADD COLUMN arrival_seq INTEGER NOT NULL DEFAULT 0;
UPDATE receipt SET arrival_seq=rowid;
CREATE TABLE receipt_arrival_clock(id INTEGER PRIMARY KEY CHECK(id=1), seq INTEGER NOT NULL);
INSERT INTO receipt_arrival_clock(id,seq) SELECT 1,COALESCE(MAX(arrival_seq),0) FROM receipt;
CREATE TRIGGER receipt_arrival_sequence AFTER INSERT ON receipt BEGIN
 UPDATE receipt_arrival_clock SET seq=seq+1 WHERE id=1;
 UPDATE receipt SET arrival_seq=(SELECT seq FROM receipt_arrival_clock WHERE id=1) WHERE rowid=NEW.rowid;
END;
DROP INDEX receipt_human_reply_pending;
CREATE INDEX receipt_human_reply_pending ON receipt(target_thread_id,arrival_seq)
 WHERE json_extract(result_json,'$.reply.state')='pending'
   AND json_extract(result_json,'$.to_human')=1;
INSERT INTO schema_migration(version,name) VALUES(39,'0039_reply_candidate_sequence');
PRAGMA user_version=39;
