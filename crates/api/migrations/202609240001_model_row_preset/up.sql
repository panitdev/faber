-- Link a configured model to the preset that describes it.
--
-- A model row keeps what a run needs to reach the endpoint — alias, base_url,
-- wire, wire_id, family, credential, params — and drops the `capabilities`
-- blob, whose descriptive half (vision, tools, the context window, prices) a
-- preset already owns. `preset_id` is nullable: a model no catalog entry
-- describes runs against the built-in empty preset, which states nothing and
-- costs nothing. The FK is SET NULL rather than CASCADE: deleting a preset
-- must not delete the model that referenced it, only leave it undescribed.
ALTER TABLE models
  ADD COLUMN preset_id uuid REFERENCES model_presets(id) ON DELETE SET NULL;

CREATE INDEX models_preset_id_idx ON models (preset_id);

-- The reasoning settings were the only part of `capabilities` a run reads and
-- that no preset carries; they move to `params`, where `advanced` already
-- lives. The rest of the blob is dropped: it is what the preset now says.
-- `params` is not always an object — older rows wrote JSON `null` — so it is
-- normalized before the merge; concatenating onto anything else builds an
-- array instead of merging.
UPDATE models
SET params = (CASE WHEN jsonb_typeof(params) = 'object' THEN params ELSE '{}'::jsonb END)
  || jsonb_strip_nulls(
    jsonb_build_object(
      'thinking', capabilities -> 'thinking',
      'reasoning_history', capabilities -> 'reasoning_history'
    )
  )
WHERE capabilities ? 'thinking' OR capabilities ? 'reasoning_history';

ALTER TABLE models DROP COLUMN capabilities;
