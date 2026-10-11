CREATE TABLE image (
  id                uuid        PRIMARY KEY,
  user_id           uuid        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name              text        NOT NULL,
  reference         text        NOT NULL,
  default_mounts    jsonb,
  default_root_path text        NOT NULL,
  created_at        timestamptz NOT NULL DEFAULT now(),
  UNIQUE (user_id, name)
);

CREATE INDEX image_user_id_idx ON image (user_id);

-- Templates are not restored: the reference alone does not say what the
-- name, root, or mounts were. Provenance is dropped with the column.
ALTER TABLE host_container
  DROP COLUMN image,
  ADD COLUMN image_id uuid REFERENCES image(id) ON DELETE SET NULL;
