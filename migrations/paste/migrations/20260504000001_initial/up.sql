CREATE TABLE pastes (
    id SERIAL PRIMARY KEY,
    key TEXT UNIQUE NOT NULL,
    content TEXT NOT NULL,
    content_type TEXT NOT NULL DEFAULT 'text',
    date_created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    date_viewed TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    exp_date TIMESTAMPTZ,
    nonce TEXT,
    salt TEXT,
    signature TEXT
);
