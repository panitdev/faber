-- The owner's limits on the agents a project's sessions spawn. Each is held
-- to the operator's ceiling; NULL is the ceiling itself.
ALTER TABLE project
  -- Runs of spawned sessions under way at once; more wait their turn.
  ADD COLUMN max_agent_runs integer CHECK (max_agent_runs > 0),
  -- Open sessions one session may have spawned; a spawn past it is refused.
  ADD COLUMN max_session_agents integer CHECK (max_session_agents > 0);
