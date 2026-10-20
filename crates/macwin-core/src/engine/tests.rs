//! Engine scenario tests (design §5.9) and the interleaving property test.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use proptest::prelude::*;

use super::*;
use crate::arrange::{Edge, Placement};
use crate::geometry::tests::screen;
use crate::msg::{CAP_INPUT_CAPTURE, Hello, Ping};
use crate::types::{GesturePhase, MouseButton, Point, Side};

const MS: Duration = Duration::from_millis(1);
const KEY_A: Hid = Hid(0x07_0004);
const KEY_C: Hid = Hid(0x07_0006);

fn mac_screens() -> Vec<ScreenInfo> {
    vec![screen(1, 0, 0, 1440, 900, 1.0)]
}

fn win_screens(npl: f64) -> Vec<ScreenInfo> {
    vec![screen(7, 0, 0, 1920, 1080, npl)]
}

/// An engine whose placement of the peer is the legacy left/right `side`.
fn engine_with(os: Os, screens: Vec<ScreenInfo>, side: Side, remap: bool) -> Engine {
    engine_placing(os, screens, Some(Placement::side(side)), remap)
}

fn engine_placing(
    os: Os,
    screens: Vec<ScreenInfo>,
    placement: Option<Placement>,
    remap: bool,
) -> Engine {
    Engine::new(EngineConfig {
        local_os: os,
        local_screens: screens,
        placement,
        remap,
        timings: Timings::default(),
    })
    .unwrap()
}

fn peer_with(os: Os, screens: Vec<ScreenInfo>, caps: &[&str]) -> PeerInfo {
    PeerInfo {
        os,
        screens,
        capabilities: caps.iter().map(|c| c.to_string()).collect(),
        device_name: "peer".into(),
        placement: None,
    }
}

fn full_caps() -> [&'static str; 2] {
    [CAP_INPUT_CAPTURE, CAP_INPUT_INJECT]
}

/// A Mac with a Windows PC to its right.
fn mac_session(t0: Instant, role: TransportRole) -> Engine {
    let mut e = engine_with(Os::MacOs, mac_screens(), Side::Right, true);
    assert_eq!(
        e.on_connected(
            t0,
            peer_with(Os::Windows, win_screens(1.0), &full_caps()),
            role
        ),
        vec![]
    );
    e
}

/// A Windows PC (scale `npl`) with a Mac to its left.
fn win_session(t0: Instant, role: TransportRole, npl: f64) -> Engine {
    let mut e = engine_with(Os::Windows, win_screens(npl), Side::Left, true);
    assert_eq!(
        e.on_connected(t0, peer_with(Os::MacOs, mac_screens(), &full_caps()), role),
        vec![]
    );
    e
}

fn mv(x: f64, y: f64) -> CapturedEvent {
    CapturedEvent::MouseMove {
        pos: PointF::new(x, y),
        delta: PointF::default(),
    }
}

fn delta(dx: f64, dy: f64) -> CapturedEvent {
    CapturedEvent::MouseMove {
        pos: PointF::default(),
        delta: PointF::new(dx, dy),
    }
}

fn key(hid: Hid, down: bool) -> CapturedEvent {
    CapturedEvent::Key {
        hid,
        down,
        repeat: false,
    }
}

fn button(b: MouseButton, down: bool) -> CapturedEvent {
    CapturedEvent::Button {
        button: b,
        down,
        pos: PointF::default(),
    }
}

fn delivered() -> LocalOutcome {
    LocalOutcome {
        deliver: true,
        actions: vec![],
    }
}

fn swallowed(actions: Vec<Action>) -> LocalOutcome {
    LocalOutcome {
        deliver: false,
        actions,
    }
}

fn mask(keys: &[Hid]) -> ModifierMask {
    let mut m = ModifierMask::EMPTY;
    for &k in keys {
        m.insert(k);
    }
    m
}

fn send_key(hid: Hid, down: bool, repeat: bool) -> Action {
    Action::SendInput(InputMsg::Key(KeyMsg { hid, down, repeat }))
}

fn inj_key(hid: Hid, down: bool, repeat: bool) -> Action {
    Action::Inject(InjectCommand::Key { hid, down, repeat })
}

fn enter_actions(
    t: Instant,
    epoch: u64,
    at: Point,
    mods: ModifierMask,
    exit: Point,
) -> Vec<Action> {
    vec![
        Action::SendInput(InputMsg::Enter(Enter { epoch, at })),
        Action::SendInput(InputMsg::ModifierSync(mods)),
        Action::Capture(GrabCommand::Grab { exit }),
        Action::RenewLease(t + GRAB_LEASE),
    ]
}

/// Mac (dialer) pushes into the PC: Controlling(1), virtual cursor (0, 540).
fn mac_controlling(t0: Instant) -> Engine {
    let mut e = mac_session(t0, TransportRole::Dialer);
    let out = e.on_local_event(t0, mv(1439.5, 450.0));
    assert_eq!(
        out,
        swallowed(enter_actions(
            t0,
            1,
            Point::new(0, 540),
            ModifierMask::EMPTY,
            Point::new(1439, 450)
        ))
    );
    e
}

/// PC (listener) controlled by the Mac at epoch 1.
fn win_controlled(t0: Instant) -> Engine {
    let mut e = win_session(t0, TransportRole::Listener, 1.0);
    assert_eq!(
        e.on_input(
            t0,
            InputMsg::Enter(Enter {
                epoch: 1,
                at: Point::new(0, 540)
            })
        ),
        vec![Action::Inject(InjectCommand::MoveTo(Point::new(0, 540)))]
    );
    assert_eq!(e.role(), RoleKind::Controlled);
    e
}

// 1
#[test]
fn enter_at_right_and_left_edges_with_remapped_modifier_sync() {
    let t0 = Instant::now();
    let mut e = mac_session(t0, TransportRole::Dialer);
    assert_eq!(e.on_local_event(t0, key(Hid::LEFT_GUI, true)), delivered());
    assert_eq!(
        e.on_local_event(t0, mv(1439.5, 450.0)),
        swallowed(enter_actions(
            t0,
            1,
            Point::new(0, 540),
            mask(&[Hid::LEFT_CTRL]),
            Point::new(1439, 450)
        ))
    );
    assert_eq!(e.role(), RoleKind::Controlling);

    let mut w = win_session(t0, TransportRole::Listener, 1.0);
    assert_eq!(
        w.on_local_event(t0, mv(0.0, 540.0)),
        swallowed(enter_actions(
            t0,
            2,
            Point::new(1439, 450),
            ModifierMask::EMPTY,
            Point::new(0, 540)
        ))
    );
    assert_eq!(w.role(), RoleKind::Controlling);
}

// 2
#[test]
fn no_enter_without_session_capability_or_with_button_held() {
    let t0 = Instant::now();
    let mut lonely = engine_with(Os::MacOs, mac_screens(), Side::Right, true);
    assert_eq!(lonely.on_local_event(t0, mv(1439.5, 450.0)), delivered());
    assert_eq!(lonely.role(), RoleKind::Idle);

    let mut no_inject = engine_with(Os::MacOs, mac_screens(), Side::Right, true);
    no_inject.on_connected(
        t0,
        peer_with(Os::Windows, win_screens(1.0), &[CAP_INPUT_CAPTURE]),
        TransportRole::Dialer,
    );
    assert_eq!(no_inject.on_local_event(t0, mv(1439.5, 450.0)), delivered());

    let mut e = mac_session(t0, TransportRole::Dialer);
    assert_eq!(
        e.on_local_event(t0, button(MouseButton::Left, true)),
        delivered()
    );
    assert_eq!(
        e.on_local_event(t0, mv(1439.5, 450.0)),
        delivered(),
        "no crossing mid-drag"
    );
    assert_eq!(
        e.on_local_event(t0, button(MouseButton::Left, false)),
        delivered()
    );
    assert_eq!(e.on_local_event(t0, mv(1439.5, 450.0)).actions.len(), 4);
}

// 3
#[test]
fn datagram_seq_increases_and_speed_is_normalised() {
    let t0 = Instant::now();
    let mut w = win_session(t0, TransportRole::Listener, 1.5);
    assert_eq!(w.on_local_event(t0, mv(0.0, 540.0)).actions.len(), 4);
    // Epoch 4: the listener's second control period (the first one ends below).
    let abs = |seq, x, y| {
        swallowed(vec![Action::SendDatagram(DatagramMsg::MouseAbs(
            MouseAbs {
                epoch: 4,
                seq,
                at: Point::new(x, y),
            },
        ))])
    };
    // 3 physical pixels at 150% are 2 points on the Mac.
    assert_eq!(
        w.on_local_event(t0, delta(3.0, 0.0)),
        swallowed(vec![
            Action::SendInput(InputMsg::Leave(Leave { epoch: 2 })),
            Action::Capture(GrabCommand::Release {
                warp_to: Point::new(2, 540)
            }),
        ]),
        "moving right from the Mac's right edge leaves"
    );
    assert_eq!(w.on_local_event(t0, mv(0.0, 540.0)).actions.len(), 4);
    assert_eq!(w.on_local_event(t0, delta(-3.0, 0.0)), abs(1, 1437, 450));
    assert_eq!(w.on_local_event(t0, delta(-1.5, 0.0)), abs(2, 1436, 450));
    assert_eq!(w.on_local_event(t0, delta(0.0, 1.5)), abs(3, 1436, 451));
}

