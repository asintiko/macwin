//! The Engine: the controller state machine shared by both peers.
//!
//! The Engine is pure: no I/O, threads or clocks. Every entry point takes the
//! current `Instant` and returns the actions the caller must execute, in
//! order. One Engine lives for the whole agent; a `Session` exists per
//! connection.
//!
//! Roles per session: `Idle`, `Controlling` (my input is grabbed and
//! forwarded) and `Controlled` (the peer's input is injected here). Each
//! control period has an epoch: the dialer creates odd epochs, the listener
//! even ones, so simultaneous edge crossings resolve deterministically (the
//! higher epoch wins). The machine whose physical input moved last controls:
//! physical input on a controlled machine takes control back.

mod session;
#[cfg(test)]
mod tests;

use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

pub use crate::heartbeat::{HEARTBEAT_INTERVAL, HEARTBEAT_TIMEOUT};
pub use crate::layout::EDGE_REENTRY_INSET;

use crate::arrange::{Placement, resolve};
use crate::geometry::ScreenSet;
use crate::heartbeat::HeartbeatMonitor;
use crate::keys::{ButtonSet, Pressed};
use crate::layout::{Layout, Step};
use crate::msg::{
    ButtonMsg, CAP_INPUT_INJECT, CAP_WHEEL_PIXEL, ControlMsg, DatagramMsg, Enter, Heartbeat,
    InputMsg, KeyMsg, Leave, MouseAbs, Pong, TakeControl, WheelMsg,
};
use crate::os::pixels_to_notch120;
use crate::remap::Remap;
use crate::types::{
    CaptureFault, CapturedEvent, GeometryError, GrabCommand, Hid, InjectCommand, ModifierMask, Os,
    PointF, RoleKind, ScreenInfo, WheelUnit,
};
use session::{Controlled, Controlling, LocalInput, Role, Session};

/// How often the driver calls [`Engine::on_tick`].
pub const TICK: Duration = Duration::from_millis(50);
/// How long a grab survives without renewal before the capture backend
/// releases it on its own.
pub const GRAB_LEASE: Duration = Duration::from_millis(2000);
/// Physical movement (|dx| + |dy|, native units) on a controlled machine that
/// takes control back.
pub const TAKEOVER_THRESHOLD: f64 = 8.0;
/// Movement accumulator reset interval for the takeover rule.
pub const TAKEOVER_WINDOW: Duration = Duration::from_millis(500);

/// All Engine timings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timings {
    pub tick: Duration,
    pub heartbeat_interval: Duration,
    pub heartbeat_timeout: Duration,
    pub grab_lease: Duration,
    pub takeover_threshold: f64,
    pub takeover_window: Duration,
}

