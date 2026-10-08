//! Computer players (BOT-*). A bot sees only what its seat may see: the
//! opening near row, cards it draws or peeks at, and every public event. Its
//! memory follows cards as events move them, and it forgets some of what it
//! knows as turns go by. How well it plays is its [`Level`] (BOT-5).

use crate::game::{special, Action, Event, Game, Move, Stage, Status};
use serde::{Deserialize, Serialize};

/// What a bot counts a card it has not seen as: the mean card value.
const UNSEEN: i32 = 6;

/// How well a bot plays (BOT-5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Easy,
    #[default]
    Normal,
    Hard,
}

impl Level {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "easy" => Some(Level::Easy),
            "normal" => Some(Level::Normal),
            "hard" => Some(Level::Hard),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Easy => "easy",
            Level::Normal => "normal",
            Level::Hard => "hard",
        }
    }

    /// The chance, when its turn ends, of forgetting each card it knows in
    /// another hand, and in its own.
    fn forget(&self) -> (f64, f64) {
        match self {
            Level::Easy => (0.35, 0.1),
            Level::Normal => (0.15, 0.03),
            Level::Hard => (0.05, 0.0),
        }
    }

    /// How long after a discard lands it may match it.
    pub fn reaction_ms(&self) -> i64 {
        match self {
            Level::Easy => 3_000,
            Level::Normal => 2_000,
            Level::Hard => 2_000,
        }
    }

    /// The chance a card it learns is remembered one off.
    fn misremember(&self) -> f64 {
        match self {
            Level::Easy => 0.15,
            Level::Normal => 0.03,
            Level::Hard => 0.0,
        }
    }

    /// How far off its sense of its own total can be when deciding to call.
    fn call_noise(&self) -> i32 {
        match self {
            Level::Easy => 3,
            Level::Normal => 1,
            Level::Hard => 0,
        }
    }

    /// The chance it swaps a card into a random slot instead of its worst.
    fn slip(&self) -> f64 {
        match self {
            Level::Easy => 0.2,
            Level::Normal => 0.05,
            Level::Hard => 0.0,
        }
    }
}

/// True with probability `p`.
fn chance(rng: &mut impl rand::Rng, p: f64) -> bool {
    p > 0.0 && f64::from(rng.next_u32()) < p * f64::from(u32::MAX)
}

/// One card a bot knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Known {
    pub seat: usize,
    pub slot: usize,
    pub v: i8,
}

/// A bot's memory of the table.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mind {
    pub player: String,
    pub seat: usize,
    pub known: Vec<Known>,
    /// The card a player took from the discard pile, until they swap it in.
    #[serde(default)]
    pub taken: Option<(String, i8)>,
    #[serde(default)]
    pub level: Level,
}

impl Mind {
    /// A fresh mind that has seen `seat`'s opening cards.
    pub fn new(
        player: &str,
        seat: usize,
        level: Level,
        opening: impl Iterator<Item = (usize, i8)>,
    ) -> Self {
        Self {
            player: player.to_string(),
            seat,
            known: opening.map(|(slot, v)| Known { seat, slot, v }).collect(),
            taken: None,
            level,
        }
    }

    /// Remember a card seen just now, one off at the level's rate.
    fn learn(&mut self, seat: usize, slot: usize, v: i8, rng: &mut impl rand::Rng) {
        let v = if chance(rng, self.level.misremember()) {
            // one up or down, staying within -1 to 13
            if rng.next_u32().is_multiple_of(2) && v < 13 || v == -1 {
                v + 1
            } else {
                v - 1
            }
        } else {
            v
        };
        self.set(seat, slot, Some(v));
    }

    pub fn get(&self, seat: usize, slot: usize) -> Option<i8> {
        self.known
            .iter()
            .find(|k| k.seat == seat && k.slot == slot)
            .map(|k| k.v)
    }

    fn set(&mut self, seat: usize, slot: usize, v: Option<i8>) {
        self.known.retain(|k| !(k.seat == seat && k.slot == slot));
        if let Some(v) = v {
            self.known.push(Known { seat, slot, v });
        }
    }

    /// Move what is known of one slot to another, leaving the first unknown.
    fn carry(&mut self, from: (usize, usize), to: (usize, usize)) {
        let v = self.get(from.0, from.1);
        self.set(from.0, from.1, None);
        self.set(to.0, to.1, v);
    }

