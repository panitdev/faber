-- Images stop being a saved unit.
--
-- A template was a name over a registry reference, a default root path, and
-- default mounts. In practice it was the reference and nothing else, and
-- making the user name and save one before they could start a container was
-- ceremony. A spawn now takes the reference directly.
--
-- The container keeps the reference it was started from as provenance, in
-- place of a pointer to a template. Like `image_id` before it, nothing
-- resolves through it: the container's ref is what execution uses.

ALTER TABLE host_container ADD COLUMN image text;

UPDATE host_container c
   SET image = i.reference
  FROM image i
 WHERE c.image_id = i.id;

ALTER TABLE host_container DROP COLUMN image_id;

DROP TABLE image;

COMMENT ON COLUMN host_container.image IS
  'The registry reference faber started this container from. Provenance only; NULL for containers the user registered.';
