-- Reversing this restores the schema but not the rows: the shared machines,
-- their tenants, grants, and subject ids deleted on the way up have no owner
-- to restore them to, and a downgraded binary against this schema must
-- re-provision them.
ALTER TABLE host DROP CONSTRAINT host_user_id_name_key;
ALTER TABLE host ALTER COLUMN user_id DROP NOT NULL;
CREATE UNIQUE INDEX host_name_owned   ON host (user_id, name) WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX host_name_service ON host (name)          WHERE user_id IS NULL;

ALTER TABLE host
  ADD COLUMN default_cpu_millis    INT,
  ADD COLUMN default_memory_bytes  BIGINT,
  ADD COLUMN default_storage_bytes BIGINT,
  ADD COLUMN default_container_max INT,
  ADD COLUMN user_data_root        TEXT,
  ADD COLUMN container_root_uid    BIGINT;
ALTER TABLE host ADD CONSTRAINT host_service_needs_data_root
  CHECK (user_id IS NOT NULL OR user_data_root IS NOT NULL);
ALTER TABLE host ADD CONSTRAINT host_service_needs_container_root_uid
  CHECK (user_id IS NOT NULL OR container_root_uid IS NOT NULL) NOT VALID;

CREATE INDEX host_container_owner ON host_container (host_id, user_id)
  WHERE unregistered_at IS NULL;

CREATE SEQUENCE subject_seq START WITH 500000 MINVALUE 500000 MAXVALUE 2000000000;

CREATE TABLE user_subject (
  user_id    UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  subject_id INT  NOT NULL UNIQUE DEFAULT nextval('subject_seq'),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE host_user (
  id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  host_id     UUID NOT NULL REFERENCES host(id) ON DELETE CASCADE,
  user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  released_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX host_user_live ON host_user (host_id, user_id)
  WHERE released_at IS NULL;

CREATE TABLE host_user_quota (
  id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  host_id       UUID NOT NULL REFERENCES host(id) ON DELETE CASCADE,
  user_id       UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  cpu_millis    INT,
  memory_bytes  BIGINT,
  storage_bytes BIGINT,
  container_max INT,
  granted_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
  granted_by    UUID,
  expires_at    TIMESTAMPTZ,
  retired_at    TIMESTAMPTZ,
  note          TEXT
);

CREATE UNIQUE INDEX host_user_quota_live ON host_user_quota (host_id, user_id)
  WHERE retired_at IS NULL;

ALTER TABLE image DROP CONSTRAINT image_user_id_name_key;
ALTER TABLE image ALTER COLUMN user_id DROP NOT NULL;
CREATE UNIQUE INDEX image_name_owned   ON image (user_id, name) WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX image_name_service ON image (name)          WHERE user_id IS NULL;
