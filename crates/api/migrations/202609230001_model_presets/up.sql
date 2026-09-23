-- Model providers and the presets they publish.
--
-- A preset is a third party's description of a model — what it can do and what
-- it costs — and is not a row a run calls. Both tables carry a nullable
-- `user_id`: `NULL` marks a system row (the catalog the service fetches at
-- boot, shared by every user and replaced on each load), and a set value marks
-- a row the user wrote, private to them. The ownership column is what keeps
-- "the user chose this" and "we fetched this" distinguishable.
--
-- A preset and its provider carry the same owner: a user's preset references a
-- user's provider, a system preset references a system provider. Enforced by
-- the API rather than a composite foreign key, which a nullable owner cannot
-- express cleanly; the invariant is what lets a system refresh delete and
-- reseed its providers without touching anybody's rows.
CREATE TABLE model_providers (
  id           uuid        PRIMARY KEY,
  user_id      uuid        REFERENCES users(id) ON DELETE CASCADE,
  provider_id  text        NOT NULL,
  name         text        NOT NULL,
  website      text,
  api_base_url text,
  created_at   timestamptz NOT NULL DEFAULT now()
);

-- One row per provider key per owner. System rows get their own partial index
-- because Postgres treats each NULL as distinct, so a plain UNIQUE would let
-- duplicate system providers through.
CREATE UNIQUE INDEX model_providers_owner_key_idx
  ON model_providers (user_id, provider_id)
  WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX model_providers_system_key_idx
  ON model_providers (provider_id)
  WHERE user_id IS NULL;
CREATE INDEX model_providers_user_id_idx ON model_providers (user_id);

CREATE TABLE model_presets (
  id                uuid        PRIMARY KEY,
  user_id           uuid        REFERENCES users(id) ON DELETE CASCADE,
  model_provider_id uuid        NOT NULL REFERENCES model_providers(id) ON DELETE CASCADE,
  model_id          text        NOT NULL,
  name              text        NOT NULL,
  vision            boolean     NOT NULL,
  attachment        boolean     NOT NULL,
  reasoning         boolean     NOT NULL,
  tools             boolean     NOT NULL,
  structured_output boolean     NOT NULL,
  temperature       boolean     NOT NULL,
  price_input           double precision,
  price_output          double precision,
  price_cache_read      double precision,
  price_cache_write     double precision,
  price_input_audio     double precision,
  price_output_audio    double precision,
  price_reasoning       double precision,
  limit_context         bigint,
  limit_input           bigint,
  limit_output          bigint,
  modalities_input      jsonb       NOT NULL DEFAULT '[]',
  modalities_output     jsonb       NOT NULL DEFAULT '[]',
  release_date          bigint,
  last_updated          bigint,
  knowledge_cutoff      bigint,
  open_weights          boolean,
  created_at            timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX model_presets_owner_model_idx
  ON model_presets (user_id, model_provider_id, model_id)
  WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX model_presets_system_model_idx
  ON model_presets (model_provider_id, model_id)
  WHERE user_id IS NULL;
CREATE INDEX model_presets_user_id_idx ON model_presets (user_id);
CREATE INDEX model_presets_model_provider_id_idx ON model_presets (model_provider_id);