impl Default for Timings {
    fn default() -> Timings {
        Timings {
            tick: TICK,
            heartbeat_interval: HEARTBEAT_INTERVAL,
            heartbeat_timeout: HEARTBEAT_TIMEOUT,
            grab_lease: GRAB_LEASE,
            takeover_threshold: TAKEOVER_THRESHOLD,
            takeover_window: TAKEOVER_WINDOW,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EngineConfig {
    pub local_os: Os,
    pub local_screens: Vec<ScreenInfo>,
    /// Where I placed the peer's screens next to mine: `Some` on the primary
    /// computer (or for a legacy left/right setting), `None` on a secondary,
    /// which follows the placement in the peer's Hello.
    pub placement: Option<Placement>,
    /// Apply the Ctrl <-> Cmd/Win remap as a controller.
    pub remap: bool,
    pub timings: Timings,
}

/// Which side opened the connection; decides epoch parity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportRole {
    Dialer,
    Listener,
}

/// What the Engine needs to know about the peer (from its Hello).
#[derive(Debug, Clone, PartialEq)]
pub struct PeerInfo {
    pub os: Os,
    pub screens: Vec<ScreenInfo>,
    pub capabilities: Vec<String>,
    pub device_name: String,
    /// Where the peer placed my screens next to its own, if it is the
    /// primary computer.
    pub placement: Option<Placement>,
}

/// Result of a local input event.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalOutcome {
    /// True: let the event reach the local OS. False: swallow it.
    pub deliver: bool,
    pub actions: Vec<Action>,
}

/// Something the driver must do, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    SendControl(ControlMsg),
    SendInput(InputMsg),
    SendDatagram(DatagramMsg),
    Inject(InjectCommand),
    Capture(GrabCommand),
    /// Extend the capture backend's grab lease until the given instant.
    RenewLease(Instant),
    /// Close the connection; the session is already gone.
    Close(CloseReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    HeartbeatTimeout,
    ProtocolViolation(&'static str),
    Shutdown,
    Internal(&'static str),
}

/// The controller state machine.
#[derive(Debug, Clone)]
pub struct Engine {
    local_os: Os,
    local: ScreenSet,
    placement: Option<Placement>,
    remap_enabled: bool,
    timings: Timings,
    input: LocalInput,
    session: Option<Session>,
}

fn deliver(actions: Vec<Action>) -> LocalOutcome {
    LocalOutcome {
        deliver: true,
        actions,
    }
}

fn swallow(actions: Vec<Action>) -> LocalOutcome {
    LocalOutcome {
        deliver: false,
        actions,
    }
}

fn key_up(hid: Hid) -> InjectCommand {
    InjectCommand::Key {
        hid,
        down: false,
        repeat: false,
    }
}

/// Bookkeeping whenever Controlling ends: keys and buttons that are still
/// physically held were forwarded (swallowed locally), so their repeats and
/// ups must not reach the local OS.
fn end_controlling(input: &mut LocalInput, c: &Controlling) {
    input.held_from_remote = input.keys.difference(&c.pre_grab);
    input.buttons_from_remote = input.buttons;
}

fn release_target(c: &mut Controlled, actions: &mut Vec<Action>) {
    actions.extend(
        c.injected
            .release_all(c.last_pos)
            .into_iter()
            .map(Action::Inject),
    );
}

/// Ends the current role: Controlling ungrabs at `inset(exit, exit_edge)` (optionally
/// telling the peer with a Leave first), Controlled releases everything it
/// injected. The role becomes Idle.
fn leave_role(
    input: &mut LocalInput,
    s: &mut Session,
    actions: &mut Vec<Action>,
    send_leave: bool,
) {
    match std::mem::replace(&mut s.role, Role::Idle) {
        Role::Idle => {}
        Role::Controlling(c) => {
            if send_leave {
                actions.push(Action::SendInput(InputMsg::Leave(Leave { epoch: c.epoch })));
            }
            actions.push(Action::Capture(GrabCommand::Release {
                warp_to: s.layout.inset(c.exit, c.exit_edge),
            }));
            end_controlling(input, &c);
            info!(epoch = c.epoch, "stopped controlling the peer");
        }
        Role::Controlled(mut c) => {
            release_target(&mut c, actions);
            info!(epoch = c.epoch, "no longer controlled by the peer");
        }
    }
}

impl Engine {
    pub fn new(cfg: EngineConfig) -> Result<Engine, GeometryError> {
        Ok(Engine {
            local_os: cfg.local_os,
            local: ScreenSet::new(cfg.local_screens)?,
            placement: cfg.placement,
            remap_enabled: cfg.remap,
            timings: cfg.timings,
            input: LocalInput::default(),
            session: None,
        })
    }

    /// Replaces my screens. Sessions started afterwards use them; a running
    /// session keeps the layout it started with.
    pub fn set_local_screens(&mut self, screens: Vec<ScreenInfo>) -> Result<(), GeometryError> {
        self.local = ScreenSet::new(screens)?;
        Ok(())
    }

    pub fn role(&self) -> RoleKind {
        self.session
            .as_ref()
            .map_or(RoleKind::Idle, |s| s.role.kind())
    }

    pub fn has_session(&self) -> bool {
        self.session.is_some()
    }

    pub fn timings(&self) -> Timings {
        self.timings
    }

    /// A connection to the peer is established. A previous session, if any,
    /// ends first (its release actions are returned).
    pub fn on_connected(
        &mut self,
        now: Instant,
        peer: PeerInfo,
        role: TransportRole,
    ) -> Vec<Action> {
        let mut actions = self.on_disconnected(now);
        let peer_screens = match ScreenSet::new(peer.screens.clone()) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "peer announced invalid screens");
                actions.push(Action::Close(CloseReason::ProtocolViolation(
                    "invalid peer screens",
                )));
                return actions;
            }
        };
        // My own placement wins; a secondary mirrors the primary's.
        let map = self
            .placement
            .map(|p| resolve(&self.local, &peer_screens, &p))
            .or_else(|| {
                peer.placement
                    .map(|p| resolve(&peer_screens, &self.local, &p).inverse())
            });
        let session = Session {
            layout: Layout::new(self.local.clone(), peer_screens, map),
            remap: Remap::new(self.local_os, peer.os, self.remap_enabled),
            parity: match role {
                TransportRole::Dialer => 1,
                TransportRole::Listener => 0,
            },
            epoch_max: 0,
            role: Role::Idle,
            heartbeat: HeartbeatMonitor::new(
                now,
                self.timings.heartbeat_interval,
                self.timings.heartbeat_timeout,
            ),
            peer_can_inject: peer.capabilities.iter().any(|c| c == CAP_INPUT_INJECT),
            peer_takes_pixels: peer.capabilities.iter().any(|c| c == CAP_WHEEL_PIXEL),
            seq: 0,
            last_seq: 0,
            peer,
        };
        info!(peer = %session.peer.device_name, ?role, "session started");
        self.session = Some(session);
        actions
    }

