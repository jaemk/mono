ALTER TABLE room_members DROP COLUMN name;

DROP TABLE sessions;

ALTER TABLE players
    DROP CONSTRAINT players_account,
    DROP COLUMN username,
    DROP COLUMN password_hash,
    DROP COLUMN failed_logins,
    DROP COLUMN locked_until;
