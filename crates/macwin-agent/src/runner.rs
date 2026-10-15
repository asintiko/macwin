//! `macwin run`: keeps a session with the paired peer up and reconnects
//! after every disconnect. The machine that knows the peer's address dials
//! it; the other one listens. Only the emergency chord or a stop request
//! (Ctrl+C) ends the loop.
//!
//! With LAN discovery, the listening machine answers its peer's queries and
//! a dialing machine whose dial failed asks where the peer is now, so a
//! changed IP address heals by itself.

use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, bail};
use macwin_core::engine::TransportRole;
use macwin_core::msg::Hello;
use macwin_core::types::RoleKind;
use macwin_net::discovery::{Offer, Responder, find_peer};
use macwin_net::{Fingerprint, Identity, NetError, Session, connect, listen};
use macwin_platform::PlatformError;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::driver::{Backends, Connected, SessionConfig, SessionEnd, kvm_hello, spawn_session};

/// First reconnect delay of a dialer.
pub const RETRY_MIN: Duration = Duration::from_secs(1);
/// Longest reconnect delay of a dialer.
pub const RETRY_MAX: Duration = Duration::from_secs(10);
/// Bound on one dial attempt. A QUIC dial to a machine that is off only
/// fails at the idle timeout otherwise; on a LAN the handshake takes
/// milliseconds.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Bound on letting the close of an ended session reach the peer.
const DRAIN: Duration = Duration::from_secs(2);

/// How this machine finds its peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Listen(SocketAddr),
    /// `HOST:PORT`, resolved again before every attempt.
    Dial(String),
}

/// LAN discovery of the run loop.
#[derive(Debug, Clone)]
pub struct RunDiscovery {
    /// Where a listening machine answers its peer's queries.
    pub responder: SocketAddr,
    /// Where a dialing machine asks after a failed dial.
    pub targets: Vec<SocketAddr>,
    /// How long it waits for the answer.
    pub wait: Duration,
}

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub session: SessionConfig,
    pub mode: Mode,
    /// The paired peer's certificate fingerprint.
    pub peer: Fingerprint,
    /// First delay between two dial attempts (doubling up to `retry_max`),
    /// and the pause before redialing after a session ended.
    pub retry_min: Duration,
    /// Longest delay between two dial attempts.
    pub retry_max: Duration,
    /// Bound on one dial attempt.
    pub connect_timeout: Duration,
    /// `None`: no LAN discovery.
    pub discovery: Option<RunDiscovery>,
}

/// Progress reports for the user.
#[derive(Debug, Clone, PartialEq)]
pub enum RunEvent {
    Listening(SocketAddr),
    Dialing(String),
    Connected {
        name: String,
        remote: SocketAddr,
    },
    /// The peer answered a discovery query from a new address and the
    /// session there is up; later dials use that address.
    PeerFound(SocketAddr),
    Role(RoleKind),
    Ended(SessionEnd),
    /// A connection attempt failed; the next one starts after `after`.
    Retrying {
        after: Duration,
        error: String,
    },
}

/// Why [`run`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunExit {
    /// The stop signal was raised.
    Stopped,
    /// The user pressed the emergency chord.
    EmergencyStop,
}

/// Binds the dialer's socket on loopback for loopback peers, otherwise on
/// the unspecified address of the peer's family.
pub fn bind_for(remote: SocketAddr) -> SocketAddr {
    let ip = match (remote.ip().is_loopback(), remote.is_ipv4()) {
        (true, true) => IpAddr::V4(Ipv4Addr::LOCALHOST),
        (true, false) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        (false, true) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        (false, false) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    };
    SocketAddr::new(ip, 0)
}

/// Runs `bind` again while its port is still held, for up to 3 s: a QUIC
/// endpoint that just closed (the previous session, a pairing listener)
/// releases its socket only once its connections have drained.
pub async fn retry_in_use<T, F: Future<Output = Result<T, NetError>>>(
    mut bind: impl FnMut() -> F,
) -> Result<T, NetError> {
    for _ in 0..30 {
        match bind().await {
            Err(NetError::Io(e)) if e.kind() == io::ErrorKind::AddrInUse => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            other => return other,
        }
    }
    bind().await
}

/// Resolves `HOST:PORT` to its first address.
pub async fn resolve(target: &str) -> anyhow::Result<SocketAddr> {
    tokio::net::lookup_host(target)
        .await
        .with_context(|| format!("cannot resolve {target}"))?
        .next()
        .with_context(|| format!("{target} has no address"))
}

