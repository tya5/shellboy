ALTER TABLE thread ADD COLUMN selection_nonappend_generation INTEGER NOT NULL DEFAULT 0 CHECK(typeof(selection_nonappend_generation)='integer' AND selection_nonappend_generation>=0);
-- A revision writer must never carry a stale explicit witness assignment. This
-- column-list guard runs independently of nested trigger order/recursive_triggers.
CREATE TRIGGER IF NOT EXISTS selection_generation_combined_write BEFORE UPDATE OF selection_nonappend_generation ON thread
WHEN NEW.selection_revision != OLD.selection_revision
BEGIN SELECT RAISE(ABORT,'combined revision and generation write is not supported'); END;
-- Constant-space witness: every non-tail selected-path mutation invalidates snapshots.
CREATE TRIGGER IF NOT EXISTS selection_generation_update BEFORE UPDATE OF selection_nonappend_generation ON thread
WHEN typeof(NEW.selection_nonappend_generation) != 'integer' OR NEW.selection_nonappend_generation < OLD.selection_nonappend_generation OR NEW.selection_nonappend_generation < 0
BEGIN SELECT RAISE(ABORT,'invalid selection generation'); END;
-- Supported thread writers never conflict-insert an existing owner. Forbid REPLACE
-- independently of recursive_triggers, including stale whole-thread UPSERT resets.
CREATE TRIGGER IF NOT EXISTS selection_thread_conflict BEFORE INSERT ON thread
WHEN EXISTS(SELECT 1 FROM thread WHERE id=NEW.id)
BEGIN SELECT RAISE(ABORT,'thread conflict insert is not supported'); END;
CREATE TRIGGER IF NOT EXISTS selection_generation_insert BEFORE INSERT ON thread
WHEN typeof(NEW.selection_nonappend_generation) != 'integer' OR NEW.selection_nonappend_generation < 0
BEGIN SELECT RAISE(ABORT,'invalid selection generation'); END;
CREATE TRIGGER IF NOT EXISTS selection_path_insert BEFORE INSERT ON thread_path
WHEN NEW.ordinal != COALESCE((SELECT MAX(ordinal)+1 FROM thread_path WHERE thread_id=NEW.thread_id),0)
 OR EXISTS(SELECT 1 FROM thread_path WHERE thread_id=NEW.thread_id AND (ordinal=NEW.ordinal OR node_id=NEW.node_id))
BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM thread WHERE id=NEW.thread_id AND (typeof(selection_nonappend_generation)!='integer' OR selection_nonappend_generation<0 OR selection_nonappend_generation=9223372036854775807)) THEN RAISE(ABORT,'selection generation exhausted') END;
 UPDATE thread SET selection_nonappend_generation=selection_nonappend_generation+1 WHERE id=NEW.thread_id;
END;
CREATE TRIGGER IF NOT EXISTS selection_path_delete BEFORE DELETE ON thread_path BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM thread WHERE id=OLD.thread_id AND (typeof(selection_nonappend_generation)!='integer' OR selection_nonappend_generation<0 OR selection_nonappend_generation=9223372036854775807)) THEN RAISE(ABORT,'selection generation exhausted') END;
 UPDATE thread SET selection_nonappend_generation=selection_nonappend_generation+1 WHERE id=OLD.thread_id;
END;
CREATE TRIGGER IF NOT EXISTS selection_path_update BEFORE UPDATE OF thread_id,ordinal,node_id ON thread_path BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM thread WHERE id IN (OLD.thread_id,NEW.thread_id) AND (typeof(selection_nonappend_generation)!='integer' OR selection_nonappend_generation<0 OR selection_nonappend_generation=9223372036854775807)) THEN RAISE(ABORT,'selection generation exhausted') END;
 UPDATE thread SET selection_nonappend_generation=selection_nonappend_generation+1 WHERE id IN (OLD.thread_id,NEW.thread_id);
END;
-- Statement-order independent conservative invalidation for same-cursor metadata
-- changes. Real path rewrites are caught by row triggers in either statement order.
CREATE TRIGGER IF NOT EXISTS selection_revision_same_cursor BEFORE UPDATE OF selection_revision ON thread
WHEN NEW.selection_revision != OLD.selection_revision AND NEW.cursor_node_id IS OLD.cursor_node_id
BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM thread WHERE id=OLD.id AND (typeof(selection_nonappend_generation)!='integer' OR selection_nonappend_generation<0 OR selection_nonappend_generation=9223372036854775807)) THEN RAISE(ABORT,'selection generation exhausted') END;
 UPDATE thread SET selection_nonappend_generation=selection_nonappend_generation+1 WHERE id=OLD.id;
END;
INSERT INTO schema_migration(version,name) VALUES(40,'0040_selection_nonappend_generation');
PRAGMA user_version=40;
