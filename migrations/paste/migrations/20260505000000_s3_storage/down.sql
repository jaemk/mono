-- Reverse: remove storage_uri and restore the original inline-content columns.
DELETE FROM pastes;

ALTER TABLE pastes
    DROP COLUMN storage_uri,
    ADD COLUMN content   TEXT NOT NULL DEFAULT '',
    ADD COLUMN nonce     TEXT,
    ADD COLUMN salt      TEXT,
    ADD COLUMN signature TEXT;

ALTER TABLE pastes
    ALTER COLUMN content DROP DEFAULT;

