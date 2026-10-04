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

## Confirmation

### UI-10
Every card action is two steps: select, then confirm. Selecting highlights the card(s)
involved and opens a confirm dialog describing the action (e.g. `swap drawn 4 into slot 2?`)
with confirm and cancel buttons. Nothing is sent to the server until confirm. The confirm
dialog is the "confirm bar" referred to below.

### UI-11
UI-10 applies to: drawing, taking the discard, swapping, discarding, using each special move
(and skipping one), matching, and pressing ready. Matching another player's card selects the
target, then the card to give (RULE-19), then confirms both as one action.

### UI-22
While a discarded special move waits (RULE-10), the turn player's taps on cards are matches
(UI-13), and the controls offer `use <move>` and `end turn`. Once the move is in use, taps
pick its targets, and a `match <value>` button switches taps back to matching.

### UI-12
Confirmation is client-side only. The server treats each received action as final.

### UI-13
A match is selected by tapping any face-down card while a matchable discard is up. The confirm
bar for a match is shown immediately and stays reachable with one more tap, so a match takes
two taps total.

### UI-14
If the action becomes invalid while its confirm bar is open (e.g. the discard was already
matched, or the turn changed), the bar closes and shows why.

### UI-19
The confirm dialog is centered on the screen over a dimmed backdrop, with full-width confirm
and cancel buttons at least 48px tall. Confirm takes focus when the dialog opens, so Enter
confirms; Escape or tapping the backdrop cancels.

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

## Testing

### UI-21
The client script exports a factory over an injected window (`document`, `location`, `fetch`,
`WebSocket`, timers) and only starts itself in a browser. Node tests in `crates/komino/web`
drive it under jsdom with a fake socket and fetch, covering rendering, every confirm flow,
socket messages, reconnects, and the watch page. Run them with `make test-js`.
