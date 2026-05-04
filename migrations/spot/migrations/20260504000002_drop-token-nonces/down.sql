begin;
alter table spot.users add column access_nonce text not null default '';
alter table spot.users add column refresh_nonce text not null default '';
commit;
