-- Turns share an unchanged instruction snapshot (#450). History GC deletes a
-- snapshot no Turn references; this index makes that check, and the foreign-key
-- check of the delete, a seek instead of a scan of every Turn.
CREATE INDEX turns_by_instruction_snapshot ON turn(instruction_snapshot_id);
INSERT INTO schema_migration(version,name) VALUES(69,'0069_instruction_snapshot_turns');
PRAGMA user_version=69;
