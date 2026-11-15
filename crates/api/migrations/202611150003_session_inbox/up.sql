-- The queue of messages waiting on a session, for any project session and
-- any sender, where it was only for sessions a plugin's binding opened and
-- only for that binding's messages. An agent's prompt to another agent, and
-- an agent's report to the one that started it, wait the same way.
ALTER TABLE binding_pending RENAME TO session_inbox;
ALTER INDEX binding_pending_pkey RENAME TO session_inbox_pkey;
ALTER INDEX binding_pending_session_idx RENAME TO session_inbox_session_idx;
ALTER SEQUENCE binding_pending_id_seq RENAME TO session_inbox_id_seq;
ALTER TABLE session_inbox
  RENAME CONSTRAINT binding_pending_session_id_fkey TO session_inbox_session_id_fkey;

-- Who sent a waiting message, as its transcript row will carry it: the
-- sender is settled when the message is accepted, not when a run takes it.
ALTER TABLE session_inbox ADD COLUMN origin jsonb;

UPDATE session_inbox i
SET origin = jsonb_strip_nulls(jsonb_build_object(
  'author', jsonb_build_object(
    'source', CASE WHEN b.plugin_type IN ('faber', 'agent')
                   THEN 'plugin-' || b.plugin_type ELSE b.plugin_type END,
    'id', i.author->>'id',
    'name', i.author->>'name',
    'avatar', i.author->>'avatar'),
  'replyTo', CASE
    WHEN i.reply_to = '"agent"'::jsonb THEN jsonb_build_object('kind', 'agent')
    WHEN i.reply_to ? 'author' THEN jsonb_build_object(
      'kind', 'author',
      'id', i.reply_to->'author'->>'id',
      'name', i.reply_to->'author'->>'name')
  END))
FROM binding_session b
WHERE b.session_id = i.session_id;

-- Only bindings wrote rows, so none is left without a sender.
DELETE FROM session_inbox WHERE origin IS NULL;

ALTER TABLE session_inbox
  ALTER COLUMN origin SET NOT NULL,
  DROP COLUMN author,
  DROP COLUMN reply_to;

-- Dedupe ids are per kind of sender, so an agent's id and a plugin's never
-- meet. Every id recorded so far is a plugin's.
ALTER TABLE binding_dedupe RENAME TO session_inbox_dedupe;
ALTER INDEX binding_dedupe_created_idx RENAME TO session_inbox_dedupe_created_idx;
ALTER TABLE session_inbox_dedupe
  RENAME CONSTRAINT binding_dedupe_id_shape TO session_inbox_dedupe_id_shape;
ALTER TABLE session_inbox_dedupe
  RENAME CONSTRAINT binding_dedupe_session_id_fkey TO session_inbox_dedupe_session_id_fkey;
ALTER TABLE session_inbox_dedupe ADD COLUMN scope text NOT NULL DEFAULT 'plugin';
ALTER TABLE session_inbox_dedupe ALTER COLUMN scope DROP DEFAULT;
ALTER TABLE session_inbox_dedupe
  DROP CONSTRAINT binding_dedupe_pkey,
  ADD CONSTRAINT session_inbox_dedupe_pkey PRIMARY KEY (session_id, scope, dedupe_id);
