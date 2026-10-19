//! Domain vocabulary types shared by every macwin crate.
//!
//! Coordinates are always in the owning machine's native space:
//! - macOS: CG global display space in points (origin at the top-left of the
//!   main display, y down);
//! - Windows: virtual-screen physical pixels under per-monitor-v2 DPI
//!   awareness (origin at the primary monitor's top-left; may be negative).
//!
//! Wheel convention everywhere in macwin: `dy > 0` scrolls toward the top of
//! the content (wheel rotated away from the user), `dx > 0` scrolls right
//! (the `WM_MOUSEWHEEL` / `WM_MOUSEHWHEEL` convention). Backends convert to
//! and from their OS.

/// A USB HID usage: `(usage_page << 16) | usage`. Page 0x07 is the keyboard
/// page, page 0x0C the consumer page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hid(pub u32);

impl Hid {
    pub const ESCAPE: Hid = Hid(0x07_0029);
    pub const LEFT_CTRL: Hid = Hid(0x07_00E0);
    pub const LEFT_SHIFT: Hid = Hid(0x07_00E1);
    pub const LEFT_ALT: Hid = Hid(0x07_00E2);
    pub const LEFT_GUI: Hid = Hid(0x07_00E3);
    pub const RIGHT_CTRL: Hid = Hid(0x07_00E4);
    pub const RIGHT_SHIFT: Hid = Hid(0x07_00E5);
    pub const RIGHT_ALT: Hid = Hid(0x07_00E6);
    pub const RIGHT_GUI: Hid = Hid(0x07_00E7);
    pub const TAB: Hid = Hid(0x07_002B);
    pub const SPACE: Hid = Hid(0x07_002C);
    pub const RIGHT_ARROW: Hid = Hid(0x07_004F);
    pub const LEFT_ARROW: Hid = Hid(0x07_0050);
    pub const DOWN_ARROW: Hid = Hid(0x07_0051);
    pub const UP_ARROW: Hid = Hid(0x07_0052);
    /// The Mac's Fn / 🌐 key (Apple vendor top-case page). It has no native
    /// code on either OS in the keymap, so it is never injected: the
    /// controller turns a tap of it into the peer's language switch.
    pub const APPLE_FN: Hid = Hid(0x00FF_0003);

    /// The eight modifier keys in `ModifierMask` bit order.
    pub const MODIFIERS: [Hid; 8] = [
        Hid::LEFT_CTRL,
        Hid::LEFT_SHIFT,
        Hid::LEFT_ALT,
        Hid::LEFT_GUI,
        Hid::RIGHT_CTRL,
        Hid::RIGHT_SHIFT,
        Hid::RIGHT_ALT,
        Hid::RIGHT_GUI,
    ];

    /// The usage page (bits 16..32).
    pub const fn page(self) -> u16 {
        (self.0 >> 16) as u16
    }

    /// The usage within the page (bits 0..16).
    pub const fn usage(self) -> u16 {
        self.0 as u16
    }

    /// Bit index of a modifier key in a `ModifierMask` / USB boot-report
    /// modifier byte: LeftCtrl (0xE0) ..= RightGUI (0xE7) -> 0..=7.
    pub const fn modifier_bit(self) -> Option<u8> {
        match self.0 {
            0x07_00E0..=0x07_00E7 => Some((self.0 - Hid::LEFT_CTRL.0) as u8),
            _ => None,
        }
    }

    /// True for the eight modifier keys.
    pub const fn is_modifier(self) -> bool {
        self.modifier_bit().is_some()
    }
}

/// Modifier key state: bit `i` is set while `Hid(0x07_00E0 + i)` is held.
/// Same layout as the USB boot-report modifier byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ModifierMask(pub u8);

impl ModifierMask {
    pub const EMPTY: ModifierMask = ModifierMask(0);
    /// Either Ctrl key.
    pub const CTRL: ModifierMask = ModifierMask(0b0001_0001);
    /// Either Shift key.
    pub const SHIFT: ModifierMask = ModifierMask(0b0010_0010);
    /// Either Alt / Option key.
    pub const ALT: ModifierMask = ModifierMask(0b0100_0100);
    /// Either GUI key (Windows key / Command).
    pub const GUI: ModifierMask = ModifierMask(0b1000_1000);

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// True when the modifier key `hid` is set. Always false for non-modifiers.
    pub const fn contains(self, hid: Hid) -> bool {
        match hid.modifier_bit() {
            Some(bit) => self.0 & (1 << bit) != 0,
            None => false,
        }
    }

