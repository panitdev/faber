-- How fast a call answered: the wait for its first output, its tokens per
-- second, and the characters that arrived in each slice of time.  NULL for an
-- exchange written before this was measured, and for a call that was never
-- sent.
ALTER TABLE exchange ADD COLUMN timing jsonb;
