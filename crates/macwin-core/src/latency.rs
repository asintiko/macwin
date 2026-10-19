//! Link quality statistics for the Diagnostics view and
//! `macwin probe latency`: the datagram ping (RTT, jitter, loss) and mouse
//! datagram rates. Pure: every call takes the current time.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::msg::{Ping, Pong};

/// How often a session pings over the datagram path (the heartbeat rate).
pub const PING_INTERVAL: Duration = Duration::from_millis(250);
/// An unanswered ping counts as lost after this long.
pub const PING_TIMEOUT: Duration = Duration::from_secs(1);
/// Ping outcomes kept for the statistics: one minute at the ping rate.
pub const PING_WINDOW: usize = 240;

/// RTT, jitter and loss of the datagram path, from the pings this side sends.
/// Only the local clock is used, so the peer's clock does not matter.
#[derive(Debug, Clone)]
pub struct LinkProbe {
    /// Zero of the `sent_micros` clock.
    base: Instant,
    next_id: u64,
    next_due: Instant,
    /// Unanswered pings, oldest first.
    pending: VecDeque<(u64, Instant)>,
    /// RTTs of the answered pings in the window, microseconds.
    rtts: VecDeque<u32>,
    /// Outcomes in the window: answered (true) or lost.
    outcomes: VecDeque<bool>,
    last_rtt: Option<u32>,
    /// Smoothed RTT variation (the RFC 3550 estimator), microseconds.
    jitter: f64,
}

impl LinkProbe {
    pub fn new(now: Instant) -> LinkProbe {
        LinkProbe {
            base: now,
            next_id: 1,
            next_due: now,
            pending: VecDeque::new(),
            rtts: VecDeque::new(),
            outcomes: VecDeque::new(),
            last_rtt: None,
            jitter: 0.0,
        }
    }

    /// The ping to send now, if one is due.
    pub fn poll(&mut self, now: Instant) -> Option<Ping> {
        self.expire(now);
        if now < self.next_due {
            return None;
        }
        // From now, not from the missed deadline: late ticks do not bunch pings.
        self.next_due = now + PING_INTERVAL;
        let id = self.next_id;
        self.next_id += 1;
        self.pending.push_back((id, now));
        Some(Ping {
            id,
            sent_micros: micros(now.saturating_duration_since(self.base)),
        })
    }

    /// Records the answer to one of my pings; unknown, duplicate and expired
    /// ids are ignored.
    pub fn on_pong(&mut self, now: Instant, pong: Pong) {
        let Some(i) = self.pending.iter().position(|&(id, _)| id == pong.id) else {
            return;
        };
        let Some((_, sent)) = self.pending.remove(i) else {
            return;
        };
        let rtt = u32::try_from(micros(now.saturating_duration_since(sent))).unwrap_or(u32::MAX);
        if let Some(prev) = self.last_rtt {
            let d = f64::from(rtt.abs_diff(prev));
            self.jitter += (d - self.jitter) / 16.0;
        }
        self.last_rtt = Some(rtt);
        push_bounded(&mut self.rtts, rtt);
        push_bounded(&mut self.outcomes, true);
    }

    fn expire(&mut self, now: Instant) {
        while let Some(&(_, sent)) = self.pending.front() {
            if now.saturating_duration_since(sent) < PING_TIMEOUT {
                break;
            }
            self.pending.pop_front();
            push_bounded(&mut self.outcomes, false);
        }
    }

    pub fn stats(&self) -> PingStats {
        let rtts: Vec<u32> = self.rtts.iter().copied().collect();
        let answered = self.outcomes.iter().filter(|&&ok| ok).count();
        PingStats {
            rtt_p50: percentile(&rtts, 50.0),
            rtt_p95: percentile(&rtts, 95.0),
            rtt_max: rtts.iter().max().map(|&m| from_micros(m)),
            jitter: (rtts.len() >= 2).then(|| Duration::from_secs_f64(self.jitter / 1e6)),
            answered: count(answered),
            lost: count(self.outcomes.len() - answered),
        }
    }
}

/// The datagram ping over the last [`PING_WINDOW`] pings.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PingStats {
    pub rtt_p50: Option<Duration>,
    pub rtt_p95: Option<Duration>,
    pub rtt_max: Option<Duration>,
    pub jitter: Option<Duration>,
    pub answered: u32,
    pub lost: u32,
}

