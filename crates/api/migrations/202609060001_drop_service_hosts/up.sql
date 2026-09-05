-- Drop the service-host concept: every host and every image is owned.
--
-- Service rows are deleted rather than migrated — a shared machine has no
-- single owner to attribute it to, and a template nobody owns has nobody to
-- give it to. Everything hanging off a service host goes with it by cascade
-- (`host_container`, `host_probe`, `session_environment`, `agent_credential`,
-- `agent_enrollment`), and the tenancy tables (`host_user`,
-- `host_user_quota`, `user_subject`) are dropped whole.
--
-- `host_container.user_id` stays: it still scopes container authorization to
-- the container's owner without a join through its host.

DELETE FROM host WHERE user_id IS NULL;
DELETE FROM image WHERE user_id IS NULL;

DROP INDEX image_name_service;
DROP INDEX image_name_owned;
ALTER TABLE image ALTER COLUMN user_id SET NOT NULL;
ALTER TABLE image ADD CONSTRAINT image_user_id_name_key UNIQUE (user_id, name);

DROP TABLE host_user_quota;
DROP TABLE host_user;
DROP TABLE user_subject;
DROP SEQUENCE subject_seq;

DROP INDEX host_container_owner;

ALTER TABLE host DROP CONSTRAINT host_service_needs_container_root_uid;
ALTER TABLE host DROP CONSTRAINT host_service_needs_data_root;
ALTER TABLE host
  DROP COLUMN default_cpu_millis,
  DROP COLUMN default_memory_bytes,
  DROP COLUMN default_storage_bytes,
  DROP COLUMN default_container_max,
  DROP COLUMN user_data_root,
  DROP COLUMN container_root_uid;

DROP INDEX host_name_service;
DROP INDEX host_name_owned;
ALTER TABLE host ALTER COLUMN user_id SET NOT NULL;
ALTER TABLE host ADD CONSTRAINT host_user_id_name_key UNIQUE (user_id, name);
