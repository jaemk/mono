ALTER TABLE init_upload DROP COLUMN user_id;
ALTER TABLE upload      DROP COLUMN user_id;
DROP TABLE pending_registration;
DROP TABLE transfer_session;
DROP TABLE transfer_user;