impl PingStats {
    /// Lost pings in percent of the answered and lost ones.
    pub fn loss_percent(&self) -> Option<f64> {
        let total = self.answered + self.lost;
        (total > 0).then(|| f64::from(self.lost) * 100.0 / f64::from(total))
    }
}

/// What the Diagnostics view shows about the current session.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SessionStats {
    pub ping: PingStats,
    /// Mouse datagrams sent in the last second while controlling the peer.
    pub mouse_out_per_sec: u32,
    /// Mouse datagrams applied in the last second while controlled.
    pub mouse_in_per_sec: u32,
    /// Mouse datagrams applied this session.
    pub mouse_applied: u64,
    /// Mouse datagrams of a control period that were never applied (lost on
    /// the way, or overtaken by a newer one).
    pub mouse_skipped: u64,
}

const SLOT: Duration = Duration::from_millis(100);
const SLOTS: u64 = 10;

/// Counts events in the last second, in ten 100 ms slots.
#[derive(Debug, Clone, Default)]
pub struct RateMeter {
    base: Option<Instant>,
    /// Number (since `base`) of the newest slot with events.
    newest: u64,
    counts: [u32; SLOTS as usize],
}

impl RateMeter {
    fn slot(base: Instant, now: Instant) -> u64 {
        let n = now.saturating_duration_since(base).as_millis() / SLOT.as_millis();
        u64::try_from(n).unwrap_or(u64::MAX)
    }

    pub fn record(&mut self, now: Instant) {
        let base = *self.base.get_or_insert(now);
        let slot = Self::slot(base, now);
        if slot > self.newest {
            for k in 1..=(slot - self.newest).min(SLOTS) {
                self.counts[((self.newest + k) % SLOTS) as usize] = 0;
            }
            self.newest = slot;
        }
        let i = (self.newest % SLOTS) as usize;
        self.counts[i] = self.counts[i].saturating_add(1);
    }

    /// Events in the last second (0.9 to 1 s, by slot alignment).
    pub fn per_second(&self, now: Instant) -> u32 {
        let Some(base) = self.base else {
            return 0;
        };
        let now_slot = Self::slot(base, now);
        (0..SLOTS)
            .map_while(|k| self.newest.checked_sub(k))
            .take_while(|&s| s + SLOTS > now_slot)
            .map(|s| self.counts[(s % SLOTS) as usize])
            .fold(0u32, u32::saturating_add)
    }
}

/// Nearest-rank percentile (`p` from 0 to 100) of microsecond samples.
pub fn percentile(samples: &[u32], p: f64) -> Option<Duration> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    Some(from_micros(sorted[rank.clamp(1, sorted.len()) - 1]))
}

