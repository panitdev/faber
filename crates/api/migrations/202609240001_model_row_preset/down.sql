-- Restore the shape the previous code reads: `capabilities` comes back, and
-- the reasoning settings this migration moved into `params` move back out.
-- The descriptive half of `capabilities` cannot be recovered — it was dropped,
-- not relocated — so the restored column holds only what `params` still
-- carries. `preset_id` is dropped with it.
ALTER TABLE models
  ADD COLUMN capabilities jsonb NOT NULL DEFAULT '{}';

UPDATE models
SET capabilities = jsonb_strip_nulls(
  jsonb_build_object(
    'thinking', params -> 'thinking',
    'reasoning_history', params -> 'reasoning_history'
  )
)
WHERE jsonb_typeof(params) = 'object'
  AND (params ? 'thinking' OR params ? 'reasoning_history');

ALTER TABLE models DROP COLUMN preset_id;
