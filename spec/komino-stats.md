# Komino Stats

Per-room, per-player stats accumulated across games, and each player's lifetime totals.

### STAT-1
Stats are kept per (room, player) and accumulate across every game played in that room. They
are not aggregated across rooms.

### STAT-2
Tracked counters:

| Stat | Increments when |
|------|-----------------|
| games played | a game the player was seated in reaches scoring or ends by forfeit |
| wins | the player is a winner (RULE-26, RULE-27); shared wins count for each winner |
| komino calls | the player calls komino |
| komino wins | the player calls komino and wins |
| matches | the player makes a correct match |
| failed matches | the player makes an incorrect match |
| special moves | the player uses a special move (skipped moves do not count) |
| cards interacted | see STAT-3 |
| forfeits | the player forfeits (RULE-27) |
| draws | the player draws a card from the deck (not a take, not a penalty card) |
| swaps | the player swaps in a drawn or taken card, blind swaps, or swaps after a look |
| views | the player looks at a card with a special move (peek own, peek other, or the look in look and swap); the opening peek does not count |
| others matched | the player makes a correct match on a card in another player's hand |
| match ms, timed matches | a correct match adds its reaction time (STAT-9) and one timed match |
| bot games | a game the player was seated in, with a bot also seated, reaches scoring or ends by forfeit |

The room page table (STAT-5) shows the first nine; the rest are kept for lifetime stats.

### STAT-3
`cards interacted` counts each card a player handles: +1 per draw, take, swap (each card
moved), discard, peek, card given after a match, and match attempt. A blind swap or look and
swap counts both cards.

### STAT-4
Counters update in the same transaction as the action that causes them (RT-2).

### STAT-5
The room page shows a stats table of every member who has played in the room, sorted by wins,
with a column per counter and the player's current name.

### STAT-6
Stats of members who left or were removed stay in the table, marked as such.

## Lifetime stats

### STAT-7
Every player who is not a bot also has lifetime totals of every STAT-2 counter across all
rooms, in `player_stats`, one fixed-size row per player. They are added in the same statement
that adds the room's (STAT-4), so they cost no extra round trip, and they are never read back
from game events. They are not keyed by room, so they outlive room deletion (ROOM-11). An
anonymous player's totals carry over when it becomes an account (ACCT-2), and are deleted with
the account (ACCT-16). Totals that rooms kept before lifetime stats existed were added once,
by summing each player's room rows; the counters added with them started at zero.

### STAT-8
A match's reaction time is the one its claim was settled with (RT-17, RT-18), capped at 60
seconds for stats. Missed matches carry no time.

### STAT-9
The average match time is the summed reaction time over the timed matches. For the median,
`player_match_times` counts each player's correct matches in 50ms bands of reaction time
(`band = ms / 50`), with every time from 5 seconds up in the last band, so a player has at most
101 rows however much they play. The median is the middle of the band holding the middle
match (the lower middle of an even count), within 25ms of the exact value; in the last band it
reads as `5s+`.

### STAT-10
`player_partners` counts, for each pair of players, the games they were seated in together,
one row each way, added once per game when it scores. It grows with distinct partners, not
with games. Bots are left out; a player's games with bots are counted instead (STAT-2).
Deleting a player removes their rows on both sides.

### STAT-11
`GET /komino/api/account/stats` returns the requesting player's totals, `match_time` with
`timed`, `average_ms`, and `median_ms` (null before any timed match), and the 20 partners they
played the most games with, most first, by their current default name and username. The
account page (ACCT-17), for an anonymous player too, shows games played, games won with the win rate, cards drawn, swapped,
and viewed, matches, matches from others, missed matches, average and median match time in
seconds, games with bots, and the partner list.
