//! Link quality and timing statistics for the Diagnostics view and
//! `macwin probe latency`: the datagram ping (RTT, jitter, loss), mouse
//! datagram rates and histograms of the time MacWin itself adds. Pure: every
//! call takes the current time or a measured duration.

use std::collections::VecDeque;
use std::fmt;
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
    /// Measured by the driver, not the Engine.
    pub added: AddedLatency,
}

/// Time MacWin itself adds, per stage (measured in the driver).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AddedLatency {
    /// A forwarded local event: from the capture callback to the message
    /// handed to the link.
    pub send: StageStats,
    /// A peer mouse datagram: from the driver taking it to the injection done.
    pub receive: StageStats,
    /// How late the driver handled its tick: stalls of the session loop and
    /// coarse OS timers.
    pub loop_lag: StageStats,
}

/// p50 / p95 / max of one stage.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StageStats {
    pub count: u64,
    pub p50: Option<Duration>,
    pub p95: Option<Duration>,
    pub max: Option<Duration>,
}

impl From<&Histogram> for StageStats {
    fn from(h: &Histogram) -> StageStats {
        StageStats {
            count: h.count(),
            p50: h.percentile(50.0),
            p95: h.percentile(95.0),
            max: h.max(),
        }
    }
}

/// Stage histograms over the current and the previous minute.
#[derive(Debug, Clone, Default)]
pub struct StageTimes {
    current: [Histogram; 3],
    previous: [Histogram; 3],
}

impl StageTimes {
    const SEND: usize = 0;
    const RECEIVE: usize = 1;
    const LOOP_LAG: usize = 2;

    pub fn record_send(&mut self, d: Duration) {
        self.current[Self::SEND].record(d);
    }

    pub fn record_receive(&mut self, d: Duration) {
        self.current[Self::RECEIVE].record(d);
    }

    pub fn record_loop_lag(&mut self, d: Duration) {
        self.current[Self::LOOP_LAG].record(d);
    }

    /// Starts a new minute: the oldest one is dropped.
    pub fn rotate(&mut self) {
        self.previous = std::mem::take(&mut self.current);
    }

    /// The last one to two minutes.
    pub fn summary(&self) -> AddedLatency {
        let stage = |i: usize| {
            let mut h = self.previous[i].clone();
            h.merge(&self.current[i]);
            StageStats::from(&h)
        };
        AddedLatency {
            send: stage(Self::SEND),
            receive: stage(Self::RECEIVE),
            loop_lag: stage(Self::LOOP_LAG),
        }
    }
}

/// Histogram buckets per doubling: durations are kept to about 9 %.
const STEPS_PER_OCTAVE: f64 = 8.0;
/// 0 µs, then buckets up to 2^24 µs (about 17 s); longer goes in the last.
const BUCKETS: usize = 1 + 24 * 8;

/// A log-scale histogram of durations, cheap enough for every mouse event.
#[derive(Debug, Clone)]
pub struct Histogram {
    counts: [u32; BUCKETS],
    total: u64,
    max_us: u64,
}

impl Default for Histogram {
    fn default() -> Histogram {
        Histogram {
            counts: [0; BUCKETS],
            total: 0,
            max_us: 0,
        }
    }
}

impl Histogram {
    /// Bucket `i > 0` holds [2^((i-1)/8), 2^(i/8)) µs.
    fn bucket(us: u64) -> usize {
        if us == 0 {
            return 0;
        }
        (1 + ((us as f64).log2() * STEPS_PER_OCTAVE) as usize).min(BUCKETS - 1)
    }

    fn upper_us(i: usize) -> f64 {
        if i == 0 {
            0.0
        } else {
            (i as f64 / STEPS_PER_OCTAVE).exp2()
        }
    }

    pub fn record(&mut self, d: Duration) {
        let us = micros(d);
        let i = Self::bucket(us);
        self.counts[i] = self.counts[i].saturating_add(1);
        self.total += 1;
        self.max_us = self.max_us.max(us);
    }

