# Komino Settings

Per-room game settings chosen when the room is created: hand size, turn timers, and how long
peeks stay up.

## Choosing settings

### SET-1
The lobby's `create a room` form has five settings, each preset to its default:

| Setting | Field | Allowed | Default |
|---------|-------|---------|---------|
| cards in hand | `hand_size` | 4-10 | 4 |
| away grace | `away_grace_secs` | 10-600 seconds | 30 |
| turn limit | `turn_limit_secs` | off, or 10-600 seconds | off |
| peek time | `reveal_secs` | until hidden, or 1-60 seconds | 15 |
| missed matches | `show_misses` | true (shown to all) or false (hidden) | true |
| play to | `target_score` | no target, or 25-500 points | no target |
| losing call penalty | `caller_penalty` | 0-50 points | 0 |
| exact target halves | `exact_reset` | true or false | false |
| memory marks | `memory_marks` | true (allowed) or false | false |

The form offers a select for each (away grace 15, 30, 60, 120 seconds; turn limit off, 30,
60, 90, 120, 300 seconds; peek time until hidden, 3, 5, 10, 15, 30 seconds; missed matches
shown to all or hidden; play to no target, 50, 100, 150, 200; losing call penalty none, 5, 10,
20; exact target halves off or on; memory marks off or allowed). The api accepts any value in
the allowed range.

### SET-2
`POST /komino/api/rooms` takes the settings as an optional json body. A missing body or field
takes the default; `null` means off (turn limit) or until hidden (peek time). A value outside
the allowed range is rejected as `invalid` (http 400) and no room is created.

### SET-3
Settings are fixed for the life of the room and stored on the `rooms` row. Each game copies
them into its state when it starts, so a game always plays by the settings it was dealt
with. Games saved before settings existed play by the earlier fixed rules (4 cards, 30 second
grace, no turn limit, 5 second peeks, misses hidden). Games saved with settings but before
`show_misses` existed show misses.

### SET-4
The room view carries the settings, and the room page shows them in one line under the room
code (e.g. `6 cards, 60s turns, 30s away grace, peeks until hidden, misses shown`), followed
by any match settings that differ from the defaults (`play to 100, +10 for a losing call,
exact target halves, marks allowed`).

## Hand size

### SET-5
Each seated player is dealt `hand_size` cards into slots 1 through `hand_size` (RULE-5). Hands
are laid out in two rows of `ceil(hand_size / 2)` columns. The row nearest the player holds
slots 1 through `floor(hand_size / 2)`, left to right; the row behind it holds the rest.
Penalty cards (RULE-20) take new slots after the last, in rows farther back. Other players'
hands are shown turned toward their owners (UI-30).

### SET-6
The opening peek (RULE-7) shows the row nearest the player: slots 1 through
`floor(hand_size / 2)` (slots 1 and 2 with 4 cards, slots 1-5 with 10).

### SET-7
When one 60 card deck would leave fewer than 20 cards in the draw pile after the deal and the
starter (RULE-6), the game is played with two decks shuffled together: 120 cards, 8 copies of
each value.

## Turn timers

### SET-8
The away grace (ROOM-14) lasts `away_grace_secs`.

### SET-9
With a turn limit set, every turn (RULE-9) must end within `turn_limit_secs` of its start,
whether the player is present or not. When the limit passes, the turn is skipped: a card held
mid turn (drawn or taken) goes face up onto the discard pile, a pending special move is lost,
and the log records `timeout_skip`. The away grace keeps running alongside; whichever ends
first skips the turn.

### SET-10
The view carries the current turn's deadline as `turn_deadline` (null without a limit), and the
status line shows the seconds left in the turn.

## Peek time

### SET-11
With a peek time set, a peek (RULE-12, RULE-13, RULE-15) ends `reveal_secs` after it is made.
Until hidden, a peek has no deadline: it lasts until the peeking player hides it, the peeked
slot changes, or the game is scored. Either way it ends early when the slot changes.

### SET-12
`hide` is an action any seated player can send at any time; it ends every peek that player
holds, so the slot highlight (UI-17) clears for everyone. The `hide card` button sends it, and
is shown whenever the player holds a live peek, including after a reload when its value can no
longer be fetched (SEAL-9). Hiding carries no turn token and adds no log event.

## Missed matches

### SET-13
With `show_misses` on, a wrong match (RULE-20) makes the targeted card's value public: the
match event carries it as `value`, so every player and observer sees it. The card stays face
down in its hand; each page turns it face up for 3 seconds as the miss plays (UI-24), and the
log and caption name the value (`bob missed a match on your card 2, a 9, and took a penalty`).
With it off, the event carries no value and nobody, the matcher included, learns the card.

## Matches over several games

### SET-14
Every scored game adds each player's score to a running total kept on the room. The room
records each scored game in its `history`: the match number `m`, whether it ended the match
(`over`), and each player's `totals` and `scores`. A game starts by carrying each seat's total
in (`carry`), and its scoring sets `total` = carry + score. A player new to a running match
carries the highest total in it, so sitting out early games is no advantage. With
`target_score` set, a game that leaves any playing seat's total at or past the target ends the
match: the lowest totals win it (`match_won`, and `match_winners` on the `scored` event). The
next game starts match `m + 1` with every total at 0. Without a target totals keep running and
no match ends. The room view carries the newest match's history entries as `room.history`.
Rooms and games saved before match play start their first match at the next game.

### SET-15
With `caller_penalty` above 0, a caller who does not win (RULE-26) adds it to their score.

### SET-16
With `exact_reset` on and a target set, a total that lands exactly on the target is halved
(rounding down) instead of ending the match.

### SET-17
With `memory_marks` on, a seated player may press and hold a card, or right click it, to mark
it with the value they believe it has (-1 to 13), or clear the mark. Marks are kept in that
browser only, per room and game, never sent to the server, and show as a small badge on a face
down card. They follow their card through swaps between hands, gives, and matches given for up
front; a swap from the hand or a right match on the card clears its mark. A new game starts
unmarked. The guide (UI-34) explains them.
