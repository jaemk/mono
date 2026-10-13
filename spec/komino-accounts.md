# Komino Accounts

Optional username and password accounts over the anonymous player (ROOM-1), session cookies,
default and per-room display names, and the cookie notice.

## Accounts

### ACCT-1
An account is a player with a username and a password hash. Anonymous players (ROOM-1) keep
working unchanged; an account is never required to create, join, or watch a room.

### ACCT-2
Any anonymous player can create an account by choosing a username and password. The current
player becomes the account: its id, rooms, memberships, and stats are kept. A player that is
already an account must sign out before creating another.

### ACCT-3
Usernames are 3-24 characters of `a-z`, `0-9`, `_`, `-`, and `.`, compared case-insensitively
and stored lower case. A username belongs to one account; a taken one is refused with `409
taken`.

### ACCT-4
Passwords are 8 or more characters and at most 256 bytes. They are stored only as a salted
PBKDF2-HMAC-SHA512 hash (`common::crypto::hash_password`, 210,000 rounds, 64-byte random salt)
in the form `pbkdf2-sha512$<rounds>$<salt>$<hash>`, so the rounds can be raised later.
Verification is constant time. Hashing runs off the async workers.

### ACCT-5
Signing in takes a username and password and starts a new session (ACCT-6) for the account. A
wrong password and an unknown username give the same `401 unauthorized` "wrong username or
password", and an unknown username still costs one password hash, so the two can't be told
apart by timing. Signing in from a browser that already holds a session ends that session.

## Sessions and cookies

### ACCT-6
A session is a random 256-bit token in the `komino_session` cookie: http-only, secure,
`SameSite=Lax`, path `/komino`, max age 30 days. Only the token's SHA-256 is stored. A session
lasts 30 days from when it was last extended; a request on a session with under 29 days left
extends it and sends the cookie again, so an account in use stays signed in. Expired sessions
are refused and swept with the idle room cleanup (ROOM-11).

### ACCT-7
The request's player is the live session's account if there is one, and otherwise the
anonymous `komino_player` cookie's player (ROOM-2). The player cookie never resolves to an
account: once a player becomes an account, only a session reaches it, and a player cookie
naming one gets a fresh anonymous id. Sign up drops the player cookie. Room sockets follow the
same rules and refuse a cookie that names no one.

### ACCT-8
Signing out deletes the session server side and clears its cookie, so a copy of the token
reaches no one. The browser then returns to the anonymous player its player cookie names, or
gets a fresh one.

### ACCT-9
Five wrong passwords in a row lock an account's sign in for 5 minutes, during which even the
right password gets `429 locked`. A successful sign in resets the count, and a recovery
(ACCT-18) lifts the lock. Open sessions are not affected.

### ACCT-10
Account endpoints take json bodies, which a cross-site form can't send, and the session cookie
is `SameSite=Lax`, so other sites can't sign a browser in or act as it.

## Names

### ACCT-11
A player has a default display name, used in every room they haven't named themselves in.
An account's default is its username unless a display name is given at sign up or changed
later. Clearing an account's default name sets it back to the username. An anonymous player's
default follows ROOM-4.

### ACCT-12
A member can set a display name for one room, over their default, at any time, including
mid-game. It shows in that room's member list, game, log, and stats, and is broadcast to the
room. A blank room name clears it, so the default shows again. Changing the default does not
change a room name. Room names follow the ROOM-4 limits, and only current members can set one.

## Interface

### ACCT-13
The header shows `sign in` for an anonymous player, and `signed in as <username>`, linking to
the account page (ACCT-17), with `sign out` for an account. Sign in opens a dialog with username
and password that switches to `create an account`, which adds an optional display name and says
the player's rooms and stats move to the account, or to `forgot your password?`, which asks for
the username, a recovery code, and a new password (ACCT-18). Errors show in the dialog. A
successful sign in, sign up, recovery, or sign out loads the page again as the new identity;
after a sign up or recovery, once the new recovery code has been shown and acknowledged. The password field is cleared when the dialog
closes. In a room the header name field edits the room name (`name here`); in the lobby and on
a watch page it edits the default name.

