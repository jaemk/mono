-- Replace the inline content/nonce/salt/signature columns with a pointer to S3.
-- Existing rows (if any) cannot be migrated automatically since their content
-- is no longer accessible here; they are removed by this migration.

DELETE FROM pastes;

ALTER TABLE pastes
    DROP COLUMN content,
    DROP COLUMN nonce,
    DROP COLUMN salt,
    DROP COLUMN signature,
    ADD COLUMN storage_uri TEXT NOT NULL DEFAULT '';

-- Remove the temporary default — all future inserts supply the value explicitly.
ALTER TABLE pastes
    ALTER COLUMN storage_uri DROP DEFAULT;

