# Komino Rooms

Anonymous cookie-identified players, room creation, share codes, joining, leaving, and host removal.

## Identity

### ROOM-1
A player is identified by a random 128-bit player id stored in an http-only, secure,
`SameSite=Lax` cookie named `komino_player`, scoped to path `/komino`, max age 365 days. No
account, email, or password exists.

### ROOM-2
The cookie is issued on the first request to any `/komino` route that lacks a valid one. A
missing, malformed, or unknown cookie value gets a fresh id; it never resolves to another
player.

### ROOM-3
The cookie value is the player id signed with `KOMINO_SIGNING_KEY` (hmac). A cookie whose
signature fails is treated as missing (ROOM-2).

### ROOM-4
A player has one display name, shared across all rooms. New players get a generated default
(e.g. `player-4821`). The name is 1-24 characters after trimming, with no control characters,
and is rendered as text only.

### ROOM-5
A player can change their name at any time, including mid-game. The change is broadcast to
every room the player is a current member of.

## Rooms

### ROOM-6
Any player can create a room, choosing its game settings (SET-1). The creator becomes the
room's host.

### ROOM-7
Each room has a share code: 6 characters drawn from `ABCDEFGHJKMNPQRSTUVWXYZ23456789` (no
`0/O/1/I/L`). Codes are unique across live rooms and case-insensitive on input.

### ROOM-8
The share url is `/komino/r/<CODE>`. The room page shows the code and a copy-link button.
`/komino` has a field to enter a code by hand.

### ROOM-9
Opening a share url as a player not in the room adds them as a member of the room. Opening it
as an existing member returns them to the room with their membership and stats intact.

### ROOM-10
A room holds at most 8 seated players per game. Additional members can stay in the room as
spectators and are seated in the next game if a seat is free.

### ROOM-11
A room with no member activity for 30 days is deleted along with its games and stats.

## Presence

### ROOM-12
A member is `present` while at least one websocket from their cookie is connected to the room,
and `away` otherwise. Sockets refresh a 25 second presence lease every 10 seconds, and joining
or opening the room grants one lease. Presence is shown next to each name.

### ROOM-13
Leaving (closing the tab or navigating away) does not remove membership. A returning member
resumes whatever seat and hand they had, if their game is still running.

### ROOM-14
A seated player who is `away` when their turn starts gets a grace period (SET-8, 30 seconds
by default). If they
have not returned when it ends, their turn is skipped (no draw, no action). Their cards stay
in play and are scored normally.

### ROOM-15
A member can explicitly leave a room. If seated in a running game, they forfeit (RULE-27).
Their stats for the room are kept and restored if they rejoin.

## Host controls

### ROOM-16
Only the host can start a game (RULE-1) and remove members.

### ROOM-17
The host can remove any other member. A removed member's sockets for that room are closed, and
their cookie is barred from rejoining that room by the share url.

### ROOM-18
Removing a seated player mid-game forfeits them (RULE-27).

### ROOM-19
The host can unban a removed member from a list of removed players in the room settings.

### ROOM-20
If the host leaves the room (ROOM-15), the host role passes to the longest-standing present
member. A host who is merely `away` keeps the role. A host who leaves an otherwise empty room
keeps the role only until someone joins: the next member to join becomes host.
