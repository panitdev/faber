-- The administrator marker is no longer part of the API contract.
ALTER TABLE users DROP COLUMN IF EXISTS admin_since;