fn push_bounded<T>(q: &mut VecDeque<T>, v: T) {
    if q.len() == PING_WINDOW {
        q.pop_front();
    }
    q.push_back(v);
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

fn from_micros(us: u32) -> Duration {
    Duration::from_micros(u64::from(us))
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn answer(p: &mut LinkProbe, now: Instant, ping: Ping) {
        p.on_pong(
            now,
            Pong {
                id: ping.id,
                ping_sent_micros: ping.sent_micros,
            },
        );
    }

    #[test]
    fn pings_go_out_every_interval_with_the_local_clock() {
        let t0 = Instant::now();
        let mut p = LinkProbe::new(t0);
        let first = p.poll(t0).expect("the first ping is due at once");
        assert_eq!((first.id, first.sent_micros), (1, 0));
        assert_eq!(p.poll(t0 + 100 * MS), None);
        assert_eq!(p.poll(t0 + 249 * MS), None);
        let second = p.poll(t0 + 250 * MS).unwrap();
        assert_eq!((second.id, second.sent_micros), (2, 250_000));
        // A late tick does not make up for the missed pings.
        let third = p.poll(t0 + 900 * MS).unwrap();
        assert_eq!(third.id, 3);
        assert_eq!(p.poll(t0 + 1000 * MS), None);
        assert!(p.poll(t0 + 1150 * MS).is_some());
    }

    #[test]
    fn rtt_percentiles_and_jitter_come_from_the_answers() {
        let t0 = Instant::now();
        let mut p = LinkProbe::new(t0);
        assert_eq!(p.stats(), PingStats::default());
        // RTTs 2, 4, 2, 4 ... ms: p50 2 ms, p95 4 ms, jitter towards 2 ms.
        let mut now = t0;
        for i in 0..40u32 {
            let ping = p.poll(now).unwrap();
            let rtt = if i % 2 == 0 { 2 * MS } else { 4 * MS };
            answer(&mut p, now + rtt, ping);
            now += PING_INTERVAL;
        }
        let s = p.stats();
        assert_eq!(s.rtt_p50, Some(2 * MS));
        assert_eq!(s.rtt_p95, Some(4 * MS));
        assert_eq!(s.rtt_max, Some(4 * MS));
        let jitter = s.jitter.unwrap();
        assert!(jitter > MS * 3 / 2 && jitter <= 2 * MS, "{jitter:?}");
        assert_eq!((s.answered, s.lost), (40, 0));
        assert_eq!(s.loss_percent(), Some(0.0));
    }

    #[test]
    fn unanswered_pings_count_as_lost_after_the_timeout() {
        let t0 = Instant::now();
        let mut p = LinkProbe::new(t0);
        let a = p.poll(t0).unwrap();
        let _b = p.poll(t0 + 250 * MS).unwrap();
        answer(&mut p, t0 + 260 * MS, a);
        // Not lost yet: the timeout has not passed.
        assert!(p.poll(t0 + 500 * MS).is_some());
        assert_eq!(p.stats().lost, 0);
        // b (sent at 250 ms) times out at 1250 ms.
        assert!(p.poll(t0 + 1250 * MS).is_some());
        let s = p.stats();
        assert_eq!((s.answered, s.lost), (1, 1));
        assert_eq!(s.loss_percent(), Some(50.0));
        assert_eq!(s.jitter, None, "one RTT has no variation");
    }

    #[test]
    fn late_duplicate_and_unknown_pongs_are_ignored() {
        let t0 = Instant::now();
        let mut p = LinkProbe::new(t0);
        let a = p.poll(t0).unwrap();
        // Expired: counted as lost, its late answer changes nothing.
        p.poll(t0 + 1000 * MS);
        answer(&mut p, t0 + 1100 * MS, a);
        let b = p.poll(t0 + 1250 * MS).unwrap();
        answer(&mut p, t0 + 1255 * MS, b);
        answer(&mut p, t0 + 1300 * MS, b);
        p.on_pong(
            t0 + 1300 * MS,
            Pong {
                id: 999,
                ping_sent_micros: 0,
            },
        );
        let s = p.stats();
        assert_eq!((s.answered, s.lost), (1, 1));
        assert_eq!(s.rtt_max, Some(5 * MS));
    }

    #[test]
    fn the_window_keeps_the_last_minute() {
        let t0 = Instant::now();
        let mut p = LinkProbe::new(t0);
        let mut now = t0;
        // A slow minute, then a fast one: only the fast one remains.
        for rtt in [50 * MS, MS] {
            for _ in 0..PING_WINDOW {
                let ping = p.poll(now).unwrap();
                answer(&mut p, now + rtt, ping);
                now += PING_INTERVAL;
            }
        }
        let s = p.stats();
        assert_eq!(s.rtt_max, Some(MS));
        assert_eq!(s.answered as usize, PING_WINDOW);
    }

    #[test]
    fn percentile_uses_the_nearest_rank() {
        assert_eq!(percentile(&[], 50.0), None);
        assert_eq!(percentile(&[7], 95.0), Some(Duration::from_micros(7)));
        let v: Vec<u32> = (1..=100).collect();
        assert_eq!(percentile(&v, 50.0), Some(Duration::from_micros(50)));
        assert_eq!(percentile(&v, 95.0), Some(Duration::from_micros(95)));
        assert_eq!(percentile(&v, 100.0), Some(Duration::from_micros(100)));
        assert_eq!(percentile(&v, 0.0), Some(Duration::from_micros(1)));
    }

    #[test]
    fn rate_meter_counts_the_last_second() {
        let t0 = Instant::now();
        let mut r = RateMeter::default();
        assert_eq!(r.per_second(t0), 0);
        // 1000 Hz for 2 s.
        for i in 0..2000u32 {
            r.record(t0 + Duration::from_micros(u64::from(i) * 1000));
        }
        let rate = r.per_second(t0 + 2000 * MS);
        assert!((900..=1000).contains(&rate), "{rate}");
        // Half a second later only half of the window has events.
        let half = r.per_second(t0 + 2500 * MS);
        assert!((400..=500).contains(&half), "{half}");
        // A second after the last event the rate is zero.
        assert_eq!(r.per_second(t0 + 3100 * MS), 0);
        // A new burst after a long pause starts from scratch.
        r.record(t0 + 60_000 * MS);
        assert_eq!(r.per_second(t0 + 60_000 * MS), 1);
    }
}
