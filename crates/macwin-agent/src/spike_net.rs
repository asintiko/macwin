//! `macwin spike net`: round-trip latency over the reliable input stream and
//! over datagrams, between two machines that pinned each other's identity.

use std::collections::HashSet;
use std::fmt;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use macwin_core::heartbeat::{HEARTBEAT_INTERVAL, HeartbeatMonitor};
use macwin_core::msg::{
    CAP_SPIKE_NET, ControlMsg, DatagramMsg, Heartbeat, Hello, InputMsg, Ping, Pong,
};
use macwin_core::types::{Os, Rect, RoleKind, ScreenInfo};
use macwin_net::link::{Inbound, Link, LinkClosed, LinkSender};
use macwin_net::{Session, codes};
use macwin_proto::PROTOCOL_VERSION;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use tracing::debug;

/// How long the dialer waits for missing pongs after its last ping.
pub const PONG_GRACE: Duration = Duration::from_secs(2);
/// Datagram ping ids start here so late stream pongs never match.
const DATAGRAM_ID_BASE: u64 = 1 << 32;

/// The Hello of the latency tool. `spike net` has no screens of its own, so
/// it announces a 1x1 placeholder (the protocol requires at least one).
pub fn spike_hello(os: Os, device_name: &str) -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        capabilities: vec![CAP_SPIKE_NET.into()],
        os,
        os_version: String::new(),
        device_name: device_name.into(),
        screens: vec![ScreenInfo {
            id: 0,
            rect: Rect::new(0, 0, 1, 1).expect("valid placeholder"),
            native_per_logical: 1.0,
            primary: true,
        }],
        peer_side: None,
        placement: None,
        agent_version: env!("CARGO_PKG_VERSION").into(),
    }
}

/// Dialer options.
#[derive(Debug, Clone, Copy)]
pub struct SpikeOpts {
    /// Pings per path.
    pub count: u32,
    /// Pause between pings.
    pub interval: Duration,
}

/// Latency of one path, nearest-rank percentiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatencyStats {
    pub sent: usize,
    pub received: usize,
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,
    pub max: Duration,
}

impl LatencyStats {
    pub fn from_samples(sent: usize, mut samples: Vec<Duration>) -> LatencyStats {
        samples.sort_unstable();
        let rank = |p: f64| -> Duration {
            if samples.is_empty() {
                return Duration::ZERO;
            }
            let k = ((p / 100.0) * samples.len() as f64).ceil() as usize;
            samples[k.clamp(1, samples.len()) - 1]
        };
        LatencyStats {
            sent,
            received: samples.len(),
            p50: rank(50.0),
            p95: rank(95.0),
            p99: rank(99.0),
            max: samples.last().copied().unwrap_or_default(),
        }
    }

    pub fn lost(&self) -> usize {
        self.sent.saturating_sub(self.received)
    }
}

fn ms(d: Duration) -> String {
    format!("{:.3}ms", d.as_secs_f64() * 1000.0)
}

impl fmt::Display for LatencyStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "n={} lost={} p50={} p95={} p99={} max={}",
            self.received,
            self.lost(),
            ms(self.p50),
            ms(self.p95),
            ms(self.p99),
            ms(self.max)
        )
    }
}

/// Both paths of one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub stream: LatencyStats,
    pub datagram: LatencyStats,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "stream   {}", self.stream)?;
        write!(f, "datagram {}", self.datagram)
    }
}

fn heartbeat() -> ControlMsg {
    ControlMsg::Heartbeat(Heartbeat {
        epoch: 0,
        role: RoleKind::Idle,
    })
}

fn heartbeat_ticker() -> tokio::time::Interval {
    let mut t = tokio::time::interval(HEARTBEAT_INTERVAL);
    t.set_missed_tick_behavior(MissedTickBehavior::Delay);
    t
}

/// Answers every ping on the channel it arrived on until the peer closes.
/// Returns Ok when the peer closes normally.
pub async fn run_listener(session: Session) -> anyhow::Result<()> {
    let Session {
        link: Link { tx, mut rx, .. },
        ..
    } = session;
    let mut hb = HeartbeatMonitor::with_defaults(Instant::now());
    let mut ticker = heartbeat_ticker();
    loop {
        tokio::select! {
            msg = rx.recv() => {
                hb.on_received(Instant::now());
                match msg {
                    Some(Inbound::Control(ControlMsg::Ping(p))) => tx.send_control(ControlMsg::Pong(pong(p)))?,
                    Some(Inbound::Input(InputMsg::Ping(p))) => tx.send_input(InputMsg::Pong(pong(p)))?,
                    Some(Inbound::Datagram(DatagramMsg::Ping(p))) => tx.send_datagram(DatagramMsg::Pong(pong(p))),
                    Some(Inbound::Closed(LinkClosed::PeerClosed { code })) if code == u64::from(codes::NORMAL) => return Ok(()),
                    Some(Inbound::Closed(reason)) => bail!("the link closed: {reason:?}"),
                    Some(other) => debug!(?other, "ignored"),
                    None => bail!("the link went away"),
                }
            }
            _ = ticker.tick() => {
                if hb.timed_out(Instant::now()) {
                    tx.close_now(codes::HEARTBEAT_TIMEOUT);
                    bail!("the peer stopped sending heartbeats");
                }
                tx.send_control(heartbeat())?;
            }
        }
    }
}

