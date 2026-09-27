-- Restore the shape where every preset describes its model in full. A linked
-- preset holds only its difference from its creator model, so the creator's
-- values are folded back into it before the link is dropped. The system
-- catalog is dropped rather than converted: the previous code reseeds it from
-- its own directory at boot. Serving-only fields the old shape has no column
-- for — tiered prices, reasoning options, status — are lost.
DELETE FROM model_providers WHERE user_id IS NULL;

ALTER TABLE model_presets
  ADD COLUMN vision             boolean,
  ADD COLUMN price_input        double precision,
  ADD COLUMN price_output       double precision,
  ADD COLUMN price_cache_read   double precision,
  ADD COLUMN price_cache_write  double precision,
  ADD COLUMN price_input_audio  double precision,
  ADD COLUMN price_output_audio double precision,
  ADD COLUMN price_reasoning    double precision,
  ADD COLUMN limit_context      bigint,
  ADD COLUMN limit_input        bigint,
  ADD COLUMN limit_output       bigint,
  ADD COLUMN modalities_input   jsonb NOT NULL DEFAULT '[]',
  ADD COLUMN modalities_output  jsonb NOT NULL DEFAULT '[]',
  ADD COLUMN knowledge_cutoff   bigint;

UPDATE model_presets p
SET name              = COALESCE(p.name, c.name, p.model_id),
    attachment        = COALESCE(p.attachment, c.attachment, false),
    reasoning         = COALESCE(p.reasoning, c.reasoning, false),
    tool_call         = COALESCE(p.tool_call, c.tool_call, false),
    structured_output = COALESCE(p.structured_output, c.structured_output, false),
    temperature       = COALESCE(p.temperature, c.temperature, false),
    release_date      = COALESCE(p.release_date, c.release_date),
    last_updated      = COALESCE(p.last_updated, c.last_updated),
    knowledge         = COALESCE(p.knowledge, c.knowledge),
    open_weights      = COALESCE(p.open_weights, c.open_weights),
    limits            = COALESCE(p.limits, c.limits, '{}'),
    modalities        = COALESCE(p.modalities, c.modalities, '{"input": [], "output": []}')
FROM model_presets self
LEFT JOIN creator_models c ON c.id = self.creator_model_id
WHERE self.id = p.id;

-- Only a well-formed date converts back to epoch seconds; `YYYY-MM` reads as
-- the first of its month.
UPDATE model_presets
SET vision             = COALESCE(modalities -> 'input' ? 'image', false),
    price_input        = (cost ->> 'input')::double precision,
    price_output       = (cost ->> 'output')::double precision,
    price_cache_read   = (cost ->> 'cache_read')::double precision,
    price_cache_write  = (cost ->> 'cache_write')::double precision,
    price_input_audio  = (cost ->> 'input_audio')::double precision,
    price_output_audio = (cost ->> 'output_audio')::double precision,
    price_reasoning    = (cost ->> 'reasoning')::double precision,
    limit_context      = (limits ->> 'context')::bigint,
    limit_input        = (limits ->> 'input')::bigint,
    limit_output       = (limits ->> 'output')::bigint,
    modalities_input   = COALESCE(modalities -> 'input', '[]'),
    modalities_output  = COALESCE(modalities -> 'output', '[]'),
    knowledge_cutoff   = CASE WHEN knowledge ~ '^\d{4}-\d{2}(-\d{2})?$'
      THEN extract(epoch FROM to_date(knowledge || CASE WHEN length(knowledge) = 7 THEN '-01' ELSE '' END, 'YYYY-MM-DD'))::bigint
    END;

ALTER TABLE model_presets
  ALTER COLUMN release_date TYPE bigint
    USING CASE WHEN release_date ~ '^\d{4}-\d{2}(-\d{2})?$'
      THEN extract(epoch FROM to_date(release_date || CASE WHEN length(release_date) = 7 THEN '-01' ELSE '' END, 'YYYY-MM-DD'))::bigint
    END,
  ALTER COLUMN last_updated TYPE bigint
    USING CASE WHEN last_updated ~ '^\d{4}-\d{2}(-\d{2})?$'
      THEN extract(epoch FROM to_date(last_updated || CASE WHEN length(last_updated) = 7 THEN '-01' ELSE '' END, 'YYYY-MM-DD'))::bigint
    END;

ALTER TABLE model_presets
  ALTER COLUMN vision            SET NOT NULL,
  ALTER COLUMN name              SET NOT NULL,
  ALTER COLUMN attachment        SET NOT NULL,
  ALTER COLUMN reasoning         SET NOT NULL,
  ALTER COLUMN tool_call         SET NOT NULL,
  ALTER COLUMN structured_output SET NOT NULL,
  ALTER COLUMN temperature       SET NOT NULL;

ALTER TABLE model_presets RENAME COLUMN tool_call TO tools;

ALTER TABLE model_presets
  DROP COLUMN creator_model_id,
  DROP COLUMN description,
  DROP COLUMN family,
  DROP COLUMN knowledge,
  DROP COLUMN limits,
  DROP COLUMN modalities,
  DROP COLUMN cost,
  DROP COLUMN reasoning_options,
  DROP COLUMN interleaved,
  DROP COLUMN status;

ALTER TABLE model_providers
  DROP COLUMN npm,
  DROP COLUMN env;
ALTER TABLE model_providers RENAME COLUMN api TO api_base_url;
ALTER TABLE model_providers RENAME COLUMN doc TO website;

DROP TABLE creator_models;
DROP TABLE model_preset_rebinds;
