CREATE TABLE transfer_user (
    id           SERIAL      PRIMARY KEY,
    email        TEXT        NOT NULL UNIQUE,
    auth_id      INT         REFERENCES auth(id),
    google_sub   TEXT        UNIQUE,
    date_created TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT user_has_auth CHECK (auth_id IS NOT NULL OR google_sub IS NOT NULL)
);

CREATE TABLE pending_registration (
    id           SERIAL      PRIMARY KEY,
    uuid_        UUID        UNIQUE NOT NULL,
    email        TEXT        NOT NULL,
    auth_id      INT         NOT NULL REFERENCES auth(id) ON DELETE CASCADE,
    code_hash    BYTEA       NOT NULL,
    attempts     INT         NOT NULL DEFAULT 0,
    resend_after TIMESTAMPTZ NOT NULL DEFAULT now(),
    expire_date  TIMESTAMPTZ NOT NULL,
    date_created TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE transfer_session (
    id           SERIAL      PRIMARY KEY,
    uuid_        UUID        UNIQUE NOT NULL,
    user_id      INT         NOT NULL REFERENCES transfer_user(id) ON DELETE CASCADE,
    expire_date  TIMESTAMPTZ NOT NULL,
    date_created TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE upload      ADD COLUMN user_id INT REFERENCES transfer_user(id);
ALTER TABLE init_upload ADD COLUMN user_id INT REFERENCES transfer_user(id);

CREATE INDEX ON pending_registration (expire_date);
CREATE INDEX ON transfer_session     (expire_date);
CREATE INDEX ON upload      (user_id) WHERE user_id IS NOT NULL;
CREATE INDEX ON init_upload (user_id) WHERE user_id IS NOT NULL;