    /// Learn from one event, read against the game as it stands after it.
    pub fn observe(&mut self, e: &Event, g: &Game, rng: &mut impl rand::Rng) {
        let p = &e.payload;
        let at = |k: &str| p[k].as_u64().map(|n| n as usize);
        let actor = e.player.as_deref();
        match e.kind {
            "take" => {
                if let (Some(player), Some(v)) = (actor, p["value"].as_i64()) {
                    self.taken = Some((player.to_string(), v as i8));
                }
            }
            "swap" => {
                let (Some(seat), Some(slot)) = (at("seat"), at("slot")) else {
                    return;
                };
                let v = if seat == self.seat {
                    // the bot swapped this card in itself
                    g.seats[seat].slots.get(slot).copied().flatten()
                } else {
                    // a card taken from the discard pile is public
                    self.taken
                        .as_ref()
                        .filter(|(who, _)| Some(who.as_str()) == actor)
                        .map(|(_, v)| *v)
                };
                match v {
                    Some(v) => self.learn(seat, slot, v, rng),
                    None => self.set(seat, slot, None),
                }
                self.taken = None;
            }
            "peek" => {
                let (Some(seat), Some(slot)) = (at("seat"), at("slot")) else {
                    return;
                };
                if actor == Some(self.player.as_str()) {
                    let v = g
                        .reveals
                        .iter()
                        .find(|r| r.player == self.player && r.seat == seat && r.slot == slot)
                        .map(|r| r.value);
                    if let Some(v) = v {
                        self.learn(seat, slot, v, rng);
                    }
                }
            }
            "blind_swap" | "look_swap" | "give" => {
                let (Some(a), Some(b)) = (
                    at("seat").zip(at("slot")),
                    at("target_seat").zip(at("target_slot")),
                ) else {
                    return;
                };
                if e.kind == "give" {
                    self.carry(a, b);
                } else {
                    let (va, vb) = (self.get(a.0, a.1), self.get(b.0, b.1));
                    self.set(a.0, a.1, vb);
                    self.set(b.0, b.1, va);
                }
            }
            "match" => {
                let (Some(seat), Some(slot)) = (at("seat"), at("slot")) else {
                    return;
                };
                if p["ok"] == true {
                    self.set(seat, slot, None);
                    let from = actor.and_then(|a| g.seat_of(a));
                    if let (Some(from), Some(give)) = (from, at("give_slot")) {
                        self.carry((from, give), (seat, slot));
                    }
                } else if let Some(v) = p["value"].as_i64() {
                    // a shown miss names the card for everyone (SET-13)
                    self.learn(seat, slot, v as i8, rng);
                }
            }
            "forfeit" => {
                if let Some(seat) = at("seat") {
                    self.known.retain(|k| k.seat != seat);
                }
            }
            _ => {}
        }
    }

    /// Lose some of what is known, at the level's rates.
    pub fn forget(&mut self, rng: &mut impl rand::Rng) {
        let me = self.seat;
        let (others, own) = self.level.forget();
        self.known
            .retain(|k| !chance(rng, if k.seat == me { own } else { others }));
    }

    /// What a slot is worth to this bot: its known value, or the mean card.
    fn worth(&self, seat: usize, slot: usize) -> i32 {
        self.get(seat, slot).map_or(UNSEEN, i32::from)
    }

    /// This bot's filled slots, worst first.
    fn own_worst(&self, g: &Game) -> Vec<usize> {
        let mut slots: Vec<usize> = filled(g, self.seat).collect();
        slots.sort_by_key(|&s| (-self.worth(self.seat, s), s));
        slots
    }

    fn estimate(&self, g: &Game) -> i32 {
        filled(g, self.seat).map(|s| self.worth(self.seat, s)).sum()
    }
}

fn filled(g: &Game, seat: usize) -> impl Iterator<Item = usize> + '_ {
    g.seats[seat]
        .slots
        .iter()
        .enumerate()
        .filter(|(_, c)| c.is_some())
        .map(|(i, _)| i)
}

/// Slots in other hands a move may target: active, unlocked, filled.
fn targets<'a>(g: &'a Game, me: usize) -> impl Iterator<Item = (usize, usize)> + 'a {
    (0..g.seats.len())
        .filter(move |&s| s != me && !g.seats[s].forfeited && g.caller != Some(s))
        .flat_map(move |s| filled(g, s).map(move |n| (s, n)))
}

