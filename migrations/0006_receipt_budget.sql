-- Reservation belongs to an admitted runtime operation in its owner instance.
-- Old-instance pending records retain uncertainty but no longer reserve future
-- result space: no previous-instance worker is allowed to resume automatically.
ALTER TABLE operation_receipt ADD COLUMN reserved_bytes INTEGER NOT NULL DEFAULT 0 CHECK(reserved_bytes>=0);

-- Derived actual-size accounting uses the existing quota_counter table.
-- The fixed 512-byte management charge is a logical budget, not disk usage.
CREATE INDEX operation_receipt_pending_budget ON operation_receipt(instance_id) WHERE state='pending';
INSERT INTO quota_counter(category,logical_bytes,revision) SELECT 'operation_receipt',COALESCE(SUM(512+length(CAST(operation_id AS BLOB))+length(CAST(instance_id AS BLOB))+length(CAST(origin_json AS BLOB))+length(CAST(request_hash AS BLOB))+length(CAST(created_at AS BLOB))+COALESCE(length(CAST(result_ref_json AS BLOB)),0)),0),0 FROM operation_receipt;
INSERT INTO quota_counter(category,logical_bytes,revision) SELECT 'operation_receipt.pending_actual',COALESCE(SUM(CASE WHEN state='pending' THEN 512+length(CAST(operation_id AS BLOB))+length(CAST(instance_id AS BLOB))+length(CAST(origin_json AS BLOB))+length(CAST(request_hash AS BLOB))+length(CAST(created_at AS BLOB))+COALESCE(length(CAST(result_ref_json AS BLOB)),0) ELSE 0 END),0),0 FROM operation_receipt;
CREATE TRIGGER operation_receipt_budget_insert AFTER INSERT ON operation_receipt BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(512+length(CAST(NEW.operation_id AS BLOB))+length(CAST(NEW.instance_id AS BLOB))+length(CAST(NEW.origin_json AS BLOB))+length(CAST(NEW.request_hash AS BLOB))+length(CAST(NEW.created_at AS BLOB))+COALESCE(length(CAST(NEW.result_ref_json AS BLOB)),0)),revision=revision+1 WHERE category='operation_receipt';
 UPDATE quota_counter SET logical_bytes=logical_bytes+(CASE WHEN NEW.state='pending' THEN 512+length(CAST(NEW.operation_id AS BLOB))+length(CAST(NEW.instance_id AS BLOB))+length(CAST(NEW.origin_json AS BLOB))+length(CAST(NEW.request_hash AS BLOB))+length(CAST(NEW.created_at AS BLOB))+COALESCE(length(CAST(NEW.result_ref_json AS BLOB)),0) ELSE 0 END),revision=revision+1 WHERE category='operation_receipt.pending_actual';
END;
CREATE TRIGGER operation_receipt_budget_delete AFTER DELETE ON operation_receipt BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes-(512+length(CAST(OLD.operation_id AS BLOB))+length(CAST(OLD.instance_id AS BLOB))+length(CAST(OLD.origin_json AS BLOB))+length(CAST(OLD.request_hash AS BLOB))+length(CAST(OLD.created_at AS BLOB))+COALESCE(length(CAST(OLD.result_ref_json AS BLOB)),0)),revision=revision+1 WHERE category='operation_receipt';
 UPDATE quota_counter SET logical_bytes=logical_bytes-(CASE WHEN OLD.state='pending' THEN 512+length(CAST(OLD.operation_id AS BLOB))+length(CAST(OLD.instance_id AS BLOB))+length(CAST(OLD.origin_json AS BLOB))+length(CAST(OLD.request_hash AS BLOB))+length(CAST(OLD.created_at AS BLOB))+COALESCE(length(CAST(OLD.result_ref_json AS BLOB)),0) ELSE 0 END),revision=revision+1 WHERE category='operation_receipt.pending_actual';
END;
CREATE TRIGGER operation_receipt_budget_update AFTER UPDATE ON operation_receipt BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(512+length(CAST(NEW.operation_id AS BLOB))+length(CAST(NEW.instance_id AS BLOB))+length(CAST(NEW.origin_json AS BLOB))+length(CAST(NEW.request_hash AS BLOB))+length(CAST(NEW.created_at AS BLOB))+COALESCE(length(CAST(NEW.result_ref_json AS BLOB)),0))-(512+length(CAST(OLD.operation_id AS BLOB))+length(CAST(OLD.instance_id AS BLOB))+length(CAST(OLD.origin_json AS BLOB))+length(CAST(OLD.request_hash AS BLOB))+length(CAST(OLD.created_at AS BLOB))+COALESCE(length(CAST(OLD.result_ref_json AS BLOB)),0)),revision=revision+1 WHERE category='operation_receipt';
 UPDATE quota_counter SET logical_bytes=logical_bytes+(CASE WHEN NEW.state='pending' THEN 512+length(CAST(NEW.operation_id AS BLOB))+length(CAST(NEW.instance_id AS BLOB))+length(CAST(NEW.origin_json AS BLOB))+length(CAST(NEW.request_hash AS BLOB))+length(CAST(NEW.created_at AS BLOB))+COALESCE(length(CAST(NEW.result_ref_json AS BLOB)),0) ELSE 0 END)-(CASE WHEN OLD.state='pending' THEN 512+length(CAST(OLD.operation_id AS BLOB))+length(CAST(OLD.instance_id AS BLOB))+length(CAST(OLD.origin_json AS BLOB))+length(CAST(OLD.request_hash AS BLOB))+length(CAST(OLD.created_at AS BLOB))+COALESCE(length(CAST(OLD.result_ref_json AS BLOB)),0) ELSE 0 END),revision=revision+1 WHERE category='operation_receipt.pending_actual';
END;
INSERT INTO schema_migration(version,name) VALUES(6,'0006_receipt_budget');
PRAGMA user_version=6;
