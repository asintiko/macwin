//! macOS conversions: modifier flags, scroll units and click counting.

use std::time::{Duration, Instant};

use super::PIXELS_PER_NOTCH;
use crate::types::{Gesture, GesturePhase, Hid, ModifierMask, MouseButton, PointF};

// Device-dependent modifier bits (NX_DEVICE*KEYMASK), one per physical key.
pub const LCTRL: u64 = 0x0000_0001;
pub const LSHIFT: u64 = 0x0000_0002;
pub const RSHIFT: u64 = 0x0000_0004;
pub const LCMD: u64 = 0x0000_0008;
pub const RCMD: u64 = 0x0000_0010;
pub const LALT: u64 = 0x0000_0020;
pub const RALT: u64 = 0x0000_0040;
pub const RCTRL: u64 = 0x0000_2000;

// Device-independent bits (kCGEventFlagMask*).
pub const ALPHA_SHIFT: u64 = 0x0001_0000;
pub const SHIFT: u64 = 0x0002_0000;
pub const CONTROL: u64 = 0x0004_0000;
pub const ALTERNATE: u64 = 0x0008_0000;
pub const COMMAND: u64 = 0x0010_0000;

/// macOS virtual key code of CapsLock.
pub const KVK_CAPS_LOCK: u16 = 0x39;
/// macOS virtual key code of Fn.
pub const KVK_FUNCTION: u16 = 0x3F;
/// `kCGEventFlagMaskSecondaryFn`.
pub const SECONDARY_FN: u64 = 0x0080_0000;

/// Whether Fn / 🌐 is down after a flags-changed event of [`KVK_FUNCTION`].
/// (Other keys, arrows and F-keys among them, carry this flag too, which is
/// why only Fn's own flags-changed event may be read this way.)
pub fn fn_key_down(flags: u64) -> bool {
    flags & SECONDARY_FN != 0
}

/// (key code, device-dependent bit, HID) of the eight modifier keys.
const MODIFIER_KEYS: [(u16, u64, Hid); 8] = [
    (0x3B, LCTRL, Hid::LEFT_CTRL),
    (0x38, LSHIFT, Hid::LEFT_SHIFT),
    (0x3A, LALT, Hid::LEFT_ALT),
    (0x37, LCMD, Hid::LEFT_GUI),
    (0x3E, RCTRL, Hid::RIGHT_CTRL),
    (0x3C, RSHIFT, Hid::RIGHT_SHIFT),
    (0x3D, RALT, Hid::RIGHT_ALT),
    (0x36, RCMD, Hid::RIGHT_GUI),
];

/// What a `kCGEventFlagsChanged` event means for the key that changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagsChange {
    Down,
    Up,
    /// CapsLock: macOS reports only lock-state changes, so forward a full
    /// press (down then up).
    Tap,
}

/// Interprets a flags-changed event for `keycode` with the event's `flags`.
/// Fn and non-modifier key codes yield `None`.
pub fn flags_changed(keycode: u16, flags: u64) -> Option<FlagsChange> {
    if keycode == KVK_CAPS_LOCK {
        return Some(FlagsChange::Tap);
    }
    MODIFIER_KEYS
        .iter()
        .find(|(code, _, _)| *code == keycode)
        .map(|(_, bit, _)| {
            if flags & bit != 0 {
                FlagsChange::Down
            } else {
                FlagsChange::Up
            }
        })
}

/// Event flags for injecting with modifier state `mask`: both the
/// device-dependent and the device-independent bits.
pub fn flags_for(mask: ModifierMask) -> u64 {
    let mut flags = 0;
    for (_, bit, hid) in MODIFIER_KEYS {
        if mask.contains(hid) {
            flags |= bit;
        }
    }
    if mask.intersects(ModifierMask::CTRL) {
        flags |= CONTROL;
    }
    if mask.intersects(ModifierMask::SHIFT) {
        flags |= SHIFT;
    }
    if mask.intersects(ModifierMask::ALT) {
        flags |= ALTERNATE;
    }
    if mask.intersects(ModifierMask::GUI) {
        flags |= COMMAND;
    }
    flags
}

