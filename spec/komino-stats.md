# Komino Stats

Per-room, per-player stats accumulated across games.

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