// 4
#[test]
fn leave_on_crossing_back_then_no_immediate_reentry() {
    let t0 = Instant::now();
    let mut e = mac_controlling(t0);
    assert_eq!(
        e.on_local_event(t0, delta(-5.0, 0.0)),
        swallowed(vec![
            Action::SendInput(InputMsg::Leave(Leave { epoch: 1 })),
            Action::Capture(GrabCommand::Release {
                warp_to: Point::new(1437, 450)
            }),
        ])
    );
    assert_eq!(e.role(), RoleKind::Idle);
    assert_eq!(e.on_local_event(t0, mv(1437.0, 450.0)), delivered());
    assert_eq!(
        e.on_local_event(t0, mv(1439.0, 450.0)),
        swallowed(enter_actions(
            t0,
            3,
            Point::new(0, 540),
            ModifierMask::EMPTY,
            Point::new(1439, 450)
        ))
    );
}

// 5
#[test]
fn no_leave_while_a_forwarded_button_is_held() {
    let t0 = Instant::now();
    let mut e = mac_controlling(t0);
    let at = Point::new(0, 540);
    assert_eq!(
        e.on_local_event(t0, button(MouseButton::Left, true)),
        swallowed(vec![Action::SendInput(InputMsg::Button(ButtonMsg {
            button: MouseButton::Left,
            down: true,
            at
        }))])
    );
    assert_eq!(
        e.on_local_event(t0, delta(-5.0, 0.0)),
        swallowed(vec![Action::SendDatagram(DatagramMsg::MouseAbs(
            MouseAbs {
                epoch: 1,
                seq: 1,
                at
            }
        ))])
    );
    assert_eq!(e.role(), RoleKind::Controlling);
    e.on_local_event(t0, button(MouseButton::Left, false));
    assert_eq!(
        e.on_local_event(t0, delta(-5.0, 0.0)).actions.len(),
        2,
        "leave + release"
    );
    assert_eq!(e.role(), RoleKind::Idle);
}

// 6
#[test]
fn remapped_forwarding_and_repeat_detection() {
    let t0 = Instant::now();
    let mut e = mac_controlling(t0);
    assert_eq!(
        e.on_local_event(t0, key(Hid::LEFT_GUI, true)),
        swallowed(vec![send_key(Hid::LEFT_CTRL, true, false)])
    );
    assert_eq!(
        e.on_local_event(t0, key(KEY_C, true)),
        swallowed(vec![send_key(KEY_C, true, false)])
    );
    // A second down without an up (Windows-style repeat) becomes a repeat.
    assert_eq!(
        e.on_local_event(t0, key(KEY_C, true)),
        swallowed(vec![send_key(KEY_C, true, true)])
    );
    assert_eq!(
        e.on_local_event(t0, key(KEY_C, false)),
        swallowed(vec![send_key(KEY_C, false, false)])
    );
    assert_eq!(
        e.on_local_event(t0, key(Hid::LEFT_GUI, false)),
        swallowed(vec![send_key(Hid::LEFT_CTRL, false, false)])
    );
    assert_eq!(
        e.on_local_event(t0, key(Hid::LEFT_ALT, true)),
        swallowed(vec![send_key(Hid::LEFT_ALT, true, false)])
    );

    let mut plain = engine_with(Os::MacOs, mac_screens(), Side::Right, false);
    plain.on_connected(
        t0,
        peer_with(Os::Windows, win_screens(1.0), &full_caps()),
        TransportRole::Dialer,
    );
    plain.on_local_event(t0, mv(1439.5, 450.0));
    assert_eq!(
        plain.on_local_event(t0, key(Hid::LEFT_GUI, true)),
        swallowed(vec![send_key(Hid::LEFT_GUI, true, false)])
    );
}

#[test]
fn pixel_scrolling_reaches_peers_that_take_it_and_whole_units_otherwise() {
    let t0 = Instant::now();
    let pixels = |dx, dy| CapturedEvent::Wheel {
        dx,
        dy,
        unit: WheelUnit::Pixel,
    };
    let sent = |dx, dy, unit| {
        swallowed(vec![Action::SendInput(InputMsg::Wheel(WheelMsg {
            dx,
            dy,
            unit,
        }))])
    };
    // The default peer predates pixel scrolling: 40 px make one notch.
    let mut old = mac_controlling(t0);
    assert_eq!(
        old.on_local_event(t0, pixels(-2, 40)),
        sent(-6, 120, WheelUnit::Notch120)
    );
    let mut new = engine_with(Os::MacOs, mac_screens(), Side::Right, true);
    new.on_connected(
        t0,
        peer_with(
            Os::Windows,
            win_screens(1.0),
            &[CAP_INPUT_CAPTURE, CAP_INPUT_INJECT, CAP_WHEEL_PIXEL],
        ),
        TransportRole::Dialer,
    );
    new.on_local_event(t0, mv(1439.5, 450.0));
    assert_eq!(new.role(), RoleKind::Controlling);
    assert_eq!(
        new.on_local_event(t0, pixels(-2, 40)),
        sent(-2, 40, WheelUnit::Pixel)
    );
    assert_eq!(
        new.on_local_event(
            t0,
            CapturedEvent::Wheel {
                dx: 0,
                dy: -120,
                unit: WheelUnit::Notch120
            }
        ),
        sent(0, -120, WheelUnit::Notch120)
    );
}

// 7
#[test]
fn pre_grab_and_held_from_remote_semantics() {
    let t0 = Instant::now();
    let mut e = mac_session(t0, TransportRole::Dialer);
    assert_eq!(
        e.on_local_event(t0, key(Hid::LEFT_SHIFT, true)),
        delivered()
    );
    assert_eq!(
        e.on_local_event(t0, mv(1439.5, 450.0)),
        swallowed(enter_actions(
            t0,
            1,
            Point::new(0, 540),
            mask(&[Hid::LEFT_SHIFT]),
            Point::new(1439, 450)
        ))
    );
    // Repeat of a pre-grab key: swallowed, not forwarded.
    assert_eq!(
        e.on_local_event(
            t0,
            CapturedEvent::Key {
                hid: Hid::LEFT_SHIFT,
                down: true,
                repeat: true
            }
        ),
        swallowed(vec![])
    );
    // Its up reaches the local OS and the peer.
    assert_eq!(
        e.on_local_event(t0, key(Hid::LEFT_SHIFT, false)),
        LocalOutcome {
            deliver: true,
            actions: vec![send_key(Hid::LEFT_SHIFT, false, false)]
        }
    );
    // A key pressed while controlling and still held when the cursor returns.
    assert_eq!(
        e.on_local_event(t0, key(KEY_A, true)),
        swallowed(vec![send_key(KEY_A, true, false)])
    );
    assert_eq!(e.on_local_event(t0, delta(-5.0, 0.0)).actions.len(), 2);
    assert_eq!(e.role(), RoleKind::Idle);
    assert_eq!(
        e.on_local_event(t0, key(KEY_A, true)),
        swallowed(vec![]),
        "repeat of a remote-held key"
    );
    assert_eq!(
        e.on_local_event(t0, key(KEY_A, false)),
        swallowed(vec![]),
        "up of a remote-held key"
    );
    assert_eq!(
        e.on_local_event(t0, key(KEY_A, true)),
        delivered(),
        "fresh press is local again"
    );
}

// 8
#[test]
fn target_inject_then_release_on_leave_in_exact_order() {
    let t0 = Instant::now();
    let mut w = win_controlled(t0);
    assert_eq!(
        w.on_input(t0, InputMsg::ModifierSync(mask(&[Hid::LEFT_CTRL]))),
        vec![inj_key(Hid::LEFT_CTRL, true, false)]
    );
    assert_eq!(
        w.on_input(
            t0,
            InputMsg::Key(KeyMsg {
                hid: KEY_C,
                down: true,
                repeat: false
            })
        ),
        vec![inj_key(KEY_C, true, false)]
    );
    assert_eq!(
        w.on_input(
            t0,
            InputMsg::Key(KeyMsg {
                hid: KEY_C,
                down: true,
                repeat: true
            })
        ),
        vec![inj_key(KEY_C, true, true)]
    );
    assert_eq!(
        w.on_input(
            t0,
            InputMsg::Key(KeyMsg {
                hid: KEY_A,
                down: true,
                repeat: true
            })
        ),
        vec![],
        "repeat of a key that is not down"
    );
    assert_eq!(
        w.on_input(
            t0,
            InputMsg::Key(KeyMsg {
                hid: KEY_A,
                down: false,
                repeat: false
            })
        ),
        vec![],
        "up of a key that is not down"
    );
    let at = Point::new(100, 200);
    assert_eq!(
        w.on_input(
            t0,
            InputMsg::Button(ButtonMsg {
                button: MouseButton::Left,
                down: true,
                at
            })
        ),
        vec![
            Action::Inject(InjectCommand::MoveTo(at)),
            Action::Inject(InjectCommand::Button {
                button: MouseButton::Left,
                down: true,
                at
            }),
        ]
    );
    assert_eq!(
        w.on_input(
            t0,
            InputMsg::Wheel(WheelMsg {
                dx: 0,
                dy: 120,
                unit: WheelUnit::Notch120
            })
        ),
        vec![Action::Inject(InjectCommand::Wheel {
            dx: 0,
            dy: 120,
            unit: WheelUnit::Notch120
        })]
    );
    assert_eq!(
        w.on_input(
            t0,
            InputMsg::Wheel(WheelMsg {
                dx: 0,
                dy: 3,
                unit: WheelUnit::Pixel
            })
        ),
        vec![Action::Inject(InjectCommand::Wheel {
            dx: 0,
            dy: 3,
            unit: WheelUnit::Pixel
        })]
    );
    assert_eq!(
        w.on_input(t0, InputMsg::Leave(Leave { epoch: 1 })),
        vec![
            Action::Inject(InjectCommand::Button {
                button: MouseButton::Left,
                down: false,
                at
            }),
            inj_key(KEY_C, false, false),
            inj_key(Hid::LEFT_CTRL, false, false),
        ]
    );
    assert_eq!(w.role(), RoleKind::Idle);
    assert_eq!(
        w.on_input(
            t0,
            InputMsg::Key(KeyMsg {
                hid: KEY_A,
                down: true,
                repeat: false
            })
        ),
        vec![],
        "no injection while idle"
    );
}

