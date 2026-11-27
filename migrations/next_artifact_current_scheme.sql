ALTER TABLE artifact ADD COLUMN current_scheme TEXT;

-- Match artifact add/update. Check the original URI before text substr can stop at NUL.
UPDATE artifact AS a SET current_scheme = (
    SELECT CASE
        WHEN instr(uri, ':') > 1
            AND substr(uri, 1, 1) GLOB '[A-Za-z]'
            AND substr(uri, 1, instr(uri, ':') - 1) NOT GLOB '*[^A-Za-z0-9+.-]*'
            AND (instr(uri, char(0)) = 0 OR instr(uri, char(0)) > instr(uri, ':'))
        THEN lower(substr(uri, 1, instr(uri, ':') - 1))
        END
    FROM artifact_version
    WHERE artifact_id = a.id
    ORDER BY version DESC LIMIT 1
);
CREATE INDEX artifact_scheme_page ON artifact(current_scheme,updated_at DESC,id);

INSERT INTO schema_migration(version,name) VALUES(70,'next_artifact_current_scheme');
PRAGMA user_version=70;
