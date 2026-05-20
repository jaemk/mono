create table auth (
    id            serial      primary key,
    salt          bytea       not null,
    hash          bytea       not null,
    date_created  timestamptz not null default now()
);

create table init_upload (
    id                serial      primary key,
    uuid_             uuid        unique not null,
    file_name_hash    bytea       not null,
    content_hash      bytea       not null,
    size_             bigint      not null,
    nonce             bytea       not null,
    access_password   int         not null unique references auth(id) on delete cascade,
    deletion_password int         unique references auth(id) on delete cascade,
    download_limit    int,
    expire_date       timestamptz not null,
    date_created      timestamptz not null default now()
);

create table upload (
    id                serial      primary key,
    uuid_             uuid        unique not null,
    file_name_hash    bytea       not null,
    content_hash      bytea       not null,
    size_             bigint      not null,
    storage_uri       text        not null,
    nonce             bytea       not null,
    access_password   int         not null unique references auth(id) on delete cascade,
    deletion_password int         unique references auth(id) on delete cascade,
    download_limit    int,
    expire_date       timestamptz not null,
    deleted           bool        not null default false,
    date_created      timestamptz not null default now()
);

create table init_download (
    id           serial      primary key,
    uuid_        uuid        unique not null,
    usage        text        not null check (usage in ('content', 'confirm')),
    upload_id    int         not null references upload(id) on delete cascade,
    date_created timestamptz not null default now()
);

create table download (
    id           serial      primary key,
    upload_id    int         not null references upload(id) on delete cascade,
    date_created timestamptz not null default now()
);

create table status (
    id            serial      primary key,
    upload_count  bigint      not null default 0,
    total_bytes   bigint      not null default 0,
    date_modified timestamptz not null default now()
);
insert into status (upload_count, total_bytes) values (0, 0);