    /// True when any bit of `other` is set in `self`.
    pub const fn intersects(self, other: ModifierMask) -> bool {
        self.0 & other.0 != 0
    }

    /// Sets the bit of `hid`; no-op for non-modifiers.
    pub fn insert(&mut self, hid: Hid) {
        if let Some(bit) = hid.modifier_bit() {
            self.0 |= 1 << bit;
        }
    }

    /// Clears the bit of `hid`; no-op for non-modifiers.
    pub fn remove(&mut self, hid: Hid) {
        if let Some(bit) = hid.modifier_bit() {
            self.0 &= !(1 << bit);
        }
    }

    /// The modifier keys that are set, in bit order.
    pub fn keys(self) -> impl Iterator<Item = Hid> {
        Hid::MODIFIERS
            .into_iter()
            .filter(move |&hid| self.contains(hid))
    }
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

impl MouseButton {
    pub const ALL: [MouseButton; 5] = [
        MouseButton::Left,
        MouseButton::Right,
        MouseButton::Middle,
        MouseButton::Back,
        MouseButton::Forward,
    ];

    /// Bit index used by `ButtonSet`.
    pub const fn bit(self) -> u8 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Right => 1,
            MouseButton::Middle => 2,
            MouseButton::Back => 3,
            MouseButton::Forward => 4,
        }
    }
}

/// Unit of a wheel delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WheelUnit {
    /// 120 = one detent (Windows `WHEEL_DELTA`); smaller values are high-res.
    Notch120,
    /// Pixels of continuous scrolling (Mac trackpads and Magic Mouse);
    /// [`crate::os::PIXELS_PER_NOTCH`] pixels make one notch.
    Pixel,
}

/// Operating system of a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Os {
    MacOs,
    Windows,
}

/// Where the peer sits relative to me ("peer is to my left/right").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub const fn opposite(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
}

/// The role of one machine in the current control period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RoleKind {
    /// Local input stays local.
    #[default]
    Idle,
    /// Local input is grabbed and forwarded to the peer.
    Controlling,
    /// The peer's input is injected here.
    Controlled,
}

/// An integer point in native coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Point {
        Point { x, y }
    }

    pub fn to_f(self) -> PointF {
        PointF::new(f64::from(self.x), f64::from(self.y))
    }
}

/// A sub-pixel point in native coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PointF {
    pub x: f64,
    pub y: f64,
}

impl PointF {
    pub const fn new(x: f64, y: f64) -> PointF {
        PointF { x, y }
    }

    /// The pixel containing this point. Float-to-int casts saturate, and NaN
    /// maps to 0.
    pub fn floor(self) -> Point {
        Point::new(self.x.floor() as i32, self.y.floor() as i32)
    }

    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

/// Error building geometry values.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum GeometryError {
    #[error("rectangle size {w}x{h} is outside 1..=65536")]
    InvalidSize { w: i32, h: i32 },
    #[error("rectangle origin ({x}, {y}) is outside +-2^20")]
    InvalidOrigin { x: i32, y: i32 },
    #[error("the screen list is empty")]
    NoScreens,
    #[error("{0} screens given, at most 16 are supported")]
    TooManyScreens(usize),
    #[error("duplicate screen id {0}")]
    DuplicateScreenId(u32),
    #[error("native_per_logical {0} is outside 0.25..=8")]
    InvalidScale(f64),
}

