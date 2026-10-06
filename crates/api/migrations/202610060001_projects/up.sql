-- Projects and plugin bindings (Faber Core — Projects & Sessions).
--
-- A project owns its plugin bindings; a session is one conversation in one
-- project. `rev` increments on any change to the project or its bindings and
-- is what `If-Match` is checked against.

CREATE TABLE project (
  id          uuid        PRIMARY KEY,
  owner_id    uuid        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name        text        NOT NULL,
  description text        NOT NULL DEFAULT '',
  rev         bigint      NOT NULL DEFAULT 1,
  created_at  timestamptz NOT NULL DEFAULT now(),
  updated_at  timestamptz NOT NULL DEFAULT now(),
  CONSTRAINT project_name_per_owner UNIQUE (owner_id, name),
  CONSTRAINT project_name_not_blank CHECK (length(btrim(name)) > 0)
);

-- At most one binding per type. `config` is opaque to the core; the plugin
-- validates and migrates it. `created_at` is creation order, which sets tool,
-- notice and head order — kept at microsecond resolution so two bindings made
-- in one request still have an order.
CREATE TABLE project_binding (
  project_id     uuid        NOT NULL REFERENCES project(id) ON DELETE CASCADE,
  plugin_type    text        NOT NULL,
  version        text        NOT NULL,
  config_version integer     NOT NULL CHECK (config_version > 0),
  config         jsonb       NOT NULL,
  enabled        boolean     NOT NULL DEFAULT true,
  created_at     timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (project_id, plugin_type)
);

-- A session started in a project runs through its plugins. NULL keeps the
-- workspace-only behaviour sessions had before projects existed.
ALTER TABLE session
  ADD COLUMN project_id uuid REFERENCES project(id) ON DELETE CASCADE;

-- The binding snapshot the session's last committed run used; NULL until one
-- has. Compared at each run start to decide what to open and what to tell the
-- agent.
ALTER TABLE session ADD COLUMN plugin_snapshot jsonb;

CREATE INDEX session_project_idx ON session (project_id) WHERE project_id IS NOT NULL;
