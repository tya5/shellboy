-- History owns the replay-prefix revision. Tail append preserves the prefix;
-- every edit invalidates it in the same transaction, including shared nodes.
ALTER TABLE thread ADD COLUMN replay_path_revision INTEGER NOT NULL DEFAULT 0
 CHECK(typeof(replay_path_revision)='integer' AND replay_path_revision>=0);
CREATE TRIGGER replay_path_insert BEFORE INSERT ON thread_path
WHEN NEW.ordinal != COALESCE((SELECT max(ordinal)+1 FROM thread_path WHERE thread_id=NEW.thread_id),0)
 OR EXISTS(SELECT 1 FROM thread_path WHERE thread_id=NEW.thread_id AND node_id=NEW.node_id)
BEGIN
 UPDATE thread SET replay_path_revision=replay_path_revision+1 WHERE id=NEW.thread_id;
END;
CREATE TRIGGER replay_path_remove AFTER DELETE ON thread_path BEGIN
 UPDATE thread SET replay_path_revision=replay_path_revision+1 WHERE id=OLD.thread_id;
END;
CREATE TRIGGER replay_path_edit AFTER UPDATE OF thread_id,ordinal,node_id ON thread_path BEGIN
 UPDATE thread SET replay_path_revision=replay_path_revision+1 WHERE id IN (OLD.thread_id,NEW.thread_id);
END;
CREATE TRIGGER replay_node_edit AFTER UPDATE OF payload_json,payload_revision,deleted_reason,kind,import_origin_json,turn_id ON node
WHEN OLD.payload_revision IS NOT NEW.payload_revision OR OLD.deleted_reason IS NOT NEW.deleted_reason
 OR OLD.kind IS NOT NEW.kind OR (OLD.import_origin_json IS NULL) != (NEW.import_origin_json IS NULL)
 OR OLD.turn_id IS NOT NEW.turn_id OR OLD.payload_json IS NOT NEW.payload_json
BEGIN
 UPDATE thread SET replay_path_revision=replay_path_revision+1
 WHERE id IN (SELECT thread_id FROM thread_path WHERE node_id=NEW.id);
END;
CREATE TRIGGER replay_replacement_state AFTER UPDATE OF state,output_node_id ON replacement
WHEN OLD.state IS NOT NEW.state OR OLD.output_node_id IS NOT NEW.output_node_id
BEGIN
 UPDATE thread SET replay_path_revision=replay_path_revision+1
 WHERE id IN (SELECT thread_id FROM thread_path WHERE node_id IN (OLD.output_node_id,NEW.output_node_id));
END;
CREATE TRIGGER replay_replacement_add AFTER INSERT ON thread_replacement BEGIN
 UPDATE thread SET replay_path_revision=replay_path_revision+1 WHERE id=NEW.thread_id;
END;
CREATE TRIGGER replay_replacement_remove AFTER DELETE ON thread_replacement BEGIN
 UPDATE thread SET replay_path_revision=replay_path_revision+1 WHERE id=OLD.thread_id;
END;
CREATE TRIGGER replay_replacement_edit AFTER UPDATE OF thread_id,replacement_id ON thread_replacement BEGIN
 UPDATE thread SET replay_path_revision=replay_path_revision+1 WHERE id IN (OLD.thread_id,NEW.thread_id);
END;
-- Preamble inputs belong to the receipt, not the immutable message body.
-- Exclude delivery-only JSON patches and non-admitted receipts.
CREATE TRIGGER replay_receipt_add AFTER INSERT ON receipt WHEN NEW.path_admission='admitted' BEGIN
 UPDATE thread SET replay_path_revision=replay_path_revision+1
 WHERE id IN (SELECT thread_id FROM thread_path WHERE node_id=NEW.message_node_id);
END;
CREATE TRIGGER replay_receipt_remove AFTER DELETE ON receipt WHEN OLD.path_admission='admitted' BEGIN
 UPDATE thread SET replay_path_revision=replay_path_revision+1
 WHERE id IN (SELECT thread_id FROM thread_path WHERE node_id=OLD.message_node_id);
END;
CREATE TRIGGER replay_receipt_edit AFTER UPDATE OF message_node_id,target_thread_id,path_admission,sender_json,result_json ON receipt
WHEN (OLD.path_admission='admitted' OR NEW.path_admission='admitted') AND (
 OLD.path_admission IS NOT NEW.path_admission OR OLD.sender_json IS NOT NEW.sender_json
 OR OLD.message_node_id IS NOT NEW.message_node_id OR OLD.target_thread_id IS NOT NEW.target_thread_id
 OR json_extract(OLD.result_json,'$.expect_reply') IS NOT json_extract(NEW.result_json,'$.expect_reply')
 OR json_type(OLD.result_json,'$.reply_to') IS NOT json_type(NEW.result_json,'$.reply_to')
 OR json_extract(OLD.result_json,'$.reply_to') IS NOT json_extract(NEW.result_json,'$.reply_to')
 OR json_extract(OLD.result_json,'$.to_human') IS NOT json_extract(NEW.result_json,'$.to_human'))
BEGIN
 UPDATE thread SET replay_path_revision=replay_path_revision+1
 WHERE id IN (SELECT thread_id FROM thread_path WHERE node_id IN (OLD.message_node_id,NEW.message_node_id));
END;
INSERT INTO schema_migration(version,name) VALUES(75,'0075_replay_path_revision');
PRAGMA user_version=75;
