# Komino Realtime

Server-authoritative state, websocket delivery, cross-machine fanout, and first-wins matching.

## Authority

### RT-1
Postgres is the source of truth for every room and game. No game state lives only in a
machine's memory, so any machine can serve any room and a restart loses nothing.

### RT-2
Every action is validated and applied server side inside one database transaction that
locks the room row, then the game row (`SELECT ... FOR UPDATE`, always in that order). Actions
on a game are therefore serialized.

### RT-3
Each game has a monotonically increasing `version`, incremented by every applied change and
included in each view. Actions carry a one-time token scoped to what they depend on, checked
under the row lock (RT-15, RT-9); a spent or outdated token is rejected, and the rejected
client already holds or receives the current view.

## Transport

### RT-4
Clients connect to `/komino/r/<CODE>/ws` (axum websocket). Actions are sent as json messages
over the socket with an optional `ref`; the server replies to the sender with a `result`
echoing the `ref`, either ok or a typed rejection (`not_your_turn`, `not_seated`, `invalid`,
`too_late`, `stale`, `forbidden`, `removed`). The same actions are accepted over http at
`POST /komino/api/rooms/<CODE>/action`.

### RT-5
After every applied action the server pushes each connected member a fresh view of the game
redacted for that member: public card values, slot positions and counts for everything else,
and references to any private values they may reveal (SEAL-1), plus the event that caused the
update.

### RT-6
Machines fan updates out to each other with postgres `LISTEN`/`NOTIFY` on the shared `komino`
channel, sent inside the action's transaction so it fires on commit. The payload is only the
room id; each machine then loads and redacts state for its own sockets.

### RT-7
On connect, reconnect, and every update, the client receives the full redacted view. Events
are never replayed.

### RT-8
The client reconnects with backoff after a dropped socket and shows a `reconnecting` state
while disconnected.

## Matching race

### RT-9
A match message names the discard it targets by its discard sequence number, not by version,
so a match is not rejected as `stale` because an unrelated action landed first.

### RT-10
The first match transaction to commit against a discard sequence number wins. Any later match
against the same sequence number is rejected as `too_late` (RULE-21), with no penalty.

### RT-11
The turn player's next action (draw, take, call) and a match can race. Whichever commits first
applies; the other is re-checked against the new state and either applied or rejected. A
discard stays matchable while a later turn proceeds until a newer card covers it.

### RT-12
At the end of the last final turn, scoring waits 2 seconds so match attempts against the last
discard can land before cards are revealed (RULE-24).

### RT-13
Every match attempt, successful or not, is broadcast with the matcher, the targeted slot, and
the outcome, so all players see who got there first.

## Timers

### RT-14
Ready (RULE-7), turn grace (ROOM-14), and scoring delay (RT-12) deadlines are stored on the
game row. Any machine handling the room can fire them; firing re-checks the deadline under the
row lock so a timer applies once.

## Turn token

### RT-15
Each game has a `turn_seq`, bumped whenever the turn's status, player, or stage changes
(including by timers and forfeits). Every turn action (draw, take, swap, discard, komino, peek,
blind swap, look and swap, skip) must carry the `turn_seq` from the view it was chosen in. A
missing, spent, or outdated token is rejected as `stale` (http 409), so a repeated or outdated
confirm never applies. Matches carry the discard sequence number instead (RT-9) and do not move
`turn_seq`; ready and start carry no token.
