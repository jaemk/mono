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

### RT-21
Everything a view shows (room, observer count, members, newest game, its newest 40 events,
stats) loads in one query. Each ping (RT-6) starts a new snapshot for the room on that machine;
the first of the room's sockets to need it loads it, the rest reuse it, and each renders and
redacts its own view in memory. A socket's membership check reads the same snapshot. A socket's
first view on connect loads its own.

### RT-8
The client reconnects with backoff after a dropped socket and shows a `reconnecting` state
while disconnected.

## Matching race

### RT-9
A match message names the discard it targets by its discard sequence number, not by version,
so a match is not rejected as `stale` because an unrelated action landed first.

### RT-10
Among the matches settled against a discard sequence number, the first to apply correctly
wins, in the order of RT-17. Any later match against the same sequence number is rejected as
`too_late` (RULE-21), with no penalty.

### RT-11
The turn player's next action and a match can race. Pending matches settle before any action
that changes the top discard or a hand (RT-19); a match that arrives after such an action is
checked against the new state and either claimed or rejected. A discard stays matchable while
a later turn proceeds until a newer card covers it.

### RT-12
At the end of the last final turn, scoring waits 2 seconds so match attempts against the last
discard can land before cards are revealed (RULE-24).

### RT-13
Every match that settles, right or wrong, is broadcast with the matcher, the targeted slot, and
the outcome, so all players see who got there first. Matches rejected as `too_late` are only
reported to their sender.

## Timers

### RT-14
Ready (RULE-7), turn grace (ROOM-14), and scoring delay (RT-12) deadlines are stored on the
game row. Any machine handling the room can fire them; firing re-checks the deadline under the
row lock so a timer applies once. One machine sweeps at a time: it takes a postgres advisory
lock once, on a connection of its own, and keeps it while that connection lives; the others
retry every 5 seconds. Each second the sweep reads every unfinished game with its present
players in one query, without locks, runs each game's tick on a copy, and locks and ticks for
real only the games whose copy changed.

## Turn token

### RT-15
Each game has a `turn_seq`, bumped whenever the turn's status, player, or stage changes
(including by timers and forfeits). Every turn action (draw, take, swap, discard, komino, peek,
blind swap, look and swap, skip) must carry the `turn_seq` from the view it was chosen in. A
missing, spent, or outdated token is rejected as `stale` (http 409), so a repeated or outdated
confirm never applies. Matches carry the discard sequence number instead (RT-9) and do not move
`turn_seq`; ready and start carry no token.

## Lag compensation

A match is won by the fastest reaction to the discard, not the shortest network path to the
server.

### RT-16
A match is checked when it arrives (RT-9, RULE-19) and stored on the game as a pending claim;
it changes nothing yet and nobody else sees it. A player holds at most one pending claim. The
sender's `result` (RT-4) comes once the claim settles: ok when the match applied, right or
wrong, or `too_late` when another claim got the discard first. Over http the request waits
for the same result.

### RT-17
The first claim against a discard opens a 250ms window; claims arriving within it join it.
When it closes, the claims settle in order of reaction time (RT-18), ties going to the
earliest arrival, each checked against the table as the ones before it left it. A wrong match
takes its penalty and leaves the discard open to the next claim; after a correct one the rest
are `too_late`.

### RT-18
A member socket pings every 2 seconds with websocket ping frames, which browsers answer on
their own, and keeps its last 10 round trips. It notes when it first sent a view carrying each
`discard_seq`. A match over the socket may report `reaction_ms`, the time the page measured
from showing the discard to sending the match (UI-32). With `elapsed` the time from sending
the discard to receiving the match, the reaction used is the report bounded to between
`elapsed` less the slowest recent round trip and `elapsed`; without a report it is `elapsed`
less the fastest. Either credit is capped at 200ms, so a client that delays its pongs or
underreports gains at most that. A match over http, or for a discard the socket never sent,
is timed from when the discard landed.

### RT-19
Pending claims settle before a take, swap, discard, blind swap, look and swap, or komino call
is applied, before a turn is skipped by its limit or by absence, and before scoring, since
each can change the discard or a targeted card. A draw, peek, or other match leaves them
waiting. A forfeiting player's claims are rejected as `not_seated`; claims pending when a game
ends by forfeit are rejected as `too_late`.

### RT-20
Every claimer waits for its window's deadline, then runs the room's timers (RT-14), so the
first to lock the game settles all of the window's claims. The timer pass also settles a
window whose waiters are gone. Settled results are kept on the game for the waiters, newest
32.