fn pong(p: Ping) -> Pong {
    Pong {
        id: p.id,
        ping_sent_micros: p.sent_micros,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Path {
    Stream,
    Datagram,
}

/// Measures the stream path, then the datagram path, then closes the link
/// normally and waits for the close to reach the peer.
pub async fn run_dialer(session: Session, opts: SpikeOpts) -> anyhow::Result<Report> {
    let Session {
        link: Link { tx, mut rx, .. },
        ..
    } = session;
    let anchor = Instant::now();
    let mut hb = HeartbeatMonitor::with_defaults(anchor);
    let stream = measure(&tx, &mut rx, &mut hb, Path::Stream, opts, anchor).await?;
    let datagram = measure(&tx, &mut rx, &mut hb, Path::Datagram, opts, anchor).await?;
    tx.close(codes::NORMAL);
    tokio::time::timeout(Duration::from_secs(3), tx.wait_closed())
        .await
        .context("the link did not close in time")?;
    Ok(Report { stream, datagram })
}

async fn measure(
    tx: &LinkSender,
    rx: &mut mpsc::Receiver<Inbound>,
    hb: &mut HeartbeatMonitor,
    path: Path,
    opts: SpikeOpts,
    anchor: Instant,
) -> anyhow::Result<LatencyStats> {
    let count = opts.count as usize;
    let base = match path {
        Path::Stream => 0,
        Path::Datagram => DATAGRAM_ID_BASE,
    };
    let mut send_tick = tokio::time::interval(opts.interval.max(Duration::from_millis(1)));
    send_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut hb_tick = heartbeat_ticker();
    let mut sent = 0usize;
    let mut seen = HashSet::new();
    let mut samples = Vec::with_capacity(count);
    let mut deadline: Option<tokio::time::Instant> = None;
    let micros = |at: Instant| at.duration_since(anchor).as_micros() as u64;
    while samples.len() < count {
        let grace = async {
            match deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = send_tick.tick(), if sent < count => {
                let ping = Ping { id: base + sent as u64, sent_micros: micros(Instant::now()) };
                match path {
                    Path::Stream => tx.send_input(InputMsg::Ping(ping))?,
                    Path::Datagram => tx.send_datagram(DatagramMsg::Ping(ping)),
                }
                sent += 1;
                if sent == count {
                    deadline = Some(tokio::time::Instant::now() + PONG_GRACE);
                }
            }
            msg = rx.recv() => {
                let now = Instant::now();
                hb.on_received(now);
                let pong = match msg {
                    Some(Inbound::Input(InputMsg::Pong(p))) if path == Path::Stream => p,
                    Some(Inbound::Datagram(DatagramMsg::Pong(p))) if path == Path::Datagram => p,
                    Some(Inbound::Closed(reason)) => bail!("the link closed during the measurement: {reason:?}"),
                    None => bail!("the link went away"),
                    Some(_) => continue,
                };
                let in_range = pong.id >= base && pong.id < base + count as u64;
                if in_range && seen.insert(pong.id) {
                    samples.push(Duration::from_micros(micros(now).saturating_sub(pong.ping_sent_micros)));
                }
            }
            _ = hb_tick.tick() => {
                if hb.timed_out(Instant::now()) {
                    tx.close_now(codes::HEARTBEAT_TIMEOUT);
                    bail!("the peer stopped sending heartbeats");
                }
                tx.send_control(heartbeat())?;
            }
            () = grace => break,
        }
    }
    Ok(LatencyStats::from_samples(sent, samples))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn nearest_rank_percentiles() {
        let samples: Vec<Duration> = (1..=100).rev().map(d).collect();
        let s = LatencyStats::from_samples(100, samples);
        assert_eq!((s.p50, s.p95, s.p99, s.max), (d(50), d(95), d(99), d(100)));
        assert_eq!((s.received, s.lost()), (100, 0));

        let s = LatencyStats::from_samples(5, vec![d(3), d(1), d(2)]);
        assert_eq!((s.p50, s.p95, s.p99, s.max), (d(2), d(3), d(3), d(3)));
        assert_eq!(s.lost(), 2);

        let s = LatencyStats::from_samples(1, vec![d(7)]);
        assert_eq!((s.p50, s.p99), (d(7), d(7)));
    }

    #[test]
    fn empty_samples_report_everything_lost() {
        let s = LatencyStats::from_samples(10, vec![]);
        assert_eq!((s.received, s.lost(), s.max), (0, 10, Duration::ZERO));
        assert_eq!(
            s.to_string(),
            "n=0 lost=10 p50=0.000ms p95=0.000ms p99=0.000ms max=0.000ms"
        );
    }

    #[test]
    fn display_format() {
        let s = LatencyStats::from_samples(
            2,
            vec![Duration::from_micros(412), Duration::from_micros(1500)],
        );
        assert_eq!(
            s.to_string(),
            "n=2 lost=0 p50=0.412ms p95=1.500ms p99=1.500ms max=1.500ms"
        );
        let r = Report {
            stream: s.clone(),
            datagram: s,
        };
        assert!(
            r.to_string().starts_with("stream   n=2") && r.to_string().contains("\ndatagram n=2")
        );
    }
}