/// The next step of the bot's own turn.
pub fn turn_action(g: &Game, mind: &Mind, rng: &mut impl rand::Rng) -> Action {
    let me = mind.seat;
    let mut worst = mind.own_worst(g);
    let worst_worth = worst.first().map_or(i32::MIN, |&s| mind.worth(me, s));
    // a weaker bot sometimes swaps into the wrong slot
    if chance(rng, mind.level.slip()) {
        if let Some(slot) = pick(&worst, rng) {
            worst.insert(0, slot);
        }
    }
    match g.stage {
        Stage::Start => {
            let unseen = filled(g, me).filter(|&s| mind.get(me, s).is_none()).count();
            let noise = mind.level.call_noise();
            let est = mind.estimate(g)
                + if noise > 0 {
                    (rng.next_u32() % (2 * noise as u32 + 1)) as i32 - noise
                } else {
                    0
                };
            if g.may_call(me).is_ok() && (est <= 4 || (unseen == 0 && est <= 7)) {
                return Action::Komino;
            }
            match g.discard.last() {
                Some(&top) if i32::from(top) <= 2 && worst_worth - i32::from(top) >= 4 => {
                    Action::Take
                }
                _ => Action::Draw,
            }
        }
        Stage::Drawn { card } => match worst.first() {
            // a special card is worth more as a move than a middling swap
            Some(&slot)
                if i32::from(card) + 1 < worst_worth
                    && (special(card).is_none() || worst_worth - i32::from(card) >= 4) =>
            {
                Action::Swap { slot }
            }
            _ => Action::Discard,
        },
        Stage::Taken { .. } => Action::Swap {
            slot: worst.first().copied().unwrap_or(0),
        },
        Stage::Earned { mv } => {
            if special_step(g, mind, mv, rng).is_some() {
                Action::UseSpecial
            } else {
                Action::Skip
            }
        }
        Stage::Special { mv } => special_step(g, mind, mv, rng).unwrap_or(Action::Skip),
        Stage::Looked { seat, slot } => {
            let seen = mind.get(seat, slot).map_or(UNSEEN, i32::from);
            match worst.first() {
                Some(&mine) if seat != me && seen + 1 < worst_worth => {
                    Action::LookSwap { slot: Some(mine) }
                }
                _ => Action::LookSwap { slot: None },
            }
        }
    }
}

/// One of `cands` at random.
fn pick<T: Copy>(cands: &[T], rng: &mut impl rand::Rng) -> Option<T> {
    (!cands.is_empty()).then(|| cands[rng.next_u32() as usize % cands.len()])
}

/// How the bot would use a move now, if it has a use for it.
fn special_step(g: &Game, mind: &Mind, mv: Move, rng: &mut impl rand::Rng) -> Option<Action> {
    let me = mind.seat;
    let unseen_other: Vec<(usize, usize)> = targets(g, me)
        .filter(|&(s, n)| mind.get(s, n).is_none())
        .collect();
    match mv {
        Move::PeekOwn => filled(g, me)
            .find(|&s| mind.get(me, s).is_none())
            .map(|slot| Action::Peek { seat: me, slot }),
        Move::PeekOther => pick(&unseen_other, rng).map(|(seat, slot)| Action::Peek { seat, slot }),
        Move::BlindSwap => {
            // give away a known high card for a known lower one, or a guess
            let (mine, high) = filled(g, me)
                .filter_map(|s| mind.get(me, s).map(|v| (s, v)))
                .max_by_key(|&(s, v)| (v, std::cmp::Reverse(s)))?;
            if high < 7 {
                return None;
            }
            let known_low = targets(g, me)
                .filter_map(|(s, n)| mind.get(s, n).map(|v| (s, n, v)))
                .filter(|&(_, _, v)| v < high)
                .min_by_key(|&(s, n, v)| (v, s, n))
                .map(|(s, n, _)| (s, n));
            let (seat, target_slot) = known_low.or_else(|| pick(&unseen_other, rng))?;
            Some(Action::BlindSwap {
                slot: mine,
                seat,
                target_slot,
            })
        }
        Move::LookSwap => pick(&unseen_other, rng).map(|(seat, slot)| Action::Peek { seat, slot }),
    }
}

/// A match the bot is sure of, if the top discard has one: its own card
/// first (when that lowers its total), else another player's, giving its
/// worst card up front.
pub fn match_action(g: &Game, mind: &Mind) -> Option<Action> {
    if !matches!(g.status, Status::Playing | Status::Final | Status::Scoring) || !g.matchable {
        return None;
    }
    let top = *g.discard.last()?;
    let me = mind.seat;
    let sure = |&(s, n): &(usize, usize)| mind.get(s, n) == Some(top);
    let seq = g.discard_seq;
    if top > 0 {
        if let Some(slot) = filled(g, me).find(|&n| sure(&(me, n))) {
            return Some(Action::Match {
                seq,
                seat: me,
                slot,
                give_slot: None,
            });
        }
    }
    let (seat, slot) = targets(g, me).find(sure)?;
    // giving a card only helps when it is worth more than nothing
    let give = mind
        .own_worst(g)
        .into_iter()
        .find(|&s| mind.worth(me, s) > 0)?;
    Some(Action::Match {
        seq,
        seat,
        slot,
        give_slot: Some(give),
    })
}

/// The card the bot gives for a match it owes one for: its worst.
pub fn give_action(g: &Game, mind: &Mind) -> Option<Action> {
    mind.own_worst(g).first().map(|&slot| Action::Give { slot })
}
