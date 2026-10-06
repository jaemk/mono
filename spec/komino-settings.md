# Komino Settings

Per-room game settings chosen when the room is created: hand size, turn timers, and how long
peeks stay up.

## Choosing settings

### SET-1
The lobby's `create a room` form has four settings, each preset to its default:

| Setting | Field | Allowed | Default |
|---------|-------|---------|---------|
| cards in hand | `hand_size` | 4-10 | 4 |
| away grace | `away_grace_secs` | 10-600 seconds | 30 |
| turn limit | `turn_limit_secs` | off, or 10-600 seconds | off |
| peek time | `reveal_secs` | until hidden, or 1-60 seconds | until hidden |

The form offers a select for each (away grace 15, 30, 60, 120 seconds; turn limit off, 30,
60, 90, 120, 300 seconds; peek time until hidden, 3, 5, 10, 30 seconds). The api accepts any
value in the allowed range.

### SET-2
`POST /komino/api/rooms` takes the settings as an optional json body. A missing body or field
takes the default; `null` means off (turn limit) or until hidden (peek time). A value outside
the allowed range is rejected as `invalid` (http 400) and no room is created.

### SET-3
Settings are fixed for the life of the room and stored on the `rooms` row. Each game copies
them into its state when it starts, so a game always plays by the settings it was dealt
with. Games saved before settings existed play by the earlier fixed rules (4 cards, 30 second
grace, no turn limit, 5 second peeks).

### SET-4
The room view carries the settings, and the room page shows them in one line under the room
code (e.g. `6 cards, 60s turns, 30s away grace, peeks until hidden`).

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