/// Modifier state from event flags. Device-dependent bits win; a family whose
/// device-independent bit is set without any device bit (synthetic events)
/// maps to its left key.
pub fn mask_from_flags(flags: u64) -> ModifierMask {
    let mut mask = ModifierMask::EMPTY;
    for (_, bit, hid) in MODIFIER_KEYS {
        if flags & bit != 0 {
            mask.insert(hid);
        }
    }
    for (independent, family, left) in [
        (CONTROL, ModifierMask::CTRL, Hid::LEFT_CTRL),
        (SHIFT, ModifierMask::SHIFT, Hid::LEFT_SHIFT),
        (ALTERNATE, ModifierMask::ALT, Hid::LEFT_ALT),
        (COMMAND, ModifierMask::GUI, Hid::LEFT_GUI),
    ] {
        if flags & independent != 0 && !mask.intersects(family) {
            mask.insert(left);
        }
    }
    mask
}

/// Converts `kCGScrollWheelEventFixedPtDeltaAxis1/2` (lines, positive = up /
/// left) into macwin wheel units (120 per line, `dy > 0` up, `dx > 0` right).
/// The axis-2 sign is unverified on real hardware.
pub fn scroll_from_fixed(axis1: f64, axis2: f64) -> (i32, i32) {
    let dy = (axis1 * 120.0).round();
    let dx = (-axis2 * 120.0).round();
    (clamp_i32(dx), clamp_i32(dy))
}

fn clamp_i32(v: f64) -> i32 {
    if v.is_nan() {
        0
    } else {
        v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
    }
}

/// Converts `kCGScrollWheelEventPointDeltaAxis1/2` of a continuous scroll
/// (pixels, positive = up / left) into macwin pixel deltas (`dy > 0` up,
/// `dx > 0` right), like [`scroll_from_fixed`].
pub fn scroll_from_points(axis1: i64, axis2: i64) -> (i32, i32) {
    let clamp = |v: i64| v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    (clamp(axis2.saturating_neg()), clamp(axis1))
}

/// Turns 1/120 wheel units into whole steps (lines, or pixels), carrying
/// the remainder (with its sign) to the next call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WheelAccumulator {
    /// 1/120 units per step.
    step: i64,
    remainder: i64,
}

impl Default for WheelAccumulator {
    fn default() -> WheelAccumulator {
        WheelAccumulator::new()
    }
}

impl WheelAccumulator {
    /// Whole lines (120 units each).
    pub fn new() -> WheelAccumulator {
        WheelAccumulator {
            step: 120,
            remainder: 0,
        }
    }

    /// Whole pixels, [`PIXELS_PER_NOTCH`] per 120 units.
    pub fn pixels() -> WheelAccumulator {
        WheelAccumulator {
            step: i64::from(120 / PIXELS_PER_NOTCH),
            remainder: 0,
        }
    }

    /// Adds `delta` (1/120 units) and returns the whole steps to scroll now.
    pub fn push(&mut self, delta: i32) -> i32 {
        let total = self.remainder + i64::from(delta);
        let steps = total / self.step; // truncates toward zero
        self.remainder = total - steps * self.step;
        steps.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
    }

    pub fn remainder(&self) -> i32 {
        self.remainder as i32
    }
}

// ---- trackpad gestures ---------------------------------------------------
//
// Gesture events are private CGS event types. The field numbers and values
// are the ones Mac Mouse Fix, iss and InstantSpaceSwitcher rely on.

/// CGS event type of trackpad gestures: pinch, rotation, smart zoom and the
/// events that accompany scrolls and Dock swipes.
pub const CGS_GESTURE: u32 = 29;
/// CGS event type of Dock swipes: Spaces, Mission Control, App Exposé,
/// Launchpad. Dropping them in an event tap keeps the Dock from seeing them.
pub const CGS_DOCK_CONTROL: u32 = 30;
/// CGEventField numbers.
pub const FIELD_CGS_TYPE: u32 = 55;
pub const FIELD_GESTURE_HID_TYPE: u32 = 110;
pub const FIELD_MAGNIFICATION: u32 = 113;
pub const FIELD_SWIPE_MOTION: u32 = 123;
pub const FIELD_SWIPE_PROGRESS: u32 = 124;
pub const FIELD_SWIPE_VELOCITY_X: u32 = 129;
pub const FIELD_SWIPE_VELOCITY_Y: u32 = 130;
pub const FIELD_GESTURE_PHASE: u32 = 132;

/// IOHIDEventType values in [`FIELD_GESTURE_HID_TYPE`].
const HID_ZOOM: i64 = 8;
const HID_DOCK_SWIPE: i64 = 23;
/// IOHIDGestureMotion values in [`FIELD_SWIPE_MOTION`].
const MOTION_HORIZONTAL: i64 = 1;
const MOTION_VERTICAL: i64 = 2;
/// Gesture phases in [`FIELD_GESTURE_PHASE`].
const PHASE_BEGAN: i64 = 1;
const PHASE_CHANGED: i64 = 2;
const PHASE_ENDED: i64 = 4;
const PHASE_CANCELLED: i64 = 8;

