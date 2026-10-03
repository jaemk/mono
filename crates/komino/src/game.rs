//! The komino rules engine. Pure state transitions with no database or io,
//! so every rule can be driven from a fixed deck in tests.
//!
//! A `Game` is stored whole as jsonb on the `games` row; field names are part
//! of that stored format.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const HAND_SIZE: usize = 4;
pub const MAX_SEATS: usize = 8;
pub const READY_MS: i64 = 30_000;
pub const GRACE_MS: i64 = 30_000;
pub const SCORE_DELAY_MS: i64 = 2_000;
pub const PEEK_MS: i64 = 5_000;

/// Slots seen during the opening peek: the row nearest the player.
const OPENING_PEEK: [usize; 2] = [2, 3];

/// The 60 card deck, unshuffled.
pub fn deck() -> Vec<i8> {
    let mut cards = Vec::with_capacity(60);
    for value in -1..=13 {
        for _ in 0..4 {
            cards.push(value);
        }
    }
    cards
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Move {
    PeekOwn,
    PeekOther,
    BlindSwap,
    LookSwap,
}

pub fn special(value: i8) -> Option<Move> {
    match value {
        7 | 8 => Some(Move::PeekOwn),
        9 | 10 => Some(Move::PeekOther),
        11 | 12 => Some(Move::BlindSwap),
        13 => Some(Move::LookSwap),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Peeking,
    Playing,
    Final,
    Scoring,
    Scored,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Peeking => "peeking",
            Status::Playing => "playing",
            Status::Final => "final",
            Status::Scoring => "scoring",
            Status::Scored => "scored",
        }
    }

    fn in_play(&self) -> bool {
        matches!(self, Status::Playing | Status::Final)
    }
}

/// Where the turn player is within their turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Stage {
    Start,
    Drawn { card: i8 },
    Taken { card: i8 },
    Special { mv: Move },
    Looked { seat: usize, slot: usize },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Seat {
    pub player: String,
    /// `None` is a slot emptied by a match.
    pub slots: Vec<Option<i8>>,
    pub ready: bool,
    pub forfeited: bool,
    pub turns: u32,
    pub score: Option<i32>,
    pub won: bool,
}

/// A card value shown to one player until `until`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reveal {
    pub player: String,
    pub seat: usize,
    pub slot: usize,
    pub value: i8,
    pub until: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Game {
    pub status: Status,
    pub seats: Vec<Seat>,
    /// Top of each pile is the last element.
    pub deck: Vec<i8>,
    pub discard: Vec<i8>,
    /// Bumped whenever the top discard changes, so a match names the exact
    /// card it targets.
    pub discard_seq: u64,
    pub matchable: bool,
    pub turn: usize,
    pub stage: Stage,
    pub caller: Option<usize>,
    pub final_remaining: Vec<usize>,
    pub reveals: Vec<Reveal>,
    pub ready_deadline: Option<i64>,
    pub away_since: Option<i64>,
    /// Token for turn actions, bumped whenever the turn's status, player, or
    /// stage changes. Defaulted so states saved before it existed still load.
    #[serde(default)]
    pub turn_seq: u64,
    pub score_at: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Ready,
    Draw,
    Take,
    Swap {
        slot: usize,
    },
    Discard,
    Komino,
    Peek {
        seat: usize,
        slot: usize,
    },
    BlindSwap {
        slot: usize,
        seat: usize,
        target_slot: usize,
    },
    LookSwap {
        slot: Option<usize>,
    },
    Skip,
    Match {
        seq: u64,
        seat: usize,
        slot: usize,
        give_slot: Option<usize>,
    },
}

