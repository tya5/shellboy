-- Settle questions created by the supported published writer as ever admitted.
UPDATE receipt SET path_admission='admitted'
 WHERE state='applied' AND json_type(result_json,'$.to_human')='true';
INSERT INTO schema_migration(version,name) VALUES(48,'0048_human_question_path_admission');
PRAGMA user_version=48;