    /// The connection is gone: release whatever this machine injected and
    /// ungrab local input.
    pub fn on_disconnected(&mut self, _now: Instant) -> Vec<Action> {
        let Some(mut s) = self.session.take() else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        leave_role(&mut self.input, &mut s, &mut actions, false);
        info!("session ended");
        actions
    }

    /// Graceful stop: tell the peer, release, and close the connection.
    pub fn shutdown(&mut self, _now: Instant) -> Vec<Action> {
        let Some(mut s) = self.session.take() else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        leave_role(&mut self.input, &mut s, &mut actions, true);
        actions.push(Action::Close(CloseReason::Shutdown));
        info!("session shut down");
        actions
    }

    fn violation(&mut self, now: Instant, reason: &'static str) -> Vec<Action> {
        warn!(reason, "peer protocol violation");
        let mut actions = self.on_disconnected(now);
        actions.push(Action::Close(CloseReason::ProtocolViolation(reason)));
        actions
    }

    /// Periodic driver tick (every [`TICK`]).
    pub fn on_tick(&mut self, now: Instant) -> Vec<Action> {
        let Some(mut s) = self.session.take() else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        if s.heartbeat.timed_out(now) {
            warn!("peer heartbeat timed out");
            leave_role(&mut self.input, &mut s, &mut actions, false);
            actions.push(Action::Close(CloseReason::HeartbeatTimeout));
            return actions;
        }
        if s.heartbeat.due(now) {
            actions.push(Action::SendControl(ControlMsg::Heartbeat(Heartbeat {
                epoch: s.heartbeat_epoch(),
                role: s.role.kind(),
            })));
            s.heartbeat.on_sent(now);
        }
        if matches!(s.role, Role::Controlling(_)) {
            actions.push(Action::RenewLease(now + self.timings.grab_lease));
        }
        self.session = Some(s);
        actions
    }

    /// A capture fail-safe fired; the backend has already released locally.
    pub fn on_capture_fault(&mut self, _now: Instant, fault: CaptureFault) -> Vec<Action> {
        let mut actions = Vec::new();
        if let Some(s) = self.session.as_mut() {
            if matches!(s.role, Role::Controlling(_)) {
                if let Role::Controlling(c) = std::mem::replace(&mut s.role, Role::Idle) {
                    actions.push(Action::SendInput(InputMsg::Leave(Leave { epoch: c.epoch })));
                    end_controlling(&mut self.input, &c);
                    info!(
                        epoch = c.epoch,
                        ?fault,
                        "capture fault while controlling; back to idle"
                    );
                }
            } else {
                debug!(?fault, "capture fault");
            }
        }
        actions.push(Action::Capture(GrabCommand::Rearm));
        actions
    }

