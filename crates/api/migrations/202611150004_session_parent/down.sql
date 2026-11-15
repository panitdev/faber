DELETE FROM session WHERE parent_session_id IS NOT NULL;
DROP INDEX session_parent_idx;
ALTER TABLE session
  DROP CONSTRAINT session_parent_fkey,
  DROP CONSTRAINT session_id_project_workspace_key,
  DROP COLUMN agent_name,
  DROP COLUMN agent_type,
  DROP COLUMN spawned_by_run_id,
  DROP COLUMN parent_session_id;
