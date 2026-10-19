//! Per-connection and long-lived Engine state.

use std::time::Instant;

use crate::arrange::Edge;
use crate::heartbeat::HeartbeatMonitor;
use crate::keys::{ButtonSet, KeySet, Pressed};
use crate::latency::{LinkProbe, RateMeter};
use crate::layout::Layout;
use crate::remap::Remap;
use crate::shortcuts::ZoomSteps;
use crate::types::{Point, PointF, RoleKind};

use super::PeerInfo;

/// Physical local input, tracked for the whole Engine lifetime.
#[derive(Debug, Clone, Default)]
pub(crate) struct LocalInput {
    /// Physically pressed keys.
    pub(crate) keys: KeySet,
    /// Physically pressed mouse buttons.
    pub(crate) buttons: ButtonSet,
    /// Keys whose down was forwarded to the peer (swallowed locally) and that
    /// were still held when the grab ended: their repeats and ups must not
    /// reach the local OS.
    pub(crate) held_from_remote: KeySet,
    /// Same for mouse buttons.
    pub(crate) buttons_from_remote: ButtonSet,
}

/// One control period in which this machine drives the peer.
#[derive(Debug, Clone)]
pub(crate) struct Controlling {
    pub(crate) epoch: u64,
    /// Virtual cursor, peer native coordinates (always on a peer screen).
    pub(crate) v: PointF,
    /// Where the local cursor is parked.
    pub(crate) exit: Point,
    /// The side of my screen the cursor left through.
    pub(crate) exit_edge: Edge,
    pub(crate) npl_src: f64,
    /// Keys the local OS saw go down before the grab.
    pub(crate) pre_grab: KeySet,
    /// Buttons forwarded down and not yet up.
    pub(crate) fwd_buttons: ButtonSet,
    /// `Some` while Fn / 🌐 is held: `true` until another key or a button
    /// goes down with it. Released alone, it switches the peer's language.
    pub(crate) fn_tap: Option<bool>,
    /// Pinch magnification not yet sent as a zoom step.
    pub(crate) zoom: ZoomSteps,
}

/// One control period in which the peer drives this machine.
#[derive(Debug, Clone)]
pub(crate) struct Controlled {
    pub(crate) epoch: u64,
    /// Injected and not yet released.
    pub(crate) injected: Pressed,
    pub(crate) last_pos: Point,
    pub(crate) takeover_accum: f64,
    pub(crate) last_phys_move: Option<Instant>,
}

#[derive(Debug, Clone)]
pub(crate) enum Role {
    Idle,
    Controlling(Controlling),
    Controlled(Controlled),
}

impl Role {
    pub(crate) fn kind(&self) -> RoleKind {
        match self {
            Role::Idle => RoleKind::Idle,
            Role::Controlling(_) => RoleKind::Controlling,
            Role::Controlled(_) => RoleKind::Controlled,
        }
    }

    pub(crate) fn epoch(&self) -> Option<u64> {
        match self {
            Role::Idle => None,
            Role::Controlling(c) => Some(c.epoch),
            Role::Controlled(c) => Some(c.epoch),
        }
    }
}

/// State of one connection to the peer.
#[derive(Debug, Clone)]
pub(crate) struct Session {
    pub(crate) peer: PeerInfo,
    pub(crate) layout: Layout,
    pub(crate) remap: Remap,
    /// Parity of the epochs I create: 1 (odd) for the dialer, 0 (even) for
    /// the listener.
    pub(crate) parity: u64,
    /// Highest epoch sent or received in an Enter.
    pub(crate) epoch_max: u64,
    pub(crate) role: Role,
    pub(crate) heartbeat: HeartbeatMonitor,
    pub(crate) peer_can_inject: bool,
    /// The peer injects pixel wheel deltas ([`crate::msg::CAP_WHEEL_PIXEL`]).
    pub(crate) peer_takes_pixels: bool,
    /// Last datagram sequence number sent.
    pub(crate) seq: u64,
    /// Last datagram sequence number accepted.
    pub(crate) last_seq: u64,
    /// Epoch of the datagram that set `last_seq`.
    pub(crate) last_seq_epoch: u64,
    /// The datagram ping and its statistics.
    pub(crate) probe: LinkProbe,
    pub(crate) mouse_out: RateMeter,
    pub(crate) mouse_in: RateMeter,
    pub(crate) mouse_applied: u64,
    pub(crate) mouse_skipped: u64,
}

impl Session {
    pub(crate) fn peer_parity(&self) -> u64 {
        1 - self.parity
    }

    /// The smallest epoch above `epoch_max` with my parity.
    pub(crate) fn next_epoch(&self) -> u64 {
        let mut e = self.epoch_max.saturating_add(1);
        if e % 2 != self.parity {
            e = e.saturating_add(1);
        }
        e
    }

    /// The epoch reported in heartbeats.
    pub(crate) fn heartbeat_epoch(&self) -> u64 {
        self.role.epoch().unwrap_or(self.epoch_max)
    }
}
