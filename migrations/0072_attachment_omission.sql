ALTER TABLE attachment_send ADD COLUMN omission_reason TEXT CHECK(omission_reason IN ('attachment_changed','attachment_unavailable'));
UPDATE attachment_send AS sent
SET omission_reason='attachment_unavailable'
WHERE sent.state='unavailable' AND EXISTS (
    SELECT 1 FROM model_attempt AS attempt
    WHERE attempt.id=sent.attempt_id
      AND json_extract(attempt.input_manifest_json,'$.adoption.outcome')='adopted'
);
INSERT INTO schema_migration(version,name) VALUES(72,'0072_attachment_omission');
PRAGMA user_version=72;