    pub fn merge(&mut self, other: &Histogram) {
        for (c, o) in self.counts.iter_mut().zip(other.counts.iter()) {
            *c = c.saturating_add(*o);
        }
        self.total += other.total;
        self.max_us = self.max_us.max(other.max_us);
    }

    pub fn count(&self) -> u64 {
        self.total
    }

    pub fn max(&self) -> Option<Duration> {
        (self.total > 0).then(|| Duration::from_micros(self.max_us))
    }

    /// The `p`-th percentile (0 to 100): the upper bound of its bucket, never
    /// above the largest value recorded.
    pub fn percentile(&self, p: f64) -> Option<Duration> {
        if self.total == 0 {
            return None;
        }
        let rank = ((p / 100.0) * self.total as f64).ceil().max(1.0) as u64;
        let mut seen = 0u64;
        for (i, &c) in self.counts.iter().enumerate() {
            seen += u64::from(c);
            if seen >= rank {
                let us = Self::upper_us(i).min(self.max_us as f64);
                return Some(Duration::from_micros(us.round() as u64));
            }
        }
        self.max()
    }
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

/// The text `macwin probe latency` prints (and the Diagnostics tab copies),
/// ready to paste into an issue.
pub struct Report<'a> {
    /// How long the measurement ran while connected, if it was timed.
    pub connected_for: Option<Duration>,
    /// This computer, e.g. `"MacBook" (macOS), MacWin 0.2.0`.
    pub this: &'a str,
    /// The other computer, e.g. `"pc" (Windows), MacWin 0.2.0, 192.168.1.20:47800`.
    pub other: &'a str,
    /// The last statistics; `None` if none arrived.
    pub stats: Option<&'a SessionStats>,
    /// Mouse datagram rates: the highest seen while measuring (`peak`) or
    /// the current ones.
    pub out_per_sec: u32,
    pub in_per_sec: u32,
    pub peak: bool,
}

impl fmt::Display for Report<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.connected_for {
            Some(d) => writeln!(f, "MacWin latency report: {} s connected", d.as_secs())?,
            None => writeln!(f, "MacWin latency report")?,
        }
        writeln!(f, "this computer:  {}", self.this)?;
        writeln!(f, "other computer: {}", self.other)?;
        let Some(s) = self.stats else {
            return writeln!(f, "no statistics: the connection did not last long enough");
        };
        let p = &s.ping;
        writeln!(f, "link (datagram ping, last minute):")?;
        writeln!(
            f,
            "  rtt p50 {}, p95 {}, max {}; jitter {}",
            ms(p.rtt_p50),
            ms(p.rtt_p95),
            ms(p.rtt_max),
            ms(p.jitter)
        )?;
        let loss = p
            .loss_percent()
            .map_or_else(|| "-".to_owned(), |l| format!("{l:.1} %"));
        writeln!(
            f,
            "  loss {loss} ({} of {} pings)",
            p.lost,
            p.answered + p.lost
        )?;
        writeln!(
            f,
            "mouse datagrams: {}{}/s sent, {}/s applied; {} skipped of {}",
            if self.peak { "peak " } else { "" },
            self.out_per_sec,
            self.in_per_sec,
            s.mouse_skipped,
            s.mouse_applied + s.mouse_skipped
        )?;
        writeln!(f, "time MacWin adds (p50 / p95 / max, count):")?;
        let a = &s.added;
        for (name, st) in [
            ("send (capture to link)      ", &a.send),
            ("receive (datagram to inject)", &a.receive),
            ("session loop lag            ", &a.loop_lag),
        ] {
            writeln!(
                f,
                "  {name} {} / {} / {} ({})",
                ms(st.p50),
                ms(st.p95),
                ms(st.max),
                st.count
            )?;
        }
        Ok(())
    }
}

