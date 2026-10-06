//! Per-socket latency for fair matches (RT-18). Each member socket measures
//! its round trip with websocket pings and remembers when it first sent the
//! current discard, so a match can be ranked by how fast the player reacted
//! rather than how close they are to the server.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How often a socket pings to measure its round trip.
pub const PING_EVERY: Duration = Duration::from_secs(2);
/// The most round trip a match is credited with, so a client that delays its
/// pongs gains at most this much.
pub const MAX_CREDIT: Duration = Duration::from_millis(200);
/// Round trip samples kept; the lowest is the estimate.
const SAMPLES: usize = 10;
/// Pings awaiting a pong; an older one is forgotten.
const IN_FLIGHT: usize = 4;

/// A socket's round trip, from websocket ping and pong frames.
#[derive(Default)]
pub struct Rtt {
    next: u64,
    in_flight: VecDeque<(u64, Instant)>,
    samples: VecDeque<Duration>,
}

impl Rtt {
    /// The payload of the next ping, sent at `now`.
    pub fn ping(&mut self, now: Instant) -> Vec<u8> {
        let id = self.next;
        self.next += 1;
        if self.in_flight.len() == IN_FLIGHT {
            self.in_flight.pop_front();
        }
        self.in_flight.push_back((id, now));
        id.to_be_bytes().to_vec()
    }

    /// Record a pong received at `now`. Unknown payloads are ignored.
    pub fn pong(&mut self, payload: &[u8], now: Instant) {
        let Ok(bytes) = <[u8; 8]>::try_from(payload) else {
            return;
        };
        let id = u64::from_be_bytes(bytes);
        let Some(i) = self.in_flight.iter().position(|&(p, _)| p == id) else {
            return;
        };
        let (_, sent) = self.in_flight.remove(i).expect("position is in range");
        if self.samples.len() == SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back(now.saturating_duration_since(sent));
    }

    /// The fastest and slowest recent round trips.
    pub fn estimate(&self) -> Option<RoundTrip> {
        Some(RoundTrip {
            low: *self.samples.iter().min()?,
            high: *self.samples.iter().max()?,
        })
    }
}

/// The spread of a socket's recent round trips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoundTrip {
    /// Queueing only ever adds to a sample, and a client can delay a pong
    /// but never hurry one, so this is the safe estimate.
    pub low: Duration,
    /// How slow the path has been lately, jitter included.
    pub high: Duration,
}

/// When this socket first sent each discard, kept for the newest one only.
#[derive(Default)]
pub struct Seen {
    latest: Option<(u64, Instant)>,
}

impl Seen {
    /// Note a view carrying discard `seq` sent at `now`.
    pub fn sent(&mut self, seq: u64, now: Instant) {
        if self.latest.is_none_or(|(s, _)| s != seq) {
            self.latest = Some((seq, now));
        }
    }

    /// How long since this socket first sent discard `seq`, if it did.
    pub fn since(&self, seq: u64, now: Instant) -> Option<Duration> {
        self.latest
            .filter(|&(s, _)| s == seq)
            .map(|(_, at)| now.saturating_duration_since(at))
    }
}

