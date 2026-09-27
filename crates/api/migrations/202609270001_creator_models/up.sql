-- Split a model's description from its serving.
--
-- Until now every preset carried the whole description of its model — name,
-- capabilities, limits, modalities, dates — so a model served by twenty
-- providers was described twenty times, and the copies drifted. A creator
-- model is that description once, as the lab that made the model states it,
-- keyed `<creator>/<model>` the way models.dev keys it. A preset is now one
-- provider serving a model: it links to the creator model when one exists
-- and stores only what it says differently, plus what only a provider can
-- say — the price, the reasoning controls its API exposes, its lifecycle.
--
-- Creator models are system rows only: they are the catalog's, upserted at
-- boot and never deleted, which is what lets a user's preset link to one
-- without the two sharing an owner.
CREATE TABLE creator_models (
  id                uuid        PRIMARY KEY,
  model_id          text        NOT NULL UNIQUE,
  creator           text        NOT NULL,
  name              text        NOT NULL,
  description       text,
  family            text,
  attachment        boolean     NOT NULL,
  reasoning         boolean     NOT NULL,
  tool_call         boolean     NOT NULL,
  structured_output boolean,
  temperature       boolean,
  knowledge         text,
  release_date      text,
  last_updated      text,
  open_weights      boolean,
  limits            jsonb       NOT NULL DEFAULT '{}',
  modalities        jsonb       NOT NULL DEFAULT '{"input": [], "output": []}',
  license           text,
  created_at        timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX creator_models_creator_idx ON creator_models (creator);

-- The system catalog came from a different directory; left in place it would
-- linger beside the new one forever, since a refresh never deletes. It is
-- reseeded at boot. A model bound to a system preset loses the binding
-- through `models.preset_id`'s SET NULL, so the binding is first written
-- down by its natural key — provider key and served id, which the two
-- directories largely share — and the first catalog load after this
-- migration binds the model to the new preset under that key, if there is
-- one, and empties this table. A user's own providers and presets are kept
-- and converted below.
CREATE TABLE model_preset_rebinds (
  model_id     uuid PRIMARY KEY REFERENCES models(id) ON DELETE CASCADE,
  provider_key text NOT NULL,
  served_id    text NOT NULL
);

INSERT INTO model_preset_rebinds (model_id, provider_key, served_id)
SELECT m.id, pr.provider_id, p.model_id
FROM models m
JOIN model_presets p ON p.id = m.preset_id
JOIN model_providers pr ON pr.id = p.model_provider_id
WHERE p.user_id IS NULL;

DELETE FROM model_providers WHERE user_id IS NULL;

-- Providers take models.dev's fields: `doc` is what `website` held, `api` is
-- what `api_base_url` held, and `npm` and `env` are new.
ALTER TABLE model_providers RENAME COLUMN website TO doc;
ALTER TABLE model_providers RENAME COLUMN api_base_url TO api;
ALTER TABLE model_providers
  ADD COLUMN npm text,
  ADD COLUMN env jsonb NOT NULL DEFAULT '[]';

-- Every descriptive column becomes an override: NULL is "as the creator
-- model says". A preset with no creator model reads its overrides over an
-- empty description, so an unlinked row keeps stating everything itself —
-- which is what every converted row is. The FK is RESTRICT: a preset stores
-- only its difference from the creator model, so losing the link would
-- silently lose the rest of its description.
ALTER TABLE model_presets
  ADD COLUMN creator_model_id  uuid REFERENCES creator_models(id) ON DELETE RESTRICT,
  ADD COLUMN description       text,
  ADD COLUMN family            text,
  ADD COLUMN knowledge         text,
  ADD COLUMN limits            jsonb,
  ADD COLUMN modalities        jsonb,
  ADD COLUMN cost              jsonb,
  ADD COLUMN reasoning_options jsonb,
  ADD COLUMN interleaved       jsonb,
  ADD COLUMN status            text;

ALTER TABLE model_presets RENAME COLUMN tools TO tool_call;

ALTER TABLE model_presets
  ALTER COLUMN name              DROP NOT NULL,
  ALTER COLUMN attachment        DROP NOT NULL,
  ALTER COLUMN reasoning         DROP NOT NULL,
  ALTER COLUMN tool_call         DROP NOT NULL,
  ALTER COLUMN structured_output DROP NOT NULL,
  ALTER COLUMN temperature       DROP NOT NULL;

-- Dates are written the way models.dev writes them, `YYYY-MM-DD`, rather than
-- as epoch seconds.
ALTER TABLE model_presets
  ALTER COLUMN release_date TYPE text
    USING to_char(to_timestamp(release_date) AT TIME ZONE 'UTC', 'YYYY-MM-DD'),
  ALTER COLUMN last_updated TYPE text
    USING to_char(to_timestamp(last_updated) AT TIME ZONE 'UTC', 'YYYY-MM-DD');

UPDATE model_presets
SET knowledge = to_char(to_timestamp(knowledge_cutoff) AT TIME ZONE 'UTC', 'YYYY-MM-DD'),
    limits = jsonb_strip_nulls(jsonb_build_object(
      'context', limit_context,
      'input', limit_input,
      'output', limit_output
    )),
    modalities = jsonb_build_object('input', modalities_input, 'output', modalities_output),
    cost = CASE
      WHEN num_nonnulls(price_input, price_output, price_cache_read, price_cache_write,
                        price_input_audio, price_output_audio, price_reasoning) > 0
      THEN jsonb_strip_nulls(jsonb_build_object(
        'input', price_input,
        'output', price_output,
        'cache_read', price_cache_read,
        'cache_write', price_cache_write,
        'input_audio', price_input_audio,
        'output_audio', price_output_audio,
        'reasoning', price_reasoning
      ))
    END;

ALTER TABLE model_presets
  DROP COLUMN vision,
  DROP COLUMN price_input,
  DROP COLUMN price_output,
  DROP COLUMN price_cache_read,
  DROP COLUMN price_cache_write,
  DROP COLUMN price_input_audio,
  DROP COLUMN price_output_audio,
  DROP COLUMN price_reasoning,
  DROP COLUMN limit_context,
  DROP COLUMN limit_input,
  DROP COLUMN limit_output,
  DROP COLUMN modalities_input,
  DROP COLUMN modalities_output,
  DROP COLUMN knowledge_cutoff;

CREATE INDEX model_presets_creator_model_id_idx ON model_presets (creator_model_id);