// 9
#[test]
fn takeover_by_key_and_by_movement_above_the_threshold() {
    let t0 = Instant::now();
    let mut w = win_controlled(t0);
    assert_eq!(
        w.on_local_event(t0, key(KEY_A, true)),
        LocalOutcome {
            deliver: true,
            actions: vec![Action::SendInput(InputMsg::TakeControl(TakeControl {
                epoch: 1
            }))]
        }
    );
    assert_eq!(w.role(), RoleKind::Idle);
    assert_eq!(w.on_local_event(t0, key(KEY_A, false)), delivered());

    w.on_input(
        t0,
        InputMsg::Enter(Enter {
            epoch: 3,
            at: Point::new(0, 540),
        }),
    );
    assert_eq!(w.on_local_event(t0, delta(3.0, 0.0)), delivered());
    assert_eq!(
        w.on_local_event(t0 + 100 * MS, delta(0.0, -4.0)),
        delivered(),
        "7 units: below the threshold"
    );
    assert_eq!(
        w.on_local_event(t0 + 700 * MS, delta(5.0, 0.0)),
        delivered(),
        "accumulator reset after 600 ms"
    );
    assert_eq!(
        w.on_local_event(t0 + 800 * MS, delta(3.0, 0.0)),
        LocalOutcome {
            deliver: true,
            actions: vec![Action::SendInput(InputMsg::TakeControl(TakeControl {
                epoch: 3
            }))]
        }
    );

    w.on_input(
        t0,
        InputMsg::Enter(Enter {
            epoch: 5,
            at: Point::new(0, 540),
        }),
    );
    w.on_input(
        t0,
        InputMsg::Key(KeyMsg {
            hid: KEY_C,
            down: true,
            repeat: false,
        }),
    );
    assert_eq!(
        w.on_local_event(t0, button(MouseButton::Right, true)),
        LocalOutcome {
            deliver: true,
            actions: vec![
                inj_key(KEY_C, false, false),
                Action::SendInput(InputMsg::TakeControl(TakeControl { epoch: 5 }))
            ]
        }
    );
}

// 10
#[test]
fn take_control_handling_including_a_stale_epoch() {
    let t0 = Instant::now();
    let mut e = mac_controlling(t0);
    assert_eq!(
        e.on_input(t0, InputMsg::TakeControl(TakeControl { epoch: 3 })),
        vec![]
    );
    assert_eq!(e.role(), RoleKind::Controlling);
    assert_eq!(
        e.on_input(t0, InputMsg::TakeControl(TakeControl { epoch: 1 })),
        vec![Action::Capture(GrabCommand::Release {
            warp_to: Point::new(1437, 450)
        })]
    );
    assert_eq!(e.role(), RoleKind::Idle);
}

// 11
#[test]
fn simultaneous_enter_is_resolved_by_parity() {
    let t0 = Instant::now();
    // The dialer (odd) crossed first; the listener's even epoch 2 is higher and wins.
    let mut mac = mac_controlling(t0);
    assert_eq!(
        mac.on_input(
            t0,
            InputMsg::Enter(Enter {
                epoch: 2,
                at: Point::new(1439, 450)
            })
        ),
        vec![
            Action::Capture(GrabCommand::Release {
                warp_to: Point::new(1437, 450)
            }),
            Action::Inject(InjectCommand::MoveTo(Point::new(1439, 450))),
        ]
    );
    assert_eq!(mac.role(), RoleKind::Controlled);

    // The listener holds epoch 2; the dialer's epoch 1 is stale.
    let mut win = win_session(t0, TransportRole::Listener, 1.0);
    win.on_local_event(t0, mv(0.0, 540.0));
    assert_eq!(
        win.on_input(
            t0,
            InputMsg::Enter(Enter {
                epoch: 1,
                at: Point::new(0, 540)
            })
        ),
        vec![]
    );
    assert_eq!(win.role(), RoleKind::Controlling);
}

// 12
#[test]
fn stale_and_out_of_order_mouse_abs() {
    let t0 = Instant::now();
    let mut w = win_controlled(t0);
    let abs = |epoch, seq, x, y| {
        DatagramMsg::MouseAbs(MouseAbs {
            epoch,
            seq,
            at: Point::new(x, y),
        })
    };
    let moved = |x, y| vec![Action::Inject(InjectCommand::MoveTo(Point::new(x, y)))];
    assert_eq!(w.on_datagram(t0, abs(1, 5, 10, 10)), moved(10, 10));
    assert_eq!(w.on_datagram(t0, abs(1, 4, 11, 11)), vec![]);
    assert_eq!(w.on_datagram(t0, abs(1, 5, 12, 12)), vec![]);
    assert_eq!(w.on_datagram(t0, abs(1, 6, 20, 20)), moved(20, 20));
    assert_eq!(w.on_datagram(t0, abs(3, 7, 30, 30)), vec![], "other epoch");
    assert_eq!(
        w.on_datagram(t0, abs(1, 8, 5000, -5000)),
        moved(1919, 0),
        "clamped to my screens"
    );
}

// 13
#[test]
fn heartbeat_cadence_and_timeout() {
    let t0 = Instant::now();
    let hb = |epoch, role| Action::SendControl(ControlMsg::Heartbeat(Heartbeat { epoch, role }));
    let mut e = mac_session(t0, TransportRole::Dialer);
    assert_eq!(e.on_tick(t0), vec![hb(0, RoleKind::Idle)]);
    assert_eq!(e.on_tick(t0 + 50 * MS), vec![]);
    assert_eq!(e.on_tick(t0 + 250 * MS), vec![hb(0, RoleKind::Idle)]);
    assert_eq!(
        e.on_control(
            t0 + 1000 * MS,
            ControlMsg::Heartbeat(Heartbeat {
                epoch: 0,
                role: RoleKind::Idle
            })
        ),
        vec![]
    );
    assert_eq!(e.on_tick(t0 + 2499 * MS), vec![hb(0, RoleKind::Idle)]);
    assert_eq!(
        e.on_tick(t0 + 2500 * MS),
        vec![Action::Close(CloseReason::HeartbeatTimeout)]
    );
    assert!(!e.has_session());
    assert_eq!(e.on_tick(t0 + 3000 * MS), vec![]);

    // A controlled machine releases what it injected before closing.
    let mut w = win_controlled(t0);
    w.on_input(
        t0,
        InputMsg::Key(KeyMsg {
            hid: KEY_C,
            down: true,
            repeat: false,
        }),
    );
    assert_eq!(
        w.on_tick(t0 + 1600 * MS),
        vec![
            inj_key(KEY_C, false, false),
            Action::Close(CloseReason::HeartbeatTimeout)
        ]
    );
}

// 14
#[test]
fn heartbeat_reconciliation_in_both_roles() {
    let t0 = Instant::now();
    let hb = |epoch, role| ControlMsg::Heartbeat(Heartbeat { epoch, role });
    let mut e = mac_controlling(t0);
    assert_eq!(e.on_control(t0, hb(1, RoleKind::Controlled)), vec![]);
    assert_eq!(
        e.on_control(t0, hb(3, RoleKind::Idle)),
        vec![],
        "other epoch"
    );
    assert_eq!(
        e.on_control(t0, hb(1, RoleKind::Idle)),
        vec![Action::Capture(GrabCommand::Release {
            warp_to: Point::new(1437, 450)
        })]
    );
    assert_eq!(e.role(), RoleKind::Idle);

    let mut w = win_controlled(t0);
    w.on_input(
        t0,
        InputMsg::Key(KeyMsg {
            hid: KEY_C,
            down: true,
            repeat: false,
        }),
    );
    assert_eq!(w.on_control(t0, hb(1, RoleKind::Controlling)), vec![]);
    assert_eq!(w.on_control(t0, hb(0, RoleKind::Idle)), vec![]);
    assert_eq!(
        w.on_control(t0, hb(1, RoleKind::Idle)),
        vec![inj_key(KEY_C, false, false)]
    );
    assert_eq!(w.role(), RoleKind::Idle);
}