/// A half-open rectangle `[x, x + w) x [y, y + h)`. Built only through
/// [`Rect::new`], so sizes and coordinates are always in range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Rect {
    /// Largest accepted width or height.
    pub const MAX_SIZE: i32 = 65_536;
    /// Largest accepted absolute origin coordinate.
    pub const MAX_COORD: i32 = 1 << 20;

    /// Validates `w, h` in `1..=65536` and `|x|, |y| <= 2^20`.
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Result<Rect, GeometryError> {
        if !(1..=Self::MAX_SIZE).contains(&w) || !(1..=Self::MAX_SIZE).contains(&h) {
            return Err(GeometryError::InvalidSize { w, h });
        }
        if x.unsigned_abs() > Self::MAX_COORD.unsigned_abs()
            || y.unsigned_abs() > Self::MAX_COORD.unsigned_abs()
        {
            return Err(GeometryError::InvalidOrigin { x, y });
        }
        Ok(Rect { x, y, w, h })
    }

    pub const fn left(self) -> i32 {
        self.x
    }

    pub const fn top(self) -> i32 {
        self.y
    }

    /// Exclusive right edge.
    pub const fn right(self) -> i32 {
        self.x + self.w
    }

    /// Exclusive bottom edge.
    pub const fn bottom(self) -> i32 {
        self.y + self.h
    }

    pub const fn width(self) -> i32 {
        self.w
    }

    pub const fn height(self) -> i32 {
        self.h
    }

    pub const fn contains(self, p: Point) -> bool {
        p.x >= self.x && p.x < self.x + self.w && p.y >= self.y && p.y < self.y + self.h
    }

    /// Clamps `p` into the closed pixel box `[x, x+w-1] x [y, y+h-1]`.
    pub fn clamp_f(self, p: PointF) -> PointF {
        PointF::new(
            p.x.clamp(f64::from(self.x), f64::from(self.right() - 1)),
            p.y.clamp(f64::from(self.y), f64::from(self.bottom() - 1)),
        )
    }

    /// Clamps `p` into the closed pixel box.
    pub fn clamp(self, p: Point) -> Point {
        Point::new(
            p.x.clamp(self.x, self.right() - 1),
            p.y.clamp(self.y, self.bottom() - 1),
        )
    }
}

/// One monitor of a machine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenInfo {
    pub id: u32,
    pub rect: Rect,
    /// Native units per logical unit: 1.0 on macOS (points), DPI/96 on Windows.
    pub native_per_logical: f64,
    pub primary: bool,
}

/// An input event from a capture backend. Backends only report physical
/// input: our own injected events and other synthetic input are filtered out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CapturedEvent {
    /// `pos` is the local cursor position (native units). `delta` is the
    /// post-acceleration movement; while grabbed it is the only meaningful
    /// field because the cursor is parked.
    MouseMove {
        pos: PointF,
        delta: PointF,
    },
    Button {
        button: MouseButton,
        down: bool,
        pos: PointF,
    },
    Wheel {
        dx: i32,
        dy: i32,
        unit: WheelUnit,
    },
    Key {
        hid: Hid,
        down: bool,
        repeat: bool,
    },
    /// A trackpad gesture (macOS).
    Gesture(Gesture),
}

/// A trackpad gesture as the macOS capture backend recognises it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gesture {
    /// Two-finger pinch: the relative change of this event, > 0 when the
    /// fingers spread (zoom in).
    Magnify { delta: f64, phase: GesturePhase },
    /// A finished horizontal three/four-finger swipe that switches Spaces:
    /// to the next one (the Space on the right) or the previous one.
    SwitchSpace { next: bool },
    /// A finished vertical three/four-finger swipe: Mission Control or App
    /// Exposé.
    Overview,
    /// Any other gesture event (rotation, smart zoom, Launchpad, the events
    /// that accompany a swipe or a scroll): nothing to forward, but it must
    /// not reach the local OS while the peer is controlled.
    Other,
}

/// Where a continuous gesture is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GesturePhase {
    Began,
    Changed,
    Ended,
    Other,
}

/// Command from the Engine to the capture backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrabCommand {
    /// Swallow local input, hide/park the cursor at `exit`.
    Grab { exit: Point },
    /// Stop swallowing and show the cursor at `warp_to`.
    Release { warp_to: Point },
    /// After a fault the backend has already released locally; re-enable
    /// normal capture so a new grab is possible.
    Rearm,
}

/// A fail-safe that fired inside the capture backend. The backend has
/// already restored local control when it reports one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureFault {
    /// The emergency chord (Ctrl+Alt+Shift+Esc) was pressed.
    EmergencyChord,
    /// The grab lease was not renewed in time.
    LeaseExpired,
    /// The sink did not answer within the backend's bound.
    SinkTimeout,
    /// The OS disabled the event tap / hook.
    TapDisabled,
    /// The capture callback panicked.
    CallbackPanic,
}

