-- The owner's limits on what one binding may start: sessions it holds open,
-- runs at once, and messages a minute. Each is capped by the operator's
-- ceiling; NULL is the ceiling itself. They never touch the project's own
-- sessions, which no binding opened.
ALTER TABLE project_binding
  ADD COLUMN max_sessions integer CHECK (max_sessions > 0),
  ADD COLUMN max_runs integer CHECK (max_runs > 0),
  ADD COLUMN messages_per_minute integer CHECK (messages_per_minute > 0);