// 15
#[test]
fn capture_fault_handling() {
    let t0 = Instant::now();
    let mut e = mac_controlling(t0);
    e.on_local_event(t0, key(KEY_A, true));
    assert_eq!(
        e.on_capture_fault(t0, CaptureFault::LeaseExpired),
        vec![
            Action::SendInput(InputMsg::Leave(Leave { epoch: 1 })),
            Action::Capture(GrabCommand::Rearm)
        ]
    );
    assert_eq!(e.role(), RoleKind::Idle);
    assert_eq!(
        e.on_local_event(t0, key(KEY_A, false)),
        swallowed(vec![]),
        "the peer got the down"
    );
    assert_eq!(
        e.on_capture_fault(t0, CaptureFault::TapDisabled),
        vec![Action::Capture(GrabCommand::Rearm)]
    );
    let mut lonely = engine_with(Os::MacOs, mac_screens(), Side::Right, true);
    assert_eq!(
        lonely.on_capture_fault(t0, CaptureFault::EmergencyChord),
        vec![Action::Capture(GrabCommand::Rearm)]
    );
}

// 16
#[test]
fn disconnect_and_shutdown_in_both_roles() {
    let t0 = Instant::now();
    let release = Action::Capture(GrabCommand::Release {
        warp_to: Point::new(1437, 450),
    });

    let mut e = mac_controlling(t0);
    assert_eq!(e.on_disconnected(t0), vec![release.clone()]);
    assert!(!e.has_session());
    assert_eq!(e.role(), RoleKind::Idle);

    let mut w = win_controlled(t0);
    let at = Point::new(100, 200);
    w.on_input(
        t0,
        InputMsg::Key(KeyMsg {
            hid: KEY_C,
            down: true,
            repeat: false,
        }),
    );
    w.on_input(
        t0,
        InputMsg::Button(ButtonMsg {
            button: MouseButton::Left,
            down: true,
            at,
        }),
    );
    assert_eq!(
        w.on_disconnected(t0),
        vec![
            Action::Inject(InjectCommand::Button {
                button: MouseButton::Left,
                down: false,
                at
            }),
            inj_key(KEY_C, false, false),
        ]
    );

    let mut e = mac_controlling(t0);
    assert_eq!(
        e.shutdown(t0),
        vec![
            Action::SendInput(InputMsg::Leave(Leave { epoch: 1 })),
            release,
            Action::Close(CloseReason::Shutdown)
        ]
    );
    let mut w = win_controlled(t0);
    w.on_input(
        t0,
        InputMsg::Key(KeyMsg {
            hid: KEY_C,
            down: true,
            repeat: false,
        }),
    );
    assert_eq!(
        w.shutdown(t0),
        vec![
            inj_key(KEY_C, false, false),
            Action::Close(CloseReason::Shutdown)
        ]
    );
    assert_eq!(w.shutdown(t0), vec![], "no session left");
}

// 17
#[test]
fn wrong_parity_enter_is_a_protocol_violation() {
    let t0 = Instant::now();
    let violation = Action::Close(CloseReason::ProtocolViolation(
        "enter epoch has the wrong parity",
    ));
    let mut e = mac_session(t0, TransportRole::Dialer);
    assert_eq!(
        e.on_input(
            t0,
            InputMsg::Enter(Enter {
                epoch: 1,
                at: Point::new(0, 0)
            })
        ),
        vec![violation.clone()]
    );
    assert!(!e.has_session());
    let mut e = mac_session(t0, TransportRole::Listener);
    assert_eq!(
        e.on_input(
            t0,
            InputMsg::Enter(Enter {
                epoch: 0,
                at: Point::new(0, 0)
            })
        ),
        vec![violation.clone()]
    );

    let mut w = win_controlled(t0);
    w.on_input(
        t0,
        InputMsg::Key(KeyMsg {
            hid: KEY_C,
            down: true,
            repeat: false,
        }),
    );
    assert_eq!(
        w.on_input(
            t0,
            InputMsg::Enter(Enter {
                epoch: 4,
                at: Point::new(0, 0)
            })
        ),
        vec![inj_key(KEY_C, false, false), violation]
    );
}

// 18
#[test]
fn lease_renewal_only_while_controlling() {
    let t0 = Instant::now();
    let mut e = mac_session(t0, TransportRole::Dialer);
    assert_eq!(e.on_tick(t0).len(), 1, "heartbeat only");
    e.on_local_event(t0, mv(1439.5, 450.0));
    assert_eq!(
        e.on_tick(t0 + 50 * MS),
        vec![Action::RenewLease(t0 + 50 * MS + GRAB_LEASE)]
    );
    let mut w = win_controlled(t0);
    assert!(
        w.on_tick(t0 + 50 * MS)
            .iter()
            .all(|a| !matches!(a, Action::RenewLease(_)))
    );
}

#[test]
fn ping_is_answered_on_its_channel_and_late_hello_is_a_violation() {
    let t0 = Instant::now();
    let mut e = mac_session(t0, TransportRole::Dialer);
    let ping = Ping {
        id: 9,
        sent_micros: 1234,
    };
    let pong = Pong {
        id: 9,
        ping_sent_micros: 1234,
    };
    assert_eq!(
        e.on_control(t0, ControlMsg::Ping(ping)),
        vec![Action::SendControl(ControlMsg::Pong(pong))]
    );
    assert_eq!(
        e.on_input(t0, InputMsg::Ping(ping)),
        vec![Action::SendInput(InputMsg::Pong(pong))]
    );
    assert_eq!(
        e.on_datagram(t0, DatagramMsg::Ping(ping)),
        vec![Action::SendDatagram(DatagramMsg::Pong(pong))]
    );
    let hello = Hello {
        protocol_version: 1,
        capabilities: vec![],
        os: Os::Windows,
        os_version: String::new(),
        device_name: String::new(),
        screens: win_screens(1.0),
        peer_side: None,
        placement: None,
        agent_version: String::new(),
    };
    assert_eq!(
        e.on_control(t0, ControlMsg::Hello(hello)),
        vec![Action::Close(CloseReason::ProtocolViolation(
            "unexpected hello"
        ))]
    );
}

#[test]
fn reconnect_replaces_the_session_and_releases_first() {
    let t0 = Instant::now();
    let mut e = mac_controlling(t0);
    assert_eq!(
        e.on_connected(
            t0,
            peer_with(Os::Windows, win_screens(1.0), &full_caps()),
            TransportRole::Dialer
        ),
        vec![Action::Capture(GrabCommand::Release {
            warp_to: Point::new(1437, 450)
        })]
    );
    assert_eq!(e.role(), RoleKind::Idle);
    assert!(e.has_session());
    // Epochs restart with the new session.
    assert_eq!(
        e.on_local_event(t0, mv(1439.5, 450.0)).actions[0],
        Action::SendInput(InputMsg::Enter(Enter {
            epoch: 1,
            at: Point::new(0, 540)
        }))
    );
}

// 20
#[test]
fn a_secondary_follows_the_placement_in_the_peers_hello() {
    let t0 = Instant::now();
    // The Mac placed the PC to its right; the PC has no placement of its own.
    let mut w = engine_placing(Os::Windows, win_screens(1.0), None, true);
    let mut mac = peer_with(Os::MacOs, mac_screens(), &full_caps());
    mac.placement = Some(Placement::side(Side::Right));
    assert_eq!(w.on_connected(t0, mac, TransportRole::Listener), vec![]);
    // The PC's left edge touches the Mac's right edge, its right edge nothing.
    assert_eq!(w.on_local_event(t0, mv(1919.0, 540.0)), delivered());
    assert_eq!(
        w.on_local_event(t0, mv(0.0, 540.0)),
        swallowed(enter_actions(
            t0,
            2,
            Point::new(1439, 450),
            ModifierMask::EMPTY,
            Point::new(0, 540)
        ))
    );
    // Back over the Mac's right edge: the PC's cursor returns inside its left.
    assert_eq!(
        w.on_local_event(t0, delta(5.0, 0.0)),
        swallowed(vec![
            Action::SendInput(InputMsg::Leave(Leave { epoch: 2 })),
            Action::Capture(GrabCommand::Release {
                warp_to: Point::new(2, 540)
            }),
        ])
    );
}