    /// A physical local input event from the capture backend.
    pub fn on_local_event(&mut self, now: Instant, ev: CapturedEvent) -> LocalOutcome {
        let ev = self.track_local(ev);
        match self.role() {
            RoleKind::Idle => self.local_idle(now, ev),
            RoleKind::Controlling => self.local_controlling(ev),
            RoleKind::Controlled => self.local_controlled(now, ev),
        }
    }

    /// Updates the physical state; a key down for an already-pressed key
    /// becomes a repeat (Windows reports repeats as plain downs).
    fn track_local(&mut self, ev: CapturedEvent) -> CapturedEvent {
        match ev {
            CapturedEvent::Key { hid, down, repeat } => {
                let repeat = repeat || (down && self.input.keys.contains(hid));
                if down {
                    self.input.keys.insert(hid);
                } else {
                    self.input.keys.remove(hid);
                }
                CapturedEvent::Key { hid, down, repeat }
            }
            CapturedEvent::Button { button, down, .. } => {
                if down {
                    self.input.buttons.insert(button);
                } else {
                    self.input.buttons.remove(button);
                }
                ev
            }
            CapturedEvent::MouseMove { .. } | CapturedEvent::Wheel { .. } => ev,
        }
    }

    /// Swallows repeats and ups of keys/buttons whose down went to the peer.
    fn swallow_remote_held(&mut self, ev: CapturedEvent) -> bool {
        match ev {
            CapturedEvent::Key { hid, down, repeat }
                if self.input.held_from_remote.contains(hid) && (repeat || !down) =>
            {
                if !down {
                    self.input.held_from_remote.remove(hid);
                }
                true
            }
            CapturedEvent::Button {
                button,
                down: false,
                ..
            } => self.input.buttons_from_remote.remove(button),
            _ => false,
        }
    }

    fn local_idle(&mut self, now: Instant, ev: CapturedEvent) -> LocalOutcome {
        if self.swallow_remote_held(ev) {
            return swallow(Vec::new());
        }
        match ev {
            CapturedEvent::MouseMove { pos, delta } => self.try_enter(now, pos, delta),
            _ => deliver(Vec::new()),
        }
    }

    fn try_enter(&mut self, now: Instant, pos: PointF, delta: PointF) -> LocalOutcome {
        let Some(s) = self.session.as_mut() else {
            return deliver(Vec::new());
        };
        if !matches!(s.role, Role::Idle)
            || !s.peer_can_inject
            || !self.input.buttons.is_empty()
            || !pos.is_finite()
        {
            return deliver(Vec::new());
        }
        let Some(crossing) = s.layout.try_enter(pos, delta) else {
            return deliver(Vec::new());
        };
        let epoch = s.next_epoch();
        s.epoch_max = epoch;
        let mods = s.remap.map_mask(self.input.keys.modifier_mask());
        let actions = vec![
            Action::SendInput(InputMsg::Enter(Enter {
                epoch,
                at: crossing.entry.floor(),
            })),
            Action::SendInput(InputMsg::ModifierSync(mods)),
            Action::Capture(GrabCommand::Grab {
                exit: crossing.exit,
            }),
            Action::RenewLease(now + self.timings.grab_lease),
        ];
        s.role = Role::Controlling(Controlling {
            epoch,
            v: crossing.entry,
            exit: crossing.exit,
            exit_edge: crossing.edge,
            npl_src: crossing.npl_src,
            pre_grab: self.input.keys.difference(&self.input.held_from_remote),
            fwd_buttons: ButtonSet::new(),
        });
        info!(epoch, "entered the peer's screen");
        swallow(actions)
    }

