-- Which of the project's plugins the sessions a binding opens may use. A
-- channel member is not the owner, so the owner may keep, say, the hosts a
-- project reaches out of what a Discord thread can drive. NULL is every
-- plugin, as before. The opening plugin itself is always kept: its sessions
-- need its tools to answer.
ALTER TABLE project_binding ADD COLUMN session_plugins text[];