// 21
#[test]
fn the_arranged_desk_crosses_from_either_side() {
    let t0 = Instant::now();
    let desk = vec![
        screen(1, 0, 0, 1512, 982, 1.0),
        screen(2, -524, -1440, 2560, 1440, 1.0),
        screen(3, 2036, -1440, 1920, 1080, 1.0),
    ];
    let laptop = vec![screen(1, 0, 0, 2304, 1296, 1.5)];
    let placement = Placement {
        display: Some(1),
        edge: Edge::Left,
        offset: Some(0),
    };
    let mut mac = engine_placing(Os::MacOs, desk.clone(), Some(placement), true);
    assert_eq!(
        mac.on_connected(
            t0,
            peer_with(Os::Windows, laptop.clone(), &full_caps()),
            TransportRole::Dialer
        ),
        vec![]
    );
    let mut pc = engine_placing(Os::Windows, laptop, None, true);
    let mut hello = peer_with(Os::MacOs, desk, &full_caps());
    hello.placement = Some(placement);
    assert_eq!(pc.on_connected(t0, hello, TransportRole::Listener), vec![]);

    // Where the screens do not touch the cursor stays home.
    assert_eq!(mac.on_local_event(t0, mv(0.0, 950.0)), delivered());
    assert_eq!(mac.on_local_event(t0, mv(-524.0, -700.0)), delivered());
    assert_eq!(pc.on_local_event(t0, mv(1000.0, 1295.0)), delivered());
    // The MacBook's left edge leads to the laptop's right edge ...
    assert_eq!(
        mac.on_local_event(t0, mv(0.0, 500.0)),
        swallowed(enter_actions(
            t0,
            1,
            Point::new(2303, 750),
            ModifierMask::EMPTY,
            Point::new(0, 500)
        ))
    );
    // ... a grab that ends any other way parks the cursor inside that edge.
    assert_eq!(
        mac.on_disconnected(t0),
        vec![Action::Capture(GrabCommand::Release {
            warp_to: Point::new(2, 500)
        })]
    );
    // The laptop's right edge leads to the MacBook's left edge.
    assert_eq!(
        pc.on_local_event(t0, mv(2303.0, 750.0)),
        swallowed(enter_actions(
            t0,
            2,
            Point::new(0, 500),
            ModifierMask::EMPTY,
            Point::new(2303, 750)
        ))
    );
    assert_eq!(
        pc.on_disconnected(t0),
        vec![Action::Capture(GrabCommand::Release {
            warp_to: Point::new(2301, 750)
        })]
    );
}

// 22
#[test]
fn a_grab_ends_inside_the_edge_it_left_by() {
    let t0 = Instant::now();
    let above = Placement {
        display: None,
        edge: Edge::Top,
        offset: None,
    };
    let mut e = engine_placing(Os::MacOs, mac_screens(), Some(above), true);
    let connect = |e: &mut Engine| {
        let peer = peer_with(Os::Windows, win_screens(1.0), &full_caps());
        e.on_connected(t0, peer, TransportRole::Dialer)
    };
    connect(&mut e);
    let up = CapturedEvent::MouseMove {
        pos: PointF::new(500.0, 0.0),
        delta: PointF::new(0.0, -5.0),
    };
    // The 1920 pixel wide PC is centred above the 1440 wide Mac.
    assert_eq!(
        e.on_local_event(t0, up),
        swallowed(enter_actions(
            t0,
            1,
            Point::new(740, 1079),
            ModifierMask::EMPTY,
            Point::new(500, 0)
        ))
    );
    // A grab that ends without crossing back parks the cursor just inside
    // the Mac's top edge ...
    assert_eq!(
        e.on_disconnected(t0),
        vec![Action::Capture(GrabCommand::Release {
            warp_to: Point::new(500, 2)
        })]
    );
    // ... and so does crossing back over the PC's bottom edge.
    connect(&mut e);
    assert!(!e.on_local_event(t0, up).deliver);
    assert_eq!(
        e.on_local_event(t0, delta(0.0, 5.0)),
        swallowed(vec![
            Action::SendInput(InputMsg::Leave(Leave { epoch: 1 })),
            Action::Capture(GrabCommand::Release {
                warp_to: Point::new(500, 2)
            }),
        ])
    );
}

// 23
#[test]
fn my_own_placement_wins_and_without_any_the_peer_stays_out_of_reach() {
    let t0 = Instant::now();
    // Both place the other to the right: this Mac keeps its own placement.
    let mut mac = engine_with(Os::MacOs, mac_screens(), Side::Right, true);
    let mut peer = peer_with(Os::Windows, win_screens(1.0), &full_caps());
    peer.placement = Some(Placement::side(Side::Right));
    mac.on_connected(t0, peer, TransportRole::Dialer);
    assert_eq!(mac.on_local_event(t0, mv(0.0, 450.0)), delivered());
    assert!(!mac.on_local_event(t0, mv(1439.5, 450.0)).deliver);

    // Neither side placed anything.
    let mut lost = engine_placing(Os::MacOs, mac_screens(), None, true);
    lost.on_connected(
        t0,
        peer_with(Os::Windows, win_screens(1.0), &full_caps()),
        TransportRole::Dialer,
    );
    assert_eq!(lost.on_local_event(t0, mv(1439.5, 450.0)), delivered());
    assert_eq!(lost.on_local_event(t0, mv(0.0, 450.0)), delivered());
    // It still takes the peer's input.
    assert_eq!(
        lost.on_input(
            t0,
            InputMsg::Enter(Enter {
                epoch: 2,
                at: Point::new(9999, 450)
            })
        ),
        vec![Action::Inject(InjectCommand::MoveTo(Point::new(1439, 450)))]
    );
}

// ---- property test: random interleavings ----

const KEYS: [Hid; 6] = [
    KEY_A,
    KEY_C,
    Hid::LEFT_CTRL,
    Hid::LEFT_GUI,
    Hid::LEFT_SHIFT,
    Hid::RIGHT_ALT,
];

#[derive(Debug, Clone, Copy)]
enum Pick {
    Valid,
    Stale,
    WrongParity,
}

#[derive(Debug, Clone)]
enum Op {
    Move {
        edge: bool,
        y: f64,
        dx: f64,
        dy: f64,
    },
    Key {
        k: usize,
        down: bool,
    },
    Button {
        b: usize,
        down: bool,
    },
    Wheel,
    PeerEnter(Pick),
    PeerLeave {
        current: bool,
    },
    PeerTakeControl {
        current: bool,
    },
    PeerKey {
        k: usize,
        down: bool,
        repeat: bool,
    },
    PeerButton {
        b: usize,
        down: bool,
    },
    PeerModifierSync(u8),
    PeerMouseAbs {
        current: bool,
    },
    PeerHeartbeat {
        current: bool,
        role: u8,
    },
    Tick {
        ms: u16,
    },
    Fault,
    Disconnect,
    Connect,
    Shutdown,
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (any::<bool>(), 0.0f64..900.0, -40.0f64..40.0, -40.0f64..40.0)
            .prop_map(|(edge, y, dx, dy)| Op::Move { edge, y, dx, dy }),
        3 => (0usize..KEYS.len(), any::<bool>()).prop_map(|(k, down)| Op::Key { k, down }),
        1 => (0usize..5, any::<bool>()).prop_map(|(b, down)| Op::Button { b, down }),
        1 => Just(Op::Wheel),
        2 => prop_oneof![3 => Just(Pick::Valid), 1 => Just(Pick::Stale), 1 => Just(Pick::WrongParity)].prop_map(Op::PeerEnter),
        1 => any::<bool>().prop_map(|current| Op::PeerLeave { current }),
        1 => any::<bool>().prop_map(|current| Op::PeerTakeControl { current }),
        3 => (0usize..KEYS.len(), any::<bool>(), any::<bool>()).prop_map(|(k, down, repeat)| Op::PeerKey { k, down, repeat }),
        1 => (0usize..5, any::<bool>()).prop_map(|(b, down)| Op::PeerButton { b, down }),
        1 => any::<u8>().prop_map(Op::PeerModifierSync),
        2 => any::<bool>().prop_map(|current| Op::PeerMouseAbs { current }),
        1 => (any::<bool>(), 0u8..3).prop_map(|(current, role)| Op::PeerHeartbeat { current, role }),
        3 => (0u16..2000).prop_map(|ms| Op::Tick { ms }),
        1 => Just(Op::Fault),
        1 => Just(Op::Disconnect),
        2 => Just(Op::Connect),
        1 => Just(Op::Shutdown),
    ]
}

struct Harness {
    e: Engine,
    now: Instant,
    role: TransportRole,
    seq: u64,
    last_sent_epoch: u64,
    injected_keys: BTreeSet<Hid>,
    injected_buttons: BTreeSet<MouseButton>,
    grabbed: bool,
}

impl Harness {
    fn new(role: TransportRole) -> Harness {
        Harness {
            e: mac_session(Instant::now(), role),
            now: Instant::now(),
            role,
            seq: 0,
            last_sent_epoch: 0,
            injected_keys: BTreeSet::new(),
            injected_buttons: BTreeSet::new(),
            grabbed: false,
        }
    }

    fn my_parity(&self) -> u64 {
        match self.role {
            TransportRole::Dialer => 1,
            TransportRole::Listener => 0,
        }
    }

    fn epoch_max(&self) -> u64 {
        self.e.session.as_ref().map_or(0, |s| s.epoch_max)
    }

    fn current_epoch(&self) -> u64 {
        self.e
            .session
            .as_ref()
            .and_then(|s| s.role.epoch())
            .unwrap_or(0)
    }

    fn pick_epoch(&self, current: bool) -> u64 {
        if current {
            self.current_epoch()
        } else {
            self.current_epoch() + 2
        }
    }

