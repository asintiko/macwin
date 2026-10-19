//! Pure helper math for the OS backends. Compiled and tested on every OS so
//! the Windows conversions are verified on the Mac.

pub mod macos;
pub mod windows;

use crate::types::{Point, PointF};

/// How far (native units, on either axis) a grabbed cursor may be from its
/// parking point before the backend puts it back: one mouse count is caught,
/// sub-pixel rounding is not.
pub const PARK_TOLERANCE: f64 = 0.5;

/// Whether a grabbed cursor left its parking point. While another computer
/// is controlled the local cursor must stay put, but the OS does not always
/// keep it there (macOS freezes it only for the frontmost app), so a backend
/// that sees this warps it back. A non-finite position never counts.
pub fn left_park(at: PointF, park: Point) -> bool {
    at.is_finite()
        && ((at.x - f64::from(park.x)).abs() >= PARK_TOLERANCE
            || (at.y - f64::from(park.y)).abs() >= PARK_TOLERANCE)
}

/// Tag written into every injected event (`kCGEventSourceUserData` on macOS,
/// `dwExtraInfo` on Windows) so capture can recognise and ignore our own
/// events: "macwin" + 0001.
pub const INJECT_TAG: u64 = 0x6D61_6377_696E_0001;

/// Pixels per wheel notch (120 units) when converting between the two
/// units: close to the 3 lines Windows scrolls per notch.
pub const PIXELS_PER_NOTCH: i32 = 40;

/// A pixel wheel delta in 1/120 notch units.
pub fn pixels_to_notch120(px: i32) -> i32 {
    px.saturating_mul(120 / PIXELS_PER_NOTCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARK: Point = Point::new(1439, 450);

    #[test]
    fn a_frozen_cursor_stays_parked() {
        assert!(!left_park(PARK.to_f(), PARK));
        // Sub-pixel noise (Retina half points, rounding) is not a move.
        assert!(!left_park(PointF::new(1439.3, 449.6), PARK));
    }

    #[test]
    fn one_mouse_count_on_either_axis_leaves_the_park() {
        assert!(left_park(PointF::new(1438.0, 450.0), PARK));
        assert!(left_park(PointF::new(1439.0, 451.0), PARK));
        assert!(left_park(PointF::new(1439.5, 450.0), PARK));
        // A cursor that followed the mouse across the screen.
        assert!(left_park(PointF::new(700.0, 300.0), PARK));
    }

    #[test]
    fn works_on_screens_left_of_and_above_the_main_one() {
        let park = Point::new(-1200, -80);
        assert!(!left_park(PointF::new(-1200.2, -80.0), park));
        assert!(left_park(PointF::new(-1203.0, -80.0), park));
    }

    #[test]
    fn a_non_finite_position_never_counts() {
        assert!(!left_park(PointF::new(f64::NAN, 450.0), PARK));
        assert!(!left_park(PointF::new(1439.0, f64::INFINITY), PARK));
    }
}
