DELETE FROM session_inbox_dedupe WHERE scope <> 'plugin';
ALTER TABLE session_inbox_dedupe
  DROP CONSTRAINT session_inbox_dedupe_pkey,
  DROP COLUMN scope,
  ADD CONSTRAINT binding_dedupe_pkey PRIMARY KEY (session_id, dedupe_id);
ALTER TABLE session_inbox_dedupe
  RENAME CONSTRAINT session_inbox_dedupe_session_id_fkey TO binding_dedupe_session_id_fkey;
ALTER TABLE session_inbox_dedupe
  RENAME CONSTRAINT session_inbox_dedupe_id_shape TO binding_dedupe_id_shape;
ALTER INDEX session_inbox_dedupe_created_idx RENAME TO binding_dedupe_created_idx;
ALTER TABLE session_inbox_dedupe RENAME TO binding_dedupe;

-- Only what a binding sent to a session it opened can go back.
DELETE FROM session_inbox i
WHERE i.origin->'author'->>'source' = 'agent'
   OR NOT EXISTS (SELECT 1 FROM binding_session b WHERE b.session_id = i.session_id);

ALTER TABLE session_inbox
  ADD COLUMN author   jsonb,
  ADD COLUMN reply_to jsonb;

UPDATE session_inbox
SET author = CASE WHEN origin->'author' ? 'id' THEN jsonb_strip_nulls(jsonb_build_object(
      'id', origin->'author'->>'id',
      'name', coalesce(origin->'author'->>'name', ''),
      'avatar', origin->'author'->>'avatar')) END,
    reply_to = CASE origin->'replyTo'->>'kind'
      WHEN 'agent' THEN '"agent"'::jsonb
      WHEN 'author' THEN jsonb_build_object('author', jsonb_build_object(
        'id', origin->'replyTo'->>'id',
        'name', coalesce(origin->'replyTo'->>'name', '')))
    END;

ALTER TABLE session_inbox DROP COLUMN origin;
ALTER TABLE session_inbox
  RENAME CONSTRAINT session_inbox_session_id_fkey TO binding_pending_session_id_fkey;
ALTER SEQUENCE session_inbox_id_seq RENAME TO binding_pending_id_seq;
ALTER INDEX session_inbox_session_idx RENAME TO binding_pending_session_idx;
ALTER INDEX session_inbox_pkey RENAME TO binding_pending_pkey;
ALTER TABLE session_inbox RENAME TO binding_pending;
