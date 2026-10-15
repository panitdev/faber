-- Mount declarations in a session's scratch (Environments v2).
--
-- A plugin mounts a live view of its own content at /mnt/<plugin>/<name>;
-- the files are served by the plugin, never copied, so only the declaration is
-- kept here. Declarations live with the session and are re-established when
-- its scratch is restored; a row goes when the session does, and a plugin's
-- rows go when it is removed from the project. Mounting the same name again
-- replaces the row.
CREATE TABLE session_mount (
  session_id uuid        NOT NULL REFERENCES session(id) ON DELETE CASCADE,
  plugin_id  text        NOT NULL,
  name       text        NOT NULL,
  read_only  boolean     NOT NULL,
  -- clock_timestamp so two mounts made in one transaction still have an
  -- order; the head lists a session's mounts in it.
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (session_id, plugin_id, name)
);

-- A plugin removed from a project drops its mounts across that project's
-- sessions.
CREATE INDEX session_mount_plugin_idx ON session_mount (plugin_id);