/// The statistics on one line, for the log.
pub fn summary_line(s: &SessionStats) -> String {
    let p = &s.ping;
    let loss = p
        .loss_percent()
        .map_or_else(|| "-".to_owned(), |l| format!("{l:.1} %"));
    format!(
        "rtt p50 {} p95 {} max {}, jitter {}, loss {loss} of {}; \
         mouse {}/s out {}/s in, {} skipped of {}; \
         added p95: send {} receive {} loop {}",
        ms(p.rtt_p50),
        ms(p.rtt_p95),
        ms(p.rtt_max),
        ms(p.jitter),
        p.answered + p.lost,
        s.mouse_out_per_sec,
        s.mouse_in_per_sec,
        s.mouse_skipped,
        s.mouse_applied + s.mouse_skipped,
        ms(s.added.send.p95),
        ms(s.added.receive.p95),
        ms(s.added.loop_lag.p95),
    )
}

/// "1.2 ms" (two decimals below a millisecond), "-" when unknown.
pub fn ms(d: Option<Duration>) -> String {
    match d {
        None => "-".to_owned(),
        Some(d) => {
            let v = d.as_secs_f64() * 1000.0;
            if v < 1.0 {
                format!("{v:.2} ms")
            } else {
                format!("{v:.1} ms")
            }
        }
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
    fn histogram_percentiles_are_within_a_bucket() {
        let mut h = Histogram::default();
        assert_eq!((h.count(), h.percentile(50.0), h.max()), (0, None, None));
        // 90 events of 1 ms, 10 of 20 ms.
        for _ in 0..90 {
            h.record(MS);
        }
        for _ in 0..10 {
            h.record(20 * MS);
        }
        assert_eq!(h.count(), 100);
        assert_eq!(h.max(), Some(20 * MS));
        // Capped by the max: exact here.
        assert_eq!(h.percentile(100.0), Some(20 * MS));
        let p50 = h.percentile(50.0).unwrap();
        assert!(p50 >= MS && p50 <= MS * 110 / 100, "{p50:?}");
        let p95 = h.percentile(95.0).unwrap();
        assert!(p95 >= 20 * MS * 90 / 100 && p95 <= 20 * MS, "{p95:?}");
        // Zero and very long durations have buckets too.
        h.record(Duration::ZERO);
        h.record(Duration::from_secs(60));
        assert_eq!(h.percentile(0.0), Some(Duration::ZERO));
        assert_eq!(h.max(), Some(Duration::from_secs(60)));
    }

    #[test]
    fn every_duration_lands_in_a_bucket_whose_bound_covers_it() {
        for us in [
            1u64, 2, 3, 7, 100, 999, 1000, 1024, 4999, 65_536, 16_000_000,
        ] {
            let i = Histogram::bucket(us);
            let upper = Histogram::upper_us(i);
            let lower = if i <= 1 {
                0.0
            } else {
                Histogram::upper_us(i - 1)
            };
            assert!(
                lower <= us as f64 && (us as f64) < upper * 1.000_001,
                "{us} µs in [{lower}, {upper})"
            );
            assert!(
                upper <= us as f64 * 1.1 + 1.0,
                "{us} µs: bound {upper} too loose"
            );
        }
    }

    #[test]
    fn stage_times_cover_the_current_and_the_previous_minute() {
        let mut t = StageTimes::default();
        assert_eq!(t.summary(), AddedLatency::default());
        t.record_send(MS);
        t.record_receive(2 * MS);
        t.record_loop_lag(16 * MS);
        t.rotate();
        t.record_send(3 * MS);
        let s = t.summary();
        assert_eq!((s.send.count, s.send.max), (2, Some(3 * MS)));
        assert_eq!((s.receive.count, s.receive.max), (1, Some(2 * MS)));
        assert_eq!(s.loop_lag.max, Some(16 * MS));
        // Two rotations later the first minute is gone.
        t.rotate();
        t.rotate();
        assert_eq!(t.summary(), AddedLatency::default());
    }

    #[test]
    fn the_summary_line_reads_like_the_report() {
        assert_eq!(ms(None), "-");
        assert_eq!(ms(Some(Duration::from_micros(250))), "0.25 ms");
        assert_eq!(ms(Some(Duration::from_micros(12_345))), "12.3 ms");
        let empty = summary_line(&SessionStats::default());
        assert!(empty.starts_with("rtt p50 - p95 - max -, jitter -, loss - of 0;"));
        let s = SessionStats {
            ping: PingStats {
                rtt_p50: Some(2 * MS),
                rtt_p95: Some(5 * MS),
                rtt_max: Some(9 * MS),
                jitter: Some(Duration::from_micros(700)),
                answered: 238,
                lost: 2,
            },
            mouse_out_per_sec: 125,
            mouse_in_per_sec: 0,
            mouse_applied: 0,
            mouse_skipped: 0,
            added: AddedLatency {
                send: StageStats {
                    count: 10,
                    p95: Some(Duration::from_micros(80)),
                    ..StageStats::default()
                },
                ..AddedLatency::default()
            },
        };
        assert_eq!(
            summary_line(&s),
            "rtt p50 2.0 ms p95 5.0 ms max 9.0 ms, jitter 0.70 ms, loss 0.8 % of 240; \
             mouse 125/s out 0/s in, 0 skipped of 0; \
             added p95: send 0.08 ms receive - loop -"
        );
    }

    #[test]
    fn the_report_lists_link_mouse_and_added_time() {
        let stats = SessionStats {
            ping: PingStats {
                rtt_p50: Some(2 * MS),
                rtt_p95: Some(4 * MS),
                rtt_max: Some(11 * MS),
                jitter: Some(Duration::from_micros(620)),
                answered: 120,
                lost: 0,
            },
            mouse_out_per_sec: 0,
            mouse_in_per_sec: 0,
            mouse_applied: 0,
            mouse_skipped: 0,
            added: AddedLatency {
                send: StageStats {
                    count: 1530,
                    p50: Some(Duration::from_micros(40)),
                    p95: Some(Duration::from_micros(100)),
                    max: Some(Duration::from_micros(350)),
                },
                receive: StageStats::default(),
                loop_lag: StageStats {
                    count: 600,
                    p50: Some(Duration::from_micros(200)),
                    p95: Some(Duration::from_micros(1100)),
                    max: Some(3 * MS),
                },
            },
        };
        let report = Report {
            connected_for: Some(Duration::from_millis(30_400)),
            this: "\"MacBook\" (macOS), MacWin 0.2.0",
            other: "\"pc\" (Windows), MacWin 0.2.0, 192.168.1.20:47800",
            stats: Some(&stats),
            out_per_sec: 125,
            in_per_sec: 0,
            peak: true,
        };
        assert_eq!(
            report.to_string(),
            "MacWin latency report: 30 s connected\n\
             this computer:  \"MacBook\" (macOS), MacWin 0.2.0\n\
             other computer: \"pc\" (Windows), MacWin 0.2.0, 192.168.1.20:47800\n\
             link (datagram ping, last minute):\n\
             \x20 rtt p50 2.0 ms, p95 4.0 ms, max 11.0 ms; jitter 0.62 ms\n\
             \x20 loss 0.0 % (0 of 120 pings)\n\
             mouse datagrams: peak 125/s sent, 0/s applied; 0 skipped of 0\n\
             time MacWin adds (p50 / p95 / max, count):\n\
             \x20 send (capture to link)       0.04 ms / 0.10 ms / 0.35 ms (1530)\n\
             \x20 receive (datagram to inject) - / - / - (0)\n\
             \x20 session loop lag             0.20 ms / 1.1 ms / 3.0 ms (600)\n"
        );
        let live = Report {
            connected_for: None,
            peak: false,
            ..report
        };
        let text = live.to_string();
        assert!(text.starts_with("MacWin latency report\n"), "{text}");
        assert!(
            text.contains("mouse datagrams: 125/s sent, 0/s applied;"),
            "{text}"
        );
        let empty = Report {
            stats: None,
            ..live
        };
        assert!(
            empty
                .to_string()
                .ends_with("no statistics: the connection did not last long enough\n")
        );
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
