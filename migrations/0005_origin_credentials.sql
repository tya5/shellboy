-- P3: provenance is not an authentication credential. Remove only fields that
-- old writers copied from Origin; never rewrite raw output, user payloads,
-- operation fingerprints, or saved results. Invalid historical metadata remains
-- diagnosable rather than preventing owner startup through a JSON parse error.
UPDATE operation_receipt SET origin_json=json_remove(origin_json,'$.token')
 WHERE CASE WHEN json_valid(origin_json) THEN json_type(origin_json,'$.token') IS NOT NULL ELSE 0 END;
UPDATE receipt SET sender_json=json_remove(sender_json,'$.token')
 WHERE CASE WHEN json_valid(sender_json) THEN json_type(sender_json,'$.token') IS NOT NULL ELSE 0 END;
UPDATE attachment_delivery SET origin_json=json_remove(origin_json,'$.token')
 WHERE CASE WHEN json_valid(origin_json) THEN json_type(origin_json,'$.token') IS NOT NULL ELSE 0 END;
UPDATE app_meta SET value=json_remove(value,'$.caller.token')
 WHERE key LIKE 'wait.pending.%' AND
 CASE WHEN json_valid(value) THEN json_type(value,'$.caller.token') IS NOT NULL ELSE 0 END;
INSERT INTO schema_migration(version,name) VALUES(5,'0005_origin_credentials');
PRAGMA user_version=5;
