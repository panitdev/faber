-- A subagent is a session, started by a run of another session. These tie a
-- child to what started it: the parent's tools, the report a child sends
-- back, stopping children with their parent and the UI all follow the link.
ALTER TABLE session
  -- The session whose agent spawned this one. A child goes with its parent.
  ADD COLUMN parent_session_id uuid,
  -- The parent's run that spawned it; NULL once that run is gone.
  ADD COLUMN spawned_by_run_id uuid REFERENCES run(id) ON DELETE SET NULL,
  -- The name of the agent definition it was spawned as. A name and not a
  -- reference: deleting or renaming a definition leaves its children as
  -- they were.
  ADD COLUMN agent_type text,
  -- What the parent called it.
  ADD COLUMN agent_name text,
  ADD CONSTRAINT session_parent_not_self CHECK (parent_session_id <> id),
  -- Only a project session has children: a child runs as the project's owner.
  ADD CONSTRAINT session_child_in_project
    CHECK (parent_session_id IS NULL OR project_id IS NOT NULL),
  ADD CONSTRAINT session_child_fields CHECK (
    parent_session_id IS NOT NULL
    OR (spawned_by_run_id IS NULL AND agent_type IS NULL AND agent_name IS NULL)),
  ADD CONSTRAINT session_agent_type_shape
    CHECK (agent_type IS NULL OR char_length(agent_type) BETWEEN 1 AND 128),
  ADD CONSTRAINT session_agent_name_shape
    CHECK (agent_name IS NULL OR char_length(agent_name) BETWEEN 1 AND 128),
  ADD CONSTRAINT session_id_project_workspace_key UNIQUE (id, project_id, workspace_id);

-- A child is in its parent's project and workspace, by construction: the
-- parent it names must be a session with the same two.
ALTER TABLE session
  ADD CONSTRAINT session_parent_fkey
    FOREIGN KEY (parent_session_id, project_id, workspace_id)
    REFERENCES session (id, project_id, workspace_id) ON DELETE CASCADE;

CREATE INDEX session_parent_idx ON session (parent_session_id)
  WHERE parent_session_id IS NOT NULL;
