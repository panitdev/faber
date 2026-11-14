-- How far each listening binding has been told a session's events.
--
-- A plugin with the `events` capability is told a session's events live, and
-- this is where its delivery resumes after it fell behind or the server
-- restarted: past (run_id, seq), the last event it was given. Runs of a
-- session are ordered by id (UUIDv7), and seq orders a run's events. No
-- foreign key to project_binding, as with the binding's store: rebinding
-- picks up where it left off. Deleting the session removes it.
CREATE TABLE binding_event_cursor (
  session_id  uuid        NOT NULL REFERENCES session(id) ON DELETE CASCADE,
  plugin_type text        NOT NULL,
  run_id      uuid        NOT NULL,
  seq         bigint      NOT NULL,
  updated_at  timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (session_id, plugin_type)
);
