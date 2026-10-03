# Komino Observers

Read-only watch view of a room, at most 4 connected observers, no hidden cards.

## Access

### OBS-1
Any room can be watched at `/komino/r/<CODE>/watch`. Watching does not join the room: an
observer is not a member, is never seated, and does not appear in the member list or stats.

### OBS-2
A player removed from the room (ROOM-17) cannot watch it either; the watch page shows the same
removed notice as the room page. A removal while watching closes the watch socket with a
`removed` message, as for a member socket.

### OBS-3
At most 4 observer sockets are connected to a room at once, counted across every machine. A
fifth observer gets a `full` message and its socket is closed; the page says the room is full
instead of reconnecting.

### OBS-4
An observer socket holds a 25 second lease refreshed every 10 seconds, and releases it on
disconnect, so an observer slot left by a crashed machine frees itself within 25 seconds. A
socket whose lease is gone when it refreshes (expired, possibly reclaimed) closes; the page
reconnects and claims a slot again, or hears that the room is full.

## View

### OBS-5
Observers get the same live view as members (RT-5, RT-7) with no viewer seat: every card value
a player has not revealed publicly stays hidden. In particular the opening peek of slots 3 and
4 (RULE-7), the drawn card (RULE-9), and peeked values (RULE-16) are never sent to an
observer. All hands are shown face up once the game is scored, as for members.

### OBS-6
Observers see which slots were peeked (UI-17), the event log, presence, the komino banner,
timers, and the stats table.

### OBS-7
Observers cannot act. The watch page shows no turn controls, no confirm dialog, and no host
controls, and any action sent over an observer socket is rejected as `forbidden`.

### OBS-8
Members see how many observers are connected, and the room page has a `copy watch link`
button next to `copy link`. The watch page links to the room page so an observer can join as
a player.

## Storage

### OBS-9
Observer leases live in a `room_observers` table (id, room_id, until) that cascades with the
room. Claiming a lease locks the room row, so concurrent claims on any machine cannot exceed
OBS-3. Claiming or releasing a lease notifies the room (RT-6) so members' observer counts
update.
