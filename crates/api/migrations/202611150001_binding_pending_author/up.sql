-- Who said a waiting message, as the plugin named them, and what it answers.
-- Both NULL for a message with no person behind it.
ALTER TABLE binding_pending
  ADD COLUMN author   jsonb,
  ADD COLUMN reply_to jsonb;
