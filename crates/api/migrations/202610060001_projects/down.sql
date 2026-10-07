DROP INDEX IF EXISTS session_project_idx;
ALTER TABLE session DROP COLUMN plugin_snapshot;
ALTER TABLE session DROP COLUMN project_id;
DROP TABLE project_binding;
DROP TABLE project;