### ACCT-14
komino sets only strictly necessary cookies (`komino_player`, `komino_session`), so there is
nothing to opt into. A notice says so and lists what they are for, with an `ok` button; it
shows until acknowledged, which is remembered in browser storage, not a cookie.

## Managing an account

### ACCT-15
A signed in account can change its password by giving the current one and a new one that meets
ACCT-4. A wrong current password gets `401` "wrong password" and counts toward the lock
(ACCT-9), so a stolen session can't be used to guess the password. A change ends every session
of the account, and the browser that made it gets a new one, so it stays signed in and every
other device is signed out.

### ACCT-16
A signed in account can delete itself by giving its password, checked as in ACCT-15. Deletion
is permanent and in one transaction: the account leaves every room as in ROOM-15, forfeiting a
running game and passing the host role on (ROOM-20). A room it still hosts with no current
member to take over goes to its earliest member who was not removed, until the next join takes
the role; a room with no such member is deleted. The player row is then deleted with its
sessions, memberships, and stats, its cookie is cleared, and the username is free again. Rooms
show a deleted player's past moves as `someone`.

### ACCT-17
`/komino/account` is the account page. Every player sees the rooms they are a current member
of, up to 50, most recently active first, each linking to the room and saying whether they host
it, how many players are in it, and whether a game is running; and their lifetime stats
(STAT-11). An anonymous player is told these belong to this browser and offered sign in. An
account also sees its username and how many devices it is signed in on, a sign out other
devices button (ACCT-19), and forms to change the username (ACCT-20), change the password,
make a new recovery code (ACCT-18), and delete the account, which asks to confirm before
sending. Errors show under each form. A changed password clears both fields and says other
devices were signed out; a deleted account returns to `/komino` as a new anonymous player.
`GET /komino/api/account` returns the player, `sessions` (live sessions), `recovery` (whether a
recovery code is set), and `rooms` (`code`, `host`, `members`, `playing`, `last_active`).

### ACCT-18
An account has at most one recovery code: 25 random characters from `0-9a-z` without `i`, `l`,
`o`, and `u` (125 bits), shown in five groups of five. Only its SHA-256 is stored. Sign up
returns one, and so does `POST /komino/api/account/recovery` given the password (checked as in
ACCT-15), which replaces the old one. `POST /komino/api/account/recover` takes the username,
the code, and a new password meeting ACCT-4: it sets the password, replaces the code with a new
one, lifts the lock (ACCT-9), ends every session, and signs this browser in. Case, spaces, and
dashes in a typed code don't matter, and `i`, `l`, and `o` read as `1`, `1`, and `0`. A wrong
username or code gets the same `401` "wrong username or recovery code"; a code works once. A
new code is shown in a dialog that stays until the player says it is saved, with a copy button,
and responses holding one are `Cache-Control: no-store`.

### ACCT-19
`POST /komino/api/account/sign-out-others` ends every session of the signed in account except
the one making the request, and returns how many ended. It needs no password, since it only
takes access away.

### ACCT-20
A signed in account can change its username, given its password (checked as in ACCT-15). The
new one follows ACCT-3; a taken one is refused with `409 taken`, and the old one is free again.
A default name that was the old username becomes the new one; a chosen default name stays.
Rooms the account is in are refreshed. Sessions are not affected.

### ACCT-21
An anonymous player not seen for 90 days, or for 7 days if it has no lifetime stats, is
deleted with its stats, match times, and partner rows, by the hourly cleanup (ROOM-11). A
player still in any room's member list, or hosting one, is kept until those rooms are deleted.
Accounts are never deleted this way.

### ACCT-22
Every request that checks or sets a password (sign up, sign in, recovery, a new recovery code,
password and username changes, and deletion) draws from a bucket per client address, as the
`fly-client-ip` header gives it, with an IPv6 address counted by its /64: 30 at once, refilled one every 2 seconds, per machine. An empty
bucket gets `429 slow_down` before any password work. A machine runs at most half its cores'
worth of password hashes at once; a request that waits 10 seconds for one gets `503 busy`.

### ACCT-23
A stored password hash made with fewer rounds than today's (ACCT-4), or another scheme, is made
again with today's settings the next time its password is checked successfully.