/// One bounded dial attempt.
async fn dial(
    remote: SocketAddr,
    opts: &RunOptions,
    id: &Identity,
    hello: &Hello,
) -> anyhow::Result<Session> {
    tokio::time::timeout(
        opts.connect_timeout,
        connect(bind_for(remote), remote, id, opts.peer, hello),
    )
    .await
    .map_err(|_| anyhow::anyhow!("no answer from {remote}"))?
    .with_context(|| format!("cannot connect to {remote}"))
}

/// Dials `target`; when that fails, asks the LAN where the peer is now and
/// dials the address it answers from. Returns the session and that new
/// address, if it was used.
async fn dial_or_find(
    target: &str,
    opts: &RunOptions,
    id: &Identity,
    hello: &Hello,
) -> anyhow::Result<(Session, Option<SocketAddr>)> {
    let (error, tried) = match resolve(target).await {
        Ok(remote) => match dial(remote, opts, id, hello).await {
            Ok(s) => return Ok((s, None)),
            Err(e) => (e, Some(remote)),
        },
        Err(e) => (e, None),
    };
    let Some(d) = &opts.discovery else {
        return Err(error);
    };
    let found = match find_peer(&d.targets, id.fingerprint(), opts.peer, d.wait).await {
        Ok(Some(found)) if Some(found) != tried => found,
        Ok(_) => return Err(error),
        Err(e) => {
            debug!(error = %e, "LAN discovery failed");
            return Err(error);
        }
    };
    info!(%found, "the other machine answered from a new address");
    match dial(found, opts, id, hello).await {
        Ok(s) => Ok((s, Some(found))),
        Err(e) => Err(error.context(format!("{e:#} (found it at {found})"))),
    }
}

/// Answers the peer's discovery queries while `listener` listens; `None`
/// (after a warning) when its port is unavailable.
async fn peer_responder(
    d: &RunDiscovery,
    listener: &macwin_net::Listener,
    id: &Identity,
    peer: Fingerprint,
) -> anyhow::Result<Option<Responder>> {
    let offer = Offer::Peer {
        port: listener.local_addr()?.port(),
        own: id.fingerprint(),
        peer,
    };
    Ok(match Responder::bind(d.responder, offer).await {
        Ok(r) => Some(r),
        Err(e) => {
            warn!(
                addr = %d.responder,
                error = %e,
                "LAN discovery is unavailable: the other machine cannot find this one after an \
                 address change"
            );
            None
        }
    })
}

/// Resolves when `stop` is raised (or its sender is gone).
async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|s| *s).await;
}

/// Sleeps for `d`; true when the stop signal came first.
async fn sleep_or_stop(d: Duration, stop: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        () = tokio::time::sleep(d) => false,
        () = stopped(stop) => true,
    }
}

