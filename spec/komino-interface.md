# Komino Interface

Card faces, special-move symbols, confirmation flow, and the komino button.

## Card faces

### UI-1
Card faces are drawn as inline svg, not image files. Every face shows its value in two
opposite corners and large in the center.

### UI-2
Plain cards (-1 through 6) have a neutral face. Value tints distinguish low from high: -1 and 0
on a green tint, 1-6 on white.

### UI-3
Special cards replace the center value with a symbol made of simple shapes and a color unique
to the move, keeping the corner values:

| Move | Values | Color | Symbol |
|------|--------|-------|--------|
| peek own | 7, 8 | blue | an eye over a single card outline |
| peek other | 9, 10 | orange | an eye with an arrow pointing away |
| blind swap | 11, 12 | purple | two crossing curved arrows between two card outlines, both outlines filled solid |
| look and swap | 13 | red | an eye above two crossing curved arrows |

### UI-4
Symbol and color both identify the move, so neither alone is required to tell moves apart.
Every special face has an accessible label naming the move (e.g. `9, peek other`).

### UI-5
Card backs are a single shared design. Locked slots (RULE-23) show a lock badge.

### UI-20
Both corner values sit fully inside the card at every rendered size. The bottom corner is the
top corner rotated 180 degrees about the card's center.

## Table layout

### UI-6
The viewing player's hand is at the bottom; other seated players are arranged around the
table with their names, presence (ROOM-12), and card counts. The draw pile and discard pile
are in the center.

### UI-7
Whose turn it is, the current phase (dealing, peeking, playing, final turns, scoring), and the
remaining grace, turn (SET-10), or ready time are always visible.

### UI-8
A log lists recent public events (draws, discards, swaps, peeks, matches with outcome, calls,
forfeits) with player names.

### UI-9
The layout works on a phone in portrait at 360px wide without horizontal scrolling.

### UI-30
A seated player sees the other players' hands as if sitting across from them: turned 180
degrees, so each owner's near row (SET-5) is farthest away and their slot 1 is top right.
Your own hand, and every hand on the watch page, is seen as its owner sees it: slots 1 and 2
on the bottom row, 3 and 4 above.

