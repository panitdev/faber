-- What runs a binding's sessions use, and the messages waiting on them.
--
-- A session a plugin's daemon opens has no user at the keyboard to pick a
-- model, so the owner picks one for the binding. The host stores it here, on
-- the binding row, and never reads it from the plugin's own config. NULL
-- leaves that binding's messages waiting until the owner chooses one.
ALTER TABLE project_binding
  ADD COLUMN model_alias text,
  ADD COLUMN thinking_effort text;

-- Messages a binding sent that no run has taken yet: `send` returns once a
-- row is here, and the session's next run takes every row in id order. One
-- run at a time per session, so a message sent during a run waits for it.
CREATE TABLE binding_pending (
  id         bigserial   PRIMARY KEY,
  session_id uuid        NOT NULL REFERENCES session(id) ON DELETE CASCADE,
  content    text        NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX binding_pending_session_idx ON binding_pending (session_id, id);