impl Action {
    /// Actions that only the turn player takes, guarded by `turn_seq`.
    pub fn is_turn_action(&self) -> bool {
        !matches!(self, Action::Ready | Action::Match { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reject {
    pub code: &'static str,
    pub message: String,
}

impl Reject {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid", message)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stat {
    GamesPlayed,
    Wins,
    KominoCalls,
    KominoWins,
    Matches,
    FailedMatches,
    SpecialMoves,
    CardsInteracted,
    Forfeits,
}

impl Stat {
    pub const ALL: [Stat; 9] = [
        Stat::GamesPlayed,
        Stat::Wins,
        Stat::KominoCalls,
        Stat::KominoWins,
        Stat::Matches,
        Stat::FailedMatches,
        Stat::SpecialMoves,
        Stat::CardsInteracted,
        Stat::Forfeits,
    ];

    /// Column in `room_stats`.
    pub fn column(&self) -> &'static str {
        match self {
            Stat::GamesPlayed => "games_played",
            Stat::Wins => "wins",
            Stat::KominoCalls => "komino_calls",
            Stat::KominoWins => "komino_wins",
            Stat::Matches => "matches",
            Stat::FailedMatches => "failed_matches",
            Stat::SpecialMoves => "special_moves",
            Stat::CardsInteracted => "cards_interacted",
            Stat::Forfeits => "forfeits",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Event {
    pub player: Option<String>,
    pub kind: &'static str,
    pub payload: Value,
}

/// What an action or tick did, beyond the state change itself.
#[derive(Default, Debug)]
pub struct Outcome {
    pub events: Vec<Event>,
    pub stats: Vec<(String, Stat, i32)>,
    /// True when state changed even if nothing public happened.
    pub changed: bool,
}

impl Outcome {
    fn event(&mut self, player: Option<&str>, kind: &'static str, payload: Value) {
        self.changed = true;
        self.events.push(Event {
            player: player.map(str::to_string),
            kind,
            payload,
        });
    }
    fn stat(&mut self, player: &str, stat: Stat, n: i32) {
        self.stats.push((player.to_string(), stat, n));
    }
}

impl Game {
    /// Deal a new game. `deck` is already shuffled, top last.
    pub fn new(players: Vec<String>, mut deck: Vec<i8>, first: usize, now: i64) -> Self {
        let mut seats: Vec<Seat> = players
            .into_iter()
            .map(|player| Seat {
                player,
                slots: Vec::with_capacity(HAND_SIZE),
                ready: false,
                forfeited: false,
                turns: 0,
                score: None,
                won: false,
            })
            .collect();
        for _ in 0..HAND_SIZE {
            for seat in seats.iter_mut() {
                seat.slots.push(deck.pop());
            }
        }
        let discard = deck.pop().into_iter().collect();
        let turn = first % seats.len().max(1);
        Self {
            status: Status::Peeking,
            seats,
            deck,
            discard,
            discard_seq: 0,
            // the flipped starter is not matchable
            matchable: false,
            turn,
            stage: Stage::Start,
            caller: None,
            final_remaining: vec![],
            reveals: vec![],
            ready_deadline: Some(now + READY_MS),
            away_since: None,
            turn_seq: 0,
            score_at: None,
        }
    }

    pub fn seat_of(&self, player: &str) -> Option<usize> {
        self.seats.iter().position(|s| s.player == player)
    }

    fn active(&self, seat: usize) -> bool {
        self.seats.get(seat).is_some_and(|s| !s.forfeited)
    }

    fn active_count(&self) -> usize {
        self.seats.iter().filter(|s| !s.forfeited).count()
    }

    fn locked(&self, seat: usize) -> bool {
        self.caller == Some(seat)
    }

    fn card_at(&self, seat: usize, slot: usize) -> Option<i8> {
        self.seats
            .get(seat)
            .and_then(|s| s.slots.get(slot))
            .copied()
            .flatten()
    }

    /// Any change to a slot ends reveals of it, so a peek never shows the
    /// card that replaced the one peeked.
    fn touch(&mut self, seat: usize, slot: usize) {
        self.reveals.retain(|r| !(r.seat == seat && r.slot == slot));
    }

    fn draw_card(&mut self, rng: &mut impl rand::Rng) -> Option<i8> {
        if self.deck.is_empty() && self.discard.len() > 1 {
            use rand::seq::SliceRandom;
            let top = self.discard.pop();
            self.deck = std::mem::take(&mut self.discard);
            self.deck.shuffle(rng);
            self.discard.extend(top);
        }
        self.deck.pop()
    }

    fn push_discard(&mut self, card: i8) {
        self.discard.push(card);
        self.discard_seq += 1;
        self.matchable = true;
    }

    fn require_turn(&self, seat: usize) -> Result<(), Reject> {
        if !self.status.in_play() {
            return Err(Reject::invalid("not in play"));
        }
        if self.turn != seat {
            return Err(Reject::new("not_your_turn", "it is not your turn"));
        }
        Ok(())
    }

    fn require_filled(&self, seat: usize, slot: usize) -> Result<i8, Reject> {
        self.card_at(seat, slot)
            .ok_or_else(|| Reject::invalid("there is no card in that slot"))
    }

    /// A slot another player may target with a special move.
    fn require_target(&self, me: usize, seat: usize, slot: usize) -> Result<i8, Reject> {
        if !self.active(seat) {
            return Err(Reject::invalid("that player is not in the game"));
        }
        if seat != me && self.locked(seat) {
            return Err(Reject::invalid("the caller's cards are locked"));
        }
        self.require_filled(seat, slot)
    }

    fn all_have_played(&self) -> bool {
        self.seats.iter().all(|s| s.forfeited || s.turns > 0)
    }

    fn start_play(&mut self, out: &mut Outcome) {
        self.status = Status::Playing;
        self.ready_deadline = None;
        for seat in self.seats.iter_mut() {
            seat.ready = true;
        }
        let first = self.seats[self.turn].player.clone();
        out.event(None, "play", json!({ "first": first }));
    }

    fn next_turn(&mut self) {
        let n = self.seats.len();
        for step in 1..=n {
            let seat = (self.turn + step) % n;
            if self.active(seat) && !self.locked(seat) {
                self.turn = seat;
                return;
            }
        }
    }

    fn end_turn(&mut self, now: i64) {
        self.stage = Stage::Start;
        self.away_since = None;
        if let Some(seat) = self.seats.get_mut(self.turn) {
            seat.turns += 1;
        }
        if self.caller.is_some() {
            let done = self.turn;
            self.final_remaining.retain(|&s| s != done);
            if self.final_remaining.is_empty() {
                self.status = Status::Scoring;
                self.score_at = Some(now + SCORE_DELAY_MS);
                return;
            }
        }
        self.next_turn();
    }

    /// Everything a turn action depends on. `turn_seq` moves whenever this
    /// does.
    fn turn_marker(&self) -> (Status, usize, Stage) {
        (self.status, self.turn, self.stage.clone())
    }

    fn bump_turn_seq(&mut self, before: (Status, usize, Stage)) {
        if self.turn_marker() != before {
            self.turn_seq += 1;
        }
    }

    /// Apply one player's action. A turn action must carry the `turn_seq`
    /// it was chosen against, so a repeated or outdated submission is
    /// rejected as `stale` instead of applied to a turn that moved on.
    /// Matches carry the discard's `seq` instead and never need this.
    pub fn apply(
        &mut self,
        player: &str,
        action: Action,
        turn_seq: Option<u64>,
        now: i64,
        rng: &mut impl rand::Rng,
    ) -> Result<Outcome, Reject> {
        if action.is_turn_action()
            && self.status.in_play()
            && self.seat_of(player) == Some(self.turn)
            && turn_seq != Some(self.turn_seq)
        {
            return Err(Reject::new(
                "stale",
                "the turn moved on since you chose that; check the table and try again",
            ));
        }
        let before = self.turn_marker();
        let out = self.apply_inner(player, action, now, rng)?;
        self.bump_turn_seq(before);
        Ok(out)
    }

    fn apply_inner(
        &mut self,
        player: &str,
        action: Action,
        now: i64,
        rng: &mut impl rand::Rng,
    ) -> Result<Outcome, Reject> {
        let me = self
            .seat_of(player)
            .filter(|&s| self.active(s))
            .ok_or_else(|| Reject::new("not_seated", "you are not playing in this game"))?;
        self.reveals.retain(|r| r.until > now);
        let mut out = Outcome::default();
        match action {
            Action::Ready => {
                if self.status != Status::Peeking {
                    return Err(Reject::invalid("not in the peek phase"));
                }
                self.seats[me].ready = true;
                out.event(Some(player), "ready", json!({}));
                if self.seats.iter().all(|s| s.ready || s.forfeited) {
                    self.start_play(&mut out);
                }
            }
            Action::Draw => {
                self.require_turn(me)?;
                if self.stage != Stage::Start {
                    return Err(Reject::invalid("you already drew"));
                }
                let card = self
                    .draw_card(rng)
                    .ok_or_else(|| Reject::invalid("the draw pile is empty"))?;
                self.stage = Stage::Drawn { card };
                out.stat(player, Stat::CardsInteracted, 1);
                out.event(Some(player), "draw", json!({}));
            }
            Action::Take => {
                self.require_turn(me)?;
                if self.stage != Stage::Start {
                    return Err(Reject::invalid("you already drew"));
                }
                let card = self
                    .discard
                    .pop()
                    .ok_or_else(|| Reject::invalid("the discard pile is empty"))?;
                // the card beneath was already passed over; matches aimed at
                // the taken card are now too late
                self.discard_seq += 1;
                self.matchable = false;
                self.stage = Stage::Taken { card };
                out.stat(player, Stat::CardsInteracted, 1);
                out.event(Some(player), "take", json!({ "value": card }));
            }
            Action::Swap { slot } => {
                self.require_turn(me)?;
                let card = match self.stage {
                    Stage::Drawn { card } | Stage::Taken { card } => card,
                    _ => return Err(Reject::invalid("nothing to swap in")),
                };
                let old = self.require_filled(me, slot)?;
                self.seats[me].slots[slot] = Some(card);
                self.touch(me, slot);
                self.push_discard(old);
                out.stat(player, Stat::CardsInteracted, 2);
                out.event(
                    Some(player),
                    "swap",
                    json!({ "seat": me, "slot": slot, "discarded": old }),
                );
                self.end_turn(now);
            }
            Action::Discard => {
                self.require_turn(me)?;
                let Stage::Drawn { card } = self.stage else {
                    return Err(Reject::invalid(
                        "only a card drawn from the deck can be discarded",
                    ));
                };
                self.push_discard(card);
                out.stat(player, Stat::CardsInteracted, 1);
                out.event(Some(player), "discard", json!({ "value": card }));
                match special(card) {
                    Some(mv) => self.stage = Stage::Special { mv },
                    None => self.end_turn(now),
                }
            }
            Action::Komino => {
                self.require_turn(me)?;
                if self.status != Status::Playing || self.stage != Stage::Start {
                    return Err(Reject::invalid(
                        "komino can only be called at the start of a turn",
                    ));
                }
                if !self.all_have_played() {
                    return Err(Reject::invalid("everyone must take a turn before komino"));
                }
                self.caller = Some(me);
                self.status = Status::Final;
                let n = self.seats.len();
                self.final_remaining = (1..n)
                    .map(|step| (me + step) % n)
                    .filter(|&s| self.active(s))
                    .collect();
                out.stat(player, Stat::KominoCalls, 1);
                out.event(Some(player), "komino", json!({ "seat": me }));
                self.end_turn(now);
            }
            Action::Peek { seat, slot } => {
                self.require_turn(me)?;
                let value = match self.stage {
                    Stage::Special { mv: Move::PeekOwn } if seat == me => {
                        self.require_filled(seat, slot)?
                    }
                    Stage::Special {
                        mv: Move::PeekOther,
                    } if seat != me => self.require_target(me, seat, slot)?,
                    Stage::Special { mv: Move::LookSwap } => self.require_target(me, seat, slot)?,
                    _ => return Err(Reject::invalid("you cannot peek at that card now")),
                };
                self.reveals.push(Reveal {
                    player: player.to_string(),
                    seat,
                    slot,
                    value,
                    until: now + PEEK_MS,
                });
                out.stat(player, Stat::SpecialMoves, 1);
                out.stat(player, Stat::CardsInteracted, 1);
                out.event(Some(player), "peek", json!({ "seat": seat, "slot": slot }));
                if matches!(self.stage, Stage::Special { mv: Move::LookSwap }) {
                    self.stage = Stage::Looked { seat, slot };
                } else {
                    self.end_turn(now);
                }
            }
            Action::BlindSwap {
                slot,
                seat,
                target_slot,
            } => {
                self.require_turn(me)?;
                if self.stage
                    != (Stage::Special {
                        mv: Move::BlindSwap,
                    })
                    || seat == me
                {
                    return Err(Reject::invalid("you cannot blind swap now"));
                }
                let mine = self.require_filled(me, slot)?;
                let theirs = self.require_target(me, seat, target_slot)?;
                self.exchange((me, slot, mine), (seat, target_slot, theirs));
                out.stat(player, Stat::SpecialMoves, 1);
                out.stat(player, Stat::CardsInteracted, 2);
                out.event(
                    Some(player),
                    "blind_swap",
                    json!({ "seat": me, "slot": slot, "target_seat": seat, "target_slot": target_slot }),
                );
                self.end_turn(now);
            }
            Action::LookSwap { slot } => {
                self.require_turn(me)?;
                let Stage::Looked {
                    seat: tseat,
                    slot: tslot,
                } = self.stage
                else {
                    return Err(Reject::invalid("look at a card first"));
                };
                if let Some(slot) = slot {
                    if tseat == me {
                        return Err(Reject::invalid("you cannot swap a card with yourself"));
                    }
                    let mine = self.require_filled(me, slot)?;
                    let theirs = self.require_target(me, tseat, tslot)?;
                    self.exchange((me, slot, mine), (tseat, tslot, theirs));
                    out.stat(player, Stat::CardsInteracted, 2);
                    out.event(
                        Some(player),
                        "look_swap",
                        json!({ "seat": me, "slot": slot, "target_seat": tseat, "target_slot": tslot }),
                    );
                } else {
                    out.event(Some(player), "skip", json!({}));
                }
                self.end_turn(now);
            }
            Action::Skip => {
                self.require_turn(me)?;
                if !matches!(self.stage, Stage::Special { .. } | Stage::Looked { .. }) {
                    return Err(Reject::invalid("there is no special move to skip"));
                }
                out.event(Some(player), "skip", json!({}));
                self.end_turn(now);
            }
            Action::Match {
                seq,
                seat,
                slot,
                give_slot,
            } => self.do_match(me, player, seq, seat, slot, give_slot, rng, &mut out)?,
        }
        Ok(out)
    }

    fn exchange(&mut self, a: (usize, usize, i8), b: (usize, usize, i8)) {
        self.seats[a.0].slots[a.1] = Some(b.2);
        self.seats[b.0].slots[b.1] = Some(a.2);
        self.touch(a.0, a.1);
        self.touch(b.0, b.1);
    }

    #[allow(clippy::too_many_arguments)]
    fn do_match(
        &mut self,
        me: usize,
        player: &str,
        seq: u64,
        seat: usize,
        slot: usize,
        give_slot: Option<usize>,
        rng: &mut impl rand::Rng,
        out: &mut Outcome,
    ) -> Result<(), Reject> {
        if !matches!(
            self.status,
            Status::Playing | Status::Final | Status::Scoring
        ) {
            return Err(Reject::invalid("matching is not open"));
        }
        if seq != self.discard_seq || !self.matchable {
            return Err(Reject::new(
                "too_late",
                "that discard was already matched or covered",
            ));
        }
        if !self.active(seat) {
            return Err(Reject::invalid("that player is not in the game"));
        }
        if seat != me && self.locked(seat) {
            return Err(Reject::invalid("the caller's cards are locked"));
        }
        let card = self.require_filled(seat, slot)?;
        let give = if seat != me {
            let give =
                give_slot.ok_or_else(|| Reject::invalid("choose one of your cards to give"))?;
            self.require_filled(me, give)?;
            Some(give)
        } else {
            None
        };
        out.stat(player, Stat::CardsInteracted, 1);
        let top = *self.discard.last().expect("matchable implies a discard");
        if card == top {
            self.seats[seat].slots[slot] = None;
            self.touch(seat, slot);
            self.push_discard(card);
            if let Some(give) = give {
                let given = self.seats[me].slots[give].take();
                self.touch(me, give);
                self.seats[seat].slots[slot] = given;
                out.stat(player, Stat::CardsInteracted, 1);
            }
            out.stat(player, Stat::Matches, 1);
            out.event(
                Some(player),
                "match",
                json!({ "ok": true, "seat": seat, "slot": slot, "value": card, "give_slot": give }),
            );
        } else {
            let penalty = self.draw_card(rng);
            if let Some(card) = penalty {
                self.seats[me].slots.push(Some(card));
            }
            out.stat(player, Stat::FailedMatches, 1);
            out.event(
                Some(player),
                "match",
                json!({ "ok": false, "seat": seat, "slot": slot, "penalty": penalty.is_some() }),
            );
        }
        Ok(())
    }

    /// Remove a player from the game. Their cards leave play.
    pub fn forfeit(&mut self, player: &str, now: i64) -> Outcome {
        let before = self.turn_marker();
        let out = self.forfeit_inner(player, now);
        self.bump_turn_seq(before);
        out
    }

    fn forfeit_inner(&mut self, player: &str, now: i64) -> Outcome {
        let mut out = Outcome::default();
        let Some(seat) = self.seat_of(player) else {
            return out;
        };
        if self.seats[seat].forfeited || self.status == Status::Scored {
            return out;
        }
        self.seats[seat].forfeited = true;
        self.seats[seat].slots.clear();
        self.reveals
            .retain(|r| r.seat != seat && r.player != player);
        self.final_remaining.retain(|&s| s != seat);
        out.stat(player, Stat::Forfeits, 1);
        out.event(Some(player), "forfeit", json!({ "seat": seat }));
        if self.active_count() < 2 {
            self.finish(&mut out);
            return out;
        }
        match self.status {
            Status::Peeking => {
                if self.seats.iter().all(|s| s.ready || s.forfeited) {
                    self.start_play(&mut out);
                }
                if self.turn == seat {
                    self.next_turn();
                }
            }
            Status::Playing | Status::Final if self.turn == seat => {
                self.stage = Stage::Start;
                self.away_since = None;
                if self.caller.is_some() && self.final_remaining.is_empty() {
                    self.status = Status::Scoring;
                    self.score_at = Some(now + SCORE_DELAY_MS);
                } else {
                    self.next_turn();
                }
            }
            Status::Final if self.final_remaining.is_empty() => {
                self.status = Status::Scoring;
                self.score_at = Some(now + SCORE_DELAY_MS);
            }
            _ => {}
        }
        out
    }

    /// Advance timers: the ready deadline, away turns, and scoring.
    pub fn tick(&mut self, now: i64, present: &dyn Fn(&str) -> bool) -> Outcome {
        let before = self.turn_marker();
        let out = self.tick_inner(now, present);
        self.bump_turn_seq(before);
        out
    }

    fn tick_inner(&mut self, now: i64, present: &dyn Fn(&str) -> bool) -> Outcome {
        let mut out = Outcome::default();
        match self.status {
            Status::Peeking => {
                if self.ready_deadline.is_some_and(|d| now >= d) {
                    self.start_play(&mut out);
                }
            }
            Status::Playing | Status::Final => {
                let player = self.seats[self.turn].player.clone();
                if present(&player) {
                    if self.away_since.take().is_some() {
                        out.changed = true;
                    }
                } else {
                    match self.away_since {
                        None => {
                            self.away_since = Some(now);
                            out.changed = true;
                        }
                        Some(since) if now - since >= GRACE_MS => {
                            if let Stage::Drawn { card } | Stage::Taken { card } = self.stage {
                                self.push_discard(card);
                            }
                            out.event(Some(&player), "away_skip", json!({}));
                            self.end_turn(now);
                        }
                        Some(_) => {}
                    }
                }
            }
            Status::Scoring => {
                if self.score_at.is_some_and(|d| now >= d) {
                    self.finish(&mut out);
                }
            }
            Status::Scored => {}
        }
        out
    }

    /// Reveal and score every hand.
    fn finish(&mut self, out: &mut Outcome) {
        for seat in self.seats.iter_mut().filter(|s| !s.forfeited) {
            seat.score = Some(seat.slots.iter().flatten().map(|&v| i32::from(v)).sum());
        }
        let caller = self.caller.filter(|&c| self.active(c));
        let score = |s: usize| self.seats[s].score.unwrap_or(i32::MAX);
        let others: Vec<usize> = (0..self.seats.len())
            .filter(|&s| self.active(s) && Some(s) != caller)
            .collect();
        let best_other = others.iter().map(|&s| score(s)).min();
        let winners: Vec<usize> = match (caller, best_other) {
            (Some(c), Some(best)) if score(c) < best => vec![c],
            (Some(c), None) => vec![c],
            (_, Some(best)) => others.into_iter().filter(|&s| score(s) == best).collect(),
            (None, None) => vec![],
        };
        for &w in &winners {
            self.seats[w].won = true;
        }
        self.status = Status::Scored;
        self.stage = Stage::Start;
        self.score_at = None;
        self.reveals.clear();
        for (i, seat) in self.seats.iter().enumerate() {
            out.stat(&seat.player, Stat::GamesPlayed, 1);
            if seat.won {
                out.stat(&seat.player, Stat::Wins, 1);
                if Some(i) == caller {
                    out.stat(&seat.player, Stat::KominoWins, 1);
                }
            }
        }
        let winners: Vec<&str> = winners
            .iter()
            .map(|&w| self.seats[w].player.as_str())
            .collect();
        out.event(None, "scored", json!({ "winners": winners }));
    }

    /// The winners of a finished game.
    pub fn winners(&self) -> Vec<&str> {
        self.seats
            .iter()
            .filter(|s| s.won)
            .map(|s| s.player.as_str())
            .collect()
    }

    /// Whether `viewer` may see the value in a slot right now. An observer
    /// (`None`) sees only what scoring makes public.
    fn visible(&self, viewer: Option<&str>, seat: usize, slot: usize, now: i64) -> bool {
        if self.status == Status::Scored {
            return true;
        }
        let Some(viewer) = viewer else {
            return false;
        };
        let s = &self.seats[seat];
        if self.status == Status::Peeking
            && s.player == viewer
            && !s.ready
            && OPENING_PEEK.contains(&slot)
        {
            return true;
        }
        self.reveals
            .iter()
            .any(|r| r.player == viewer && r.seat == seat && r.slot == slot && r.until > now)
    }

    /// The game as `viewer` may see it: hidden values are left out entirely.
    pub fn view(&self, viewer: &str, now: i64) -> Value {
        self.view_for(Some(viewer), now)
    }

    /// The game as an observer sees it: no seat, so no private values.
    pub fn observer_view(&self, now: i64) -> Value {
        self.view_for(None, now)
    }

    fn view_for(&self, viewer: Option<&str>, now: i64) -> Value {
        let me = viewer.and_then(|v| self.seat_of(v));
        let seats: Vec<Value> = self
            .seats
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let slots: Vec<Value> = s
                    .slots
                    .iter()
                    .enumerate()
                    .map(|(j, card)| match card {
                        None => Value::Null,
                        Some(v) if self.visible(viewer, i, j, now) => json!({ "v": v }),
                        Some(_) => json!({}),
                    })
                    .collect();
                json!({
                    "player": s.player,
                    "slots": slots,
                    "ready": s.ready,
                    "forfeited": s.forfeited,
                    "turns": s.turns,
                    "score": s.score,
                    "won": s.won,
                    "locked": self.locked(i),
                })
            })
            .collect();
        let is_turn = me == Some(self.turn) && self.status.in_play();
        let stage = match &self.stage {
            // the drawn card is the drawer's alone
            Stage::Drawn { card } => {
                json!({ "kind": "drawn", "card": if is_turn { Some(*card) } else { None } })
            }
            other => serde_json::to_value(other).unwrap_or(Value::Null),
        };
        let reveals: Vec<Value> = self
            .reveals
            .iter()
            .filter(|r| Some(r.player.as_str()) == viewer && r.until > now)
            .map(|r| json!({ "seat": r.seat, "slot": r.slot, "until": r.until }))
            .collect();
        // which slots someone is looking at is public; the values are not
        let peeked: Vec<Value> = self
            .reveals
            .iter()
            .filter(|r| r.until > now)
            .map(|r| json!({ "seat": r.seat, "slot": r.slot, "until": r.until }))
            .collect();
        json!({
            "status": self.status.as_str(),
            "seats": seats,
            "me": me,
            "turn": self.turn,
            "turn_seq": self.turn_seq,
            "stage": stage,
            "caller": self.caller,
            "final_remaining": self.final_remaining,
            "discard_top": self.discard.last(),
            "discard_count": self.discard.len(),
            "discard_seq": self.discard_seq,
            "matchable": self.matchable,
            "deck_count": self.deck.len(),
            "reveals": reveals,
            "peeked": peeked,
            "can_call": is_turn
                && self.status == Status::Playing
                && self.stage == Stage::Start
                && self.all_have_played(),
            "ready_deadline": self.ready_deadline,
            "away_deadline": self.away_since.map(|t| t + GRACE_MS),
            "score_at": self.score_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    fn rng() -> rand::rngs::StdRng {
        rand::rngs::StdRng::seed_from_u64(7)
    }

    /// Build a deck so that dealing gives each player the listed hand (slot
    /// order), then `starter` is flipped, then `draws` come off in order.
    fn stacked(hands: &[[i8; 4]], starter: i8, draws: &[i8]) -> Vec<i8> {
        let mut top_first = vec![];
        for slot in 0..4 {
            for hand in hands {
                top_first.push(hand[slot]);
            }
        }
        top_first.push(starter);
        top_first.extend_from_slice(draws);
        top_first.reverse();
        top_first
    }

    fn players(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("p{i}")).collect()
    }

    /// A two player game past the peek phase, p0 to move.
    fn playing(hands: &[[i8; 4]], starter: i8, draws: &[i8]) -> Game {
        let mut g = Game::new(players(hands.len()), stacked(hands, starter, draws), 0, 0);
        for p in players(hands.len()) {
            g.apply(&p, Action::Ready, None, 0, &mut rng()).unwrap();
        }
        assert_eq!(g.status, Status::Playing);
        g
    }

    /// Act with the current turn token, as a client looking at the latest
    /// view would.
    fn act(g: &mut Game, p: &str, a: Action) -> Outcome {
        let seq = g.turn_seq;
        g.apply(p, a, Some(seq), 0, &mut rng()).unwrap()
    }

    fn reject(g: &mut Game, p: &str, a: Action) -> &'static str {
        let seq = g.turn_seq;
        g.apply(p, a, Some(seq), 0, &mut rng()).unwrap_err().code
    }

    #[test]
    fn turn_actions_need_the_current_turn_token() {
        let mut g = playing(&[[1; 4], [2; 4]], 5, &[6, 7]);
        let seq = g.turn_seq;
        let draw = |g: &mut Game, s| g.apply("p0", Action::Draw, s, 0, &mut rng());
        assert_eq!(draw(&mut g, None).unwrap_err().code, "stale");
        assert_eq!(draw(&mut g, Some(seq + 1)).unwrap_err().code, "stale");
        draw(&mut g, Some(seq)).unwrap();
        assert_eq!(g.turn_seq, seq + 1);
        // the same token is spent: a repeated submission is refused
        let again = g.apply("p0", Action::Discard, Some(seq), 0, &mut rng());
        assert_eq!(again.unwrap_err().code, "stale");
        assert_eq!(g.stage, Stage::Drawn { card: 6 });
    }

    #[test]
    fn matches_do_not_spend_the_turn_token() {
        let mut g = playing(&[[1, 2, 3, 4], [4, 6, 7, 8]], 0, &[4, 9]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard); // 4, no special
        act(&mut g, "p1", Action::Draw); // 9
        let seq = g.turn_seq;
        // p0 matches out of turn while p1 holds a drawn card
        let m = g.discard_seq;
        act(
            &mut g,
            "p0",
            Action::Match {
                seq: m,
                seat: 0,
                slot: 3,
                give_slot: None,
            },
        );
        assert_eq!(g.turn_seq, seq);
        g.apply("p1", Action::Discard, Some(seq), 0, &mut rng())
            .unwrap();
    }

    #[test]
    fn timers_and_forfeits_move_the_turn_token() {
        let mut g = playing(&[[1; 4], [2; 4], [3; 4]], 5, &[]);
        let seq = g.turn_seq;
        g.forfeit("p0", 0);
        assert!(g.turn_seq > seq);
        let seq = g.turn_seq;
        g.tick(0, &|p| p != "p1");
        g.tick(GRACE_MS, &|p| p != "p1");
        assert_eq!(g.turn, 2);
        assert!(g.turn_seq > seq);
    }

    #[test]
    fn old_states_without_a_turn_token_still_load() {
        let g = playing(&[[1; 4], [2; 4]], 5, &[]);
        let mut stored = serde_json::to_value(&g).unwrap();
        stored.as_object_mut().unwrap().remove("turn_seq");
        let loaded: Game = serde_json::from_value(stored).unwrap();
        assert_eq!(loaded.turn_seq, 0);
    }

    #[test]
    fn deck_has_sixty_numeric_cards() {
        let d = deck();
        assert_eq!(d.len(), 60);
        assert_eq!(d.iter().filter(|&&v| v == -1).count(), 4);
        assert_eq!(d.iter().filter(|&&v| v == 0).count(), 4);
        assert_eq!(d.iter().filter(|&&v| v == 13).count(), 4);
        assert_eq!(*d.iter().min().unwrap(), -1);
        assert_eq!(*d.iter().max().unwrap(), 13);
    }

    #[test]
    fn special_moves_by_value() {
        assert_eq!(special(6), None);
        assert_eq!(special(7), Some(Move::PeekOwn));
        assert_eq!(special(10), Some(Move::PeekOther));
        assert_eq!(special(12), Some(Move::BlindSwap));
        assert_eq!(special(13), Some(Move::LookSwap));
        assert_eq!(special(-1), None);
    }

    #[test]
    fn deal_gives_four_cards_and_a_starter_that_cannot_be_matched() {
        let g = Game::new(
            players(2),
            stacked(&[[1, 2, 3, 4], [5, 6, 0, -1]], 9, &[]),
            0,
            0,
        );
        assert_eq!(g.seats[0].slots, vec![Some(1), Some(2), Some(3), Some(4)]);
        assert_eq!(g.seats[1].slots, vec![Some(5), Some(6), Some(0), Some(-1)]);
        assert_eq!(g.discard, vec![9]);
        assert!(!g.matchable);
        assert_eq!(g.deck.len(), 0);
    }

    #[test]
    fn opening_peek_shows_only_near_row_until_ready() {
        let mut g = Game::new(
            players(2),
            stacked(&[[1, 2, 3, 4], [5, 6, 0, -1]], 9, &[]),
            0,
            0,
        );
        let v = g.view("p0", 0);
        let slots = &v["seats"][0]["slots"];
        assert!(slots[0].get("v").is_none());
        assert!(slots[1].get("v").is_none());
        assert_eq!(slots[2]["v"], 3);
        assert_eq!(slots[3]["v"], 4);
        // never another player's cards
        assert!(v["seats"][1]["slots"][2].get("v").is_none());
        act(&mut g, "p0", Action::Ready);
        assert!(g.view("p0", 0)["seats"][0]["slots"][2].get("v").is_none());
    }

    #[test]
    fn ready_deadline_starts_play() {
        let mut g = Game::new(players(2), stacked(&[[1; 4], [2; 4]], 9, &[]), 0, 0);
        assert!(!g.tick(READY_MS - 1, &|_| true).changed);
        assert!(g.tick(READY_MS, &|_| true).changed);
        assert_eq!(g.status, Status::Playing);
    }

    #[test]
    fn draw_then_swap_discards_the_replaced_card() {
        let mut g = playing(&[[1, 2, 3, 4], [5, 5, 5, 5]], 9, &[6]);
        assert_eq!(reject(&mut g, "p1", Action::Draw), "not_your_turn");
        act(&mut g, "p0", Action::Draw);
        assert_eq!(g.view("p0", 0)["stage"]["card"], 6);
        assert!(g.view("p1", 0)["stage"]["card"].is_null());
        act(&mut g, "p0", Action::Swap { slot: 1 });
        assert_eq!(g.seats[0].slots[1], Some(6));
        assert_eq!(g.discard.last(), Some(&2));
        assert!(g.matchable);
        assert_eq!(g.turn, 1);
    }

    #[test]
    fn taking_the_discard_must_swap_and_has_no_special() {
        let mut g = playing(&[[1, 2, 3, 4], [5; 4]], 13, &[]);
        act(&mut g, "p0", Action::Take);
        assert_eq!(reject(&mut g, "p0", Action::Discard), "invalid");
        act(&mut g, "p0", Action::Swap { slot: 0 });
        assert_eq!(g.seats[0].slots[0], Some(13));
        assert_eq!(g.stage, Stage::Start);
        assert_eq!(g.turn, 1);
    }

    #[test]
    fn peek_own_reveals_to_the_peeker_only_for_a_while() {
        let mut g = playing(&[[1, 2, 3, 4], [5; 4]], 0, &[7]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        assert_eq!(g.stage, Stage::Special { mv: Move::PeekOwn });
        assert_eq!(
            reject(&mut g, "p0", Action::Peek { seat: 1, slot: 0 }),
            "invalid"
        );
        let out = act(&mut g, "p0", Action::Peek { seat: 0, slot: 0 });
        assert!(out.stats.contains(&("p0".into(), Stat::SpecialMoves, 1)));
        assert_eq!(g.view("p0", 1)["seats"][0]["slots"][0]["v"], 1);
        assert!(g.view("p1", 1)["seats"][0]["slots"][0].get("v").is_none());
        assert!(g.view("p0", PEEK_MS + 1)["seats"][0]["slots"][0]
            .get("v")
            .is_none());
        assert_eq!(g.turn, 1);
    }

    #[test]
    fn peek_other_targets_another_player() {
        let mut g = playing(&[[1, 2, 3, 4], [5, 6, 7, 8]], 0, &[9]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        assert_eq!(
            reject(&mut g, "p0", Action::Peek { seat: 0, slot: 0 }),
            "invalid"
        );
        act(&mut g, "p0", Action::Peek { seat: 1, slot: 1 });
        assert_eq!(g.view("p0", 1)["seats"][1]["slots"][1]["v"], 6);
    }

    #[test]
    fn blind_swap_exchanges_without_revealing() {
        let mut g = playing(&[[1, 2, 3, 4], [5, 6, 7, 8]], 0, &[11]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        act(
            &mut g,
            "p0",
            Action::BlindSwap {
                slot: 0,
                seat: 1,
                target_slot: 3,
            },
        );
        assert_eq!(g.seats[0].slots[0], Some(8));
        assert_eq!(g.seats[1].slots[3], Some(1));
        assert!(g.view("p0", 1)["seats"][0]["slots"][0].get("v").is_none());
    }

    #[test]
    fn look_and_swap_can_swap_or_decline() {
        let mut g = playing(&[[1, 2, 3, 4], [5, 6, 7, 8]], 0, &[13, 13]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        act(&mut g, "p0", Action::Peek { seat: 1, slot: 0 });
        assert_eq!(g.stage, Stage::Looked { seat: 1, slot: 0 });
        act(&mut g, "p0", Action::LookSwap { slot: Some(2) });
        assert_eq!(g.seats[0].slots[2], Some(5));
        assert_eq!(g.seats[1].slots[0], Some(3));
        // the reveal does not follow a different card into the looked slot
        assert!(g.view("p0", 1)["seats"][1]["slots"][0].get("v").is_none());

        act(&mut g, "p1", Action::Draw);
        act(&mut g, "p1", Action::Discard);
        act(&mut g, "p1", Action::Peek { seat: 0, slot: 0 });
        act(&mut g, "p1", Action::LookSwap { slot: None });
        assert_eq!(g.seats[0].slots[0], Some(1));
        assert_eq!(g.turn, 0);
    }

    #[test]
    fn special_move_can_be_skipped() {
        let mut g = playing(&[[1; 4], [2; 4]], 0, &[9]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        let out = act(&mut g, "p0", Action::Skip);
        assert!(!out.stats.iter().any(|s| s.1 == Stat::SpecialMoves));
        assert_eq!(g.turn, 1);
    }

    #[test]
    fn starter_cannot_be_matched_but_a_played_discard_can() {
        let mut g = playing(&[[4, 2, 3, 4], [4, 6, 7, 8]], 4, &[1]);
        let seq = g.discard_seq;
        let m = Action::Match {
            seq,
            seat: 0,
            slot: 0,
            give_slot: None,
        };
        assert_eq!(reject(&mut g, "p0", m), "too_late");
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Swap { slot: 3 });
        // p1 matches p0's discarded 4 with their own 4 out of turn order
        let seq = g.discard_seq;
        act(
            &mut g,
            "p1",
            Action::Match {
                seq,
                seat: 1,
                slot: 0,
                give_slot: None,
            },
        );
        assert_eq!(g.seats[1].slots[0], None);
        assert_eq!(g.discard.last(), Some(&4));
    }

    #[test]
    fn matching_another_players_card_gives_them_one_of_yours() {
        let mut g = playing(&[[1, 2, 3, 5], [5, 6, 7, 8]], 0, &[9]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Swap { slot: 3 }); // discards 5
        let seq = g.discard_seq;
        let no_give = Action::Match {
            seq,
            seat: 1,
            slot: 0,
            give_slot: None,
        };
        assert_eq!(reject(&mut g, "p0", no_give), "invalid");
        act(
            &mut g,
            "p0",
            Action::Match {
                seq,
                seat: 1,
                slot: 0,
                give_slot: Some(0),
            },
        );
        assert_eq!(g.seats[0].slots[0], None);
        assert_eq!(g.seats[1].slots[0], Some(1));
    }

    #[test]
    fn only_the_first_match_counts() {
        let mut g = playing(&[[5, 2, 3, 4], [5, 6, 7, 8]], 0, &[5]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        let seq = g.discard_seq;
        act(
            &mut g,
            "p1",
            Action::Match {
                seq,
                seat: 1,
                slot: 0,
                give_slot: None,
            },
        );
        let late = Action::Match {
            seq,
            seat: 0,
            slot: 0,
            give_slot: None,
        };
        assert_eq!(reject(&mut g, "p0", late), "too_late");
        // the matched card is the new top and can itself be matched
        let seq = g.discard_seq;
        act(
            &mut g,
            "p0",
            Action::Match {
                seq,
                seat: 0,
                slot: 0,
                give_slot: None,
            },
        );
        assert_eq!(g.seats[0].slots[0], None);
    }

    #[test]
    fn wrong_match_draws_a_penalty_card() {
        let mut g = playing(&[[1, 2, 3, 4], [5, 6, 7, 8]], 0, &[9, 12]);
        act(&mut g, "p0", Action::Draw); // 9
        act(&mut g, "p0", Action::Swap { slot: 0 }); // discards 1
        let seq = g.discard_seq;
        let out = act(
            &mut g,
            "p1",
            Action::Match {
                seq,
                seat: 1,
                slot: 0,
                give_slot: None,
            },
        );
        assert!(out.stats.contains(&("p1".into(), Stat::FailedMatches, 1)));
        assert_eq!(g.seats[1].slots.len(), 5);
        assert_eq!(g.seats[1].slots[4], Some(12));
        assert_eq!(g.seats[1].slots[0], Some(5));
    }

    #[test]
    fn taking_the_discard_closes_matches_on_it() {
        let mut g = playing(&[[1, 2, 3, 4], [5, 6, 7, 8]], 0, &[4]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        let seq = g.discard_seq;
        act(&mut g, "p1", Action::Take);
        let late = Action::Match {
            seq,
            seat: 0,
            slot: 3,
            give_slot: None,
        };
        assert_eq!(reject(&mut g, "p0", late), "too_late");
    }

    #[test]
    fn komino_requires_a_full_round_then_gives_everyone_one_turn() {
        let mut g = playing(&[[0; 4], [5; 4], [6; 4]], 1, &[9, 9, 9, 9, 9]);
        assert_eq!(reject(&mut g, "p0", Action::Komino), "invalid");
        for p in ["p0", "p1", "p2"] {
            act(&mut g, p, Action::Draw);
            act(&mut g, p, Action::Discard);
            act(&mut g, p, Action::Skip);
        }
        act(&mut g, "p0", Action::Komino);
        assert_eq!(g.status, Status::Final);
        assert_eq!(g.final_remaining, vec![1, 2]);
        assert!(g.view("p1", 0)["seats"][0]["locked"].as_bool().unwrap());
        // the caller's cards are locked against matches by others
        act(&mut g, "p1", Action::Draw);
        act(&mut g, "p1", Action::Discard);
        assert_eq!(
            reject(&mut g, "p1", Action::Peek { seat: 0, slot: 0 }),
            "invalid"
        );
        act(&mut g, "p1", Action::Skip);
        act(&mut g, "p2", Action::Draw);
        act(&mut g, "p2", Action::Discard);
        act(&mut g, "p2", Action::Skip);
        assert_eq!(g.status, Status::Scoring);
        let out = g.tick(SCORE_DELAY_MS, &|_| true);
        assert_eq!(g.status, Status::Scored);
        assert_eq!(g.winners(), vec!["p0"]);
        assert!(out.stats.contains(&("p0".into(), Stat::KominoWins, 1)));
        assert_eq!(
            out.stats
                .iter()
                .filter(|s| s.1 == Stat::GamesPlayed)
                .count(),
            3
        );
        // everything is revealed at the end
        assert_eq!(g.view("p1", 0)["seats"][2]["slots"][0]["v"], 6);
    }

    #[test]
    fn a_caller_without_the_strictly_lowest_score_cannot_win() {
        let mut g = playing(&[[1; 4], [1; 4], [2; 4]], 5, &[9, 9, 9, 9, 9]);
        for p in ["p0", "p1", "p2"] {
            act(&mut g, p, Action::Draw);
            act(&mut g, p, Action::Discard);
            act(&mut g, p, Action::Skip);
        }
        act(&mut g, "p0", Action::Komino);
        for p in ["p1", "p2"] {
            act(&mut g, p, Action::Draw);
            act(&mut g, p, Action::Discard);
            act(&mut g, p, Action::Skip);
        }
        g.tick(SCORE_DELAY_MS, &|_| true);
        assert_eq!(g.winners(), vec!["p1"]);
    }

    #[test]
    fn ties_among_other_players_share_the_win() {
        let mut g = playing(&[[3; 4], [1; 4], [1; 4]], 5, &[9, 9, 9, 9, 9]);
        for p in ["p0", "p1", "p2"] {
            act(&mut g, p, Action::Draw);
            act(&mut g, p, Action::Discard);
            act(&mut g, p, Action::Skip);
        }
        act(&mut g, "p0", Action::Komino);
        for p in ["p1", "p2"] {
            act(&mut g, p, Action::Draw);
            act(&mut g, p, Action::Discard);
            act(&mut g, p, Action::Skip);
        }
        g.tick(SCORE_DELAY_MS, &|_| true);
        assert_eq!(g.winners(), vec!["p1", "p2"]);
    }

    #[test]
    fn away_player_is_skipped_after_the_grace_period() {
        let mut g = playing(&[[1; 4], [2; 4]], 5, &[9]);
        let away = |p: &str| p != "p0";
        assert!(g.tick(100, &away).changed);
        assert_eq!(g.away_since, Some(100));
        assert!(g.tick(100 + GRACE_MS - 1, &away).events.is_empty());
        let out = g.tick(100 + GRACE_MS, &away);
        assert_eq!(out.events[0].kind, "away_skip");
        assert_eq!(g.turn, 1);
        assert_eq!(g.seats[0].turns, 1);
    }

    #[test]
    fn forfeit_removes_cards_and_ends_a_two_player_game() {
        let mut g = playing(&[[1; 4], [2; 4], [3; 4]], 5, &[]);
        g.forfeit("p0", 0);
        assert!(g.seats[0].slots.is_empty());
        assert_eq!(g.turn, 1);
        assert_eq!(g.status, Status::Playing);
        g.forfeit("p1", 0);
        assert_eq!(g.status, Status::Scored);
        assert_eq!(g.winners(), vec!["p2"]);
    }

    #[test]
    fn forfeited_player_cannot_act() {
        let mut g = playing(&[[1; 4], [2; 4], [3; 4]], 5, &[]);
        g.forfeit("p2", 0);
        assert_eq!(reject(&mut g, "p2", Action::Draw), "not_seated");
    }

    #[test]
    fn empty_deck_reshuffles_the_discards_under_the_top() {
        let mut g = playing(&[[1; 4], [2; 4]], 5, &[6]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        assert!(g.deck.is_empty());
        act(&mut g, "p1", Action::Draw);
        assert_eq!(g.discard, vec![6]);
        assert_eq!(g.stage, Stage::Drawn { card: 5 });
    }

    #[test]
    fn views_never_carry_hidden_values() {
        let g = playing(&[[1, 2, 3, 4], [5, 6, 7, 8]], 0, &[9, 9]);
        let text = g.view("p1", 0).to_string();
        assert!(!text.contains("\"v\""), "p1 saw a value: {text}");
        assert!(!text.contains("deck\":["));
    }

    #[test]
    fn observers_see_no_opening_peek_drawn_card_or_reveal() {
        let mut g = Game::new(
            players(2),
            stacked(&[[1, 2, 3, 4], [5, 6, 0, -1]], 9, &[7]),
            0,
            0,
        );
        // during the opening peek each player sees their near row, observers nothing
        assert!(g.view("p0", 0)["seats"][0]["slots"][2].get("v").is_some());
        let text = g.observer_view(0).to_string();
        assert!(!text.contains("\"v\""), "observer saw a value: {text}");
        assert_eq!(g.observer_view(0)["me"], Value::Null);

        g.apply("p0", Action::Ready, None, 0, &mut rng()).unwrap();
        g.apply("p1", Action::Ready, None, 0, &mut rng()).unwrap();
        act(&mut g, "p0", Action::Draw);
        assert_eq!(g.view("p0", 0)["stage"]["card"], 7);
        let obs = g.observer_view(0);
        assert_eq!(obs["stage"]["kind"], "drawn");
        assert_eq!(obs["stage"]["card"], Value::Null);
        assert_eq!(obs["can_call"], false);

        act(&mut g, "p0", Action::Discard);
        act(&mut g, "p0", Action::Peek { seat: 0, slot: 1 });
        // the peeker sees the value; observers and the other player see only the slot
        assert_eq!(g.view("p0", 1)["seats"][0]["slots"][1]["v"], 2);
        for view in [g.observer_view(1), g.view("p1", 1)] {
            assert!(view["seats"][0]["slots"][1].get("v").is_none());
            assert_eq!(view["reveals"], json!([]));
            assert_eq!(
                view["peeked"],
                json!([{ "seat": 0, "slot": 1, "until": PEEK_MS }])
            );
        }
        // the highlight ends with the reveal
        assert_eq!(g.observer_view(PEEK_MS)["peeked"], json!([]));
    }

    #[test]
    fn observers_see_every_hand_once_scored() {
        let mut g = playing(&[[1, 2, 3, 4], [5, 6, 7, 8]], 0, &[9, 9]);
        g.status = Status::Scored;
        for seat in g.observer_view(0)["seats"].as_array().unwrap() {
            for slot in seat["slots"].as_array().unwrap() {
                assert!(slot.get("v").is_some());
            }
        }
    }
}
