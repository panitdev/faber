-- Where a target's agent starts.
--
-- Paths are absolute and nothing carries over between calls, so the agent is
-- told each environment's home in its system head, read from the latest probe
-- rather than from a live call. NULL for probes recorded before the probe
-- asked, and for probes that came in through the API without one.

ALTER TABLE host_probe ADD COLUMN home text;

COMMENT ON COLUMN host_probe.home IS
  'The target''s $HOME when inside its root, else the root. NULL when the probe did not report one.';
