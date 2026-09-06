-- Reversing this restores the column shape, not its discarded timestamps.
ALTER TABLE users ADD COLUMN admin_since TIMESTAMPTZ;
