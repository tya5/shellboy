DROP INDEX gc_bytes_app_meta_history;
CREATE INDEX gc_bytes_app_meta_history ON app_meta(octet_length(value)) WHERE key LIKE 'summary.part.%' OR key LIKE 'wait.pending.%' OR key LIKE 'compact.result.%' OR key LIKE 'revisit.%';
DROP TRIGGER logical_history_app_meta_insert;
CREATE TRIGGER logical_history_app_meta_insert AFTER INSERT ON app_meta BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(CASE WHEN (NEW.key LIKE 'summary.part.%' OR NEW.key LIKE 'wait.pending.%' OR NEW.key LIKE 'compact.result.%' OR NEW.key LIKE 'revisit.%') THEN COALESCE(octet_length(NEW.value),0) ELSE 0 END),revision=revision+1 WHERE category='logical.history';
END;
DROP TRIGGER logical_history_app_meta_delete;
CREATE TRIGGER logical_history_app_meta_delete AFTER DELETE ON app_meta BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+-(CASE WHEN (OLD.key LIKE 'summary.part.%' OR OLD.key LIKE 'wait.pending.%' OR OLD.key LIKE 'compact.result.%' OR OLD.key LIKE 'revisit.%') THEN COALESCE(octet_length(OLD.value),0) ELSE 0 END),revision=revision+1 WHERE category='logical.history';
END;
DROP TRIGGER logical_history_app_meta_update;
CREATE TRIGGER logical_history_app_meta_update AFTER UPDATE OF key,value ON app_meta BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(CASE WHEN (NEW.key LIKE 'summary.part.%' OR NEW.key LIKE 'wait.pending.%' OR NEW.key LIKE 'compact.result.%' OR NEW.key LIKE 'revisit.%') THEN COALESCE(octet_length(NEW.value),0) ELSE 0 END)-(CASE WHEN (OLD.key LIKE 'summary.part.%' OR OLD.key LIKE 'wait.pending.%' OR OLD.key LIKE 'compact.result.%' OR OLD.key LIKE 'revisit.%') THEN COALESCE(octet_length(OLD.value),0) ELSE 0 END),revision=revision+1 WHERE category='logical.history';
END;
