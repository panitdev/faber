ALTER TABLE session
  -- The project's agent definitions as this session's agent was first told
  -- them, in the description of its spawn tool. Kept as first told: a tool
  -- description that changed between runs would rewrite the prefix every
  -- earlier turn was cached against. NULL until the session's first run.
  ADD COLUMN agent_listing text,
  -- For a session an agent spawned: the project's plugins it runs with, as
  -- its definition named them when it was spawned. NULL is its parent's.
  -- Kept here and not read from the definition, so that deleting the
  -- definition cannot widen what a running child may use.
  ADD COLUMN agent_plugins text[];