    fn local_controlling(&mut self, ev: CapturedEvent) -> LocalOutcome {
        let Some(s) = self.session.as_mut() else {
            return deliver(Vec::new());
        };
        let Role::Controlling(c) = &mut s.role else {
            return deliver(Vec::new());
        };
        match ev {
            CapturedEvent::Key { hid, down, repeat } => {
                if c.pre_grab.contains(hid) {
                    if down {
                        // Repeat of a key the local OS already has: not forwarded.
                        return swallow(Vec::new());
                    }
                    // The local OS saw the down, so it gets the up; the peer
                    // gets it too (it may hold the key via ModifierSync).
                    c.pre_grab.remove(hid);
                    return deliver(vec![Action::SendInput(InputMsg::Key(KeyMsg {
                        hid: s.remap.map_hid(hid),
                        down: false,
                        repeat: false,
                    }))]);
                }
                if !down {
                    self.input.held_from_remote.remove(hid);
                }
                swallow(vec![Action::SendInput(InputMsg::Key(KeyMsg {
                    hid: s.remap.map_hid(hid),
                    down,
                    repeat,
                }))])
            }
            CapturedEvent::Button { button, down, .. } => {
                if down {
                    c.fwd_buttons.insert(button);
                } else {
                    c.fwd_buttons.remove(button);
                    self.input.buttons_from_remote.remove(button);
                }
                swallow(vec![Action::SendInput(InputMsg::Button(ButtonMsg {
                    button,
                    down,
                    at: c.v.floor(),
                }))])
            }
            CapturedEvent::Wheel { dx, dy, unit } => {
                let w = if unit == WheelUnit::Pixel && !s.peer_takes_pixels {
                    WheelMsg {
                        dx: pixels_to_notch120(dx),
                        dy: pixels_to_notch120(dy),
                        unit: WheelUnit::Notch120,
                    }
                } else {
                    WheelMsg { dx, dy, unit }
                };
                swallow(vec![Action::SendInput(InputMsg::Wheel(w))])
            }
            CapturedEvent::MouseMove { delta, .. } => {
                match s
                    .layout
                    .step(c.v, delta, c.npl_src, !c.fwd_buttons.is_empty())
                {
                    Step::Moved(v) => {
                        c.v = v;
                        s.seq += 1;
                        swallow(vec![Action::SendDatagram(DatagramMsg::MouseAbs(
                            MouseAbs {
                                epoch: c.epoch,
                                seq: s.seq,
                                at: v.floor(),
                            },
                        ))])
                    }
                    Step::Exited { warp_to } => {
                        let epoch = c.epoch;
                        if let Role::Controlling(c) = std::mem::replace(&mut s.role, Role::Idle) {
                            end_controlling(&mut self.input, &c);
                        }
                        info!(epoch, "cursor came back from the peer");
                        swallow(vec![
                            Action::SendInput(InputMsg::Leave(Leave { epoch })),
                            Action::Capture(GrabCommand::Release { warp_to }),
                        ])
                    }
                }
            }
        }
    }

    fn local_controlled(&mut self, now: Instant, ev: CapturedEvent) -> LocalOutcome {
        if self.swallow_remote_held(ev) {
            return swallow(Vec::new());
        }
        let threshold = self.timings.takeover_threshold;
        let window = self.timings.takeover_window;
        let Some(s) = self.session.as_mut() else {
            return deliver(Vec::new());
        };
        let Role::Controlled(c) = &mut s.role else {
            return deliver(Vec::new());
        };
        let takeover = match ev {
            CapturedEvent::Key { down, repeat, .. } => down && !repeat,
            CapturedEvent::Button { down, .. } => down,
            CapturedEvent::Wheel { .. } => true,
            CapturedEvent::MouseMove { delta, .. } => {
                if c.last_phys_move
                    .is_some_and(|t| now.saturating_duration_since(t) > window)
                {
                    c.takeover_accum = 0.0;
                }
                if delta.is_finite() {
                    c.takeover_accum += delta.x.abs() + delta.y.abs();
                }
                c.last_phys_move = Some(now);
                c.takeover_accum >= threshold
            }
        };
        if !takeover {
            return deliver(Vec::new());
        }
        let mut actions = Vec::new();
        if let Role::Controlled(mut c) = std::mem::replace(&mut s.role, Role::Idle) {
            release_target(&mut c, &mut actions);
            actions.push(Action::SendInput(InputMsg::TakeControl(TakeControl {
                epoch: c.epoch,
            })));
            info!(epoch = c.epoch, "local input took control back");
        }
        deliver(actions)
    }