/// A match's reaction time in ms (RT-18). `elapsed` is from sending the
/// discard to receiving the match: the reaction plus that message's round
/// trip. The client's own measure is trusted between `elapsed` less the
/// slowest recent round trip and `elapsed` itself, so jitter doesn't cost an
/// honest player; without one, the reaction is `elapsed` less the fastest.
/// Either credit is capped at [`MAX_CREDIT`].
pub fn reaction_ms(reported: Option<u64>, elapsed: Duration, rtt: Option<RoundTrip>) -> i64 {
    let ms = |d: Duration| d.min(MAX_CREDIT).as_millis() as i64;
    let elapsed_ms = elapsed.as_millis() as i64;
    let (low, high) = rtt.map_or((0, 0), |r| (ms(r.low), ms(r.high)));
    match reported {
        Some(r) => {
            let r = i64::try_from(r).unwrap_or(i64::MAX);
            r.clamp((elapsed_ms - high).max(0), elapsed_ms)
        }
        None => (elapsed_ms - low).max(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn the_estimate_spans_recent_round_trips() {
        let t0 = Instant::now();
        let mut rtt = Rtt::default();
        assert_eq!(rtt.estimate(), None);
        let a = rtt.ping(t0);
        let b = rtt.ping(t0 + 10 * MS);
        rtt.pong(&b, t0 + 90 * MS);
        rtt.pong(&a, t0 + 120 * MS);
        let spread = RoundTrip {
            low: 80 * MS,
            high: 120 * MS,
        };
        assert_eq!(rtt.estimate(), Some(spread));
        // a repeated or unknown pong adds nothing
        rtt.pong(&b, t0 + 95 * MS);
        rtt.pong(b"junk", t0 + 95 * MS);
        rtt.pong(&99u64.to_be_bytes(), t0 + 95 * MS);
        assert_eq!(rtt.estimate(), Some(spread));
    }

    #[test]
    fn old_samples_and_pings_are_forgotten() {
        let t0 = Instant::now();
        let mut rtt = Rtt::default();
        let fast = rtt.ping(t0);
        rtt.pong(&fast, t0 + 5 * MS);
        for i in 0..SAMPLES as u32 {
            let p = rtt.ping(t0 + i * 100 * MS);
            rtt.pong(&p, t0 + (i * 100 + 50) * MS);
        }
        // the 5ms sample has aged out
        assert_eq!(rtt.estimate().map(|r| r.low), Some(50 * MS));

        let mut rtt = Rtt::default();
        let first = rtt.ping(t0);
        for _ in 0..IN_FLIGHT {
            rtt.ping(t0);
        }
        rtt.pong(&first, t0 + MS);
        assert_eq!(rtt.estimate(), None);
    }

    #[test]
    fn a_discard_is_timed_from_when_it_was_first_sent() {
        let t0 = Instant::now();
        let mut seen = Seen::default();
        assert_eq!(seen.since(1, t0), None);
        seen.sent(1, t0);
        // a later view of the same discard keeps the first send
        seen.sent(1, t0 + 50 * MS);
        assert_eq!(seen.since(1, t0 + 300 * MS), Some(300 * MS));
        assert_eq!(seen.since(2, t0 + 300 * MS), None);
        seen.sent(2, t0 + 400 * MS);
        assert_eq!(seen.since(1, t0 + 500 * MS), None);
        assert_eq!(seen.since(2, t0 + 500 * MS), Some(100 * MS));
    }

    #[test]
    fn reported_reactions_are_bounded_by_what_the_server_saw() {
        let rtt = Some(RoundTrip {
            low: 80 * MS,
            high: 120 * MS,
        });
        // honest, on a path slower than its best: the report stands
        assert_eq!(reaction_ms(Some(300), 410 * MS, rtt), 300);
        // too fast to be true: no faster than elapsed less the slowest trip
        assert_eq!(reaction_ms(Some(10), 410 * MS, rtt), 290);
        // never slower than the server saw
        assert_eq!(reaction_ms(Some(900), 410 * MS, rtt), 410);
        // without a report, elapsed less the fastest trip
        assert_eq!(reaction_ms(None, 410 * MS, rtt), 330);
        // without a round trip there is no credit
        assert_eq!(reaction_ms(Some(10), 410 * MS, None), 410);
        assert_eq!(reaction_ms(Some(u64::MAX), 410 * MS, None), 410);
    }

    #[test]
    fn credit_is_capped() {
        let rtt = Some(RoundTrip {
            low: MAX_CREDIT * 2,
            high: MAX_CREDIT * 3,
        });
        let elapsed = 1000 * MS;
        let floor = 1000 - MAX_CREDIT.as_millis() as i64;
        assert_eq!(reaction_ms(Some(0), elapsed, rtt), floor);
        assert_eq!(reaction_ms(None, elapsed, rtt), floor);
        // and never below zero
        assert_eq!(reaction_ms(None, 50 * MS, rtt), 0);
    }
}