/// Command from the Engine to the injection backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectCommand {
    MoveTo(Point),
    Button {
        button: MouseButton,
        down: bool,
        at: Point,
    },
    Wheel {
        dx: i32,
        dy: i32,
        unit: WheelUnit,
    },
    Key {
        hid: Hid,
        down: bool,
        repeat: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_and_usage() {
        assert_eq!((Hid::ESCAPE.page(), Hid::ESCAPE.usage()), (0x07, 0x29));
        assert_eq!(
            (Hid(0x0C_00CD).page(), Hid(0x0C_00CD).usage()),
            (0x0C, 0xCD)
        );
        assert_eq!(
            (Hid(0x0C_0183).page(), Hid(0x0C_0183).usage()),
            (0x0C, 0x0183)
        );
    }

    #[test]
    fn modifier_bits() {
        for (bit, hid) in (0u8..).zip(Hid::MODIFIERS) {
            assert_eq!(hid.modifier_bit(), Some(bit), "{hid:?}");
            assert_eq!(hid.0, 0x07_00E0 + u32::from(bit));
            assert!(hid.is_modifier());
        }
        for hid in [
            Hid::ESCAPE,
            Hid(0x07_00DF),
            Hid(0x07_00E8),
            Hid(0x0C_00E0),
            Hid(0x00_00E0),
        ] {
            assert_eq!(hid.modifier_bit(), None, "{hid:?}");
            assert!(!hid.is_modifier());
        }
    }

    #[test]
    fn modifier_mask_ops() {
        let mut m = ModifierMask::EMPTY;
        assert!(m.is_empty());
        m.insert(Hid::LEFT_SHIFT);
        m.insert(Hid::RIGHT_GUI);
        m.insert(Hid::ESCAPE); // ignored
        assert_eq!(m, ModifierMask(0b1000_0010));
        assert!(m.contains(Hid::LEFT_SHIFT) && m.contains(Hid::RIGHT_GUI));
        assert!(!m.contains(Hid::ESCAPE));
        assert!(m.intersects(ModifierMask::SHIFT) && m.intersects(ModifierMask::GUI));
        assert!(!m.intersects(ModifierMask::CTRL));
        assert_eq!(
            m.keys().collect::<Vec<_>>(),
            vec![Hid::LEFT_SHIFT, Hid::RIGHT_GUI]
        );
        m.remove(Hid::LEFT_SHIFT);
        assert_eq!(m, ModifierMask(0b1000_0000));
    }

    #[test]
    fn side_opposite() {
        assert_eq!(Side::Left.opposite(), Side::Right);
        assert_eq!(Side::Right.opposite(), Side::Left);
    }

    #[test]
    fn point_floor_saturates() {
        assert_eq!(PointF::new(1.9, -0.1).floor(), Point::new(1, -1));
        assert_eq!(
            PointF::new(f64::NAN, 1e300).floor(),
            Point::new(0, i32::MAX)
        );
    }

    #[test]
    fn rect_validation() {
        assert!(Rect::new(0, 0, 1, 1).is_ok());
        assert!(Rect::new(-(1 << 20), 1 << 20, 65_536, 65_536).is_ok());
        assert_eq!(
            Rect::new(0, 0, 0, 10),
            Err(GeometryError::InvalidSize { w: 0, h: 10 })
        );
        assert_eq!(
            Rect::new(0, 0, 10, -1),
            Err(GeometryError::InvalidSize { w: 10, h: -1 })
        );
        assert_eq!(
            Rect::new(0, 0, 65_537, 1),
            Err(GeometryError::InvalidSize { w: 65_537, h: 1 })
        );
        assert_eq!(
            Rect::new((1 << 20) + 1, 0, 1, 1),
            Err(GeometryError::InvalidOrigin {
                x: (1 << 20) + 1,
                y: 0
            })
        );
        assert_eq!(
            Rect::new(0, i32::MIN, 1, 1),
            Err(GeometryError::InvalidOrigin { x: 0, y: i32::MIN })
        );
    }

    #[test]
    fn rect_accessors_and_clamp() {
        let r = Rect::new(-10, 20, 100, 50).unwrap();
        assert_eq!(
            (r.left(), r.top(), r.right(), r.bottom()),
            (-10, 20, 90, 70)
        );
        assert_eq!((r.width(), r.height()), (100, 50));
        assert!(r.contains(Point::new(-10, 20)));
        assert!(r.contains(Point::new(89, 69)));
        assert!(!r.contains(Point::new(90, 69)));
        assert!(!r.contains(Point::new(89, 70)));
        assert_eq!(r.clamp(Point::new(1000, -1000)), Point::new(89, 20));
        // Closed pixel box: the last row/column is the integer bound.
        assert_eq!(r.clamp_f(PointF::new(95.5, 69.9)), PointF::new(89.0, 69.0));
        assert_eq!(
            r.clamp_f(PointF::new(10.5, 30.25)),
            PointF::new(10.5, 30.25)
        );
    }
}