    /// A message from the peer's control stream.
    pub fn on_control(&mut self, now: Instant, m: ControlMsg) -> Vec<Action> {
        let Some(s) = self.session.as_mut() else {
            return Vec::new();
        };
        s.heartbeat.on_received(now);
        match m {
            ControlMsg::Hello(_) => self.violation(now, "unexpected hello"),
            ControlMsg::Ping(p) => vec![Action::SendControl(ControlMsg::Pong(Pong {
                id: p.id,
                ping_sent_micros: p.sent_micros,
            }))],
            ControlMsg::Pong(_) => Vec::new(),
            ControlMsg::Heartbeat(hb) => self.reconcile(hb),
        }
    }

    /// Heartbeat reconciliation: a peer reporting my current epoch with a
    /// role that is not the complement of mine has already ended that epoch.
    fn reconcile(&mut self, hb: Heartbeat) -> Vec<Action> {
        let Some(s) = self.session.as_mut() else {
            return Vec::new();
        };
        let stale = match &s.role {
            Role::Controlling(c) => c.epoch == hb.epoch && hb.role != RoleKind::Controlled,
            Role::Controlled(c) => c.epoch == hb.epoch && hb.role != RoleKind::Controlling,
            Role::Idle => false,
        };
        if !stale {
            return Vec::new();
        }
        info!(epoch = hb.epoch, peer_role = ?hb.role, "peer already left this epoch");
        let mut actions = Vec::new();
        leave_role(&mut self.input, s, &mut actions, false);
        actions
    }

    /// A message from the peer's input stream.
    pub fn on_input(&mut self, now: Instant, m: InputMsg) -> Vec<Action> {
        let Some(s) = self.session.as_mut() else {
            return Vec::new();
        };
        s.heartbeat.on_received(now);
        match m {
            InputMsg::Ping(p) => vec![Action::SendInput(InputMsg::Pong(Pong {
                id: p.id,
                ping_sent_micros: p.sent_micros,
            }))],
            InputMsg::Pong(_) => Vec::new(),
            InputMsg::Enter(e) => self.peer_enter(now, e),
            InputMsg::Leave(l) => self.peer_leave(l),
            InputMsg::TakeControl(t) => self.peer_take_control(t),
            InputMsg::Key(k) => self.inject_key(k),
            InputMsg::Button(b) => self.inject_button(b),
            InputMsg::Wheel(w) => self.inject_wheel(w),
            InputMsg::ModifierSync(mask) => self.inject_modifier_sync(mask),
        }
    }

    fn peer_enter(&mut self, now: Instant, en: Enter) -> Vec<Action> {
        let Some(s) = self.session.as_mut() else {
            return Vec::new();
        };
        if en.epoch == 0 || en.epoch % 2 != s.peer_parity() {
            return self.violation(now, "enter epoch has the wrong parity");
        }
        if en.epoch <= s.epoch_max {
            debug!(epoch = en.epoch, max = s.epoch_max, "stale enter ignored");
            return Vec::new();
        }
        s.epoch_max = en.epoch;
        let mut actions = Vec::new();
        leave_role(&mut self.input, s, &mut actions, false);
        let at = s.layout.local().clamp(en.at);
        actions.push(Action::Inject(InjectCommand::MoveTo(at)));
        s.role = Role::Controlled(Controlled {
            epoch: en.epoch,
            injected: Pressed::default(),
            last_pos: at,
            takeover_accum: 0.0,
            last_phys_move: None,
        });
        info!(epoch = en.epoch, "the peer took control of this machine");
        actions
    }

    fn peer_leave(&mut self, l: Leave) -> Vec<Action> {
        let Some(s) = self.session.as_mut() else {
            return Vec::new();
        };
        match &s.role {
            Role::Controlled(c) if c.epoch == l.epoch => {
                let mut actions = Vec::new();
                leave_role(&mut self.input, s, &mut actions, false);
                actions
            }
            _ => {
                debug!(epoch = l.epoch, "leave for another epoch ignored");
                Vec::new()
            }
        }
    }

