-- Agent definitions: the kinds of agent a session of the project may spawn.
-- A spawn names one by `name`; the model reads `description` to choose.
-- Every project also has a built-in `general-purpose`, which is not a row.
CREATE TABLE project_agent (
  id          uuid        PRIMARY KEY,
  project_id  uuid        NOT NULL REFERENCES project(id) ON DELETE CASCADE,
  name        text        NOT NULL,
  description text        NOT NULL,
  -- Added to the system prompt of a session spawned as this.
  prompt      text        NOT NULL DEFAULT '',
  -- The owner's model alias its sessions run on; NULL is the parent's.
  model_alias text,
  -- The project's plugins its sessions get; NULL is the parent's.
  plugins     text[],
  created_at  timestamptz NOT NULL DEFAULT now(),
  updated_at  timestamptz NOT NULL DEFAULT now(),
  CONSTRAINT project_agent_name_key UNIQUE (project_id, name),
  CONSTRAINT project_agent_name_shape CHECK (name ~ '^[a-z0-9][a-z0-9-]{0,63}$'),
  CONSTRAINT project_agent_name_not_builtin CHECK (name <> 'general-purpose')
);
