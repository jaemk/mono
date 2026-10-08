# Komino Bots

Computer players a room's host can add to fill seats.

### BOT-1
`POST /komino/api/rooms/{code}/bots` (host only, else `forbidden`) adds a bot member named
`bot <name>` (`bot ava`, `bot ben`, ..., numbered once the names run out). A room holds at most
7 bots (`invalid` past that). A bot is a `players` row with `bot` set, and its membership is
always present, so the next game seats it like any present member (RULE-1). Members in the view
carry `bot`, the players list tags bots, and the host has an `add bot` button. A bot's player
row is deleted once no room holds it.

### BOT-2
The host removes a bot like any member (ROOM-15): it forfeits and leaves, and is not banned.
A bot never becomes host: a leaving host hands the room to a person, or to the next person to
join.

### BOT-3
A bot plays only on what its seat may see: its opening near row (RULE-7), cards it swaps in
from a draw, cards it peeks at, and public events (taken discards, shown misses, swaps between
hands, gives, and matches). Its memory is kept in the game state, never in a view, and follows
cards as events move them. When its turn ends it forgets cards it knows at its level's rates
(BOT-5).

### BOT-4
Bots act from the timer sweep (RT-14), once a second. Each sweep a bot readies in the peek
phase, gives its worst card for a match it owes, and matches the top discard when it knows a
card of that value, no sooner than its level's reaction time (BOT-5) after the discard landed,
timed from when it landed (RT-18). Matching its own card needs a discard above 0; matching another hand gives its
worst card with the match. On its turn a bot takes one step per 1.2 seconds: it calls komino
when its known total (unseen cards counted as 6) is 4 or less, or 7 or less with every card
seen; takes a discard of 2 or less that beats its worst card by 4; otherwise draws, swapping
the draw for its worst card when it is lower by more than 1 (by 4 or more for a special card),
else discarding it. It uses an earned move when it has a use for it: peeking at its unseen
cards, then at unseen cards in other hands; blind swapping a known card of 7 or more for a
known lower or unseen card; and after a look, swapping its worst card for the seen one when
that is lower by more than 1.

### BOT-5
Each bot has a level, `easy`, `normal` (the default), or `hard`, chosen when it is added
(`{"level": ...}` in the body; anything else is `invalid`) and kept on its `players` row as
`bot_level`. Members in the view carry it, and the players list tags the bot with it. The
level sets:

| | easy | normal | hard |
|---|---|---|---|
| forgets each card in another hand, per turn | 0.35 | 0.15 | 0.05 |
| forgets each of its own cards, per turn | 0.10 | 0.03 | 0 |
| reaction before matching | 3s | 2s | 2s |
| remembers a card it learns one off | 0.15 | 0.03 | 0 |
| error in its own total when deciding to call | up to 3 | up to 1 | none |
| swaps into a random slot instead of its worst | 0.20 | 0.05 | 0 |

A misremembered card can lead an easy or normal bot into a wrong match and its penalty, as a
person's would. Bots saved without a level play at normal.