    fn apply(&mut self, op: &Op) -> Vec<Action> {
        let now = self.now;
        match *op {
            Op::Move { edge, y, dx, dy } => {
                let x = if edge { 1439.5 } else { 700.0 };
                let ev = CapturedEvent::MouseMove {
                    pos: PointF::new(x, y),
                    delta: PointF::new(dx, dy),
                };
                self.e.on_local_event(now, ev).actions
            }
            Op::Key { k, down } => self.e.on_local_event(now, key(KEYS[k], down)).actions,
            Op::Button { b, down } => {
                self.e
                    .on_local_event(now, button(MouseButton::ALL[b], down))
                    .actions
            }
            Op::Wheel => {
                self.e
                    .on_local_event(
                        now,
                        CapturedEvent::Wheel {
                            dx: 0,
                            dy: 120,
                            unit: WheelUnit::Notch120,
                        },
                    )
                    .actions
            }
            Op::PeerEnter(pick) => {
                let max = self.epoch_max();
                let pp = 1 - self.my_parity();
                let epoch = match pick {
                    Pick::Valid => {
                        let e = max + 1;
                        if e % 2 == pp { e } else { e + 1 }
                    }
                    Pick::Stale => {
                        let e = if max % 2 == pp {
                            max
                        } else {
                            max.saturating_sub(1)
                        };
                        if e == 0 {
                            return Vec::new();
                        }
                        e
                    }
                    Pick::WrongParity => {
                        let e = max + 1;
                        if e % 2 == self.my_parity() { e } else { e + 1 }
                    }
                };
                self.e.on_input(
                    now,
                    InputMsg::Enter(Enter {
                        epoch,
                        at: Point::new(5, 5),
                    }),
                )
            }
            Op::PeerLeave { current } => self.e.on_input(
                now,
                InputMsg::Leave(Leave {
                    epoch: self.pick_epoch(current),
                }),
            ),
            Op::PeerTakeControl { current } => self.e.on_input(
                now,
                InputMsg::TakeControl(TakeControl {
                    epoch: self.pick_epoch(current),
                }),
            ),
            Op::PeerKey { k, down, repeat } => self.e.on_input(
                now,
                InputMsg::Key(KeyMsg {
                    hid: KEYS[k],
                    down,
                    repeat,
                }),
            ),
            Op::PeerButton { b, down } => self.e.on_input(
                now,
                InputMsg::Button(ButtonMsg {
                    button: MouseButton::ALL[b],
                    down,
                    at: Point::new(50, 60),
                }),
            ),
            Op::PeerModifierSync(m) => self
                .e
                .on_input(now, InputMsg::ModifierSync(ModifierMask(m))),
            Op::PeerMouseAbs { current } => {
                self.seq += 1;
                let m = MouseAbs {
                    epoch: self.pick_epoch(current),
                    seq: self.seq,
                    at: Point::new(300, 300),
                };
                self.e.on_datagram(now, DatagramMsg::MouseAbs(m))
            }
            Op::PeerHeartbeat { current, role } => {
                let role = [RoleKind::Idle, RoleKind::Controlling, RoleKind::Controlled]
                    [usize::from(role)];
                self.e.on_control(
                    now,
                    ControlMsg::Heartbeat(Heartbeat {
                        epoch: self.pick_epoch(current),
                        role,
                    }),
                )
            }
            Op::Tick { ms } => {
                self.now += Duration::from_millis(u64::from(ms));
                self.e.on_tick(self.now)
            }
            Op::Fault => self.e.on_capture_fault(now, CaptureFault::LeaseExpired),
            Op::Disconnect => self.e.on_disconnected(now),
            Op::Connect => {
                self.last_sent_epoch = 0;
                self.seq = 0;
                let role = self.role;
                self.e.on_connected(
                    now,
                    peer_with(Os::Windows, win_screens(1.0), &full_caps()),
                    role,
                )
            }
            Op::Shutdown => self.e.shutdown(now),
        }
    }

