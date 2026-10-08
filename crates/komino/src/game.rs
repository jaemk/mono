//! The komino rules engine. Pure state transitions with no database or io,
//! so every rule can be driven from a fixed deck in tests.
//!
//! A `Game` is stored whole as jsonb on the `games` row; field names are part
//! of that stored format.

use crate::bot;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const MAX_SEATS: usize = 8;
pub const READY_MS: i64 = 30_000;
pub const SCORE_DELAY_MS: i64 = 2_000;
/// How long the first match on a discard waits for faster reactions from
/// slower connections (RT-17). Covers the most latency credit a claim can
/// carry (`lag::MAX_CREDIT`) plus some jitter.
pub const MATCH_WINDOW_MS: i64 = 250;
/// Settled claim results kept for the claimers waiting on them.
const SETTLED_KEPT: usize = 32;
/// How long a matcher has to give a card for a match on another player's
/// card before one is given for them (RULE-19).
pub const GIVE_MS: i64 = 15_000;
/// How many of the newest discards the view lists (RULE-28).
pub const DISCARD_RECENT: usize = 10;
/// How long a bot waits before each step of its turn (BOT-4).
pub const BOT_STEP_MS: i64 = 1_200;

/// A deal that leaves fewer cards than this to draw is played with two decks
/// (SET-7).
const MIN_DRAW_PILE: usize = 20;

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

/// The unshuffled cards for a game of `players` with `hand_size` cards each:
/// one deck, or two when one would leave too few to draw (SET-7).
pub fn deck_for(players: usize, hand_size: usize) -> Vec<i8> {
    let mut cards = deck();
    if cards.len() < players * hand_size + 1 + MIN_DRAW_PILE {
        cards.extend(deck());
    }
    cards
}

/// Per-room game settings (SET-1), copied into each game as it starts. A
/// missing field takes its default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub hand_size: usize,
    pub away_grace_secs: i64,
    /// `None` is no turn limit.
    pub turn_limit_secs: Option<i64>,
    /// `None` keeps a peek up until the player hides it.
    pub reveal_secs: Option<i64>,
    /// Show everyone the value of a card a failed match targeted (SET-13).
    pub show_misses: bool,
    /// Play a match of games until a running total reaches this (SET-14);
    /// `None` keeps totals running without an end.
    pub target_score: Option<i64>,
    /// Points added to a caller who does not win (SET-15).
    pub caller_penalty: i64,
    /// A running total that lands exactly on the target is halved (SET-16).
    pub exact_reset: bool,
    /// Players may mark cards with the value they think they hold (SET-17).
    pub memory_marks: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hand_size: 4,
            away_grace_secs: 30,
            turn_limit_secs: None,
            reveal_secs: Some(15),
            show_misses: true,
            target_score: None,
            caller_penalty: 0,
            exact_reset: false,
            memory_marks: false,
        }
    }
}

impl Settings {
    /// The fixed rules of games saved before settings existed (SET-3).
    pub fn legacy() -> Self {
        Self {
            reveal_secs: Some(5),
            show_misses: false,
            ..Self::default()
        }
    }

    /// The allowed ranges of SET-1.
    pub fn validate(&self) -> Result<(), String> {
        if !(4..=10).contains(&self.hand_size) {
            return Err("hand_size must be 4 to 10".into());
        }
        if !(10..=600).contains(&self.away_grace_secs) {
            return Err("away_grace_secs must be 10 to 600".into());
        }
        if self
            .turn_limit_secs
            .is_some_and(|s| !(10..=600).contains(&s))
        {
            return Err("turn_limit_secs must be 10 to 600, or null".into());
        }
        if self.reveal_secs.is_some_and(|s| !(1..=60).contains(&s)) {
            return Err("reveal_secs must be 1 to 60, or null".into());
        }
        if self.target_score.is_some_and(|s| !(25..=500).contains(&s)) {
            return Err("target_score must be 25 to 500, or null".into());
        }
        if !(0..=50).contains(&self.caller_penalty) {
            return Err("caller_penalty must be 0 to 50".into());
        }
        Ok(())
    }

    fn away_grace_ms(&self) -> i64 {
        self.away_grace_secs * 1000
    }

    /// Slots seen during the opening peek: the row nearest the player
    /// (SET-6).
    fn opening_slots(&self) -> std::ops::Range<usize> {
        0..self.hand_size / 2
    }
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
    Drawn {
        card: i8,
    },
    Taken {
        card: i8,
    },
    /// A discarded draw earned `mv`; matching is open until the player uses
    /// it or ends the turn (RULE-10).
    Earned {
        mv: Move,
    },
    Special {
        mv: Move,
    },
    Looked {
        seat: usize,
        slot: usize,
    },
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
    /// The running total brought into this game (SET-14).
    #[serde(default)]
    pub carry: i32,
    /// `carry` plus this game's score, once scored.
    #[serde(default)]
    pub total: Option<i32>,
    /// Won the match this game ended (SET-14).
    #[serde(default)]
    pub match_won: bool,
    #[serde(default)]
    pub tally: Tally,
    /// A computer player (BOT-1).
    #[serde(default)]
    pub bot: bool,
}

/// What a seat did this game, for the end of game summary (UI-38).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    pub matches: u32,
    pub misses: u32,
    pub penalties: u32,
    pub specials: u32,
}

/// A card value shown to one player until `until`, or until they hide it
/// when `until` is `None` (SET-11).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reveal {
    /// Names the peek in a sealed reveal request; empty on states saved
    /// before ids existed, which can't be revealed.
    #[serde(default)]
    pub id: String,
    pub player: String,
    pub seat: usize,
    pub slot: usize,
    pub value: i8,
    pub until: Option<i64>,
}

impl Reveal {
    fn live(&self, now: i64) -> bool {
        self.until.is_none_or(|until| until > now)
    }
}

/// A match waiting for its window to close (RT-16). Claims settle in order
/// of the claimer's reaction time, not arrival.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Claim {
    pub id: String,
    pub player: String,
    /// The discard sequence number the match targets.
    pub seq: u64,
    pub seat: usize,
    pub slot: usize,
    pub give_slot: Option<usize>,
    /// How long the claimer took to react to the discard (RT-18).
    pub reaction_ms: i64,
    /// Server arrival, which breaks ties.
    pub at: i64,
}

/// A card a matcher owes for matching another player's card: it goes into
/// the slot the match emptied (RULE-19).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owed {
    pub player: String,
    pub seat: usize,
    pub slot: usize,
    /// When a card is given for the matcher if they have not chosen one.
    pub deadline: i64,
}

/// How a settled claim ended, read back by the claimer waiting on it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settled {
    pub id: String,
    /// The rejection's code and message; `None` when the match was applied,
    /// right or wrong.
    pub rejected: Option<(String, String)>,
}

impl Settled {
    fn new(id: String, result: Result<(), Reject>) -> Self {
        let rejected = result.err().map(|r| (r.code.to_string(), r.message));
        Self { id, rejected }
    }

    pub fn result(&self) -> Result<(), Reject> {
        match &self.rejected {
            None => Ok(()),
            Some((code, message)) => {
                // only these come out of settling a claim
                let code = match code.as_str() {
                    "too_late" => "too_late",
                    "not_seated" => "not_seated",
                    _ => "invalid",
                };
                Err(Reject::new(code, message.clone()))
            }
        }
    }
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
    /// When the current turn's limit runs out (SET-9).
    #[serde(default)]
    pub turn_deadline: Option<i64>,
    #[serde(default = "Settings::legacy")]
    pub settings: Settings,
    /// The turn player called komino mid turn; the call takes effect when
    /// the turn ends (RULE-22).
    #[serde(default)]
    pub calling: bool,
    /// When the top discard was put there, for timing matches that carry no
    /// measured reaction (RT-18).
    #[serde(default)]
    pub discard_at: Option<i64>,
    /// Matches on the top discard waiting for `claim_deadline` (RT-16).
    #[serde(default)]
    pub claims: Vec<Claim>,
    #[serde(default)]
    pub claim_deadline: Option<i64>,
    /// The newest settled claims, oldest first.
    #[serde(default)]
    pub settled: Vec<Settled>,
    /// Cards matchers still owe for correct matches (RULE-19).
    #[serde(default)]
    pub owed: Vec<Owed>,
    /// Which of the room's matches this game belongs to (SET-14).
    #[serde(default)]
    pub match_no: u32,
    /// Some running total reached the target when this game was scored.
    #[serde(default)]
    pub match_over: bool,
    /// Each bot's memory of the table (BOT-3). Never in a view.
    #[serde(default)]
    pub minds: Vec<bot::Mind>,
    /// The turn token a bot is to act on and when it was first seen, so
    /// bots take a step at a time at a human pace (BOT-4).
    #[serde(default)]
    pub bot_clock: Option<(u64, i64)>,
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
    /// Start the special move a discard earned.
    UseSpecial,
    Komino,
    Peek {
        seat: usize,
        slot: usize,
    },
    /// End every peek the player holds (SET-12).
    Hide,
    BlindSwap {
        slot: usize,
        seat: usize,
        target_slot: usize,
    },
    LookSwap {
        slot: Option<usize>,
    },
    Skip,
    /// `give_slot` may be left out of a match on another player's card; the
    /// card is then given after the match succeeds (`Give`).
    Match {
        seq: u64,
        seat: usize,
        slot: usize,
        give_slot: Option<usize>,
    },
    /// Give one of your cards for a correct match you owe one for.
    Give {
        slot: usize,
    },
}

/// What a sealed reveal asks for (SEAL-6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Secret {
    Opening,
    Drawn,
    Peek(String),
}

impl Action {
    /// Actions that only the turn player takes, guarded by `turn_seq`.
    pub fn is_turn_action(&self) -> bool {
        !matches!(
            self,
            Action::Ready | Action::Match { .. } | Action::Hide | Action::Give { .. }
        )
    }

