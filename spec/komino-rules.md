# Komino Rules

Deck, turn structure, special moves, matching, calling komino, and scoring.

Each game is a single round. Rooms play repeated games (see `komino-stats.md`).

## Deck

### RULE-1
A game starts when the host presses start with at least 2 present members. Up to 8 present
members are seated in join order; the rest spectate (ROOM-10).

### RULE-2
The deck has 60 cards with numeric values only, no suits or face cards:

| Value | Copies | Special move |
|-------|--------|--------------|
| -1 | 4 | none |
| 0 | 4 | none |
| 1-6 | 4 each | none |
| 7, 8 | 4 each | peek own (RULE-12) |
| 9, 10 | 4 each | peek other (RULE-13) |
| 11, 12 | 4 each | blind swap (RULE-14) |
| 13 | 4 | look and swap (RULE-15) |

### RULE-3
The server shuffles with the os rng. The client never receives the order of the deck or the
value of any card the viewing player is not entitled to see. Values a player is entitled to
see privately are only sent sealed (SEAL-1).

### RULE-4
When the draw pile is empty, every discard but the top card is shuffled into a new draw pile.

### RULE-28
Every discard is face up, so the view lists the newest 10 cards of the discard pile, top first,
as `discard_recent`. The table shows the ones under the top card next to the piles.

## Setup

### RULE-5
Each seated player is dealt 4 cards face down into slots 1-4, shown as a 2x2 grid. Slots 1
and 2 are the row nearest the player, with 3 and 4 behind them. A room can deal 4-10 cards (SET-5) and play with two
decks (SET-7).

### RULE-6
After the deal, one card is flipped to start the discard pile. It cannot be matched
(matching opens with the first discard of play).

### RULE-7
Each player sees the values of their slots 1 and 2 (the nearest row, SET-6) until they press
`ready`. Play starts when
every seated player is ready, or 30 seconds after the deal, whichever comes first.

### RULE-8
The first turn goes to the player after the previous game's winner, or a random player in a
room's first game. Turns proceed in seat order.

## Turns

### RULE-9
On their turn a player either calls komino (RULE-22) or takes one of these actions:
1. Draw the top card of the draw pile. The drawn value is shown to the drawer only.
2. Take the top card of the discard pile. Its value is public.

### RULE-10
After drawing from the draw pile, the player either:
- swaps it into one of their slots, discarding the replaced card face up, or
- discards it face up, then may use its special move if it has one.

Discarding a card with a special move does not start the move. The turn stays with the player
and the discard is open to matches by anyone (RULE-18), the player included, so they can match
before the move changes any hand. The player then either uses the move (`use_special`,
followed by the move itself) or ends the turn without it (`skip`). Matches made while the move
waits do not use it up.

### RULE-11
After taking the discard, the player must swap it into one of their slots, discarding the
replaced card face up. A card taken from the discard pile never triggers a special move.

### RULE-12
Peek own: the player looks at one of their own cards.

### RULE-13
Peek other: the player looks at one card in another player's hand.

### RULE-14
Blind swap: the player exchanges one of their cards with one of another player's cards,
without seeing either.

### RULE-15
Look and swap: the player looks at any one card in any player's hand, then may exchange it
with one of their own cards or decline the exchange.

### RULE-16
A special move is optional; the player can skip it. Peeked values are shown only to the
peeking player. All players see which slots were peeked or swapped.

### RULE-17
Special moves cannot target the slots of a player who called komino (RULE-23).

## Matching

### RULE-18
Whenever a discard is face up on top of the pile, any seated player (including the one whose
turn it is) can match it by selecting any face-down card in any player's hand, their own
included, whose value they believe equals the top discard.

### RULE-19
A correct match moves the selected card onto the discard pile. If the card came from another
player's hand, the matcher then gives one of their own cards (chosen without looking) into the
vacated slot of that player. A player with no cards left cannot match another player's card.

The card to give is chosen after the match lands (`give`), not with it: the slot stays empty
and the view's `owed` names it, the matcher's seat, and a deadline 15 seconds out. At the
deadline, or when the game is scored, the matcher's highest card is given for them (the
lowest numbered slot among equal values); a matcher with no cards left gives nothing. While a card is owed, its matcher cannot match
another player's card. A forfeit by either player drops the debt. A match may still name
`give_slot` up front, which gives that card with the match.

### RULE-20
An incorrect match leaves the selected card where it was and adds the top card of the draw pile
face down to the matcher's hand as a penalty, in a new slot they have not seen. A room can
show everyone the selected card's value (SET-13).

### RULE-21
Only one match succeeds per discard, decided by reaction time (RT-17). Once a card is matched
onto the pile, later attempts against that same discard are rejected as `too late` with no
penalty. The matched card itself
becomes the new top discard and can be matched in turn.

## Calling komino

### RULE-22
A player can call komino at any point in their own turn, once every seated player has had at
least one turn (the caller's current turn counts once they have drawn or taken). Calling at the
start of the turn, before drawing, ends the turn at once. Calling later in the turn takes
effect when the turn ends, however it ends (including a skip, SET-9 and ROOM-14), so the
player finishes their move first. Until then only the caller sees the call (the view's
`calling`), and it cannot be taken back. A call pending when its player forfeits is dropped.

### RULE-23
After a call, every other seated player gets exactly one more turn. The caller's slots are
locked: they cannot be swapped, peeked by others, or matched by others. The caller can still
match other players' cards.

### RULE-24
When the last final turn ends and the scoring delay passes (RT-12), all cards are
revealed and the game is scored.

### RULE-25
A player's score is the sum of their card values. A player with no cards scores 0.

### RULE-26
The lowest score wins. If the caller does not have the strictly lowest score, they cannot
win, and the lowest score among the other players wins. Any other tie is a shared win. A room
can add points to a caller who does not win (SET-15) and play a match over several games
(SET-14).

### RULE-27
A forfeited player (ROOM-15, ROOM-18) has their cards removed from play, is skipped for the
rest of the game, and cannot win. If fewer than 2 non-forfeited players remain, the game ends
and the remaining player wins.