    fn step(&mut self, op: &Op) -> Result<(), TestCaseError> {
        let before = self.e.role();
        let actions = self.apply(op);
        let after = self.e.role();
        for a in &actions {
            match a {
                Action::Inject(cmd) => {
                    let release = matches!(
                        cmd,
                        InjectCommand::Key { down: false, .. }
                            | InjectCommand::Button { down: false, .. }
                    );
                    // (a) only releases outside Controlled.
                    prop_assert!(
                        release || after == RoleKind::Controlled,
                        "{cmd:?} injected while {after:?} after {op:?}"
                    );
                    match *cmd {
                        InjectCommand::Key {
                            hid, down: true, ..
                        } => {
                            self.injected_keys.insert(hid);
                        }
                        InjectCommand::Key {
                            hid, down: false, ..
                        } => {
                            prop_assert!(
                                self.injected_keys.remove(&hid),
                                "unmatched key up {hid:?} after {op:?}"
                            );
                        }
                        InjectCommand::Button {
                            button, down: true, ..
                        } => {
                            self.injected_buttons.insert(button);
                        }
                        InjectCommand::Button {
                            button,
                            down: false,
                            ..
                        } => {
                            prop_assert!(
                                self.injected_buttons.remove(&button),
                                "unmatched button up after {op:?}"
                            );
                        }
                        InjectCommand::MoveTo(_) | InjectCommand::Wheel { .. } => {}
                    }
                }
                // (b) the controller forwards only while Controlling.
                Action::SendInput(InputMsg::Key(_) | InputMsg::Button(_) | InputMsg::Wheel(_))
                | Action::SendDatagram(DatagramMsg::MouseAbs(_)) => {
                    prop_assert!(
                        before == RoleKind::Controlling && after == RoleKind::Controlling,
                        "forwarded {a:?} while {before:?} -> {after:?} after {op:?}"
                    );
                }
                // (e) epochs strictly increase with my parity.
                Action::SendInput(InputMsg::Enter(en)) => {
                    prop_assert_eq!(en.epoch % 2, self.my_parity());
                    prop_assert!(
                        en.epoch > self.last_sent_epoch,
                        "epoch {} after {}",
                        en.epoch,
                        self.last_sent_epoch
                    );
                    self.last_sent_epoch = en.epoch;
                }
                Action::SendInput(InputMsg::Leave(_)) => {
                    prop_assert_eq!(before, RoleKind::Controlling, "leave while not controlling");
                }
                // (d) grab / release pairing.
                Action::Capture(GrabCommand::Grab { .. }) => {
                    prop_assert!(!self.grabbed, "grab while grabbed after {op:?}");
                    self.grabbed = true;
                }
                Action::Capture(GrabCommand::Release { .. }) => {
                    prop_assert!(self.grabbed, "release while not grabbed after {op:?}");
                    self.grabbed = false;
                }
                Action::Capture(GrabCommand::Rearm) => self.grabbed = false,
                Action::Close(_) => {
                    prop_assert!(!self.e.has_session(), "close with a live session")
                }
                _ => {}
            }
        }
        // (c) nothing stays injected once an epoch ends.
        if after != RoleKind::Controlled {
            prop_assert!(
                self.injected_keys.is_empty() && self.injected_buttons.is_empty(),
                "left injected {:?} {:?} after {op:?}",
                self.injected_keys,
                self.injected_buttons
            );
        }
        prop_assert_eq!(
            self.grabbed,
            after == RoleKind::Controlling,
            "grab state vs role after {:?}",
            op
        );
        Ok(())
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn prop_engine_invariants_hold_for_random_interleavings(
        dialer in any::<bool>(),
        ops in prop::collection::vec(arb_op(), 1..150),
    ) {
        let role = if dialer { TransportRole::Dialer } else { TransportRole::Listener };
        let mut h = Harness::new(role);
        for op in &ops {
            h.step(op)?;
        }
        // Ending the session from any state leaves nothing injected or grabbed.
        let end = h.e.shutdown(h.now);
        for a in &end {
            if let Action::Inject(InjectCommand::Key { hid, down: false, .. }) = a {
                h.injected_keys.remove(hid);
            }
            if let Action::Inject(InjectCommand::Button { button, down: false, .. }) = a {
                h.injected_buttons.remove(button);
            }
            if let Action::Capture(GrabCommand::Release { .. }) = a {
                h.grabbed = false;
            }
        }
        prop_assert!(h.injected_keys.is_empty() && h.injected_buttons.is_empty());
        prop_assert!(!h.grabbed);
    }
}

#[test]
fn datagram_ping_measures_rtt_and_loss() {
    let t0 = Instant::now();
    let mut e = mac_session(t0, TransportRole::Dialer);
    let Some(Action::SendDatagram(DatagramMsg::Ping(first))) = e.poll_ping(t0) else {
        panic!("a ping is due as soon as the session starts");
    };
    assert_eq!(e.poll_ping(t0 + 50 * MS), None);
    let pong = Pong {
        id: first.id,
        ping_sent_micros: first.sent_micros,
    };
    assert_eq!(e.on_datagram(t0 + 3 * MS, DatagramMsg::Pong(pong)), vec![]);
    assert!(matches!(
        e.poll_ping(t0 + 250 * MS),
        Some(Action::SendDatagram(DatagramMsg::Ping(_)))
    ));
    // The second ping is never answered.
    e.poll_ping(t0 + 1250 * MS);
    let s = e.stats(t0 + 1250 * MS).unwrap();
    assert_eq!(s.ping.rtt_p50, Some(3 * MS));
    assert_eq!((s.ping.answered, s.ping.lost), (1, 1));

    // No session: no ping and no statistics.
    e.on_disconnected(t0 + 1300 * MS);
    assert_eq!(e.poll_ping(t0 + 1500 * MS), None);
    assert_eq!(e.stats(t0 + 1500 * MS), None);
}

#[test]
fn mouse_datagrams_are_counted_with_gaps_per_control_period() {
    let t0 = Instant::now();
    let mut w = win_controlled(t0);
    let abs = |epoch, seq| {
        DatagramMsg::MouseAbs(MouseAbs {
            epoch,
            seq,
            at: Point::new(10, 10),
        })
    };
    w.on_datagram(t0, abs(1, 1));
    w.on_datagram(t0 + MS, abs(1, 2));
    // 3 and 4 never arrive in time; 4 comes late and is dropped.
    w.on_datagram(t0 + 2 * MS, abs(1, 5));
    assert_eq!(w.on_datagram(t0 + 3 * MS, abs(1, 4)), vec![]);
    let s = w.stats(t0 + 10 * MS).unwrap();
    assert_eq!(
        (s.mouse_applied, s.mouse_skipped, s.mouse_in_per_sec),
        (3, 2, 3)
    );
    assert_eq!(s.mouse_out_per_sec, 0);

    // The controller counts what it sends.
    let mut m = mac_controlling(t0);
    for i in 1..=5u32 {
        m.on_local_event(t0 + i * MS, delta(1.0, 0.0));
    }
    let s = m.stats(t0 + 10 * MS).unwrap();
    assert_eq!((s.mouse_out_per_sec, s.mouse_in_per_sec), (5, 0));
}

fn fn_key(down: bool) -> CapturedEvent {
    key(Hid::APPLE_FN, down)
}

fn gesture(g: Gesture) -> CapturedEvent {
    CapturedEvent::Gesture(g)
}

fn wheel_notch(dy: i32) -> Action {
    Action::SendInput(InputMsg::Wheel(WheelMsg {
        dx: 0,
        dy,
        unit: WheelUnit::Notch120,
    }))
}

#[test]
fn a_tap_of_fn_switches_the_pcs_language() {
    let t0 = Instant::now();
    let mut e = mac_controlling(t0);
    assert_eq!(e.on_local_event(t0, fn_key(true)), swallowed(vec![]));
    assert_eq!(
        e.on_local_event(t0, fn_key(false)),
        swallowed(vec![
            send_key(Hid::LEFT_GUI, true, false),
            send_key(Hid::SPACE, true, false),
            send_key(Hid::SPACE, false, false),
            send_key(Hid::LEFT_GUI, false, false),
        ])
    );
    // Fn held with another key (Fn+Delete, Fn+arrows) is not a tap; the key
    // itself goes through as usual and Fn never does.
    assert_eq!(e.on_local_event(t0, fn_key(true)), swallowed(vec![]));
    assert_eq!(
        e.on_local_event(t0, key(KEY_A, true)),
        swallowed(vec![send_key(KEY_A, true, false)])
    );
    assert_eq!(
        e.on_local_event(t0, key(KEY_A, false)),
        swallowed(vec![send_key(KEY_A, false, false)])
    );
    assert_eq!(e.on_local_event(t0, fn_key(false)), swallowed(vec![]));
    // Same with a click while Fn is held.
    e.on_local_event(t0, fn_key(true));
    e.on_local_event(t0, button(MouseButton::Left, true));
    e.on_local_event(t0, button(MouseButton::Left, false));
    assert_eq!(e.on_local_event(t0, fn_key(false)), swallowed(vec![]));
}

#[test]
fn held_input_regression_fn_chord_preserves_already_pressed_space() {
    let t0 = Instant::now();
    let mut e = mac_controlling(t0);
    assert_eq!(
        e.on_local_event(t0, key(Hid::SPACE, true)),
        swallowed(vec![send_key(Hid::SPACE, true, false)])
    );
    assert_eq!(e.on_local_event(t0, fn_key(true)), swallowed(vec![]));
    assert_eq!(
        e.on_local_event(t0, fn_key(false)),
        swallowed(vec![
            send_key(Hid::LEFT_GUI, true, false),
            send_key(Hid::SPACE, true, false),
            send_key(Hid::LEFT_GUI, false, false),
        ])
    );
    assert_eq!(
        e.on_local_event(t0, key(Hid::SPACE, false)),
        swallowed(vec![send_key(Hid::SPACE, false, false)])
    );
}

#[test]
fn held_input_regression_gesture_chord_preserves_already_pressed_arrow() {
    let t0 = Instant::now();
    let mut e = mac_controlling(t0);
    assert_eq!(
        e.on_local_event(t0, key(Hid::RIGHT_ARROW, true)),
        swallowed(vec![send_key(Hid::RIGHT_ARROW, true, false)])
    );
    assert_eq!(
        e.on_local_event(t0, gesture(Gesture::SwitchSpace { next: true })),
        swallowed(vec![
            send_key(Hid::LEFT_CTRL, true, false),
            send_key(Hid::LEFT_GUI, true, false),
            send_key(Hid::RIGHT_ARROW, true, false),
            send_key(Hid::LEFT_GUI, false, false),
            send_key(Hid::LEFT_CTRL, false, false),
        ])
    );
    assert_eq!(
        e.on_local_event(t0, key(Hid::RIGHT_ARROW, false)),
        swallowed(vec![send_key(Hid::RIGHT_ARROW, false, false)])
    );
}

#[test]
fn fn_chord_after_reentry_preserves_remote_held_space() {
    let t0 = Instant::now();
    let mut mac = mac_controlling(t0);
    let mut pc = win_controlled(t0);

    assert_eq!(
        mac.on_local_event(t0, key(Hid::SPACE, true)),
        swallowed(vec![send_key(Hid::SPACE, true, false)])
    );
    assert_eq!(
        pc.on_input(
            t0,
            InputMsg::Key(KeyMsg {
                hid: Hid::SPACE,
                down: true,
                repeat: false,
            })
        ),
        vec![inj_key(Hid::SPACE, true, false)]
    );

    assert_eq!(
        mac.on_local_event(t0, delta(-5.0, 0.0)),
        swallowed(vec![
            Action::SendInput(InputMsg::Leave(Leave { epoch: 1 })),
            Action::Capture(GrabCommand::Release {
                warp_to: Point::new(1437, 450),
            }),
        ])
    );
    assert_eq!(
        pc.on_input(t0, InputMsg::Leave(Leave { epoch: 1 })),
        vec![inj_key(Hid::SPACE, false, false)]
    );
    assert_eq!(mac.on_local_event(t0, mv(1437.0, 450.0)), delivered());

    let entered = mac.on_local_event(t0, mv(1439.0, 450.0));
    assert_eq!(
        entered,
        swallowed(enter_actions(
            t0,
            3,
            Point::new(0, 540),
            ModifierMask::EMPTY,
            Point::new(1439, 450)
        ))
    );
    for action in entered.actions {
        if let Action::SendInput(message) = action {
            pc.on_input(t0, message);
        }
    }

    let repeated = mac.on_local_event(t0, key(Hid::SPACE, true));
    assert_eq!(repeated, swallowed(vec![send_key(Hid::SPACE, true, true)]));
    assert_eq!(
        pc.on_input(
            t0,
            InputMsg::Key(KeyMsg {
                hid: Hid::SPACE,
                down: true,
                repeat: true,
            })
        ),
        vec![],
        "the new control epoch has no initial Space down yet"
    );

    mac.on_local_event(t0, fn_key(true));
    let shortcut = mac.on_local_event(t0, fn_key(false));
    assert_eq!(
        shortcut,
        swallowed(vec![
            send_key(Hid::LEFT_GUI, true, false),
            send_key(Hid::SPACE, true, false),
            send_key(Hid::LEFT_GUI, false, false),
        ])
    );
    let mut injected = Vec::new();
    for action in shortcut.actions {
        let Action::SendInput(message) = action else {
            panic!("Fn shortcut should only send input messages");
        };
        injected.extend(pc.on_input(t0, message));
    }
    assert_eq!(
        injected,
        vec![
            inj_key(Hid::LEFT_GUI, true, false),
            inj_key(Hid::SPACE, true, false),
            inj_key(Hid::LEFT_GUI, false, false),
        ]
    );

    assert_eq!(
        mac.on_local_event(t0, key(Hid::SPACE, false)),
        swallowed(vec![send_key(Hid::SPACE, false, false)])
    );
    assert_eq!(
        pc.on_input(
            t0,
            InputMsg::Key(KeyMsg {
                hid: Hid::SPACE,
                down: false,
                repeat: false,
            })
        ),
        vec![inj_key(Hid::SPACE, false, false)],
        "the physical up releases Space after the shortcut"
    );
}

#[test]
fn fn_chord_does_not_adopt_space_held_before_crossing() {
    let t0 = Instant::now();
    let mut mac = mac_session(t0, TransportRole::Dialer);
    assert_eq!(mac.on_local_event(t0, key(Hid::SPACE, true)), delivered());
    assert_eq!(
        mac.on_local_event(t0, mv(1439.5, 450.0)),
        swallowed(enter_actions(
            t0,
            1,
            Point::new(0, 540),
            ModifierMask::EMPTY,
            Point::new(1439, 450)
        ))
    );

    mac.on_local_event(t0, fn_key(true));
    assert_eq!(
        mac.on_local_event(t0, fn_key(false)),
        swallowed(vec![
            send_key(Hid::LEFT_GUI, true, false),
            send_key(Hid::SPACE, true, false),
            send_key(Hid::SPACE, false, false),
            send_key(Hid::LEFT_GUI, false, false),
        ])
    );
}

#[test]
fn held_input_regression_enter_respects_local_key_until_its_release() {
    let t0 = Instant::now();
    let mut e = win_session(t0, TransportRole::Listener, 1.0);
    assert_eq!(e.on_local_event(t0, key(KEY_A, true)), delivered());
    let enter = InputMsg::Enter(Enter {
        epoch: 1,
        at: Point::new(0, 540),
    });
    assert_eq!(
        e.on_input(t0, enter),
        vec![Action::SendInput(InputMsg::TakeControl(TakeControl {
            epoch: 1
        }))]
    );
    assert_eq!(e.role(), RoleKind::Idle);
    assert_eq!(
        e.on_input(
            t0,
            InputMsg::Key(KeyMsg {
                hid: KEY_A,
                down: true,
                repeat: false,
            })
        ),
        vec![]
    );
    assert_eq!(
        e.on_local_event(t0, key(KEY_A, false)),
        delivered(),
        "the key-up belongs to the local down"
    );
    assert_eq!(
        e.on_input(
            t0,
            InputMsg::Key(KeyMsg {
                hid: KEY_A,
                down: false,
                repeat: false,
            })
        ),
        vec![]
    );
    assert_eq!(e.role(), RoleKind::Idle);
    assert_eq!(e.on_input(t0, enter), vec![], "the declined epoch is stale");
}

#[test]
fn held_input_regression_enter_respects_local_button_until_its_release() {
    let t0 = Instant::now();
    let mut e = win_session(t0, TransportRole::Listener, 1.0);
    let at = Point::new(100, 100);
    assert_eq!(
        e.on_local_event(t0, button(MouseButton::Left, true)),
        delivered()
    );
    assert_eq!(
        e.on_input(
            t0,
            InputMsg::Enter(Enter {
                epoch: 1,
                at: Point::new(0, 540),
            })
        ),
        vec![Action::SendInput(InputMsg::TakeControl(TakeControl {
            epoch: 1
        }))]
    );
    assert_eq!(e.role(), RoleKind::Idle);
    assert_eq!(
        e.on_input(
            t0,
            InputMsg::Button(ButtonMsg {
                button: MouseButton::Left,
                down: true,
                at,
            })
        ),
        vec![]
    );
    assert_eq!(
        e.on_local_event(t0, button(MouseButton::Left, false)),
        delivered(),
        "the button-up belongs to the local down"
    );
    assert_eq!(
        e.on_input(
            t0,
            InputMsg::Button(ButtonMsg {
                button: MouseButton::Left,
                down: false,
                at,
            })
        ),
        vec![]
    );
    assert_eq!(e.role(), RoleKind::Idle);
}

#[test]
fn fn_keeps_a_held_modifier_and_stays_local_when_idle() {
    let t0 = Instant::now();
    let mut e = mac_controlling(t0);
    // Cmd held on the Mac is Ctrl held on the PC.
    e.on_local_event(t0, key(Hid::LEFT_GUI, true));
    e.on_local_event(t0, fn_key(true));
    assert_eq!(
        e.on_local_event(t0, fn_key(false)),
        swallowed(vec![
            send_key(Hid::LEFT_GUI, true, false),
            send_key(Hid::SPACE, true, false),
            send_key(Hid::SPACE, false, false),
            send_key(Hid::LEFT_GUI, false, false),
        ])
    );
    // Idle: Fn is the Mac's own (its input source switch).
    let mut idle = mac_session(t0, TransportRole::Dialer);
    assert_eq!(idle.on_local_event(t0, fn_key(true)), delivered());
    assert_eq!(idle.on_local_event(t0, fn_key(false)), delivered());
}

#[test]
fn fn_pressed_before_the_crossing_stays_with_the_mac() {
    let t0 = Instant::now();
    let mut e = mac_session(t0, TransportRole::Dialer);
    assert_eq!(e.on_local_event(t0, fn_key(true)), delivered());
    e.on_local_event(t0, mv(1439.5, 450.0));
    assert_eq!(e.role(), RoleKind::Controlling);
    // The Mac saw the down: it gets the up, and nothing is switched.
    assert_eq!(e.on_local_event(t0, fn_key(false)), delivered());
}

#[test]
fn gestures_are_swallowed_and_sent_as_the_pcs_shortcuts() {
    let t0 = Instant::now();
    let mut e = mac_controlling(t0);
    assert_eq!(
        e.on_local_event(t0, gesture(Gesture::SwitchSpace { next: true })),
        swallowed(vec![
            send_key(Hid::LEFT_CTRL, true, false),
            send_key(Hid::LEFT_GUI, true, false),
            send_key(Hid::RIGHT_ARROW, true, false),
            send_key(Hid::RIGHT_ARROW, false, false),
            send_key(Hid::LEFT_GUI, false, false),
            send_key(Hid::LEFT_CTRL, false, false),
        ])
    );
    assert_eq!(
        e.on_local_event(t0, gesture(Gesture::Overview)),
        swallowed(vec![
            send_key(Hid::LEFT_GUI, true, false),
            send_key(Hid::TAB, true, false),
            send_key(Hid::TAB, false, false),
            send_key(Hid::LEFT_GUI, false, false),
        ])
    );
    // Rotation, smart zoom and the like: swallowed, nothing sent.
    assert_eq!(
        e.on_local_event(t0, gesture(Gesture::Other)),
        swallowed(vec![])
    );
    // A pinch zooms in whole Ctrl+wheel steps.
    let pinch = |delta, phase| gesture(Gesture::Magnify { delta, phase });
    assert_eq!(
        e.on_local_event(t0, pinch(0.05, GesturePhase::Began)),
        swallowed(vec![])
    );
    assert_eq!(
        e.on_local_event(t0, pinch(0.2, GesturePhase::Changed)),
        swallowed(vec![
            send_key(Hid::LEFT_CTRL, true, false),
            wheel_notch(120),
            wheel_notch(120),
            send_key(Hid::LEFT_CTRL, false, false),
        ])
    );
    assert_eq!(
        e.on_local_event(t0, pinch(-0.13, GesturePhase::Ended)),
        swallowed(vec![
            send_key(Hid::LEFT_CTRL, true, false),
            wheel_notch(-120),
            send_key(Hid::LEFT_CTRL, false, false),
        ])
    );
}

#[test]
fn repeated_chord_key_stays_pressed_at_peer_until_physical_up() {
    let t0 = Instant::now();
    let mut mac = mac_controlling(t0);
    let mut pc = win_controlled(t0);

    assert_eq!(
        mac.on_local_event(t0, key(Hid::SPACE, true)),
        swallowed(vec![send_key(Hid::SPACE, true, false)])
    );
    assert_eq!(
        pc.on_input(
            t0,
            InputMsg::Key(KeyMsg {
                hid: Hid::SPACE,
                down: true,
                repeat: false,
            })
        ),
        vec![inj_key(Hid::SPACE, true, false)]
    );

    mac.on_local_event(t0, fn_key(true));
    let shortcut = mac.on_local_event(t0, fn_key(false));
    assert_eq!(
        shortcut,
        swallowed(vec![
            send_key(Hid::LEFT_GUI, true, false),
            send_key(Hid::SPACE, true, false),
            send_key(Hid::LEFT_GUI, false, false),
        ])
    );

    let mut remote_results = Vec::new();
    for action in shortcut.actions {
        let Action::SendInput(message) = action else {
            panic!("Fn shortcut should only send input messages");
        };
        remote_results.push(pc.on_input(t0, message));
    }
    assert_eq!(
        remote_results,
        vec![
            vec![inj_key(Hid::LEFT_GUI, true, false)],
            vec![inj_key(Hid::SPACE, true, true)],
            vec![inj_key(Hid::LEFT_GUI, false, false)],
        ],
        "the peer interprets the extra down as a repeat, without releasing Space"
    );

    assert_eq!(
        mac.on_local_event(t0, key(Hid::SPACE, false)),
        swallowed(vec![send_key(Hid::SPACE, false, false)])
    );
    assert_eq!(
        pc.on_input(
            t0,
            InputMsg::Key(KeyMsg {
                hid: Hid::SPACE,
                down: false,
                repeat: false,
            })
        ),
        vec![inj_key(Hid::SPACE, false, false)],
        "the physical up remains the release for the held key"
    );
}

#[test]
fn gestures_stay_local_unless_controlling() {
    let t0 = Instant::now();
    let mut idle = mac_session(t0, TransportRole::Dialer);
    for g in [
        Gesture::Overview,
        Gesture::SwitchSpace { next: false },
        Gesture::Other,
    ] {
        assert_eq!(idle.on_local_event(t0, gesture(g)), delivered());
    }
    // A controlled Mac keeps its user's gestures, and they do not take over.
    let mut w = win_controlled(t0);
    assert_eq!(
        w.on_local_event(t0, gesture(Gesture::Overview)),
        delivered()
    );
    assert_eq!(w.role(), RoleKind::Controlled);
}
