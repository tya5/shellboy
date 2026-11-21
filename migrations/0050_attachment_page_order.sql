CREATE INDEX attachment_page_order ON attachment_selection(thread_id,selected_order,attachment_id);
INSERT INTO schema_migration(version,name) VALUES(50,'0050_attachment_page_order');
PRAGMA user_version=50;
