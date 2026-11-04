CREATE TABLE IF NOT EXISTS tool_group_recovery_candidate(
 group_id TEXT PRIMARY KEY NOT NULL
);

CREATE TRIGGER IF NOT EXISTS tool_group_recovery_group_insert AFTER INSERT ON tool_group BEGIN
 INSERT OR IGNORE INTO tool_group_recovery_candidate(group_id)
 SELECT NEW.id WHERE NEW.state<>'completed';
END;
CREATE TRIGGER IF NOT EXISTS tool_group_recovery_group_update AFTER UPDATE OF id,state ON tool_group BEGIN
 DELETE FROM tool_group_recovery_candidate WHERE group_id=OLD.id;
 INSERT OR IGNORE INTO tool_group_recovery_candidate(group_id)
 SELECT NEW.id WHERE NEW.state<>'completed'
 OR EXISTS(SELECT 1 FROM tool_call c WHERE c.group_id=NEW.id AND
   (c.state IN ('pending','running') OR NOT EXISTS(
     SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id)));
END;
CREATE TRIGGER IF NOT EXISTS tool_group_recovery_group_delete AFTER DELETE ON tool_group BEGIN
 DELETE FROM tool_group_recovery_candidate WHERE group_id=OLD.id;
END;

CREATE TRIGGER IF NOT EXISTS tool_group_recovery_call_insert AFTER INSERT ON tool_call BEGIN
 INSERT OR IGNORE INTO tool_group_recovery_candidate(group_id)
 SELECT g.id FROM tool_group g WHERE g.id=NEW.group_id AND
 (g.state<>'completed' OR NEW.state IN ('pending','running') OR NOT EXISTS(
   SELECT 1 FROM tool_result r WHERE r.group_id=NEW.group_id AND r.call_id=NEW.call_id));
END;
CREATE TRIGGER IF NOT EXISTS tool_group_recovery_call_update AFTER UPDATE OF group_id,call_id,state ON tool_call BEGIN
 DELETE FROM tool_group_recovery_candidate WHERE group_id=OLD.group_id AND NOT EXISTS(
  SELECT 1 FROM tool_group g WHERE g.id=OLD.group_id AND (g.state<>'completed' OR EXISTS(
   SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND
    (c.state IN ('pending','running') OR NOT EXISTS(
      SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id)))));
 INSERT OR IGNORE INTO tool_group_recovery_candidate(group_id)
 SELECT g.id FROM tool_group g WHERE g.id=NEW.group_id AND
 (g.state<>'completed' OR EXISTS(SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND
   (c.state IN ('pending','running') OR NOT EXISTS(
     SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id))));
END;
CREATE TRIGGER IF NOT EXISTS tool_group_recovery_call_delete AFTER DELETE ON tool_call BEGIN
 DELETE FROM tool_group_recovery_candidate WHERE group_id=OLD.group_id AND NOT EXISTS(
  SELECT 1 FROM tool_group g WHERE g.id=OLD.group_id AND (g.state<>'completed' OR EXISTS(
   SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND
    (c.state IN ('pending','running') OR NOT EXISTS(
      SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id)))));
END;

CREATE TRIGGER IF NOT EXISTS tool_group_recovery_result_insert AFTER INSERT ON tool_result BEGIN
 DELETE FROM tool_group_recovery_candidate WHERE group_id=NEW.group_id AND NOT EXISTS(
  SELECT 1 FROM tool_group g WHERE g.id=NEW.group_id AND (g.state<>'completed' OR EXISTS(
   SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND
    (c.state IN ('pending','running') OR NOT EXISTS(
      SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id)))));
END;
CREATE TRIGGER IF NOT EXISTS tool_group_recovery_result_update AFTER UPDATE OF group_id,call_id ON tool_result BEGIN
 INSERT OR IGNORE INTO tool_group_recovery_candidate(group_id)
 SELECT g.id FROM tool_group g WHERE g.id=OLD.group_id AND
 (g.state<>'completed' OR EXISTS(SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND
   (c.state IN ('pending','running') OR NOT EXISTS(
     SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id))));
 DELETE FROM tool_group_recovery_candidate WHERE group_id=OLD.group_id AND NOT EXISTS(
  SELECT 1 FROM tool_group g WHERE g.id=OLD.group_id AND (g.state<>'completed' OR EXISTS(
   SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND
    (c.state IN ('pending','running') OR NOT EXISTS(
      SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id)))));
 INSERT OR IGNORE INTO tool_group_recovery_candidate(group_id)
 SELECT g.id FROM tool_group g WHERE g.id=NEW.group_id AND
 (g.state<>'completed' OR EXISTS(SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND
   (c.state IN ('pending','running') OR NOT EXISTS(
     SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id))));
 DELETE FROM tool_group_recovery_candidate WHERE group_id=NEW.group_id AND NOT EXISTS(
  SELECT 1 FROM tool_group g WHERE g.id=NEW.group_id AND (g.state<>'completed' OR EXISTS(
   SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND
    (c.state IN ('pending','running') OR NOT EXISTS(
      SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id)))));
END;
CREATE TRIGGER IF NOT EXISTS tool_group_recovery_result_delete AFTER DELETE ON tool_result BEGIN
 INSERT OR IGNORE INTO tool_group_recovery_candidate(group_id)
 SELECT g.id FROM tool_group g WHERE g.id=OLD.group_id AND
 (g.state<>'completed' OR EXISTS(SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND
   (c.state IN ('pending','running') OR NOT EXISTS(
     SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id))));
END;

INSERT OR IGNORE INTO tool_group_recovery_candidate(group_id)
SELECT g.id FROM tool_group g WHERE g.state<>'completed' OR EXISTS(
 SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND
 (c.state IN ('pending','running') OR NOT EXISTS(
  SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id)));
INSERT INTO schema_migration(version,name) VALUES(28,'0028_tool_group_recovery_candidates');
PRAGMA user_version=28;