### UI-31
Every card in a hand shows its slot number (1 through the hand's size, then penalty slots),
the same for every viewer and the same number the log and confirms use (`bob's card 3`). It
sits in the center of a card back or an empty slot, and in a tab above a face-up card.

## Confirmation

### UI-10
Every card action is two steps: select, then confirm. Selecting highlights the card(s)
involved and opens a confirm dialog describing the action (e.g. `swap drawn 4 into slot 2?`)
with confirm and cancel buttons. Nothing is sent to the server until confirm. The confirm
dialog is the "confirm bar" referred to below.

### UI-11
UI-10 applies to: drawing, taking the discard, swapping, discarding, using each special move
(and skipping one), matching (unless fast match is on, UI-40), giving a card, and pressing
ready. Matching another player's
card confirms at once, saying a right match is followed by giving a card. Once a right match
leaves a card owed (RULE-19), the matcher's taps on their own cards pick the card to give,
each confirmed; taps on other hands do nothing until it is given. The prompt (UI-33) says so
with the seconds left, and the emptied slot shows a dashed outline and reads `waiting for
<name>'s card` for everyone.

### UI-22
While a discarded special move waits (RULE-10), the turn player's taps on cards are matches
(UI-13), and the controls offer `use <move>`, in the move's color, and `end turn`. Once the
move is in use, taps pick its targets, and a `match <value>` button switches taps back to
matching. So the move is not mistaken for already running, the prompt (UI-33) says it is
earned but not started and that a tap is a match attempt, the discard confirm says the move
does not start on its own, and a match confirm in this stage says it is a match attempt, not
the move.

### UI-12
Confirmation is client-side only. The server treats each received action as final.

### UI-13
A match is selected by tapping any face-down card while a matchable discard is up. The confirm
bar for a match is shown immediately and stays reachable with one more tap, so a match takes
two taps total.

### UI-14
If the action becomes invalid while its confirm bar is open (e.g. the discard was already
matched, or the turn changed), the bar closes and shows why.

### UI-32
A sent match carries how long the page had shown its discard, from the first view carrying
that `discard_seq` to the confirm, on the page's monotonic clock (RT-18). The targeted card
shows a pulsing dashed outline, and its label ends in `matching`, until the match's result
arrives or the discard changes (RT-16).

### UI-19
The confirm dialog is centered on the screen over a dimmed backdrop, with full-width confirm
and cancel buttons at least 48px tall. Confirm takes focus when the dialog opens, so Enter
confirms; Escape or tapping the backdrop cancels.

### UI-33
The action buttons sit in an action bar under the table, led by a prompt line saying what to
do next (the step of your turn, or a card you owe). The bar sticks to the bottom of the screen
while the table runs past it, and is hidden when there is nothing to show (as on the watch
page).

### UI-40
A `fast match` toggle in the room bar, kept in browser storage and off by default, makes a tap
on a face down card send its match at once, with no confirm (an exception to UI-10). Giving a
card and every turn action still confirm. Storage that is missing or throws leaves it off.

### UI-39
Keyboard shortcuts press the control whose label they name: `d` draw, `t` take, `x` discard,
`u` use the move, `e` end turn, skip move, or keep my cards, `m` match or cancel match, `h`
hide card, `r` ready, `k` KOMINO. `1`-`9` tap your own card of that number (`0` is 10), and
`?` opens the guide. Keys do nothing while typing in a field, with a modifier held, or while a
dialog is open, where Enter confirms and Escape cancels.

## Table look

### UI-36
Each seat has a color, in seat order. It underlines the seat's name over its hand, marks its
lines in the log and its captions, outlines the cards its events send flying, and shows as a
chip next to seated members. On screens 900px and wider, the other hands sit around the table
clockwise from your left: a third on the left, a third on the right, the rest across. Narrower
screens stack them above the piles in the same order.

### UI-41
While the top discard can be matched, it glows and the glow fades over 6 seconds. A re-render
continues the glow where it was; a new discard starts it over.

### UI-43
The log and captions give each match's reaction time as the server timed it (`bob matched your
4 in 180ms`). A match that lost the race to a faster one says by how much (`too late: a match
120ms faster got there first`).

## Alerts and help

### UI-35
When your turn starts, a right match leaves you a card to give, or a new game starts, a phone
vibrates. If the page is hidden, the title reads `(!) your turn - komino` (or the reason) and
the icon shows a red dot until the page is shown again. An `alerts` toggle in the room bar,
kept in browser storage and off by default, also shows a browser notification then; turning
it on asks for permission once. A notification the browser refuses is ignored.

### UI-37
On a player's first games, a tip above the prompt explains the step they are at: the opening
peek, drawing, a drawn card, matching, an earned move, giving a card, and calling KOMINO. Each
shows until `got it`, once per browser; `no more tips` turns them all off. Observers see none.

### UI-38
When a game is scored, a summary under the table lists each player by score: score, running
total (with a target, or after two games), matches, misses, penalty cards, and special moves.
A finished match names its winners; a room playing to a target says the match goes on. With
two or more games in the match, a chart draws each player's running total by game, with the
target as a dashed line.

### UI-42
A visually hidden polite live region reads each new event aloud, your own included, and `your
turn` when your turn starts.

## Guide

### UI-34
A `rules` button in the header opens a modal guide to the game: goal, setup, turns, each
special card with its face, matching (including giving a card), komino, scoring, matches over
several games, memory marks, and the keyboard shortcuts. In a room it uses the room's settings
(hand size and near row, turn limit, away grace, peek time, whether misses are shown, target,
caller penalty, exact reset, and marks); in the lobby it names them as room choices. Close, Escape, or
tapping the backdrop closes it, and Escape closes it before any open confirm.

## Komino button

### UI-15
A dedicated komino button is always visible on the table. It is enabled only when calling is
allowed (RULE-22) and disabled with a tooltip explaining why otherwise.

### UI-16
Pressing it opens a confirmation (UI-10). On confirm, all players see a prominent komino
banner naming the caller, and the final-turn countdown of remaining players. A call made mid
turn (RULE-22) shows only the caller a banner saying it takes effect when the turn ends, and
the button reads `KOMINO called`, disabled, until then.

## Peeks and reveals

### UI-17
A peeked card (RULE-12, RULE-13, RULE-15) stays face up for the peeking player until they
dismiss it with a `hide card` button (SET-12) or the room's peek time passes (SET-11), then
flips back. Other players and observers see the slot highlighted until the peek ends; the view
carries the peeked slots and deadlines without their values. Deadlines are read against the
server clock: each view carries `server_now`, and the client corrects for its own clock's
offset. The opening peek (RULE-7) is not a reveal: it lasts until `ready`. Peeked, drawn,
and opening values are fetched sealed (SEAL-6) and dropped once hidden (SEAL-9).

### UI-18
At scoring, all hands flip face up, each player's total is shown, and the winner(s) are
highlighted. The host sees a `next game` button.

## Action effects

### UI-23
Every view's events carry an `id` that only grows. A client plays effects only for events
newer than the last id it has seen in the same game, at most the six newest. The first view
after a page load plays nothing, and a repeated view replays nothing. The first view of a new
game plays its events. Effects are cosmetic: the table always renders from the view alone.

### UI-24
Each new event marks the cards and piles it touched for a short animation, for every player
and observer:

| Event | Effect |
|-------|--------|
| draw | the deck pulses and a face-down card flies to the player's hand |
| take | the discard pulses and its card flies to the player's hand |
| swap | the slot flips in and the replaced card flies face up to the discard |
| discard | the card flies face up from the player's hand to the discard |
| peek | the peeked card lifts and tilts, as if being looked at |
| blind swap, look and swap | both cards pulse and fly past each other between the two slots |
| match | a hit flies the card to the discard (and the given card to the emptied slot); a miss shakes the card, shows its face for 3 seconds when the room shows misses (SET-13), and a penalty card flies from the deck |
| give | the given card flies face down into the emptied slot, which flips in |
| komino | the status banner flashes |
| scored | every card flips face up |

Marks survive the once-a-second re-render without restarting. With `prefers-reduced-motion`,
cards are outlined instead of moved and no cards fly.

### UI-25
While another player peeks at a card, the slot shows an eye badge for everyone but the
peeker, alongside the peek highlight (UI-17).

### UI-26
Another player's action is also shown as a short caption over the table, worded as in the
log (UI-8). A caption about one of your own cards is emphasized. Your own actions get the
animations but no caption.

### UI-27
Each effect has a sound cue synthesized with Web Audio (no audio files): a card snap for
flips and discards, a slide for draws, takes and swaps, a soft blip for peeks, a rising pair
for a match hit, a low buzz for a miss, a fanfare for komino, a shuffle for a new game, and
a chime when your turn starts. Browsers only allow audio after a gesture, so the audio
context is made on the first tap or key press.

### UI-28
A `sound on` / `sound off` button in the room bar mutes the cues. The choice is kept in
browser storage and defaults to on; storage that is missing or throws leaves sound on.

### UI-29
The page re-renders on every view and once a second for countdowns. A render rewrites the
table, controls, players, log, and stats only when their markup changed, so a button stays
the same element across renders and a tap that spans one still registers. Handlers read the
current view when pressed, not the view they were built from.

## Testing

### UI-21
The client script exports a factory over an injected window (`document`, `location`, `fetch`,
`WebSocket`, timers) and only starts itself in a browser. Node tests in `crates/komino/web`
drive it under jsdom with a fake socket and fetch, covering rendering, every confirm flow,
socket messages, reconnects, and the watch page. Run them with `make test-js`.