/// Runs sessions with the peer until stopped. `make_backends` is called
/// once per connection attempt; `on_event` reports progress.
pub async fn run(
    opts: RunOptions,
    id: Identity,
    mut make_backends: impl FnMut() -> Result<Backends, PlatformError>,
    mut on_event: impl FnMut(RunEvent),
    mut stop: watch::Receiver<bool>,
) -> anyhow::Result<RunExit> {
    let listener = match &opts.mode {
        Mode::Listen(addr) => {
            let l = retry_in_use(|| listen(*addr, &id, opts.peer))
                .await
                .with_context(|| format!("cannot listen on {addr}"))?;
            on_event(RunEvent::Listening(l.local_addr()?));
            Some(l)
        }
        Mode::Dial(_) => None,
    };
    let _responder = match (&listener, &opts.discovery) {
        (Some(l), Some(d)) => peer_responder(d, l, &id, opts.peer).await?,
        _ => None,
    };
    let mut target = match &opts.mode {
        Mode::Dial(t) => t.clone(),
        Mode::Listen(_) => String::new(),
    };
    let retry_min = opts.retry_min.min(opts.retry_max);
    let mut backoff = retry_min;
    loop {
        if *stop.borrow() {
            return Ok(RunExit::Stopped);
        }
        let backends = match make_backends().context("cannot start the input backends") {
            Ok(b) => b,
            Err(e) => {
                on_event(RunEvent::Retrying {
                    after: backoff,
                    error: format!("{e:#}"),
                });
                if sleep_or_stop(backoff, &mut stop).await {
                    return Ok(RunExit::Stopped);
                }
                backoff = (backoff * 2).min(opts.retry_max);
                continue;
            }
        };
        let screens = match backends
            .screens
            .screens()
            .context("cannot read the screens")
        {
            Ok(s) => s,
            Err(e) => {
                on_event(RunEvent::Retrying {
                    after: backoff,
                    error: format!("{e:#}"),
                });
                if sleep_or_stop(backoff, &mut stop).await {
                    return Ok(RunExit::Stopped);
                }
                backoff = (backoff * 2).min(opts.retry_max);
                continue;
            }
        };
        let hello = kvm_hello(&opts.session, screens);
        let attempt: anyhow::Result<(Session, TransportRole, Option<SocketAddr>)> =
            match (&opts.mode, &listener) {
                (Mode::Listen(_), Some(l)) => loop {
                    let accepted = tokio::select! {
                        r = l.accept(&hello) => r,
                        () = stopped(&mut stop) => return Ok(RunExit::Stopped),
                    };
                    match accepted {
                        Ok(s) => break Ok((s, TransportRole::Listener, None)),
                        Err(NetError::EndpointClosed) => {
                            bail!("the listener closed")
                        }
                        Err(e) => {
                            on_event(RunEvent::Retrying {
                                after: Duration::ZERO,
                                error: format!("{e:#}"),
                            });
                        }
                    }
                },
                (Mode::Dial(_), _) => {
                    if backoff == retry_min {
                        on_event(RunEvent::Dialing(target.clone()));
                    }
                    tokio::select! {
                        r = dial_or_find(&target, &opts, &id, &hello) => {
                            r.map(|(s, found)| (s, TransportRole::Dialer, found))
                        }
                        () = stopped(&mut stop) => return Ok(RunExit::Stopped),
                    }
                }
                (Mode::Listen(_), None) => unreachable!("listen mode always has a listener"),
            };
        let (session, role, found) = match attempt {
            Ok(x) => x,
            Err(e) => {
                if matches!(opts.mode, Mode::Listen(_)) {
                    return Err(e);
                }
                let error = format!("{e:#}");
                match opts.mode {
                    // A peer failed the handshake: keep listening.
                    Mode::Listen(_) => on_event(RunEvent::Retrying {
                        after: Duration::ZERO,
                        error,
                    }),
                    Mode::Dial(_) => {
                        on_event(RunEvent::Retrying {
                            after: backoff,
                            error,
                        });
                        if sleep_or_stop(backoff, &mut stop).await {
                            return Ok(RunExit::Stopped);
                        }
                        backoff = (backoff * 2).min(opts.retry_max);
                    }
                }
                continue;
            }
        };
        backoff = retry_min;
        if let Some(addr) = found {
            target = addr.to_string();
            on_event(RunEvent::PeerFound(addr));
        }
        on_event(RunEvent::Connected {
            name: session.peer.device_name.clone(),
            remote: session.remote,
        });
        let handle = match spawn_session(
            opts.session.clone(),
            backends,
            Connected {
                link: session.link,
                peer: session.peer,
                role,
            },
        )
        .context("cannot start the session")
        {
            Ok(h) => h,
            Err(e) => {
                on_event(RunEvent::Retrying {
                    after: backoff,
                    error: format!("{e:#}"),
                });
                if sleep_or_stop(backoff, &mut stop).await {
                    return Ok(RunExit::Stopped);
                }
                backoff = (backoff * 2).min(opts.retry_max);
                continue;
            }
        };
        let stopper = handle.stopper();
        let link = handle.link();
        let mut roles = handle.role.clone();
        let join = handle.join();
        tokio::pin!(join);
        let end = loop {
            tokio::select! {
                end = &mut join => break end,
                changed = roles.changed() => match changed {
                    Ok(()) => on_event(RunEvent::Role(*roles.borrow_and_update())),
                    Err(_) => break (&mut join).await,
                },
                () = stopped(&mut stop) => {
                    stopper.shutdown();
                    break (&mut join).await;
                }
            }
        };
        let _ = tokio::time::timeout(DRAIN, link.wait_closed()).await;
        on_event(RunEvent::Ended(end.clone()));
        if end == SessionEnd::EmergencyStop {
            return Ok(RunExit::EmergencyStop);
        }
        if *stop.borrow() {
            return Ok(RunExit::Stopped);
        }
        // Give a restarting peer a moment before dialing again.
        if matches!(opts.mode, Mode::Dial(_)) && sleep_or_stop(retry_min, &mut stop).await {
            return Ok(RunExit::Stopped);
        }
    }
}
