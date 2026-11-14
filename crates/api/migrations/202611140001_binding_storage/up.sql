-- Plugin daemons and per-binding storage.
--
-- A binding is (project_id, plugin_type), and these tables key on that pair
-- with no foreign key to project_binding: unbinding a plugin keeps its store
-- and its session keys until the owner purges them, so disabling and binding
-- it again does not lose its thread mappings. Deleting the project removes
-- them all.

-- One binding's private key-value store. `version` comes from one sequence,
-- so it changes on every write and is never 0 (which `put` reserves for
-- create-only). `size` is what the record costs against the binding's cap
-- and the project's quota: its key and its value.
CREATE SEQUENCE binding_record_version_seq AS bigint START 1;

CREATE TABLE binding_record (
  project_id  uuid        NOT NULL REFERENCES project(id) ON DELETE CASCADE,
  plugin_type text        NOT NULL,
  key         text        NOT NULL,
  value       bytea       NOT NULL,
  version     bigint      NOT NULL DEFAULT nextval('binding_record_version_seq'),
  size        bigint      GENERATED ALWAYS AS (octet_length(key) + octet_length(value)) STORED,
  updated_at  timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (project_id, plugin_type, key),
  CONSTRAINT binding_record_key_shape CHECK (key ~ '^[ -~]{1,128}$'),
  CONSTRAINT binding_record_version_positive CHECK (version > 0)
);

ALTER SEQUENCE binding_record_version_seq OWNED BY binding_record.version;

-- Every session a binding opened, under the key it chose. A binding may send
-- to and archive only the sessions listed here for it. A key names one
-- session at a time: when its session has been archived, opening the key
-- again stamps `released_at` on the old row and adds a new one, so the old
-- session stays owned but the key moves on.
CREATE TABLE binding_session (
  session_id  uuid        PRIMARY KEY REFERENCES session(id) ON DELETE CASCADE,
  project_id  uuid        NOT NULL REFERENCES project(id) ON DELETE CASCADE,
  plugin_type text        NOT NULL,
  key         text        NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now(),
  released_at timestamptz,
  CONSTRAINT binding_session_key_shape CHECK (key ~ '^[ -~]{1,128}$')
);

CREATE UNIQUE INDEX binding_session_live_key
  ON binding_session (project_id, plugin_type, key)
  WHERE released_at IS NULL;

-- Dedupe ids `send` recorded against a session, in the same transaction that
-- accepted the message. Kept for the dedupe window, then swept by
-- `created_at`.
CREATE TABLE binding_dedupe (
  session_id uuid        NOT NULL REFERENCES session(id) ON DELETE CASCADE,
  dedupe_id  text        NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (session_id, dedupe_id),
  CONSTRAINT binding_dedupe_id_shape CHECK (dedupe_id ~ '^[ -~]{1,128}$')
);

CREATE INDEX binding_dedupe_created_idx ON binding_dedupe (created_at);
