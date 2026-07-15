CREATE TABLE users (
    id BIGSERIAL PRIMARY KEY,
    email TEXT UNIQUE NOT NULL,
    name TEXT NOT NULL,
    -- hex encoded pbkdf2 salt and derived hash
    pw_salt TEXT NOT NULL,
    pw_hash TEXT NOT NULL,
    email_verified BOOLEAN NOT NULL DEFAULT FALSE,
    created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    modified TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- session tokens, stores the hmac of the token handed out as a cookie
CREATE TABLE auth_tokens (
    id BIGSERIAL PRIMARY KEY,
    hash TEXT UNIQUE NOT NULL,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires TIMESTAMPTZ NOT NULL,
    created TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE email_verifications (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash TEXT UNIQUE NOT NULL,
    expires TIMESTAMPTZ NOT NULL,
    created TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- anonymous browser identities, identified by an hmac'd cookie token
CREATE TABLE anons (
    id BIGSERIAL PRIMARY KEY,
    token_hash TEXT UNIQUE NOT NULL,
    created TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE orgs (
    id BIGSERIAL PRIMARY KEY,
    -- short random id used in urls and as the "direct id" for join requests
    public_id TEXT UNIQUE NOT NULL,
    name TEXT NOT NULL,
    -- 'standard' orgs require login, 'anon' maps are joinable by link
    kind TEXT NOT NULL DEFAULT 'standard' CHECK (kind IN ('standard', 'anon')),
    -- when set, users with a verified email under this domain can join freely
    allowed_email_domain TEXT,
    -- optional ttl, org and all its data are deleted after this time
    expires_at TIMESTAMPTZ,
    created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    modified TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE members (
    id BIGSERIAL PRIMARY KEY,
    org_id BIGINT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    user_id BIGINT REFERENCES users(id) ON DELETE CASCADE,
    anon_id BIGINT REFERENCES anons(id) ON DELETE CASCADE,
    role TEXT NOT NULL DEFAULT 'member' CHECK (role IN ('admin', 'member')),
    display_name TEXT NOT NULL,
    created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (user_id IS NOT NULL OR anon_id IS NOT NULL),
    UNIQUE (org_id, user_id),
    UNIQUE (org_id, anon_id)
);

CREATE TABLE invites (
    id BIGSERIAL PRIMARY KEY,
    org_id BIGINT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    email TEXT NOT NULL,
    token_hash TEXT UNIQUE NOT NULL,
    created_by BIGINT REFERENCES members(id) ON DELETE SET NULL,
    accepted_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
    expires TIMESTAMPTZ NOT NULL,
    created TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE join_requests (
    id BIGSERIAL PRIMARY KEY,
    org_id BIGINT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'denied')),
    created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    modified TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (org_id, user_id)
);

-- per-org pin categories, seeded with defaults at org creation
CREATE TABLE categories (
    id BIGSERIAL PRIMARY KEY,
    org_id BIGINT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    name TEXT NOT NULL,
    -- optional group label, e.g. 'favorites' for the favorite-place subcategories
    grp TEXT,
    color TEXT NOT NULL,
    -- max pins per member in this category, null = unlimited
    max_per_member INT,
    sort INT NOT NULL DEFAULT 0,
    UNIQUE (org_id, key)
);

CREATE TABLE pins (
    id BIGSERIAL PRIMARY KEY,
    org_id BIGINT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    member_id BIGINT NOT NULL REFERENCES members(id) ON DELETE CASCADE,
    category_id BIGINT NOT NULL REFERENCES categories(id) ON DELETE CASCADE,
    lat DOUBLE PRECISION NOT NULL,
    lng DOUBLE PRECISION NOT NULL,
    description TEXT,
    created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    modified TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX pins_org_idx ON pins(org_id);

-- one photo per pin, hidden from other members until approved by an admin
CREATE TABLE photos (
    id BIGSERIAL PRIMARY KEY,
    pin_id BIGINT UNIQUE NOT NULL REFERENCES pins(id) ON DELETE CASCADE,
    s3_key TEXT NOT NULL,
    content_type TEXT NOT NULL,
    approved BOOLEAN NOT NULL DEFAULT FALSE,
    approved_by BIGINT REFERENCES members(id) ON DELETE SET NULL,
    created TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
