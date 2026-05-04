begin;
-- access_token and refresh_token now store base64(nonce ‖ ciphertext) in a
-- single column via Enc::encode(); the separate nonce columns are no longer needed.
-- Existing encrypted tokens will be invalid after this migration; users will
-- need to re-authenticate via Spotify (tokens are short-lived anyway).
alter table spot.users drop column access_nonce;
alter table spot.users drop column refresh_nonce;
commit;