/// The fields of one gesture event that matter.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RawGesture {
    /// [`CGS_GESTURE`] or [`CGS_DOCK_CONTROL`].
    pub cgs_type: u32,
    pub hid_type: i64,
    pub phase: i64,
    pub motion: i64,
    pub progress: f64,
    pub velocity_x: f64,
    pub velocity_y: f64,
    pub magnification: f64,
}

fn gesture_phase(phase: i64) -> GesturePhase {
    match phase {
        PHASE_BEGAN => GesturePhase::Began,
        PHASE_CHANGED => GesturePhase::Changed,
        PHASE_ENDED | PHASE_CANCELLED => GesturePhase::Ended,
        _ => GesturePhase::Other,
    }
}

fn nonzero(v: f64) -> bool {
    v.is_finite() && v != 0.0
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Swipe {
    motion: i64,
    progress: f64,
    velocity: f64,
}

/// Recognises gestures in the event stream. A Dock swipe is reported once,
/// when it ends; a horizontal one goes to the next Space when its progress
/// (or, without one, its velocity) is positive.
#[derive(Debug, Clone, Default)]
pub struct GestureTracker {
    swipe: Option<Swipe>,
}

impl GestureTracker {
    pub fn on_event(&mut self, g: &RawGesture) -> Gesture {
        if g.cgs_type == CGS_DOCK_CONTROL && g.hid_type == HID_DOCK_SWIPE {
            return self.dock_swipe(g);
        }
        if g.cgs_type == CGS_GESTURE && g.hid_type == HID_ZOOM {
            return Gesture::Magnify {
                delta: if g.magnification.is_finite() {
                    g.magnification
                } else {
                    0.0
                },
                phase: gesture_phase(g.phase),
            };
        }
        Gesture::Other
    }

    fn dock_swipe(&mut self, g: &RawGesture) -> Gesture {
        let velocity = if g.motion == MOTION_VERTICAL {
            g.velocity_y
        } else {
            g.velocity_x
        };
        let fresh = Swipe {
            motion: g.motion,
            progress: 0.0,
            velocity: 0.0,
        };
        match g.phase {
            PHASE_BEGAN => {
                self.swipe = Some(fresh);
                Gesture::Other
            }
            PHASE_CHANGED => {
                let s = self.swipe.get_or_insert(fresh);
                if nonzero(g.progress) {
                    s.progress = g.progress;
                }
                if nonzero(velocity) {
                    s.velocity = velocity;
                }
                Gesture::Other
            }
            PHASE_ENDED => {
                let s = self.swipe.take().unwrap_or(fresh);
                let value = [s.progress, g.progress, velocity, s.velocity]
                    .into_iter()
                    .find(|&v| nonzero(v));
                match (s.motion, value) {
                    (MOTION_HORIZONTAL, Some(v)) => Gesture::SwitchSpace { next: v > 0.0 },
                    (MOTION_VERTICAL, Some(_)) => Gesture::Overview,
                    _ => Gesture::Other,
                }
            }
            PHASE_CANCELLED => {
                self.swipe = None;
                Gesture::Other
            }
            _ => Gesture::Other,
        }
    }
}

/// Counts clicks for `kCGMouseEventClickState`: the same button pressed
/// again within the double-click interval and within 4 points of the
/// previous press continues the sequence.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ClickCounter {
    last: Option<(MouseButton, PointF, Instant, i64)>,
}

/// Maximum distance (points) between presses of one click sequence.
pub const CLICK_RADIUS: f64 = 4.0;

impl ClickCounter {
    pub fn new() -> ClickCounter {
        ClickCounter::default()
    }

