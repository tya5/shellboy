-- A question's current admission duty is separate from its receipt outcome and ever-admitted fact.
ALTER TABLE receipt ADD COLUMN question_duty TEXT NOT NULL DEFAULT 'none'
 CHECK(question_duty IN ('none','pending','admitted','cancelled'));
INSERT INTO schema_migration(version,name) VALUES(48,'0048_question_admission_duty');
PRAGMA user_version=48;