    fn peer_take_control(&mut self, t: TakeControl) -> Vec<Action> {
        let Some(s) = self.session.as_mut() else {
            return Vec::new();
        };
        match &s.role {
            Role::Controlling(c) if c.epoch == t.epoch => {
                info!(epoch = t.epoch, "the peer took control back");
                let mut actions = Vec::new();
                leave_role(&mut self.input, s, &mut actions, false);
                actions
            }
            _ => {
                debug!(epoch = t.epoch, "stale take-control ignored");
                Vec::new()
            }
        }
    }

    fn controlled(&mut self) -> Option<(&mut Controlled, &ScreenSet)> {
        let s = self.session.as_mut()?;
        match &mut s.role {
            Role::Controlled(c) => Some((c, s.layout.local())),
            _ => None,
        }
    }

    fn inject_key(&mut self, k: KeyMsg) -> Vec<Action> {
        let Some((c, _)) = self.controlled() else {
            return Vec::new();
        };
        let keys = &mut c.injected.keys;
        let cmd = if k.down && !k.repeat {
            let repeat = !keys.insert(k.hid);
            Some(InjectCommand::Key {
                hid: k.hid,
                down: true,
                repeat,
            })
        } else if k.down {
            keys.contains(k.hid).then_some(InjectCommand::Key {
                hid: k.hid,
                down: true,
                repeat: true,
            })
        } else {
            keys.remove(k.hid).then(|| key_up(k.hid))
        };
        cmd.map(Action::Inject).into_iter().collect()
    }

    fn inject_button(&mut self, b: ButtonMsg) -> Vec<Action> {
        let Some((c, local)) = self.controlled() else {
            return Vec::new();
        };
        let at = local.clamp(b.at);
        c.last_pos = at;
        let mut actions = vec![Action::Inject(InjectCommand::MoveTo(at))];
        let changed = if b.down {
            c.injected.buttons.insert(b.button)
        } else {
            c.injected.buttons.remove(b.button)
        };
        if changed {
            actions.push(Action::Inject(InjectCommand::Button {
                button: b.button,
                down: b.down,
                at,
            }));
        }
        actions
    }

    fn inject_wheel(&mut self, w: WheelMsg) -> Vec<Action> {
        if self.controlled().is_none() {
            return Vec::new();
        }
        vec![Action::Inject(InjectCommand::Wheel {
            dx: w.dx,
            dy: w.dy,
            unit: w.unit,
        })]
    }

    /// Releases injected modifiers missing from `mask`, then presses the
    /// missing ones.
    fn inject_modifier_sync(&mut self, mask: ModifierMask) -> Vec<Action> {
        let Some((c, _)) = self.controlled() else {
            return Vec::new();
        };
        let keys = &mut c.injected.keys;
        let mut actions = Vec::new();
        for hid in keys.modifier_mask().keys() {
            if !mask.contains(hid) {
                keys.remove(hid);
                actions.push(Action::Inject(key_up(hid)));
            }
        }
        for hid in mask.keys() {
            if keys.insert(hid) {
                actions.push(Action::Inject(InjectCommand::Key {
                    hid,
                    down: true,
                    repeat: false,
                }));
            }
        }
        actions
    }

    /// A datagram from the peer.
    pub fn on_datagram(&mut self, now: Instant, m: DatagramMsg) -> Vec<Action> {
        let Some(s) = self.session.as_mut() else {
            return Vec::new();
        };
        s.heartbeat.on_received(now);
        match m {
            DatagramMsg::Ping(p) => vec![Action::SendDatagram(DatagramMsg::Pong(Pong {
                id: p.id,
                ping_sent_micros: p.sent_micros,
            }))],
            DatagramMsg::Pong(_) => Vec::new(),
            DatagramMsg::MouseAbs(ma) => {
                let Role::Controlled(c) = &mut s.role else {
                    return Vec::new();
                };
                if ma.epoch != c.epoch || ma.seq <= s.last_seq {
                    return Vec::new();
                }
                s.last_seq = ma.seq;
                let at = s.layout.local().clamp(ma.at);
                c.last_pos = at;
                vec![Action::Inject(InjectCommand::MoveTo(at))]
            }
        }
    }
}