    /// Records a button press and returns its click count (1, 2, 3, ...).
    pub fn on_down(
        &mut self,
        button: MouseButton,
        pos: PointF,
        now: Instant,
        interval: Duration,
    ) -> i64 {
        let count = match self.last {
            Some((b, p, t, n))
                if b == button
                    && now.saturating_duration_since(t) <= interval
                    && (pos.x - p.x).hypot(pos.y - p.y) <= CLICK_RADIUS =>
            {
                n + 1
            }
            _ => 1,
        };
        self.last = Some((button, pos, now, count));
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fn_is_down_while_its_flag_is_set() {
        assert!(fn_key_down(SECONDARY_FN));
        assert!(fn_key_down(SECONDARY_FN | COMMAND | LCMD));
        assert!(!fn_key_down(COMMAND | LCMD));
        assert_eq!(flags_changed(KVK_FUNCTION, SECONDARY_FN), None);
    }

    fn dock(phase: i64, motion: i64, progress: f64, velocity: f64) -> RawGesture {
        RawGesture {
            cgs_type: CGS_DOCK_CONTROL,
            hid_type: HID_DOCK_SWIPE,
            phase,
            motion,
            progress,
            velocity_x: if motion == MOTION_HORIZONTAL {
                velocity
            } else {
                0.0
            },
            velocity_y: if motion == MOTION_VERTICAL {
                velocity
            } else {
                0.0
            },
            magnification: 0.0,
        }
    }

    #[test]
    fn a_horizontal_swipe_switches_spaces_when_it_ends() {
        let mut t = GestureTracker::default();
        assert_eq!(t.on_event(&dock(PHASE_BEGAN, 1, 0.0, 0.0)), Gesture::Other);
        assert_eq!(
            t.on_event(&dock(PHASE_CHANGED, 1, 0.2, 1.5)),
            Gesture::Other
        );
        assert_eq!(
            t.on_event(&dock(PHASE_CHANGED, 1, 0.6, 2.0)),
            Gesture::Other
        );
        // The ended event may carry no progress: the last one counts.
        assert_eq!(
            t.on_event(&dock(PHASE_ENDED, 1, 0.0, 0.0)),
            Gesture::SwitchSpace { next: true }
        );
        // The other way, a flick known only by its velocity.
        t.on_event(&dock(PHASE_BEGAN, 1, 0.0, 0.0));
        assert_eq!(
            t.on_event(&dock(PHASE_ENDED, 1, 0.0, -3.0)),
            Gesture::SwitchSpace { next: false }
        );
    }

    #[test]
    fn vertical_swipes_open_the_overview_and_cancelled_ones_nothing() {
        let mut t = GestureTracker::default();
        t.on_event(&dock(PHASE_BEGAN, 2, 0.0, 0.0));
        t.on_event(&dock(PHASE_CHANGED, 2, -0.4, -1.0));
        assert_eq!(
            t.on_event(&dock(PHASE_ENDED, 2, 0.0, 0.0)),
            Gesture::Overview
        );
        t.on_event(&dock(PHASE_BEGAN, 1, 0.0, 0.0));
        t.on_event(&dock(PHASE_CHANGED, 1, 0.3, 1.0));
        assert_eq!(
            t.on_event(&dock(PHASE_CANCELLED, 1, 0.0, 0.0)),
            Gesture::Other
        );
        // An end without any motion, and the thumb pinch (Launchpad), do nothing.
        assert_eq!(t.on_event(&dock(PHASE_ENDED, 1, 0.0, 0.0)), Gesture::Other);
        t.on_event(&dock(PHASE_BEGAN, 3, 0.0, 0.0));
        assert_eq!(t.on_event(&dock(PHASE_ENDED, 3, 0.5, 0.0)), Gesture::Other);
        // A swipe whose start was missed is still recognised.
        t.on_event(&dock(PHASE_CHANGED, 1, -0.5, 0.0));
        assert_eq!(
            t.on_event(&dock(PHASE_ENDED, 1, 0.0, 0.0)),
            Gesture::SwitchSpace { next: false }
        );
    }

    #[test]
    fn pinches_report_magnification_and_the_rest_is_other() {
        let mut t = GestureTracker::default();
        let zoom = RawGesture {
            cgs_type: CGS_GESTURE,
            hid_type: HID_ZOOM,
            phase: PHASE_CHANGED,
            magnification: 0.04,
            ..RawGesture::default()
        };
        assert_eq!(
            t.on_event(&zoom),
            Gesture::Magnify {
                delta: 0.04,
                phase: GesturePhase::Changed
            }
        );
        let bogus = RawGesture {
            magnification: f64::NAN,
            phase: PHASE_ENDED,
            ..zoom
        };
        assert_eq!(
            t.on_event(&bogus),
            Gesture::Magnify {
                delta: 0.0,
                phase: GesturePhase::Ended
            }
        );
        // Rotation, smart zoom, scroll companions.
        for hid_type in [5, 22, 6] {
            let other = RawGesture { hid_type, ..zoom };
            assert_eq!(t.on_event(&other), Gesture::Other);
        }
        // A zoom-typed Dock event is not a pinch.
        let odd = RawGesture {
            cgs_type: CGS_DOCK_CONTROL,
            ..zoom
        };
        assert_eq!(t.on_event(&odd), Gesture::Other);
    }

    #[test]
    fn flags_changed_for_modifiers() {
        for (code, bit, _) in MODIFIER_KEYS {
            assert_eq!(
                flags_changed(code, bit | CONTROL),
                Some(FlagsChange::Down),
                "{code:#x}"
            );
            assert_eq!(
                flags_changed(code, CONTROL),
                Some(FlagsChange::Up),
                "{code:#x}"
            );
        }
        assert_eq!(
            flags_changed(KVK_CAPS_LOCK, ALPHA_SHIFT),
            Some(FlagsChange::Tap)
        );
        assert_eq!(flags_changed(KVK_CAPS_LOCK, 0), Some(FlagsChange::Tap));
        assert_eq!(flags_changed(KVK_FUNCTION, 0x80_0000), None);
        assert_eq!(flags_changed(0x00, LCTRL), None, "KeyA is no modifier");
    }

    #[test]
    fn flags_and_masks_round_trip() {
        for raw in 0..=255u8 {
            let mask = ModifierMask(raw);
            assert_eq!(mask_from_flags(flags_for(mask)), mask, "{raw:#010b}");
        }
        let f = flags_for(ModifierMask(0b0001_0010)); // LShift + RCtrl
        assert_eq!(f, LSHIFT | RCTRL | SHIFT | CONTROL);
        // Synthetic flags without device bits map to the left keys.
        assert_eq!(mask_from_flags(COMMAND | SHIFT), ModifierMask(0b0000_1010));
    }

    #[test]
    fn scroll_conversion() {
        assert_eq!(scroll_from_fixed(1.0, 0.0), (0, 120));
        assert_eq!(scroll_from_fixed(-0.5, 0.0), (0, -60));
        assert_eq!(scroll_from_fixed(0.0, 1.0), (-120, 0));
        assert_eq!(scroll_from_fixed(f64::NAN, f64::INFINITY), (i32::MIN, 0));
        assert_eq!(scroll_from_points(7, 0), (0, 7));
        assert_eq!(scroll_from_points(0, 3), (-3, 0));
        assert_eq!(scroll_from_points(i64::MIN, i64::MAX), (i32::MIN, i32::MIN));
    }

    #[test]
    fn wheel_accumulator_carries_remainder() {
        let mut w = WheelAccumulator::new();
        assert_eq!(w.push(60), 0);
        assert_eq!(w.push(60), 1);
        assert_eq!(w.remainder(), 0);
        assert_eq!(w.push(250), 2);
        assert_eq!(w.remainder(), 10);
        assert_eq!(w.push(-130), -1);
        assert_eq!(w.remainder(), 0);
        assert_eq!(w.push(-30), 0);
        assert_eq!(w.remainder(), -30);
        assert_eq!(w.push(-90), -1);
    }

    #[test]
    fn pixel_accumulator_keeps_touchpad_deltas() {
        let mut w = WheelAccumulator::pixels();
        assert_eq!(w.push(120), PIXELS_PER_NOTCH, "one notch");
        // A precision touchpad sends a few units at a time: nothing is lost.
        let total: i32 = (0..30).map(|_| w.push(5)).sum();
        assert_eq!(total, 50);
        assert_eq!(w.push(-1), 0);
        assert_eq!(w.push(-2), -1);
    }

    #[test]
    fn click_counter_interval_and_distance() {
        let t0 = Instant::now();
        let iv = Duration::from_millis(500);
        let ms = Duration::from_millis(1);
        let p = PointF::new(100.0, 100.0);
        let mut c = ClickCounter::new();
        assert_eq!(c.on_down(MouseButton::Left, p, t0, iv), 1);
        assert_eq!(
            c.on_down(
                MouseButton::Left,
                PointF::new(103.0, 102.0),
                t0 + 200 * ms,
                iv
            ),
            2
        );
        assert_eq!(c.on_down(MouseButton::Left, p, t0 + 400 * ms, iv), 3);
        // Too late.
        assert_eq!(c.on_down(MouseButton::Left, p, t0 + 1000 * ms, iv), 1);
        // Too far.
        assert_eq!(
            c.on_down(
                MouseButton::Left,
                PointF::new(110.0, 100.0),
                t0 + 1100 * ms,
                iv
            ),
            1
        );
        // Other button.
        assert_eq!(
            c.on_down(
                MouseButton::Right,
                PointF::new(110.0, 100.0),
                t0 + 1200 * ms,
                iv
            ),
            1
        );
    }
}
