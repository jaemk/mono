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
value of any card the viewing player is not entitled to see.

### RULE-4
When the draw pile is empty, every discard but the top card is shuffled into a new draw pile.

## Setup

### RULE-5
Each seated player is dealt 4 cards face down into slots 1-4, shown as a 2x2 grid. Slots 3
and 4 are the row nearest the player.

### RULE-6
After the deal, one card is flipped to start the discard pile. It cannot be matched
(matching opens with the first discard of play).

### RULE-7
Each player sees the values of their slots 3 and 4 until they press `ready`. Play starts when
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

### RULE-20
An incorrect match leaves the selected card where it was and adds the top card of the draw pile
face down to the matcher's hand as a penalty, in a new slot they have not seen.

### RULE-21
Only one match succeeds per discard. Once a card is matched onto the pile, later attempts
against that same discard are rejected as `too late` with no penalty. The matched card itself
becomes the new top discard and can be matched in turn.

## Calling komino

### RULE-22
A player can call komino only at the start of their own turn, before drawing, and only after
every seated player has had at least one turn. Calling ends their turn.

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
win, and the lowest score among the other players wins. Any other tie is a shared win.

### RULE-27
A forfeited player (ROOM-15, ROOM-18) has their cards removed from play, is skipped for the
rest of the game, and cannot win. If fewer than 2 non-forfeited players remain, the game ends
and the remaining player wins.
