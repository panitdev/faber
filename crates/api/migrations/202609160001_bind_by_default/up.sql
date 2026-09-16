-- Per-environment toggle: when true, new sessions auto-bind this environment
-- without requiring an @mention.
--
-- On both tables because the two represent the two kinds of bindable target:
-- a host used directly (direct exec mode with a root_path), and a container
-- on a docker-mode host.

ALTER TABLE host ADD COLUMN bind_by_default boolean NOT NULL DEFAULT false;
ALTER TABLE host_container ADD COLUMN bind_by_default boolean NOT NULL DEFAULT false;

COMMENT ON COLUMN host.bind_by_default IS
  'When true, new sessions auto-bind this host without an @mention.';
COMMENT ON COLUMN host_container.bind_by_default IS
  'When true, new sessions auto-bind this container without an @mention.';
