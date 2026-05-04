begin;
create table spot.one_time_tokens (
    token text primary key not null,
    expires timestamptz not null,
    created timestamptz not null default now()
);
create index one_time_tokens_expires on spot.one_time_tokens(expires);
commit;

