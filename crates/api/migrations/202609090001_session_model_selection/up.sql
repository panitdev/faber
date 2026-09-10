-- What the next message in a session goes to: which model, and how hard it
-- thinks.
--
-- On the session rather than on the user, because the choice is part of what
-- a conversation *is* — a thread opened to grind through a refactor at high
-- effort is not the same thread as the one opened to look something up, and a
-- single global pick would have opening the second silently change the first.
--
-- `model_alias` stores the alias, not the model's id: the alias is what the
-- user types (`faber -m fast`) and what `POST /messages` already carries, and
-- in a shared workspace it resolves per member against their own row. A
-- selection whose model was since renamed or deleted stops resolving, which
-- the caller sees as "pick a model" rather than as a message sent somewhere
-- unexpected.
ALTER TABLE session ADD COLUMN model_alias text;

-- `off`, `on`, or an effort level (`low`/`medium`/`high`/`xhigh`/`max`).
-- NULL is "never picked", which resolves to whatever the model's own
-- definition defaults to. Deliberately not a foreign key or an enum: the
-- levels a given model offers are declared on the model row, and the API
-- clamps a selection the current model does not offer rather than refusing to
-- run.
ALTER TABLE session ADD COLUMN thinking_effort text;

COMMENT ON COLUMN session.model_alias IS
  'Alias of the model new messages go to, or NULL if none was picked.';
COMMENT ON COLUMN session.thinking_effort IS
  'Thinking selection: off, on, or an effort level. NULL means the model default.';