    /// Actions that can change the top discard or a card a pending claim
    /// names, so the claims settle first (RT-19).
    fn disturbs_claims(&self) -> bool {
        matches!(
            self,
            Action::Take
                | Action::Swap { .. }
                | Action::Discard
                | Action::BlindSwap { .. }
                | Action::LookSwap { .. }
                | Action::Komino
                | Action::Give { .. }
        )
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
    /// A match claimed by this action: its id and when its window closes.
    pub claim: Option<(String, i64)>,
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
    pub fn new(
        players: Vec<String>,
        mut deck: Vec<i8>,
        first: usize,
        now: i64,
        settings: Settings,
    ) -> Self {
        let mut seats: Vec<Seat> = players
            .into_iter()
            .map(|player| Seat {
                player,
                slots: Vec::with_capacity(settings.hand_size),
                ready: false,
                forfeited: false,
                turns: 0,
                score: None,
                won: false,
                carry: 0,
                total: None,
                match_won: false,
                tally: Tally::default(),
                bot: false,
            })
            .collect();
        for _ in 0..settings.hand_size {
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
            turn_deadline: None,
            settings,
            calling: false,
            discard_at: None,
            claims: vec![],
            claim_deadline: None,
            settled: vec![],
            owed: vec![],
            match_no: 0,
            match_over: false,
            minds: vec![],
            bot_clock: None,
        }
    }

    /// Mark the seats `level` names as bots, each at its level and starting
    /// from its own opening cards (BOT-3, BOT-5).
    pub fn seat_bots(&mut self, level: impl Fn(&str) -> Option<bot::Level>) {
        let opening = self.settings.opening_slots();
        for (i, seat) in self.seats.iter_mut().enumerate() {
            let Some(level) = level(&seat.player) else {
                continue;
            };
            seat.bot = true;
            let cards = opening
                .clone()
                .filter_map(|n| seat.slots.get(n).copied().flatten().map(|v| (n, v)));
            self.minds
                .push(bot::Mind::new(&seat.player, i, level, cards));
        }
    }

    /// Let every bot learn from what just happened.
    fn observe(&mut self, events: &[Event], rng: &mut impl rand::Rng) {
        if self.minds.is_empty() {
            return;
        }
        let mut minds = std::mem::take(&mut self.minds);
        for e in events {
            for mind in minds.iter_mut() {
                mind.observe(e, self, rng);
            }
        }
        self.minds = minds;
    }

    /// One round of bot play (BOT-4): readying, giving owed cards, and
    /// matches they are sure of, then one step of a bot's turn once it has
    /// waited [`BOT_STEP_MS`].
    fn drive_bots(&mut self, now: i64, rng: &mut impl rand::Rng, out: &mut Outcome) {
        if self.minds.is_empty() || self.status == Status::Scored {
            return;
        }
        for i in 0..self.minds.len() {
            let (player, seat) = (self.minds[i].player.clone(), self.minds[i].seat);
            if self.seats[seat].forfeited {
                continue;
            }
            if self.status == Status::Peeking && !self.seats[seat].ready {
                self.bot_act(&player, Action::Ready, None, now, rng, out);
            }
            if self.owed.iter().any(|o| o.player == player) {
                if let Some(a) = bot::give_action(self, &self.minds[i]) {
                    self.bot_act(&player, a, None, now, rng, out);
                }
            }
            // a bot reacts no faster than its level allows, timed honestly
            let reaction = self.minds[i].level.reaction_ms();
            let waited = self.discard_at.is_some_and(|at| now - at >= reaction);
            if waited && !self.claims.iter().any(|c| c.player == player) {
                if let Some(a) = bot::match_action(self, &self.minds[i]) {
                    self.bot_act(&player, a, None, now, rng, out);
                }
            }
        }
        if !self.status.in_play() || !self.seats[self.turn].bot {
            return;
        }
        match self.bot_clock {
            Some((seq, since)) if seq == self.turn_seq => {
                if now - since < BOT_STEP_MS {
                    return;
                }
            }
            _ => {
                self.bot_clock = Some((self.turn_seq, now));
                out.changed = true;
                return;
            }
        }
        let player = self.seats[self.turn].player.clone();
        let Some(i) = self.minds.iter().position(|m| m.player == player) else {
            return;
        };
        let turn = self.turn;
        let seq = Some(self.turn_seq);
        let a = bot::turn_action(self, &self.minds[i], rng);
        let fallback = if self.stage == Stage::Start {
            Action::Draw
        } else {
            Action::Skip
        };
        if !self.bot_act(&player, a, seq, now, rng, out) {
            self.bot_act(&player, fallback, seq, now, rng, out);
        }
        self.bot_clock = Some((self.turn_seq, now));
        out.changed = true;
        if self.turn != turn || !self.status.in_play() {
            self.minds[i].forget(rng);
        }
    }

    /// Apply a bot's action as any player's, adding what it did to `out`.
    fn bot_act(
        &mut self,
        player: &str,
        a: Action,
        turn_seq: Option<u64>,
        now: i64,
        rng: &mut impl rand::Rng,
        out: &mut Outcome,
    ) -> bool {
        match self.apply_timed(player, a, turn_seq, None, now, rng) {
            Ok(o) => {
                out.events.extend(o.events);
                out.stats.extend(o.stats);
                out.changed = true;
                true
            }
            Err(_) => false,
        }
    }

    /// Start every seat from the running total the room carries for it
    /// (SET-14).
    pub fn carry_in(&mut self, match_no: u32, carry: impl Fn(&str) -> i32) {
        self.match_no = match_no;
        for seat in self.seats.iter_mut() {
            seat.carry = carry(&seat.player);
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

    fn push_discard(&mut self, card: i8, now: i64) {
        self.discard.push(card);
        self.discard_seq += 1;
        self.matchable = true;
        self.discard_at = Some(now);
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

    /// Whether every seated player has had a turn once `seat`'s current turn
    /// ends: before drawing it doesn't count yet, mid turn it does.
    fn all_have_played(&self, seat: usize) -> bool {
        let counting = self.stage != Stage::Start;
        self.seats
            .iter()
            .enumerate()
            .all(|(i, s)| s.forfeited || s.turns > 0 || (counting && i == seat))
    }

    /// Whether the turn player at `seat` may call komino now (RULE-22).
    pub(crate) fn may_call(&self, seat: usize) -> Result<(), Reject> {
        if self.status != Status::Playing {
            return Err(Reject::invalid("komino was already called"));
        }
        if self.calling {
            return Err(Reject::invalid(
                "you already called komino; it takes effect when your turn ends",
            ));
        }
        if !self.all_have_played(seat) {
            return Err(Reject::invalid("everyone must take a turn before komino"));
        }
        Ok(())
    }

    fn call_komino(&mut self, out: &mut Outcome) {
        let me = self.turn;
        let player = self.seats[me].player.clone();
        self.caller = Some(me);
        self.status = Status::Final;
        let n = self.seats.len();
        self.final_remaining = (1..n)
            .map(|step| (me + step) % n)
            .filter(|&s| self.active(s))
            .collect();
        out.stat(&player, Stat::KominoCalls, 1);
        out.event(Some(&player), "komino", json!({ "seat": me }));
    }

    fn start_play(&mut self, now: i64, out: &mut Outcome) {
        self.status = Status::Playing;
        self.ready_deadline = None;
        for seat in self.seats.iter_mut() {
            seat.ready = true;
        }
        self.begin_turn(now);
        let first = self.seats[self.turn].player.clone();
        out.event(None, "play", json!({ "first": first }));
    }

    /// Start the turn limit's clock for the turn starting now (SET-9).
    fn begin_turn(&mut self, now: i64) {
        self.turn_deadline = self.settings.turn_limit_secs.map(|s| now + s * 1000);
    }

    fn start_scoring(&mut self, now: i64) {
        self.status = Status::Scoring;
        self.score_at = Some(now + SCORE_DELAY_MS);
        self.turn_deadline = None;
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

    fn end_turn(&mut self, now: i64, out: &mut Outcome) {
        self.stage = Stage::Start;
        self.away_since = None;
        if let Some(seat) = self.seats.get_mut(self.turn) {
            seat.turns += 1;
        }
        if std::mem::take(&mut self.calling) {
            self.call_komino(out);
        }
        if self.caller.is_some() {
            let done = self.turn;
            self.final_remaining.retain(|&s| s != done);
            if self.final_remaining.is_empty() {
                self.start_scoring(now);
                return;
            }
        }
        self.next_turn();
        self.begin_turn(now);
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
        self.apply_timed(player, action, turn_seq, None, now, rng)
    }

    /// [`Game::apply`] with the claimer's measured reaction time for a match
    /// (RT-18). Without one, a match is timed from when the discard landed.
    pub fn apply_timed(
        &mut self,
        player: &str,
        action: Action,
        turn_seq: Option<u64>,
        reaction_ms: Option<i64>,
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
        let out = self.apply_inner(player, action, reaction_ms, now, rng)?;
        self.bump_turn_seq(before);
        self.observe(&out.events, rng);
        Ok(out)
    }

    fn apply_inner(
        &mut self,
        player: &str,
        action: Action,
        reaction_ms: Option<i64>,
        now: i64,
        rng: &mut impl rand::Rng,
    ) -> Result<Outcome, Reject> {
        let me = self
            .seat_of(player)
            .filter(|&s| self.active(s))
            .ok_or_else(|| Reject::new("not_seated", "you are not playing in this game"))?;
        self.reveals.retain(|r| r.live(now));
        let mut out = Outcome::default();
        if action.disturbs_claims() {
            // claims made before this action land before it (RT-19)
            self.settle_claims(now, rng, &mut out);
        }
        match action {
            Action::Ready => {
                if self.status != Status::Peeking {
                    return Err(Reject::invalid("not in the peek phase"));
                }
                self.seats[me].ready = true;
                out.event(Some(player), "ready", json!({}));
                if self.seats.iter().all(|s| s.ready || s.forfeited) {
                    self.start_play(now, &mut out);
                }
            }
            Action::Hide => {
                self.reveals.retain(|r| r.player != player);
                out.changed = true;
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
                self.push_discard(old, now);
                out.stat(player, Stat::CardsInteracted, 2);
                out.event(
                    Some(player),
                    "swap",
                    json!({ "seat": me, "slot": slot, "discarded": old }),
                );
                self.end_turn(now, &mut out);
            }
            Action::Discard => {
                self.require_turn(me)?;
                let Stage::Drawn { card } = self.stage else {
                    return Err(Reject::invalid(
                        "only a card drawn from the deck can be discarded",
                    ));
                };
                self.push_discard(card, now);
                out.stat(player, Stat::CardsInteracted, 1);
                out.event(Some(player), "discard", json!({ "value": card }));
                match special(card) {
                    Some(mv) => self.stage = Stage::Earned { mv },
                    None => self.end_turn(now, &mut out),
                }
            }
            Action::UseSpecial => {
                self.require_turn(me)?;
                let Stage::Earned { mv } = self.stage else {
                    return Err(Reject::invalid("there is no special move to use"));
                };
                self.stage = Stage::Special { mv };
                out.changed = true;
            }
            Action::Komino => {
                self.require_turn(me)?;
                self.may_call(me)?;
                self.calling = true;
                if self.stage == Stage::Start {
                    self.end_turn(now, &mut out);
                } else {
                    // only the caller knows until the turn ends
                    out.changed = true;
                }
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
                    id: uuid::Uuid::new_v4().simple().to_string(),
                    player: player.to_string(),
                    seat,
                    slot,
                    value,
                    until: self.settings.reveal_secs.map(|s| now + s * 1000),
                });
                self.seats[me].tally.specials += 1;
                out.stat(player, Stat::SpecialMoves, 1);
                out.stat(player, Stat::CardsInteracted, 1);
                out.event(Some(player), "peek", json!({ "seat": seat, "slot": slot }));
                if matches!(self.stage, Stage::Special { mv: Move::LookSwap }) {
                    self.stage = Stage::Looked { seat, slot };
                } else {
                    self.end_turn(now, &mut out);
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
                self.seats[me].tally.specials += 1;
                out.stat(player, Stat::SpecialMoves, 1);
                out.stat(player, Stat::CardsInteracted, 2);
                out.event(
                    Some(player),
                    "blind_swap",
                    json!({ "seat": me, "slot": slot, "target_seat": seat, "target_slot": target_slot }),
                );
                self.end_turn(now, &mut out);
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
                self.end_turn(now, &mut out);
            }
            Action::Skip => {
                self.require_turn(me)?;
                if !matches!(
                    self.stage,
                    Stage::Earned { .. } | Stage::Special { .. } | Stage::Looked { .. }
                ) {
                    return Err(Reject::invalid("there is no special move to skip"));
                }
                out.event(Some(player), "skip", json!({}));
                self.end_turn(now, &mut out);
            }
            Action::Match {
                seq,
                seat,
                slot,
                give_slot,
            } => {
                self.check_match(me, seq, seat, slot, give_slot)?;
                if self.claims.iter().any(|c| c.player == player) {
                    return Err(Reject::invalid(
                        "your match on this discard is still being decided",
                    ));
                }
                let id = uuid::Uuid::new_v4().simple().to_string();
                // untimed claims count from when the discard landed
                let reaction_ms = reaction_ms
                    .unwrap_or_else(|| now - self.discard_at.unwrap_or(now))
                    .max(0);
                self.claims.push(Claim {
                    id: id.clone(),
                    player: player.to_string(),
                    seq,
                    seat,
                    slot,
                    give_slot,
                    reaction_ms,
                    at: now,
                });
                let deadline = *self.claim_deadline.get_or_insert(now + MATCH_WINDOW_MS);
                out.claim = Some((id, deadline));
                out.changed = true;
            }
            Action::Give { slot } => {
                let i = self
                    .owed
                    .iter()
                    .position(|o| o.player == player)
                    .ok_or_else(|| Reject::invalid("you do not owe a card"))?;
                self.require_filled(me, slot)?;
                self.give(i, slot, &mut out);
            }
        }
        Ok(out)
    }

    /// Settle owed cards: those past their deadline, or all of them when
    /// `now` is `None`. The matcher's highest card is given for them (the
    /// lowest slot among equals), so a missed choice never costs their best
    /// card; a matcher with no cards left owes nothing.
    fn settle_owed(&mut self, now: Option<i64>, out: &mut Outcome) {
        while let Some(i) = self
            .owed
            .iter()
            .position(|o| now.is_none_or(|now| now >= o.deadline))
        {
            let from = self.seat_of(&self.owed[i].player);
            let slot = from.and_then(|s| {
                let slots = &self.seats[s].slots;
                (0..slots.len())
                    .filter_map(|i| slots[i].map(|v| (v, std::cmp::Reverse(i))))
                    .max()
                    .map(|(_, std::cmp::Reverse(i))| i)
            });
            match slot {
                Some(slot) => self.give(i, slot, out),
                None => {
                    self.owed.remove(i);
                    out.changed = true;
                }
            }
        }
    }

    /// Move the matcher's `slot` into the slot owed entry `i` names.
    fn give(&mut self, i: usize, slot: usize, out: &mut Outcome) {
        let o = self.owed.remove(i);
        let me = self.seat_of(&o.player).expect("owed by a seated player");
        let given = self.seats[me].slots[slot].take();
        self.touch(me, slot);
        self.seats[o.seat].slots[o.slot] = given;
        self.touch(o.seat, o.slot);
        out.stat(&o.player, Stat::CardsInteracted, 1);
        out.event(
            Some(&o.player),
            "give",
            json!({ "seat": me, "slot": slot, "target_seat": o.seat, "target_slot": o.slot }),
        );
    }

    /// Settle every pending claim now, fastest reaction first (RT-17). Each
    /// is checked against the table as the ones before it left it, so after
    /// a correct match the rest are too late, and a wrong one leaves the
    /// discard open to the next.
    fn settle_claims(&mut self, now: i64, rng: &mut impl rand::Rng, out: &mut Outcome) {
        self.claim_deadline = None;
        if self.claims.is_empty() {
            return;
        }
        let mut claims = std::mem::take(&mut self.claims);
        claims.sort_by_key(|c| (c.reaction_ms, c.at));
        // the reaction of the claim that matched this discard, if one did
        let mut beaten_by: Option<i64> = None;
        for c in claims {
            let seq = self.discard_seq;
            let result = match self.seat_of(&c.player).filter(|&s| self.active(s)) {
                None => Err(Reject::new(
                    "not_seated",
                    "you are not playing in this game",
                )),
                Some(me) => self.do_match(me, &c, now, rng, out),
            };
            // a later claim on the same discard says by how much it lost
            let result = match (result, beaten_by) {
                (Err(r), Some(best)) if r.code == "too_late" => {
                    let by = c.reaction_ms - best;
                    Err(Reject::new(
                        "too_late",
                        if by > 0 {
                            format!("too late: a match {by}ms faster got there first")
                        } else {
                            "too late: an equally fast match arrived first".to_string()
                        },
                    ))
                }
                (result, _) => result,
            };
            if result.is_ok() && self.discard_seq != seq && beaten_by.is_none() {
                beaten_by = Some(c.reaction_ms);
            }
            self.record(Settled::new(c.id, result));
        }
        out.changed = true;
    }

    /// Settle the pending claims once their window has closed.
    pub fn settle(&mut self, now: i64, rng: &mut impl rand::Rng) -> Outcome {
        let out = self.settle_due(now, rng);
        self.observe(&out.events, rng);
        out
    }

    fn settle_due(&mut self, now: i64, rng: &mut impl rand::Rng) -> Outcome {
        let mut out = Outcome::default();
        if self.claim_deadline.is_some_and(|d| now >= d) {
            self.settle_claims(now, rng, &mut out);
        }
        out
    }

    /// Reject pending claims that can no longer apply, as when the game ends.
    fn drop_claims(&mut self, keep: impl Fn(&Claim) -> bool, why: Reject) {
        let (kept, dropped): (Vec<Claim>, Vec<Claim>) =
            std::mem::take(&mut self.claims).into_iter().partition(keep);
        self.claims = kept;
        if self.claims.is_empty() {
            self.claim_deadline = None;
        }
        for c in dropped {
            self.record(Settled::new(c.id, Err(why.clone())));
        }
    }

    fn record(&mut self, settled: Settled) {
        self.settled.push(settled);
        let extra = self.settled.len().saturating_sub(SETTLED_KEPT);
        self.settled.drain(..extra);
    }

    /// How the claim `id` settled; `None` while it is pending or once it is
    /// too old to be kept.
    pub fn claim_result(&self, id: &str) -> Option<Result<(), Reject>> {
        self.settled
            .iter()
            .find(|s| s.id == id)
            .map(Settled::result)
    }

    fn exchange(&mut self, a: (usize, usize, i8), b: (usize, usize, i8)) {
        self.seats[a.0].slots[a.1] = Some(b.2);
        self.seats[b.0].slots[b.1] = Some(a.2);
        self.touch(a.0, a.1);
        self.touch(b.0, b.1);
    }

    /// Whether the player at `me` may match `seat`'s `slot` against discard
    /// `seq` now. Returns the card and the slot `me` gives in its place, if
    /// chosen up front.
    fn check_match(
        &self,
        me: usize,
        seq: u64,
        seat: usize,
        slot: usize,
        give_slot: Option<usize>,
    ) -> Result<(i8, Option<usize>), Reject> {
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
        if seat != me {
            if self.owed.iter().any(|o| o.player == self.seats[me].player) {
                return Err(Reject::invalid("give a card for your last match first"));
            }
            match give_slot {
                Some(give) => {
                    self.require_filled(me, give)?;
                }
                None if self.seats[me].slots.iter().all(Option::is_none) => {
                    return Err(Reject::invalid("you have no card to give"));
                }
                None => {}
            }
        }
        Ok((card, give_slot.filter(|_| seat != me)))
    }

    fn do_match(
        &mut self,
        me: usize,
        c: &Claim,
        now: i64,
        rng: &mut impl rand::Rng,
        out: &mut Outcome,
    ) -> Result<(), Reject> {
        let (player, seat, slot) = (c.player.as_str(), c.seat, c.slot);
        let (card, give) = self.check_match(me, c.seq, seat, slot, c.give_slot)?;
        out.stat(player, Stat::CardsInteracted, 1);
        let top = *self.discard.last().expect("matchable implies a discard");
        if card == top {
            self.seats[seat].slots[slot] = None;
            self.touch(seat, slot);
            self.push_discard(card, now);
            if let Some(give) = give {
                let given = self.seats[me].slots[give].take();
                self.touch(me, give);
                self.seats[seat].slots[slot] = given;
                out.stat(player, Stat::CardsInteracted, 1);
            }
            // the matcher picks the card to give once the match is in
            let owes = seat != me && give.is_none();
            if owes {
                self.owed.push(Owed {
                    player: player.to_string(),
                    seat,
                    slot,
                    deadline: now + GIVE_MS,
                });
            }
            self.seats[me].tally.matches += 1;
            out.stat(player, Stat::Matches, 1);
            out.event(
                Some(player),
                "match",
                json!({ "ok": true, "seat": seat, "slot": slot, "value": card, "give_slot": give,
                        "owes": owes, "reaction_ms": c.reaction_ms }),
            );
        } else {
            let penalty = self.draw_card(rng);
            if let Some(card) = penalty {
                self.seats[me].slots.push(Some(card));
                self.seats[me].tally.penalties += 1;
            }
            self.seats[me].tally.misses += 1;
            out.stat(player, Stat::FailedMatches, 1);
            let mut payload = json!({ "ok": false, "seat": seat, "slot": slot,
                                      "penalty": penalty.is_some(), "reaction_ms": c.reaction_ms });
            if self.settings.show_misses {
                payload["value"] = json!(card);
            }
            out.event(Some(player), "match", payload);
        }
        Ok(())
    }

    /// Remove a player from the game. Their cards leave play.
    pub fn forfeit(&mut self, player: &str, now: i64) -> Outcome {
        let before = self.turn_marker();
        let out = self.forfeit_inner(player, now);
        self.bump_turn_seq(before);
        // a forfeit teaches no card values, so no chance is drawn
        self.observe(&out.events, &mut rand::rng());
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
        self.drop_claims(
            |c| c.player != player,
            Reject::new("not_seated", "you left the game"),
        );
        self.reveals
            .retain(|r| r.seat != seat && r.player != player);
        // their cards left play, so they neither give nor receive one
        self.owed.retain(|o| o.player != player && o.seat != seat);
        self.final_remaining.retain(|&s| s != seat);
        out.stat(player, Stat::Forfeits, 1);
        out.event(Some(player), "forfeit", json!({ "seat": seat }));
        if self.active_count() < 2 {
            self.finish(&mut out);
            return out;
        }
        match self.status {
            Status::Peeking => {
                if self.turn == seat {
                    self.next_turn();
                }
                if self.seats.iter().all(|s| s.ready || s.forfeited) {
                    self.start_play(now, &mut out);
                }
            }
            Status::Playing | Status::Final if self.turn == seat => {
                self.stage = Stage::Start;
                self.away_since = None;
                self.calling = false;
                if self.caller.is_some() && self.final_remaining.is_empty() {
                    self.start_scoring(now);
                } else {
                    self.next_turn();
                    self.begin_turn(now);
                }
            }
            Status::Final if self.final_remaining.is_empty() => self.start_scoring(now),
            _ => {}
        }
        out
    }

    /// Advance timers: match claims, the ready deadline, away turns, turn
    /// limits, and scoring.
    pub fn tick(
        &mut self,
        now: i64,
        present: &dyn Fn(&str) -> bool,
        rng: &mut impl rand::Rng,
    ) -> Outcome {
        let before = self.turn_marker();
        let mut out = self.tick_inner(now, present, rng);
        self.bump_turn_seq(before);
        self.observe(&out.events, rng);
        // bots act on the table the timers left (BOT-4)
        self.drive_bots(now, rng, &mut out);
        out
    }

    fn tick_inner(
        &mut self,
        now: i64,
        present: &dyn Fn(&str) -> bool,
        rng: &mut impl rand::Rng,
    ) -> Outcome {
        let mut out = self.settle_due(now, rng);
        self.settle_owed(Some(now), &mut out);
        match self.status {
            Status::Peeking => {
                if self.ready_deadline.is_some_and(|d| now >= d) {
                    self.start_play(now, &mut out);
                }
            }
            Status::Playing | Status::Final => {
                let player = self.seats[self.turn].player.clone();
                if self.turn_deadline.is_some_and(|d| now >= d) {
                    self.settle_claims(now, rng, &mut out);
                    self.skip_turn(&player, "timeout_skip", now, &mut out);
                } else if present(&player) {
                    if self.away_since.take().is_some() {
                        out.changed = true;
                    }
                } else {
                    match self.away_since {
                        None => {
                            self.away_since = Some(now);
                            out.changed = true;
                        }
                        Some(since) if now - since >= self.settings.away_grace_ms() => {
                            self.settle_claims(now, rng, &mut out);
                            self.skip_turn(&player, "away_skip", now, &mut out);
                        }
                        Some(_) => {}
                    }
                }
            }
            Status::Scoring => {
                if self.score_at.is_some_and(|d| now >= d) {
                    self.settle_claims(now, rng, &mut out);
                    self.finish(&mut out);
                }
            }
            Status::Scored => {}
        }
        out
    }

    /// End the turn player's turn for them: a held card goes onto the discard
    /// pile and a pending special move is lost.
    fn skip_turn(&mut self, player: &str, kind: &'static str, now: i64, out: &mut Outcome) {
        if let Stage::Drawn { card } | Stage::Taken { card } = self.stage {
            self.push_discard(card, now);
        }
        out.event(Some(player), kind, json!({}));
        self.end_turn(now, out);
    }

    /// Reveal and score every hand.
    fn finish(&mut self, out: &mut Outcome) {
        // only a forfeit ends a game with claims still pending
        self.drop_claims(|_| false, Reject::new("too_late", "the game is over"));
        // every owed card lands before the hands are scored
        self.settle_owed(None, out);
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
        // a caller who did not win pays the room's penalty (SET-15)
        if let Some(c) = caller.filter(|c| !winners.contains(c)) {
            if let Some(score) = self.seats[c].score.as_mut() {
                *score += self.settings.caller_penalty as i32;
            }
        }
        let match_winners = self.total_up();
        self.status = Status::Scored;
        self.stage = Stage::Start;
        self.calling = false;
        self.score_at = None;
        self.turn_deadline = None;
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
        let names = |seats: &[usize]| -> Vec<&str> {
            seats
                .iter()
                .map(|&w| self.seats[w].player.as_str())
                .collect()
        };
        out.event(
            None,
            "scored",
            json!({ "winners": names(&winners), "match_over": self.match_over,
                    "match_winners": names(&match_winners) }),
        );
    }

    /// Add each seat's score to its running total and decide whether the
    /// match is over: some total at or past the target (SET-14), after a
    /// total landing exactly on it is halved (SET-16). Returns the match
    /// winners, the lowest totals, when it is.
    fn total_up(&mut self) -> Vec<usize> {
        let target = self.settings.target_score.map(|t| t as i32);
        let exact_reset = self.settings.exact_reset;
        for seat in self.seats.iter_mut() {
            let mut total = seat.carry + seat.score.unwrap_or(0);
            if exact_reset && Some(total) == target {
                total /= 2;
            }
            seat.total = Some(total);
        }
        let playing: Vec<usize> = (0..self.seats.len()).filter(|&s| self.active(s)).collect();
        let total = |s: usize| self.seats[s].total.unwrap_or(i32::MAX);
        self.match_over = target.is_some_and(|t| playing.iter().any(|&s| total(s) >= t));
        if !self.match_over {
            return vec![];
        }
        let best = playing.iter().map(|&s| total(s)).min();
        let winners: Vec<usize> = playing
            .into_iter()
            .filter(|&s| Some(total(s)) == best)
            .collect();
        for &w in &winners {
            self.seats[w].match_won = true;
        }
        winners
    }

    /// The winners of a finished game.
    pub fn winners(&self) -> Vec<&str> {
        self.seats
            .iter()
            .filter(|s| s.won)
            .map(|s| s.player.as_str())
            .collect()
    }

    /// A private value `player` may see right now, for a sealed reveal
    /// (SEAL-6). Views never carry these.
    pub fn secret(&self, player: &str, what: &Secret, now: i64) -> Result<Value, Reject> {
        let denied = || Reject::new("forbidden", "that card is not yours to see now");
        let seat = self.seat_of(player).ok_or_else(denied)?;
        match what {
            Secret::Opening => {
                if self.status != Status::Peeking || self.seats[seat].ready {
                    return Err(denied());
                }
                let cards: Vec<Value> = self
                    .settings
                    .opening_slots()
                    .filter_map(|slot| {
                        let v = self.card_at(seat, slot)?;
                        Some(json!({ "seat": seat, "slot": slot, "v": v }))
                    })
                    .collect();
                Ok(json!({ "cards": cards }))
            }
            Secret::Drawn => match self.stage {
                Stage::Drawn { card } if self.status.in_play() && self.turn == seat => {
                    Ok(json!({ "card": card }))
                }
                _ => Err(denied()),
            },
            Secret::Peek(id) => {
                let r = self
                    .reveals
                    .iter()
                    .find(|r| !r.id.is_empty() && &r.id == id && r.player == player)
                    .ok_or_else(denied)?;
                if !r.live(now) {
                    return Err(Reject::new("too_late", "that peek is over"));
                }
                Ok(json!({ "cards": [{ "seat": r.seat, "slot": r.slot, "v": r.value }] }))
            }
        }
    }

    /// The game as `viewer` may see it: only public values are included.
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
                    .map(|card| match card {
                        None => Value::Null,
                        // private values only ever travel sealed (SEAL-1)
                        Some(v) if self.status == Status::Scored => json!({ "v": v }),
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
                    "carry": s.carry,
                    "total": s.total,
                    "match_won": s.match_won,
                    "tally": s.tally,
                    "bot": s.bot,
                })
            })
            .collect();
        let is_turn = me == Some(self.turn) && self.status.in_play();
        let stage = match &self.stage {
            // the drawer reveals the drawn card sealed
            Stage::Drawn { .. } => json!({ "kind": "drawn", "card": null }),
            other => serde_json::to_value(other).unwrap_or(Value::Null),
        };
        // the viewer's own live peeks, by id and without the value
        let reveals: Vec<Value> = self
            .reveals
            .iter()
            .filter(|r| Some(r.player.as_str()) == viewer && r.live(now))
            .map(|r| json!({ "id": r.id, "seat": r.seat, "slot": r.slot, "until": r.until }))
            .collect();
        // which slots someone is looking at is public; the values are not
        let peeked: Vec<Value> = self
            .reveals
            .iter()
            .filter(|r| r.live(now))
            .map(|r| json!({ "seat": r.seat, "slot": r.slot, "until": r.until }))
            .collect();
        // slots waiting for a matcher's card, and who owes it
        let owed: Vec<Value> = self
            .owed
            .iter()
            .map(|o| {
                json!({ "from": self.seat_of(&o.player), "seat": o.seat, "slot": o.slot, "deadline": o.deadline })
            })
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
            // every discard is face up, so the newest few are public (RULE-28)
            "discard_recent": self.discard.iter().rev().take(DISCARD_RECENT).collect::<Vec<_>>(),
            "match_over": self.match_over,
            "discard_seq": self.discard_seq,
            "matchable": self.matchable,
            "deck_count": self.deck.len(),
            "reveals": reveals,
            "peeked": peeked,
            "owed": owed,
            "can_call": is_turn && self.may_call(self.turn).is_ok(),
            // a mid turn call stays the caller's until the turn ends
            "calling": is_turn && self.calling,
            "ready_deadline": self.ready_deadline,
            "away_deadline": self.away_since.map(|t| t + self.settings.away_grace_ms()),
            "turn_deadline": self.turn_deadline,
            "score_at": self.score_at,
            "hand_size": self.settings.hand_size,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    const PEEK_MS: i64 = 5_000;
    const GRACE_MS: i64 = 30_000;

    fn rng() -> rand::rngs::StdRng {
        rand::rngs::StdRng::seed_from_u64(7)
    }

    /// A new game with 4 card hands and 5 second peeks.
    fn deal(players: Vec<String>, deck: Vec<i8>) -> Game {
        Game::new(players, deck, 0, 0, Settings::legacy())
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
        let mut g = deal(players(hands.len()), stacked(hands, starter, draws));
        for p in players(hands.len()) {
            g.apply(&p, Action::Ready, None, 0, &mut rng()).unwrap();
        }
        assert_eq!(g.status, Status::Playing);
        g
    }

    /// Act with the current turn token, as a client looking at the latest
    /// view would. A match settles at once, as when nobody else claims in
    /// its window.
    fn act(g: &mut Game, p: &str, a: Action) -> Outcome {
        try_act(g, p, a).unwrap()
    }

    fn reject(g: &mut Game, p: &str, a: Action) -> &'static str {
        try_act(g, p, a).unwrap_err().code
    }

    fn try_act(g: &mut Game, p: &str, a: Action) -> Result<Outcome, Reject> {
        let seq = g.turn_seq;
        let mut out = g.apply(p, a, Some(seq), 0, &mut rng())?;
        if let Some((id, deadline)) = out.claim.take() {
            let settled = g.settle(deadline, &mut rng());
            out.events.extend(settled.events);
            out.stats.extend(settled.stats);
            g.claim_result(&id).expect("the claim settled")?;
        }
        Ok(out)
    }

    /// Claim a match timed at `reaction_ms`, arriving at `now`.
    fn claim(
        g: &mut Game,
        p: &str,
        reaction_ms: i64,
        now: i64,
        seat: usize,
        slot: usize,
    ) -> String {
        let a = Action::Match {
            seq: g.discard_seq,
            seat,
            slot,
            give_slot: None,
        };
        let out = g
            .apply_timed(p, a, None, Some(reaction_ms), now, &mut rng())
            .unwrap();
        out.claim.expect("a match is claimed").0
    }

    fn present(_: &str) -> bool {
        true
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
        g.tick(0, &|p| p != "p1", &mut rng());
        g.tick(GRACE_MS, &|p| p != "p1", &mut rng());
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
        let g = deal(players(2), stacked(&[[1, 2, 3, 4], [5, 6, 0, -1]], 9, &[]));
        assert_eq!(g.seats[0].slots, vec![Some(1), Some(2), Some(3), Some(4)]);
        assert_eq!(g.seats[1].slots, vec![Some(5), Some(6), Some(0), Some(-1)]);
        assert_eq!(g.discard, vec![9]);
        assert!(!g.matchable);
        assert_eq!(g.deck.len(), 0);
    }

    #[test]
    fn opening_peek_shows_only_near_row_until_ready() {
        let mut g = deal(players(2), stacked(&[[1, 2, 3, 4], [5, 6, 0, -1]], 9, &[]));
        // the view carries no values; the near row is revealed sealed
        assert!(!g.view("p0", 0).to_string().contains("\"v\""));
        assert_eq!(
            g.secret("p0", &Secret::Opening, 0).unwrap(),
            json!({ "cards": [{ "seat": 0, "slot": 0, "v": 1 }, { "seat": 0, "slot": 1, "v": 2 }] })
        );
        // each player only ever gets their own
        assert_eq!(
            g.secret("p1", &Secret::Opening, 0).unwrap()["cards"][0],
            json!({ "seat": 1, "slot": 0, "v": 5 })
        );
        assert_eq!(
            g.secret("p9", &Secret::Opening, 0).unwrap_err().code,
            "forbidden"
        );
        act(&mut g, "p0", Action::Ready);
        assert_eq!(
            g.secret("p0", &Secret::Opening, 0).unwrap_err().code,
            "forbidden"
        );
    }

    #[test]
    fn ready_deadline_starts_play() {
        let mut g = deal(players(2), stacked(&[[1; 4], [2; 4]], 9, &[]));
        assert!(!g.tick(READY_MS - 1, &|_| true, &mut rng()).changed);
        assert!(g.tick(READY_MS, &|_| true, &mut rng()).changed);
        assert_eq!(g.status, Status::Playing);
    }

    #[test]
    fn draw_then_swap_discards_the_replaced_card() {
        let mut g = playing(&[[1, 2, 3, 4], [5, 5, 5, 5]], 9, &[6]);
        assert_eq!(reject(&mut g, "p1", Action::Draw), "not_your_turn");
        assert_eq!(
            g.secret("p0", &Secret::Drawn, 0).unwrap_err().code,
            "forbidden"
        );
        act(&mut g, "p0", Action::Draw);
        // nobody's view carries the drawn card, the drawer's included
        assert!(g.view("p0", 0)["stage"]["card"].is_null());
        assert!(g.view("p1", 0)["stage"]["card"].is_null());
        assert_eq!(
            g.secret("p0", &Secret::Drawn, 0).unwrap(),
            json!({ "card": 6 })
        );
        assert_eq!(
            g.secret("p1", &Secret::Drawn, 0).unwrap_err().code,
            "forbidden"
        );
        act(&mut g, "p0", Action::Swap { slot: 1 });
        assert!(g.secret("p0", &Secret::Drawn, 0).is_err());
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
        assert_eq!(g.stage, Stage::Earned { mv: Move::PeekOwn });
        act(&mut g, "p0", Action::UseSpecial);
        assert_eq!(g.stage, Stage::Special { mv: Move::PeekOwn });
        assert_eq!(
            reject(&mut g, "p0", Action::Peek { seat: 1, slot: 0 }),
            "invalid"
        );
        let out = act(&mut g, "p0", Action::Peek { seat: 0, slot: 0 });
        assert!(out.stats.contains(&("p0".into(), Stat::SpecialMoves, 1)));
        let view = g.view("p0", 1);
        assert!(!view.to_string().contains("\"v\""));
        let id = view["reveals"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(id.len(), 32);
        assert_eq!(view["reveals"][0]["slot"], 0);
        let peek = Secret::Peek(id);
        assert_eq!(
            g.secret("p0", &peek, 1).unwrap(),
            json!({ "cards": [{ "seat": 0, "slot": 0, "v": 1 }] })
        );
        // the other player can't use the id, and nobody can after the deadline
        assert_eq!(g.secret("p1", &peek, 1).unwrap_err().code, "forbidden");
        assert!(g.view("p1", 1)["reveals"].as_array().unwrap().is_empty());
        assert_eq!(
            g.secret("p0", &peek, PEEK_MS + 1).unwrap_err().code,
            "too_late"
        );
        assert_eq!(
            g.secret("p0", &Secret::Peek("nope".into()), 1)
                .unwrap_err()
                .code,
            "forbidden"
        );
        assert_eq!(g.turn, 1);
    }

    #[test]
    fn peeks_saved_without_an_id_cannot_be_revealed() {
        let mut g = playing(&[[1, 2, 3, 4], [5; 4]], 0, &[]);
        g.reveals.push(Reveal {
            id: String::new(),
            player: "p0".into(),
            seat: 0,
            slot: 0,
            value: 1,
            until: Some(10),
        });
        let stored = serde_json::to_value(&g).unwrap();
        let mut old = stored.clone();
        old["reveals"][0].as_object_mut().unwrap().remove("id");
        let loaded: Game = serde_json::from_value(old).unwrap();
        assert_eq!(loaded.reveals[0].id, "");
        assert!(loaded
            .secret("p0", &Secret::Peek(String::new()), 1)
            .is_err());
    }

    #[test]
    fn peek_other_targets_another_player() {
        let mut g = playing(&[[1, 2, 3, 4], [5, 6, 7, 8]], 0, &[9]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        act(&mut g, "p0", Action::UseSpecial);
        assert_eq!(
            reject(&mut g, "p0", Action::Peek { seat: 0, slot: 0 }),
            "invalid"
        );
        act(&mut g, "p0", Action::Peek { seat: 1, slot: 1 });
        let id = g.reveals[0].id.clone();
        assert_eq!(
            g.secret("p0", &Secret::Peek(id), 1).unwrap()["cards"][0],
            json!({ "seat": 1, "slot": 1, "v": 6 })
        );
    }

    #[test]
    fn blind_swap_exchanges_without_revealing() {
        let mut g = playing(&[[1, 2, 3, 4], [5, 6, 7, 8]], 0, &[11]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        act(&mut g, "p0", Action::UseSpecial);
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
        act(&mut g, "p0", Action::UseSpecial);
        act(&mut g, "p0", Action::Peek { seat: 1, slot: 0 });
        assert_eq!(g.stage, Stage::Looked { seat: 1, slot: 0 });
        act(&mut g, "p0", Action::LookSwap { slot: Some(2) });
        assert_eq!(g.seats[0].slots[2], Some(5));
        assert_eq!(g.seats[1].slots[0], Some(3));
        // the reveal does not follow a different card into the looked slot
        assert!(g.view("p0", 1)["seats"][1]["slots"][0].get("v").is_none());

        act(&mut g, "p1", Action::Draw);
        act(&mut g, "p1", Action::Discard);
        act(&mut g, "p1", Action::UseSpecial);
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
    fn a_discarded_special_card_waits_for_its_player_to_use_it() {
        let mut g = playing(&[[9, 2, 3, 4], [5, 6, 7, 9]], 0, &[9, 1]);
        assert_eq!(reject(&mut g, "p0", Action::UseSpecial), "invalid");
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        assert_eq!(
            g.view("p1", 0)["stage"],
            json!({ "kind": "earned", "mv": "peek_other" })
        );
        // it is still p0's turn, and the discard is open to matches
        assert_eq!(g.turn, 0);
        assert_eq!(
            reject(&mut g, "p0", Action::Peek { seat: 1, slot: 0 }),
            "invalid"
        );
        let seq = g.discard_seq;
        let mine = Action::Match {
            seq,
            seat: 0,
            slot: 0,
            give_slot: None,
        };
        act(&mut g, "p0", mine);
        assert_eq!(g.seats[0].slots[0], None);
        // the matched 9 tops the pile and is matchable in turn
        let seq = g.discard_seq;
        let theirs = Action::Match {
            seq,
            seat: 1,
            slot: 3,
            give_slot: None,
        };
        act(&mut g, "p1", theirs);
        assert_eq!(g.seats[1].slots[3], None);
        // matching moved nothing in the turn, so the move is still p0's
        assert_eq!(
            g.stage,
            Stage::Earned {
                mv: Move::PeekOther
            }
        );
        let out = act(&mut g, "p0", Action::UseSpecial);
        assert!(out.events.is_empty() && out.changed);
        assert_eq!(reject(&mut g, "p0", Action::UseSpecial), "invalid");
        act(&mut g, "p0", Action::Peek { seat: 1, slot: 1 });
        assert_eq!(g.turn, 1);
    }

    #[test]
    fn using_a_special_move_moves_the_turn_token() {
        let mut g = playing(&[[1; 4], [2; 4]], 0, &[7]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        let seq = g.turn_seq;
        act(&mut g, "p0", Action::UseSpecial);
        assert_eq!(g.turn_seq, seq + 1);
        let late = g.apply("p0", Action::Skip, Some(seq), 0, &mut rng());
        assert_eq!(late.unwrap_err().code, "stale");
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
        // a give card chosen up front lands with the match
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
        assert!(g.owed.is_empty());
    }

    /// When the card `owing` leaves owed is given for p0: its match settled
    /// when its window closed.
    const OWED_DUE: i64 = MATCH_WINDOW_MS + GIVE_MS;

    /// p0 has matched p1's 5 in slot 0 without choosing a card to give.
    fn owing() -> Game {
        let mut g = playing(&[[1, 2, 3, 5], [5, 6, 7, 8]], 0, &[9, 6]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Swap { slot: 3 }); // discards 5
        let seq = g.discard_seq;
        let out = act(
            &mut g,
            "p0",
            Action::Match {
                seq,
                seat: 1,
                slot: 0,
                give_slot: None,
            },
        );
        let hit = out.events.iter().find(|e| e.kind == "match").unwrap();
        assert_eq!(hit.payload["owes"], json!(true));
        g
    }

    #[test]
    fn a_match_on_another_players_card_is_given_for_after_it_lands() {
        let mut g = owing();
        assert_eq!(g.seats[1].slots[0], None);
        assert_eq!(g.discard.last(), Some(&5));
        assert_eq!(
            g.owed,
            vec![Owed {
                player: "p0".into(),
                seat: 1,
                slot: 0,
                deadline: OWED_DUE
            }]
        );
        let view = g.view("p1", 0);
        assert_eq!(
            view["owed"],
            json!([{ "from": 0, "seat": 1, "slot": 0, "deadline": OWED_DUE }])
        );
        // only the matcher gives, and only a card they hold
        assert_eq!(reject(&mut g, "p1", Action::Give { slot: 1 }), "invalid");
        g.seats[0].slots[2] = None;
        assert_eq!(reject(&mut g, "p0", Action::Give { slot: 2 }), "invalid");
        let out = act(&mut g, "p0", Action::Give { slot: 1 });
        assert_eq!(g.seats[0].slots[1], None);
        assert_eq!(g.seats[1].slots[0], Some(2));
        assert!(g.owed.is_empty());
        assert_eq!(out.events[0].kind, "give");
        assert_eq!(
            out.events[0].payload,
            json!({ "seat": 0, "slot": 1, "target_seat": 1, "target_slot": 0 })
        );
        assert_eq!(reject(&mut g, "p0", Action::Give { slot: 0 }), "invalid");
    }

    #[test]
    fn an_owed_card_blocks_matching_other_hands_until_given() {
        let mut g = owing();
        // p1 discards the 6 they draw, which p0 then tries for
        act(&mut g, "p1", Action::Draw);
        act(&mut g, "p1", Action::Discard);
        let other = Action::Match {
            seq: g.discard_seq,
            seat: 1,
            slot: 1,
            give_slot: None,
        };
        assert_eq!(reject(&mut g, "p0", other.clone()), "invalid");
        act(&mut g, "p0", Action::Give { slot: 0 });
        act(&mut g, "p0", other);
        assert_eq!(g.seats[1].slots[1], None);
    }

    #[test]
    fn an_owed_card_is_given_from_the_highest_value_at_its_deadline() {
        let mut g = owing();
        // p0 holds 1, 2, 3, 9; a tied 9 in a later slot loses to slot 4
        g.seats[0].slots.push(Some(9));
        assert!(!g
            .tick(OWED_DUE - 1, &present, &mut rng())
            .events
            .iter()
            .any(|e| e.kind == "give"));
        let out = g.tick(OWED_DUE, &present, &mut rng());
        assert!(out.events.iter().any(|e| e.kind == "give"));
        assert_eq!(g.seats[1].slots[0], Some(9));
        assert_eq!(g.seats[0].slots[3], None);
        assert_eq!(g.seats[0].slots[4], Some(9));
        assert!(g.owed.is_empty());
    }

    #[test]
    fn owed_cards_land_before_scoring_and_leave_with_a_forfeit() {
        let mut g = owing();
        let mut out = Outcome::default();
        g.finish(&mut out);
        assert_eq!(g.seats[1].slots[0], Some(9));
        assert_eq!(g.seats[1].score, Some(9 + 6 + 7 + 8));

        let mut g = owing();
        g.forfeit("p1", 0);
        assert!(g.owed.is_empty());
        let mut g = owing();
        g.seats[0].slots = vec![None; 4];
        g.tick(OWED_DUE, &present, &mut rng());
        // nothing left to give: the slot stays empty
        assert!(g.owed.is_empty());
        assert_eq!(g.seats[1].slots[0], None);
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

    /// p0 has discarded a 5; both players hold a 5 in slot 0.
    fn fives() -> Game {
        let mut g = playing(&[[5, 2, 3, 4], [5, 6, 7, 8]], 0, &[5, 9]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        g
    }

    #[test]
    fn the_fastest_reaction_wins_not_the_first_arrival() {
        let mut g = fives();
        let near = claim(&mut g, "p0", 300, 10, 0, 0);
        let far = claim(&mut g, "p1", 200, 100, 1, 0);
        // the window opens with the first claim and later ones join it
        assert_eq!(g.claim_deadline, Some(10 + MATCH_WINDOW_MS));
        let early = g.settle(10 + MATCH_WINDOW_MS - 1, &mut rng());
        assert!(!early.changed);
        assert_eq!(g.claim_result(&far), None);
        assert_eq!(g.seats[1].slots[0], Some(5));

        let out = g.settle(10 + MATCH_WINDOW_MS, &mut rng());
        assert_eq!(g.claim_result(&far), Some(Ok(())));
        assert_eq!(g.claim_result(&near).unwrap().unwrap_err().code, "too_late");
        assert_eq!(g.seats[1].slots[0], None);
        assert_eq!(g.seats[0].slots[0], Some(5));
        assert!(g.claims.is_empty());
        assert_eq!(g.claim_deadline, None);
        let matches: Vec<_> = out.events.iter().filter(|e| e.kind == "match").collect();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].player.as_deref(), Some("p1"));
        assert!(out.stats.contains(&("p1".into(), Stat::Matches, 1)));
    }

    #[test]
    fn equal_reactions_go_to_the_first_arrival() {
        let mut g = fives();
        let first = claim(&mut g, "p1", 200, 20, 1, 0);
        let second = claim(&mut g, "p0", 200, 30, 0, 0);
        g.settle(i64::MAX, &mut rng());
        assert_eq!(g.claim_result(&first), Some(Ok(())));
        assert_eq!(
            g.claim_result(&second).unwrap().unwrap_err().code,
            "too_late"
        );
    }

    #[test]
    fn a_wrong_fastest_claim_pays_and_leaves_the_discard_open() {
        let mut g = fives();
        // p0 guesses their 2 is a 5
        let wrong = claim(&mut g, "p0", 100, 10, 0, 1);
        let right = claim(&mut g, "p1", 250, 20, 1, 0);
        let out = g.settle(i64::MAX, &mut rng());
        assert_eq!(g.claim_result(&wrong), Some(Ok(())));
        assert_eq!(g.claim_result(&right), Some(Ok(())));
        assert!(out.stats.contains(&("p0".into(), Stat::FailedMatches, 1)));
        assert_eq!(g.seats[0].slots.len(), 5);
        assert_eq!(g.seats[0].slots[4], Some(9));
        assert_eq!(g.seats[1].slots[0], None);
    }

    #[test]
    fn a_claim_is_checked_when_made_and_once_per_player() {
        let mut g = fives();
        let stale = Action::Match {
            seq: g.discard_seq - 1,
            seat: 0,
            slot: 0,
            give_slot: None,
        };
        assert_eq!(reject(&mut g, "p0", stale), "too_late");
        let empty = Action::Match {
            seq: g.discard_seq,
            seat: 1,
            slot: 0,
            give_slot: Some(9),
        };
        assert_eq!(reject(&mut g, "p0", empty), "invalid");
        assert!(g.claims.is_empty());

        claim(&mut g, "p0", 100, 10, 0, 0);
        let again = Action::Match {
            seq: g.discard_seq,
            seat: 0,
            slot: 1,
            give_slot: None,
        };
        let err = g.apply("p0", again, None, 20, &mut rng()).unwrap_err();
        assert_eq!(err.code, "invalid");
        assert_eq!(g.claims.len(), 1);
    }

    #[test]
    fn an_untimed_claim_counts_from_when_the_discard_landed() {
        let mut g = playing(&[[5, 2, 3, 4], [5, 6, 7, 8]], 0, &[5]);
        let seq = g.turn_seq;
        g.apply("p0", Action::Draw, Some(seq), 900, &mut rng())
            .unwrap();
        let seq = g.turn_seq;
        g.apply("p0", Action::Discard, Some(seq), 1_000, &mut rng())
            .unwrap();
        assert_eq!(g.discard_at, Some(1_000));
        let m = Action::Match {
            seq: g.discard_seq,
            seat: 1,
            slot: 0,
            give_slot: None,
        };
        g.apply("p1", m, None, 1_400, &mut rng()).unwrap();
        assert_eq!(g.claims[0].reaction_ms, 400);
    }

    #[test]
    fn a_take_settles_pending_claims_first_and_a_draw_does_not() {
        let mut g = fives();
        let id = claim(&mut g, "p0", 100, 10, 0, 0);
        // p1 is to move; drawing leaves the discard alone
        let seq = g.turn_seq;
        g.apply("p1", Action::Draw, Some(seq), 20, &mut rng())
            .unwrap();
        assert_eq!(g.claim_result(&id), None);
        assert_eq!(g.claims.len(), 1);

        let mut g = fives();
        let id = claim(&mut g, "p0", 100, 10, 0, 0);
        let seq = g.turn_seq;
        let out = g
            .apply("p1", Action::Take, Some(seq), 20, &mut rng())
            .unwrap();
        assert_eq!(g.claim_result(&id), Some(Ok(())));
        assert_eq!(g.seats[0].slots[0], None);
        // the matched card took the discard's place, so the take gets a 5
        assert_eq!(g.stage, Stage::Taken { card: 5 });
        let kinds: Vec<_> = out.events.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, ["match", "take"]);
    }

    #[test]
    fn forfeits_and_the_end_of_the_game_reject_pending_claims() {
        let mut g = playing(&[[5, 2, 3, 4], [5, 6, 7, 8], [5, 1, 1, 1]], 0, &[5]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        let gone = claim(&mut g, "p1", 100, 10, 1, 0);
        let stays = claim(&mut g, "p2", 200, 20, 2, 0);
        g.forfeit("p1", 30);
        assert_eq!(
            g.claim_result(&gone).unwrap().unwrap_err().code,
            "not_seated"
        );
        assert_eq!(g.claim_result(&stays), None);
        // p0 leaving too ends the game before p2's claim settles
        g.forfeit("p0", 40);
        assert_eq!(g.status, Status::Scored);
        assert_eq!(
            g.claim_result(&stays).unwrap().unwrap_err().code,
            "too_late"
        );
        assert!(g.claims.is_empty());
    }

    #[test]
    fn scoring_settles_pending_claims_before_revealing() {
        let mut g = fives();
        let id = claim(&mut g, "p1", 100, 10, 1, 0);
        g.status = Status::Scoring;
        g.score_at = Some(20);
        g.tick(20, &present, &mut rng());
        assert_eq!(g.status, Status::Scored);
        assert_eq!(g.claim_result(&id), Some(Ok(())));
        assert_eq!(g.seats[1].score, Some(6 + 7 + 8));
    }

    #[test]
    fn settled_results_are_kept_for_a_while() {
        let mut g = fives();
        let first = claim(&mut g, "p0", 100, 10, 0, 1); // wrong, so the 5 stays up
        g.settle(i64::MAX, &mut rng());
        for _ in 0..SETTLED_KEPT {
            g.record(Settled::new("x".into(), Ok(())));
        }
        assert_eq!(g.settled.len(), SETTLED_KEPT);
        assert_eq!(g.claim_result(&first), None);
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
        act(&mut g, "p1", Action::UseSpecial);
        assert_eq!(
            reject(&mut g, "p1", Action::Peek { seat: 0, slot: 0 }),
            "invalid"
        );
        act(&mut g, "p1", Action::Skip);
        act(&mut g, "p2", Action::Draw);
        act(&mut g, "p2", Action::Discard);
        act(&mut g, "p2", Action::Skip);
        assert_eq!(g.status, Status::Scoring);
        let out = g.tick(SCORE_DELAY_MS, &|_| true, &mut rng());
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
        g.tick(SCORE_DELAY_MS, &|_| true, &mut rng());
        assert_eq!(g.winners(), vec!["p1"]);
    }

    #[test]
    fn a_caller_who_does_not_win_pays_the_room_penalty() {
        let mut g = playing(&[[1; 4], [1; 4], [2; 4]], 5, &[9, 9, 9, 9, 9]);
        g.settings.caller_penalty = 10;
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
        g.tick(SCORE_DELAY_MS, &|_| true, &mut rng());
        assert_eq!(g.winners(), vec!["p1"]);
        let scores: Vec<_> = g.seats.iter().map(|s| s.score).collect();
        assert_eq!(scores, [Some(14), Some(4), Some(8)]);
    }

    /// Score `hands` straight away, carrying `carry` in, under `settings`.
    fn score_now(hands: &[[i8; 4]], carry: &[i32], settings: Settings) -> (Game, Outcome) {
        let mut g = playing(hands, 0, &[]);
        g.settings = settings;
        g.carry_in(3, |p| carry[p[1..].parse::<usize>().unwrap()]);
        g.status = Status::Scoring;
        g.score_at = Some(0);
        let out = g.tick(0, &|_| true, &mut rng());
        assert_eq!(g.status, Status::Scored);
        (g, out)
    }

    #[test]
    fn running_totals_end_a_match_at_the_target() {
        let target = Settings {
            target_score: Some(50),
            ..Settings::default()
        };
        // nobody reaches 50: the match goes on
        let (g, out) = score_now(&[[1; 4], [5; 4]], &[10, 20], target);
        let totals: Vec<_> = g.seats.iter().map(|s| s.total).collect();
        assert_eq!(totals, [Some(14), Some(40)]);
        assert_eq!(g.match_no, 3);
        assert!(!g.match_over);
        let ev = out.events.iter().find(|e| e.kind == "scored").unwrap();
        assert_eq!(ev.payload["match_over"], false);
        assert_eq!(ev.payload["match_winners"], json!([]));

        // p1 reaches it, so the lowest total wins the match
        let (g, out) = score_now(&[[1; 4], [5; 4], [0; 4]], &[10, 30, 14], target);
        assert!(g.match_over);
        let won: Vec<_> = g.seats.iter().map(|s| s.match_won).collect();
        assert_eq!(won, [true, false, true]);
        let ev = out.events.iter().find(|e| e.kind == "scored").unwrap();
        assert_eq!(ev.payload["match_winners"], json!(["p0", "p2"]));
        let view = g.view("p0", 0);
        assert_eq!(view["match_over"], true);
        assert_eq!(view["seats"][1]["total"], 50);
        assert_eq!(view["seats"][1]["carry"], 30);
        assert_eq!(view["seats"][0]["match_won"], true);

        // without a target totals keep running and no match ends
        let (g, _) = score_now(&[[13; 4], [5; 4]], &[400, 0], Settings::default());
        assert_eq!(g.seats[0].total, Some(452));
        assert!(!g.match_over);
    }

    #[test]
    fn a_total_landing_exactly_on_the_target_is_halved() {
        let settings = Settings {
            target_score: Some(50),
            exact_reset: true,
            ..Settings::default()
        };
        let (g, _) = score_now(&[[1; 4], [5; 4]], &[10, 30], settings);
        assert_eq!(g.seats[1].total, Some(25));
        assert!(!g.match_over);
        // without the option the same total ends the match
        let (g, _) = score_now(
            &[[1; 4], [5; 4]],
            &[10, 30],
            Settings {
                exact_reset: false,
                ..settings
            },
        );
        assert_eq!(g.seats[1].total, Some(50));
        assert!(g.match_over);
    }

    #[test]
    fn each_seat_tallies_its_matches_misses_and_moves() {
        let mut g = fives();
        claim(&mut g, "p0", 100, 10, 0, 1); // wrong: a 2
        claim(&mut g, "p1", 250, 20, 1, 0); // right
        g.settle(i64::MAX, &mut rng());
        assert_eq!(
            g.seats[0].tally,
            Tally {
                misses: 1,
                penalties: 1,
                ..Tally::default()
            }
        );
        assert_eq!(g.seats[1].tally.matches, 1);
        assert_eq!(g.view("p0", 0)["seats"][0]["tally"]["misses"], 1);

        let mut g = playing(&[[0; 4], [5; 4]], 1, &[9]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        act(&mut g, "p0", Action::UseSpecial);
        act(&mut g, "p0", Action::Peek { seat: 1, slot: 0 });
        assert_eq!(g.seats[0].tally.specials, 1);
    }

    #[test]
    fn a_late_claim_hears_how_much_faster_the_match_was() {
        let mut g = fives();
        let slow = claim(&mut g, "p0", 300, 10, 0, 0);
        let fast = claim(&mut g, "p1", 180, 20, 1, 0);
        let out = g.settle(i64::MAX, &mut rng());
        let err = g.claim_result(&slow).unwrap().unwrap_err();
        assert_eq!(err.code, "too_late");
        assert_eq!(
            err.message,
            "too late: a match 120ms faster got there first"
        );
        assert_eq!(g.claim_result(&fast), Some(Ok(())));
        let m = out.events.iter().find(|e| e.kind == "match").unwrap();
        assert_eq!(m.payload["reaction_ms"], 180);

        let mut g = fives();
        claim(&mut g, "p1", 200, 20, 1, 0);
        let tied = claim(&mut g, "p0", 200, 30, 0, 0);
        g.settle(i64::MAX, &mut rng());
        assert_eq!(
            g.claim_result(&tied).unwrap().unwrap_err().message,
            "too late: an equally fast match arrived first"
        );
    }

    // ------------------------------------------------------------- bots

    const HARD_REACTION: i64 = 2_000;

    /// A two player game, p1 a bot, past the peek phase with p0 to move.
    fn with_bot(hands: &[[i8; 4]], starter: i8, draws: &[i8]) -> Game {
        let mut g = deal(players(hands.len()), stacked(hands, starter, draws));
        // a hard bot never slips or misremembers, so its play is exact
        g.seat_bots(|p| (p == "p1").then_some(bot::Level::Hard));
        for p in players(hands.len()).iter().filter(|p| *p != "p1") {
            g.apply(p, Action::Ready, None, 0, &mut rng()).unwrap();
        }
        // the bot readies itself on the first tick
        g.tick(0, &present, &mut rng());
        assert!(g.seats[1].ready);
        assert_eq!(g.status, Status::Playing);
        g
    }

    fn mind(g: &Game, p: &str) -> bot::Mind {
        g.minds.iter().find(|m| m.player == p).unwrap().clone()
    }

    #[test]
    fn a_bot_knows_only_its_opening_cards() {
        let g = with_bot(&[[5, 2, 3, 4], [6, 7, 8, 9]], 0, &[]);
        assert!(g.seats[1].bot);
        assert!(!g.seats[0].bot);
        let m = mind(&g, "p1");
        assert_eq!(m.seat, 1);
        let mut known: Vec<_> = m.known.iter().map(|k| (k.seat, k.slot, k.v)).collect();
        known.sort();
        assert_eq!(known, [(1, 0, 6), (1, 1, 7)]);
        assert_eq!(g.view("p0", 0)["seats"][1]["bot"], true);
        // minds never reach a view
        assert!(!g.view("p0", 0).to_string().contains("known"));
    }

    #[test]
    fn a_bot_steps_through_its_turn_at_a_human_pace() {
        let mut g = with_bot(&[[5, 2, 3, 4], [6, 7, 8, 9]], 0, &[4, 12]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        assert_eq!(g.turn, 1);
        // the first tick only starts the bot's clock
        let out = g.tick(1_000, &present, &mut rng());
        assert!(out.changed);
        assert!(out.events.is_empty());
        let out = g.tick(1_000 + BOT_STEP_MS - 1, &present, &mut rng());
        assert!(!out.changed);
        let out = g.tick(1_000 + BOT_STEP_MS, &present, &mut rng());
        let kinds: Vec<_> = out.events.iter().map(|e| e.kind).collect();
        // the bot holds 6 7 8 9 and knows 6 and 7: it draws the 12
        assert_eq!(kinds, ["draw"]);
        // a 12 is no good to keep, so it discards it for the blind swap,
        // giving away a high card it knows
        g.tick(1_000 + 2 * BOT_STEP_MS, &present, &mut rng());
        assert_eq!(
            g.stage,
            Stage::Earned {
                mv: Move::BlindSwap
            }
        );
        g.tick(1_000 + 3 * BOT_STEP_MS, &present, &mut rng());
        assert_eq!(
            g.stage,
            Stage::Special {
                mv: Move::BlindSwap
            }
        );
        let out = g.tick(1_000 + 4 * BOT_STEP_MS, &present, &mut rng());
        let swap = out.events.iter().find(|e| e.kind == "blind_swap").unwrap();
        assert_eq!(swap.payload["seat"], 1);
        assert_eq!(swap.payload["slot"], 1);
        assert_eq!(g.turn, 0);
        // the bot's 7 went to p0; its memory followed the card there, unless
        // it forgot it as the turn ended
        let m = mind(&g, "p1");
        let target = (
            swap.payload["target_seat"].as_u64().unwrap() as usize,
            swap.payload["target_slot"].as_u64().unwrap() as usize,
        );
        assert!(matches!(m.get(target.0, target.1), Some(7) | None));
        assert_eq!(m.get(1, 1), None);
    }

    #[test]
    fn a_bot_takes_a_low_discard_and_swaps_it_for_its_worst_card() {
        let mut g = with_bot(&[[5, 2, 3, 4], [13, 2, 8, 9]], 0, &[0]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        for i in 1..=3 {
            g.tick(i * BOT_STEP_MS, &present, &mut rng());
        }
        // it knew its 13 and took the 0 for it
        assert_eq!(g.seats[1].slots[0], Some(0));
        assert_eq!(g.discard.last(), Some(&13));
        let m = mind(&g, "p1");
        assert_eq!(m.get(1, 0), Some(0));
    }

    #[test]
    fn a_bot_matches_only_cards_it_knows_and_no_faster_than_its_reaction() {
        let mut g = with_bot(&[[5, 2, 3, 4], [6, 7, 8, 9]], 0, &[7]);
        let seq = g.turn_seq;
        g.apply("p0", Action::Draw, Some(seq), 1_000, &mut rng())
            .unwrap();
        let seq = g.turn_seq;
        g.apply("p0", Action::Discard, Some(seq), 1_000, &mut rng())
            .unwrap();
        // the 7 earned p0 a move, so the turn stays with p0
        g.tick(1_000 + HARD_REACTION - 1, &present, &mut rng());
        assert!(g.claims.is_empty());
        g.tick(1_000 + HARD_REACTION, &present, &mut rng());
        assert_eq!(g.claims.len(), 1);
        assert_eq!(g.claims[0].player, "p1");
        assert_eq!(g.claims[0].reaction_ms, HARD_REACTION);
        let out = g.tick(
            1_000 + HARD_REACTION + MATCH_WINDOW_MS,
            &present,
            &mut rng(),
        );
        let m = out.events.iter().find(|e| e.kind == "match").unwrap();
        assert_eq!(m.payload["ok"], true);
        assert_eq!(g.seats[1].slots[1], None);
        assert_eq!(mind(&g, "p1").get(1, 1), None);

        // a 4 it has never seen gets no match from it
        let mut g = with_bot(&[[5, 2, 3, 4], [6, 7, 8, 9]], 0, &[4]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        g.tick(10_000, &present, &mut rng());
        assert!(g.claims.iter().all(|c| c.player != "p1"));
    }

    #[test]
    fn a_bot_matches_a_card_shown_by_a_miss_and_gives_its_worst() {
        let mut g = with_bot(&[[5, 2, 3, 4], [6, 13, 8, 9]], 0, &[3, 1]);
        // p0 misses on its own 5, which the room shows everyone
        g.settings.show_misses = true;
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
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
        assert_eq!(mind(&g, "p1").get(0, 0), Some(5));
        // later a 5 lands: the bot matches p0's 5, giving away its known 13
        g.push_discard(5, 20_000);
        g.tick(20_000 + HARD_REACTION, &present, &mut rng());
        let claim = &g.claims[0];
        assert_eq!((claim.seat, claim.slot, claim.give_slot), (0, 0, Some(1)));
    }

    #[test]
    fn a_bot_gives_its_worst_card_when_it_owes_one() {
        let mut g = with_bot(&[[5, 2, 3, 4], [6, 7, 8, 9]], 0, &[]);
        g.owed.push(Owed {
            player: "p1".into(),
            seat: 0,
            slot: 0,
            deadline: i64::MAX,
        });
        g.seats[0].slots[0] = None;
        let out = g.tick(0, &present, &mut rng());
        let give = out.events.iter().find(|e| e.kind == "give").unwrap();
        // it knows its 6 and 7 and counts unseen cards as 6, so the 7 goes
        assert_eq!(give.payload["slot"], 1);
        assert_eq!(g.seats[0].slots[0], Some(7));
        assert_eq!(mind(&g, "p1").get(0, 0), Some(7));
    }

    #[test]
    fn a_bot_peeks_at_what_it_does_not_know_and_remembers_it() {
        let mut g = with_bot(&[[5, 2, 3, 4], [6, 7, 8, 9]], 0, &[4, 8]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        // the bot draws an 8, which earns peek own
        for i in 1..=5 {
            g.tick(i * BOT_STEP_MS, &present, &mut rng());
        }
        let m = mind(&g, "p1");
        assert_eq!(m.get(1, 2), Some(8));
        assert_eq!(g.seats[1].tally.specials, 1);
    }

    #[test]
    fn bots_alone_play_a_game_to_the_end() {
        for seed in 0..5 {
            let mut deck = deck_for(4, 4);
            use rand::seq::SliceRandom;
            let mut r = rand::rngs::StdRng::seed_from_u64(seed);
            deck.shuffle(&mut r);
            let mut g = Game::new(players(4), deck, 0, 0, Settings::default());
            // one of each level, so every level's play is exercised
            let levels = [bot::Level::Easy, bot::Level::Normal, bot::Level::Hard];
            g.seat_bots(|p| Some(levels[p[1..].parse::<usize>().unwrap() % 3]));
            let mut now = 0;
            while g.status != Status::Scored && now < 3_600_000 {
                now += 1_000;
                g.tick(now, &present, &mut r);
            }
            assert_eq!(g.status, Status::Scored, "seed {seed} never finished");
            assert!(g.caller.is_some(), "seed {seed} ended without a call");
            assert!(!g.winners().is_empty());
        }
    }

    #[test]
    fn a_bot_forgets_by_its_level() {
        // (others kept, own kept) of 200 each, after one forget
        let kept = |level| {
            let mut m = bot::Mind::new("p1", 1, level, std::iter::empty());
            for slot in 0..200 {
                m.known.push(bot::Known {
                    seat: 0,
                    slot,
                    v: 3,
                });
                m.known.push(bot::Known {
                    seat: 1,
                    slot,
                    v: 3,
                });
            }
            m.forget(&mut rng());
            let n = |seat| m.known.iter().filter(|k| k.seat == seat).count();
            (n(0), n(1))
        };
        let (easy, easy_own) = kept(bot::Level::Easy);
        let (normal, normal_own) = kept(bot::Level::Normal);
        let (hard, hard_own) = kept(bot::Level::Hard);
        assert!((100..160).contains(&easy), "easy kept {easy}");
        assert!((150..190).contains(&normal), "normal kept {normal}");
        assert!((180..200).contains(&hard), "hard kept {hard}");
        // easy and normal bots forget some of their own cards; hard never does
        assert!(
            (160..200).contains(&easy_own),
            "easy kept {easy_own} of its own"
        );
        assert!(
            (185..200).contains(&normal_own),
            "normal kept {normal_own} of its own"
        );
        assert_eq!(hard_own, 200);
    }

    #[test]
    fn bot_levels_set_reaction_misremembering_and_names() {
        use bot::Level;
        assert_eq!(
            [Level::Easy, Level::Normal, Level::Hard].map(|l| l.reaction_ms()),
            [3_000, 2_000, 2_000]
        );
        for l in [Level::Easy, Level::Normal, Level::Hard] {
            assert_eq!(Level::parse(l.as_str()), Some(l));
        }
        assert_eq!(Level::parse("expert"), None);
        assert_eq!(Level::default(), Level::Normal);
        // minds saved before levels play at normal
        let m: bot::Mind =
            serde_json::from_value(json!({ "player": "p1", "seat": 1, "known": [] })).unwrap();
        assert_eq!(m.level, Level::Normal);

        // an easy bot sometimes learns a card one off; a hard one never does
        let g = with_bot(&[[5, 2, 3, 4], [6, 7, 8, 9]], 0, &[]);
        let miss = Event {
            player: Some("p0".into()),
            kind: "match",
            payload: json!({ "ok": false, "seat": 0, "slot": 0, "value": 5 }),
        };
        let mut r = rng();
        let mut wrong = |level| {
            (0..400)
                .filter(|_| {
                    let mut m = bot::Mind::new("p1", 1, level, std::iter::empty());
                    m.observe(&miss, &g, &mut r);
                    let v = m.get(0, 0).unwrap();
                    assert!((4..=6).contains(&v));
                    v != 5
                })
                .count()
        };
        let easy = wrong(Level::Easy);
        assert!((30..100).contains(&easy), "easy misremembered {easy}");
        assert_eq!(wrong(Level::Hard), 0);
        // a -1 is only ever remembered as a 0, a 13 as a 12
        for (v, off) in [(-1, 0), (13, 12)] {
            let shown = Event {
                player: Some("p0".into()),
                kind: "match",
                payload: json!({ "ok": false, "seat": 0, "slot": 0, "value": v }),
            };
            for _ in 0..200 {
                let mut m = bot::Mind::new("p1", 1, Level::Easy, std::iter::empty());
                m.observe(&shown, &g, &mut r);
                let got = m.get(0, 0).unwrap();
                assert!(got == v || got == off, "{v} remembered as {got}");
            }
        }
    }

    #[test]
    fn an_easy_bot_waits_longer_to_match() {
        let mut g = with_bot(&[[5, 2, 3, 4], [6, 7, 8, 9]], 0, &[7]);
        g.minds[0].level = bot::Level::Easy;
        let seq = g.turn_seq;
        g.apply("p0", Action::Draw, Some(seq), 0, &mut rng())
            .unwrap();
        let seq = g.turn_seq;
        g.apply("p0", Action::Discard, Some(seq), 0, &mut rng())
            .unwrap();
        g.tick(2_999, &present, &mut rng());
        assert!(g.claims.is_empty());
        g.tick(3_000, &present, &mut rng());
        assert_eq!(g.claims.len(), 1);
    }

    #[test]
    fn a_bots_memory_follows_takes_swaps_and_forfeits() {
        let mut g = with_bot(&[[5, 2, 3, 4], [6, 7, 8, 9], [1, 1, 1, 1]], 0, &[]);
        let mut m = mind(&g, "p1");
        let e = |player: &str, kind, payload| Event {
            player: Some(player.into()),
            kind,
            payload,
        };
        // p0 takes the 0 from the pile and swaps it into slot 2: public
        m.observe(&e("p0", "take", json!({ "value": 0 })), &g, &mut rng());
        m.observe(
            &e(
                "p0",
                "swap",
                json!({ "seat": 0, "slot": 2, "discarded": 3 }),
            ),
            &g,
            &mut rng(),
        );
        assert_eq!(m.get(0, 2), Some(0));
        // a drawn card swapped in is not
        m.observe(
            &e(
                "p0",
                "swap",
                json!({ "seat": 0, "slot": 2, "discarded": 0 }),
            ),
            &g,
            &mut rng(),
        );
        assert_eq!(m.get(0, 2), None);
        // the bot's own swap: it knows what it put there
        g.seats[1].slots[3] = Some(2);
        m.observe(
            &e(
                "p1",
                "swap",
                json!({ "seat": 1, "slot": 3, "discarded": 9 }),
            ),
            &g,
            &mut rng(),
        );
        assert_eq!(m.get(1, 3), Some(2));
        // a blind swap trades what it knows of the two slots
        m.observe(
            &e(
                "p0",
                "blind_swap",
                json!({ "seat": 0, "slot": 2, "target_seat": 1, "target_slot": 0 }),
            ),
            &g,
            &mut rng(),
        );
        assert_eq!((m.get(0, 2), m.get(1, 0)), (Some(6), None));
        // someone else's peek teaches it nothing
        m.observe(
            &e("p0", "peek", json!({ "seat": 1, "slot": 2 })),
            &g,
            &mut rng(),
        );
        assert_eq!(m.get(1, 2), None);
        // a seat that leaves takes its cards with it
        m.observe(&e("p2", "forfeit", json!({ "seat": 2 })), &g, &mut rng());
        m.known.push(bot::Known {
            seat: 2,
            slot: 0,
            v: 1,
        });
        m.observe(&e("p2", "forfeit", json!({ "seat": 2 })), &g, &mut rng());
        assert_eq!(m.get(2, 0), None);
        // malformed events change nothing
        let before = m.clone();
        for kind in ["swap", "peek", "give", "match", "forfeit", "draw"] {
            m.observe(&e("p0", kind, json!({})), &g, &mut rng());
        }
        assert_eq!(m, before);
    }

    #[test]
    fn a_bot_calls_komino_once_its_total_looks_low() {
        let mut g = with_bot(&[[5, 2, 3, 4], [0, 1, 0, 1]], 0, &[9, 9, 9]);
        // it has seen all four of its cards
        g.minds[0].known = (0..4)
            .map(|slot| bot::Known {
                seat: 1,
                slot,
                v: g.seats[1].slots[slot].unwrap(),
            })
            .collect();
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        act(&mut g, "p0", Action::Skip);
        // before everyone has played it can't call, so it plays a turn
        for i in 1..=5 {
            g.tick(i * BOT_STEP_MS, &present, &mut rng());
        }
        assert_eq!(g.turn, 0);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        act(&mut g, "p0", Action::Skip);
        g.tick(10 * BOT_STEP_MS, &present, &mut rng());
        g.tick(11 * BOT_STEP_MS, &present, &mut rng());
        assert_eq!(g.caller, Some(1));
    }

    #[test]
    fn the_view_lists_the_newest_discards() {
        let mut g = playing(&[[5, 2, 3, 4], [6, 6, 7, 8]], 0, &[1, 2, 3]);
        assert_eq!(g.view("p0", 0)["discard_recent"], json!([0]));
        for p in ["p0", "p1", "p0"] {
            act(&mut g, p, Action::Draw);
            act(&mut g, p, Action::Discard);
        }
        assert_eq!(g.view("p1", 0)["discard_recent"], json!([3, 2, 1, 0]));
        g.discard = (0..13).collect();
        let recent = g.observer_view(0)["discard_recent"].clone();
        assert_eq!(recent, json!([12, 11, 10, 9, 8, 7, 6, 5, 4, 3]));
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
        g.tick(SCORE_DELAY_MS, &|_| true, &mut rng());
        assert_eq!(g.winners(), vec!["p1", "p2"]);
    }

    #[test]
    fn komino_called_mid_turn_takes_effect_when_the_turn_ends() {
        let mut g = playing(&[[0; 4], [5; 4], [6; 4]], 1, &[3, 3, 3, 3, 3]);
        for p in ["p0", "p1"] {
            act(&mut g, p, Action::Draw);
            act(&mut g, p, Action::Discard);
        }
        // p2 is the last to play a first turn, so they can call during it
        assert!(!g.view("p2", 0)["can_call"].as_bool().unwrap());
        act(&mut g, "p2", Action::Draw);
        assert!(g.view("p2", 0)["can_call"].as_bool().unwrap());
        let seq = g.turn_seq;
        let out = act(&mut g, "p2", Action::Komino);
        assert!(out.events.is_empty() && out.changed);
        // only the caller knows until the turn ends
        assert_eq!(g.status, Status::Playing);
        assert_eq!(g.turn_seq, seq);
        assert_eq!(g.view("p2", 0)["calling"], true);
        assert!(!g.view("p2", 0)["can_call"].as_bool().unwrap());
        assert_eq!(g.view("p0", 0)["calling"], false);
        assert_eq!(g.observer_view(0)["calling"], false);
        assert_eq!(reject(&mut g, "p2", Action::Komino), "invalid");
        let out = act(&mut g, "p2", Action::Swap { slot: 0 });
        let kinds: Vec<_> = out.events.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec!["swap", "komino"]);
        assert!(out.stats.contains(&("p2".into(), Stat::KominoCalls, 1)));
        assert_eq!(g.status, Status::Final);
        assert_eq!(g.caller, Some(2));
        assert_eq!(g.final_remaining, vec![0, 1]);
        assert_eq!(g.turn, 0);
        assert!(!g.calling);
        // nobody calls again once komino is called
        act(&mut g, "p0", Action::Draw);
        assert_eq!(reject(&mut g, "p0", Action::Komino), "invalid");
    }

    #[test]
    fn komino_can_be_called_with_a_special_move_pending() {
        let mut g = playing(&[[0; 4], [5; 4]], 1, &[3, 3, 9]);
        for p in ["p0", "p1"] {
            act(&mut g, p, Action::Draw);
            act(&mut g, p, Action::Discard);
        }
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        act(&mut g, "p0", Action::Komino);
        act(&mut g, "p0", Action::UseSpecial);
        act(&mut g, "p0", Action::Peek { seat: 1, slot: 0 });
        assert_eq!(g.caller, Some(0));
        assert_eq!(g.final_remaining, vec![1]);
    }

    #[test]
    fn a_skipped_turn_keeps_its_komino_call() {
        let settings = Settings {
            turn_limit_secs: Some(60),
            ..Settings::default()
        };
        let mut g = with(settings, &[[0; 4], [5; 4]], 1, &[3, 3, 3]);
        for p in ["p0", "p1"] {
            act(&mut g, p, Action::Draw);
            act(&mut g, p, Action::Discard);
        }
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Komino);
        let out = g.tick(60_000, &|_| true, &mut rng());
        let kinds: Vec<_> = out.events.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec!["timeout_skip", "komino"]);
        assert_eq!(g.caller, Some(0));
        assert_eq!(g.turn, 1);
    }

    #[test]
    fn a_forfeit_drops_a_pending_komino_call() {
        let mut g = playing(&[[0; 4], [5; 4], [6; 4]], 1, &[3, 3, 3, 3]);
        for p in ["p0", "p1", "p2"] {
            act(&mut g, p, Action::Draw);
            act(&mut g, p, Action::Discard);
        }
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Komino);
        g.forfeit("p0", 0);
        assert!(!g.calling);
        assert_eq!(g.caller, None);
        assert_eq!(g.status, Status::Playing);
        assert_eq!(g.turn, 1);
    }

    #[test]
    fn away_player_is_skipped_after_the_grace_period() {
        let mut g = playing(&[[1; 4], [2; 4]], 5, &[9]);
        let away = |p: &str| p != "p0";
        assert!(g.tick(100, &away, &mut rng()).changed);
        assert_eq!(g.away_since, Some(100));
        assert!(g
            .tick(100 + GRACE_MS - 1, &away, &mut rng())
            .events
            .is_empty());
        let out = g.tick(100 + GRACE_MS, &away, &mut rng());
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
        let mut g = deal(players(2), stacked(&[[1, 2, 3, 4], [5, 6, 0, -1]], 9, &[7]));
        // during the opening peek each player may reveal their near row, observers nothing
        assert!(g.secret("p0", &Secret::Opening, 0).is_ok());
        let text = g.observer_view(0).to_string();
        assert!(!text.contains("\"v\""), "observer saw a value: {text}");
        assert_eq!(g.observer_view(0)["me"], Value::Null);

        g.apply("p0", Action::Ready, None, 0, &mut rng()).unwrap();
        g.apply("p1", Action::Ready, None, 0, &mut rng()).unwrap();
        act(&mut g, "p0", Action::Draw);
        assert_eq!(g.secret("p0", &Secret::Drawn, 0).unwrap()["card"], 7);
        let obs = g.observer_view(0);
        assert_eq!(obs["stage"]["kind"], "drawn");
        assert_eq!(obs["stage"]["card"], Value::Null);
        assert_eq!(obs["can_call"], false);

        act(&mut g, "p0", Action::Discard);
        act(&mut g, "p0", Action::UseSpecial);
        act(&mut g, "p0", Action::Peek { seat: 0, slot: 1 });
        // the peeker may reveal the value; observers and the other player see only the slot
        let id = g.reveals[0].id.clone();
        assert_eq!(
            g.secret("p0", &Secret::Peek(id), 1).unwrap()["cards"][0]["v"],
            2
        );
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

    fn with(settings: Settings, hands: &[[i8; 4]], starter: i8, draws: &[i8]) -> Game {
        let mut g = Game::new(
            players(hands.len()),
            stacked(hands, starter, draws),
            0,
            0,
            settings,
        );
        for p in players(hands.len()) {
            g.apply(&p, Action::Ready, None, 0, &mut rng()).unwrap();
        }
        g
    }

    #[test]
    fn settings_are_checked_against_their_ranges() {
        assert_eq!(Settings::default().validate(), Ok(()));
        let ok = Settings {
            hand_size: 10,
            away_grace_secs: 600,
            turn_limit_secs: Some(10),
            reveal_secs: Some(60),
            show_misses: false,
            target_score: Some(500),
            caller_penalty: 50,
            exact_reset: true,
            memory_marks: true,
        };
        assert_eq!(ok.validate(), Ok(()));
        assert_eq!(
            Settings {
                target_score: Some(25),
                caller_penalty: 0,
                ..ok
            }
            .validate(),
            Ok(())
        );
        let bad = [
            Settings { hand_size: 3, ..ok },
            Settings {
                hand_size: 11,
                ..ok
            },
            Settings {
                away_grace_secs: 9,
                ..ok
            },
            Settings {
                turn_limit_secs: Some(601),
                ..ok
            },
            Settings {
                reveal_secs: Some(0),
                ..ok
            },
            Settings {
                target_score: Some(24),
                ..ok
            },
            Settings {
                target_score: Some(501),
                ..ok
            },
            Settings {
                caller_penalty: -1,
                ..ok
            },
            Settings {
                caller_penalty: 51,
                ..ok
            },
        ];
        for s in bad {
            assert!(s.validate().is_err(), "{s:?} passed");
        }
    }

    #[test]
    fn a_second_deck_joins_when_one_would_run_short() {
        // 8 x 4 + 1 leaves 27 to draw
        assert_eq!(deck_for(8, 4).len(), 60);
        // 4 x 9 + 1 leaves 23; 5 x 8 + 1 leaves 19
        assert_eq!(deck_for(4, 9).len(), 60);
        assert_eq!(deck_for(5, 8).len(), 120);
        let two = deck_for(8, 10);
        assert_eq!(two.len(), 120);
        assert_eq!(two.iter().filter(|&&v| v == 13).count(), 8);
    }

    #[test]
    fn larger_hands_deal_and_open_the_near_row() {
        for (hand_size, near) in [(5, 0..2), (10, 0..5)] {
            let settings = Settings {
                hand_size,
                ..Settings::default()
            };
            let deck = deck_for(8, hand_size);
            let g = Game::new(players(8), deck.clone(), 0, 0, settings);
            assert!(g.seats.iter().all(|s| s.slots.len() == hand_size));
            assert_eq!(g.deck.len(), deck.len() - 8 * hand_size - 1);
            assert_eq!(g.view("p0", 0)["hand_size"], hand_size);
            let opened = g.secret("p0", &Secret::Opening, 0).unwrap();
            let slots: Vec<usize> = opened["cards"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["slot"].as_u64().unwrap() as usize)
                .collect();
            assert_eq!(slots, near.collect::<Vec<_>>());
            for c in opened["cards"].as_array().unwrap() {
                let slot = c["slot"].as_u64().unwrap() as usize;
                assert_eq!(c["v"], g.seats[0].slots[slot].unwrap());
            }
        }
    }

    #[test]
    fn a_miss_shows_the_targeted_value_only_when_the_room_says_so() {
        for (show_misses, value) in [(true, json!(5)), (false, Value::Null)] {
            let settings = Settings {
                show_misses,
                ..Settings::default()
            };
            let mut g = with(settings, &[[1, 2, 3, 4], [5, 6, 7, 8]], 0, &[9, 12]);
            act(&mut g, "p0", Action::Draw);
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
            let e = out.events.last().unwrap();
            assert_eq!(e.kind, "match");
            assert_eq!(e.payload["ok"], false);
            assert_eq!(
                e.payload.get("value").cloned().unwrap_or(Value::Null),
                value
            );
        }
        // settings saved before the option existed show misses; games saved
        // before settings existed keep them hidden
        let old: Settings = serde_json::from_value(json!({ "hand_size": 4 })).unwrap();
        assert!(old.show_misses);
        assert!(!Settings::legacy().show_misses);
    }

    fn untimed() -> Settings {
        Settings {
            reveal_secs: None,
            ..Settings::default()
        }
    }

    #[test]
    fn an_untimed_peek_lasts_until_hidden() {
        let mut g = with(untimed(), &[[1, 2, 3, 4], [5; 4]], 0, &[7, 7]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        act(&mut g, "p0", Action::UseSpecial);
        act(&mut g, "p0", Action::Peek { seat: 0, slot: 0 });
        let later = 24 * 60 * 60 * 1000;
        let view = g.view("p0", later);
        assert_eq!(view["reveals"][0]["until"], Value::Null);
        let peek = Secret::Peek(view["reveals"][0]["id"].as_str().unwrap().into());
        assert!(g.secret("p0", &peek, later).is_ok());
        assert_eq!(
            g.view("p1", later)["peeked"],
            json!([{ "seat": 0, "slot": 0, "until": null }])
        );
        // hiding takes no turn token and is open to anyone seated
        let seq = g.turn_seq;
        let out = g
            .apply("p0", Action::Hide, None, later, &mut rng())
            .unwrap();
        assert!(out.changed && out.events.is_empty());
        assert_eq!(g.turn_seq, seq);
        assert_eq!(g.view("p1", later)["peeked"], json!([]));
        assert_eq!(g.secret("p0", &peek, later).unwrap_err().code, "forbidden");
    }

    #[test]
    fn hiding_ends_only_the_hiders_peeks() {
        let mut g = with(untimed(), &[[1, 2, 3, 4], [5; 4]], 0, &[7, 7]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        act(&mut g, "p0", Action::UseSpecial);
        act(&mut g, "p0", Action::Peek { seat: 0, slot: 0 });
        act(&mut g, "p1", Action::Draw);
        act(&mut g, "p1", Action::Discard);
        act(&mut g, "p1", Action::UseSpecial);
        act(&mut g, "p1", Action::Peek { seat: 1, slot: 2 });
        act(&mut g, "p1", Action::Hide);
        assert_eq!(g.reveals.len(), 1);
        assert_eq!(g.reveals[0].player, "p0");
    }

    #[test]
    fn an_untimed_peek_ends_when_the_slot_changes() {
        let mut g = with(untimed(), &[[1, 2, 3, 4], [5; 4]], 0, &[7, 6]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        act(&mut g, "p0", Action::UseSpecial);
        act(&mut g, "p0", Action::Peek { seat: 0, slot: 0 });
        act(&mut g, "p1", Action::Draw);
        act(&mut g, "p1", Action::Swap { slot: 0 });
        assert_eq!(g.reveals.len(), 1);
        act(&mut g, "p0", Action::Take);
        act(&mut g, "p0", Action::Swap { slot: 0 });
        assert!(g.reveals.is_empty());
    }

    #[test]
    fn a_timed_peek_uses_the_room_peek_time() {
        let settings = Settings {
            reveal_secs: Some(10),
            ..Settings::default()
        };
        let mut g = with(settings, &[[1, 2, 3, 4], [5; 4]], 0, &[7]);
        act(&mut g, "p0", Action::Draw);
        act(&mut g, "p0", Action::Discard);
        act(&mut g, "p0", Action::UseSpecial);
        act(&mut g, "p0", Action::Peek { seat: 0, slot: 0 });
        assert_eq!(g.reveals[0].until, Some(10_000));
        assert_eq!(g.view("p1", 9_999)["peeked"].as_array().unwrap().len(), 1);
        assert_eq!(g.view("p1", 10_000)["peeked"], json!([]));
    }

    #[test]
    fn the_turn_limit_skips_a_present_player() {
        let settings = Settings {
            turn_limit_secs: Some(60),
            ..Settings::default()
        };
        let mut g = with(settings, &[[1; 4], [2; 4]], 5, &[9, 9]);
        assert_eq!(g.turn_deadline, Some(60_000));
        assert_eq!(g.view("p1", 0)["turn_deadline"], 60_000);
        act(&mut g, "p0", Action::Draw);
        assert!(!g.tick(59_999, &|_| true, &mut rng()).changed);
        let seq = g.turn_seq;
        let out = g.tick(60_000, &|_| true, &mut rng());
        assert_eq!(out.events[0].kind, "timeout_skip");
        // the held card goes face up and the next turn gets a fresh limit
        assert_eq!(g.discard.last(), Some(&9));
        assert!(g.matchable);
        assert_eq!(g.turn, 1);
        assert!(g.turn_seq > seq);
        assert_eq!(g.turn_deadline, Some(120_000));
        // a turn that ends in time restarts the clock from when it ended
        g.apply("p1", Action::Draw, Some(g.turn_seq), 70_000, &mut rng())
            .unwrap();
        g.apply("p1", Action::Discard, Some(g.turn_seq), 70_000, &mut rng())
            .unwrap();
        g.apply("p1", Action::Skip, Some(g.turn_seq), 70_000, &mut rng())
            .unwrap();
        assert_eq!(g.turn_deadline, Some(130_000));
    }

    #[test]
    fn without_a_turn_limit_turns_never_time_out() {
        let mut g = with(Settings::default(), &[[1; 4], [2; 4]], 5, &[9]);
        assert_eq!(g.turn_deadline, None);
        assert!(!g.tick(i64::MAX / 2, &|_| true, &mut rng()).changed);
        assert_eq!(g.turn, 0);
    }

    #[test]
    fn the_turn_limit_stops_at_scoring() {
        let settings = Settings {
            turn_limit_secs: Some(30),
            ..Settings::default()
        };
        let mut g = with(settings, &[[1; 4], [2; 4]], 5, &[9, 9, 9]);
        for p in ["p0", "p1"] {
            act(&mut g, p, Action::Draw);
            act(&mut g, p, Action::Discard);
            act(&mut g, p, Action::Skip);
        }
        act(&mut g, "p0", Action::Komino);
        assert!(g.turn_deadline.is_some());
        g.forfeit("p1", 0);
        assert_eq!(g.status, Status::Scored);
        assert_eq!(g.turn_deadline, None);
    }

    #[test]
    fn the_away_grace_follows_the_setting() {
        let settings = Settings {
            away_grace_secs: 15,
            ..Settings::default()
        };
        let mut g = with(settings, &[[1; 4], [2; 4]], 5, &[]);
        let away = |p: &str| p != "p0";
        g.tick(0, &away, &mut rng());
        assert_eq!(g.view("p1", 0)["away_deadline"], 15_000);
        assert!(g.tick(14_999, &away, &mut rng()).events.is_empty());
        assert_eq!(
            g.tick(15_000, &away, &mut rng()).events[0].kind,
            "away_skip"
        );
    }

    #[test]
    fn states_saved_before_settings_play_by_the_old_rules() {
        let g = playing(&[[1; 4], [2; 4]], 5, &[]);
        let mut stored = serde_json::to_value(&g).unwrap();
        let obj = stored.as_object_mut().unwrap();
        obj.remove("settings");
        obj.remove("turn_deadline");
        obj.insert(
            "reveals".into(),
            json!([{ "id": "a", "player": "p0", "seat": 0, "slot": 0, "value": 1, "until": 5000 }]),
        );
        let loaded: Game = serde_json::from_value(stored).unwrap();
        assert_eq!(loaded.settings, Settings::legacy());
        assert_eq!(loaded.turn_deadline, None);
        assert_eq!(loaded.reveals[0].until, Some(5000));
    }
}
