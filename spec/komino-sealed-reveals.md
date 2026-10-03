# Komino Sealed Reveals

Private card values only leave the server encrypted to a short-lived client ECDH key.

## Views

### SEAL-1
Room views (http and websocket) never carry a private card value. Before scoring, no slot in a
member's view has a value, the drawn card is `null` for everyone, and the viewer's own peeks
are listed as `{id, seat, slot, until}` without the value. Public values stay in views: the
top discard, event log payloads, and every hand once the game is scored.

### SEAL-2
A private value reaches the client only as a sealed reveal response (SEAL-6), so request logs
and websocket frames hold ciphertext that needs the client's in-memory private key to read.

## Keys

### SEAL-3
The server holds a static ECDH P-256 key pair. The private scalar is `KOMINO_ECDH_KEY` (64 hex
characters); without it a dev-only key is used, and startup fails when komino is enabled with
the dev key. `GET /komino/api/key` returns `{kid, public_key}`: the uncompressed SEC1 point,
base64url, and `kid`, the first 16 hex characters of its sha256.

### SEAL-4
The client fetches the server key once at startup and caches it for the page's lifetime.

### SEAL-5
The client generates a non-extractable ECDH P-256 key pair in memory (WebCrypto) and uses it
for at most 5 minutes, then generates a new pair and drops the old one. The server enforces
the window: the first reveal request with a client public key binds that key to the cookie
player with a timestamp. A key older than 5 minutes is rejected as `key_expired` (http 409),
and a key bound to another player as `forbidden` (http 403). On `key_expired` the client makes
a new pair and retries once.

## Reveals

### SEAL-6
`POST /komino/api/rooms/<CODE>/reveal` takes `{client_key, what, id?}` where `what` is one of:

| what | Allowed when | Returns |
|------|--------------|---------|
| `opening` | the cookie player is seated, the game is peeking, and they are not ready (RULE-7) | their slots 3 and 4 |
| `drawn` | it is the cookie player's turn and they drew from the deck (RULE-9) | the drawn card |
| `peek` | `id` names a live peek made by the cookie player (RULE-12, RULE-13, RULE-15) | the peeked slot's value at peek time |

Every check runs on the server against the cookie identity and current game state. The
requester must be a current member, so observers can never reveal (OBS-5). Anything else is
`forbidden`. A peek can be revealed once: a second request is `already_revealed` (http 409).
Opening and drawn reveals can repeat while allowed, so a reload keeps working.

### SEAL-7
The response is `{kid, salt, iv, ct}` (base64url). The AES-256-GCM key is HKDF-SHA256 over
the ECDH shared secret (the x coordinate of server private x client public), with the
response's random 16 byte `salt` and info `komino reveal v1`. The iv is 12 random bytes and
the additional data is the room code. The plaintext is json:
`{"cards":[{"seat","slot","v"}]}` or `{"card": v}`.

### SEAL-8
If decrypting a response fails, the client fetches the server key again and retries the
decrypt once with it; if that fails too, it shows an error.

### SEAL-9
The client keeps a decrypted value only while it is shown: a peek until its deadline or `hide
card`, the drawn card until the turn moves on, the opening cards until play starts or `ready`.
Then the value is deleted. Nothing is written to cookies or browser storage.

### SEAL-10
Sealing protects what is logged, not what is on screen. While a value is shown, or while the
current client key is alive, a player with dev tools can still read it.

## Storage

### SEAL-11
`client_keys` (key_hash, player_id, first_seen) records SEAL-5 bindings; rows older than a day
are deleted by the hourly cleanup. `reveal_fetches` (reveal_id, game_id) records spent peek
reveals and cascades with the game. Each peek in the game state carries a random `id`.
