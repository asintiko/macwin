//! Validated screen sets and the primitive geometry used for edge crossing.

use crate::types::{GeometryError, Point, PointF, ScreenInfo};

/// Most screens accepted per machine.
pub const MAX_SCREENS: usize = 16;
/// Accepted `native_per_logical` range.
pub const NATIVE_PER_LOGICAL_RANGE: std::ops::RangeInclusive<f64> = 0.25..=8.0;

/// Half-open bounding box of a screen set (not size-limited like `Rect`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    pub left: i32,
    pub top: i32,
    /// Exclusive.
    pub right: i32,
    /// Exclusive.
    pub bottom: i32,
}

/// A validated, non-empty set of at most 16 screens with unique ids and a
/// sane scale. Screens may overlap.
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenSet {
    screens: Vec<ScreenInfo>,
    bounds: Bounds,
}

impl ScreenSet {
    pub fn new(screens: Vec<ScreenInfo>) -> Result<ScreenSet, GeometryError> {
        if screens.is_empty() {
            return Err(GeometryError::NoScreens);
        }
        if screens.len() > MAX_SCREENS {
            return Err(GeometryError::TooManyScreens(screens.len()));
        }
        for (i, s) in screens.iter().enumerate() {
            if screens[..i].iter().any(|o| o.id == s.id) {
                return Err(GeometryError::DuplicateScreenId(s.id));
            }
            if !s.native_per_logical.is_finite()
                || !NATIVE_PER_LOGICAL_RANGE.contains(&s.native_per_logical)
            {
                return Err(GeometryError::InvalidScale(s.native_per_logical));
            }
        }
        let bounds = Bounds {
            left: screens.iter().map(|s| s.rect.left()).min().unwrap_or(0),
            top: screens.iter().map(|s| s.rect.top()).min().unwrap_or(0),
            right: screens.iter().map(|s| s.rect.right()).max().unwrap_or(1),
            bottom: screens.iter().map(|s| s.rect.bottom()).max().unwrap_or(1),
        };
        Ok(ScreenSet { screens, bounds })
    }

    pub fn screens(&self) -> &[ScreenInfo] {
        &self.screens
    }

    pub fn bounds(&self) -> Bounds {
        self.bounds
    }

    /// Index of the first screen containing the pixel `p`.
    pub fn index_at(&self, p: Point) -> Option<usize> {
        self.screens.iter().position(|s| s.rect.contains(p))
    }

    /// True when the pixel `p` lies on some screen.
    pub fn contains(&self, p: Point) -> bool {
        self.index_at(p).is_some()
    }

    /// True when the pixel containing `p` lies on some screen.
    pub fn contains_f(&self, p: PointF) -> bool {
        p.is_finite() && self.contains(p.floor())
    }

    /// `p` if it is on screen; otherwise the nearest point (Euclidean) over
    /// every screen's closed pixel box, ties to the lowest screen index.
    /// Non-finite coordinates are sanitised first, so the result is always on
    /// screen.
    pub fn clamp_f(&self, p: PointF) -> PointF {
        let p = sanitize(p);
        if self.contains_f(p) {
            return p;
        }
        let mut best = self.screens[0].rect.clamp_f(p);
        let mut best_d = dist2(best, p);
        for s in &self.screens[1..] {
            let c = s.rect.clamp_f(p);
            let d = dist2(c, p);
            if d < best_d {
                best = c;
                best_d = d;
            }
        }
        best
    }

    /// Integer version of [`ScreenSet::clamp_f`].
    pub fn clamp(&self, p: Point) -> Point {
        if self.contains(p) {
            return p;
        }
        self.clamp_f(p.to_f()).floor()
    }

    /// The screen containing `clamp_f(p)`.
    pub fn screen_for(&self, p: PointF) -> &ScreenInfo {
        let c = self.clamp_f(p).floor();
        let i = self.index_at(c).unwrap_or(0);
        &self.screens[i]
    }
}

fn dist2(a: PointF, b: PointF) -> f64 {
    let (dx, dy) = (a.x - b.x, a.y - b.y);
    dx * dx + dy * dy
}

/// Keeps coordinates finite: NaN -> 0, +-inf -> +-1e12.
fn sanitize_coord(v: f64) -> f64 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(-1e12, 1e12)
    }
}

fn sanitize(p: PointF) -> PointF {
    PointF::new(sanitize_coord(p.x), sanitize_coord(p.y))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::types::Rect;

    pub(crate) fn screen(id: u32, x: i32, y: i32, w: i32, h: i32, npl: f64) -> ScreenInfo {
        ScreenInfo {
            id,
            rect: Rect::new(x, y, w, h).unwrap(),
            native_per_logical: npl,
            primary: id == 1,
        }
    }

    #[test]
    fn validation() {
        assert_eq!(ScreenSet::new(vec![]), Err(GeometryError::NoScreens));
        let many: Vec<_> = (0..17)
            .map(|i| screen(i, i as i32 * 10, 0, 10, 10, 1.0))
            .collect();
        assert_eq!(ScreenSet::new(many), Err(GeometryError::TooManyScreens(17)));
        assert_eq!(
            ScreenSet::new(vec![
                screen(1, 0, 0, 10, 10, 1.0),
                screen(1, 10, 0, 10, 10, 1.0)
            ]),
            Err(GeometryError::DuplicateScreenId(1))
        );
        assert_eq!(
            ScreenSet::new(vec![screen(1, 0, 0, 10, 10, 0.1)]),
            Err(GeometryError::InvalidScale(0.1))
        );
        assert!(ScreenSet::new(vec![screen(1, 0, 0, 10, 10, f64::NAN)]).is_err());
        // Overlaps are allowed.
        assert!(
            ScreenSet::new(vec![
                screen(1, 0, 0, 10, 10, 1.0),
                screen(2, 5, 5, 10, 10, 1.0)
            ])
            .is_ok()
        );
    }

    #[test]
    fn bounds_of_a_set() {
        let s = ScreenSet::new(vec![
            screen(1, 0, 0, 1440, 900, 1.0),
            screen(2, 1440, -100, 1920, 1080, 1.0),
        ])
        .unwrap();
        assert_eq!(
            s.bounds(),
            Bounds {
                left: 0,
                top: -100,
                right: 3360,
                bottom: 980
            }
        );
    }

    #[test]
    fn clamp_behaviour() {
        let s = ScreenSet::new(vec![
            screen(1, 0, 0, 100, 100, 1.0),
            screen(2, 100, 50, 100, 100, 1.0),
        ])
        .unwrap();
        // On screen: unchanged, sub-pixel kept.
        assert_eq!(
            s.clamp_f(PointF::new(10.5, 20.25)),
            PointF::new(10.5, 20.25)
        );
        // In the gap below screen 1 and left of screen 2: nearest box wins.
        assert_eq!(
            s.clamp_f(PointF::new(98.0, 120.0)),
            PointF::new(100.0, 120.0)
        );
        // Far right: clamped onto screen 2's last column.
        assert_eq!(s.clamp(Point::new(500, 60)), Point::new(199, 60));
        // Tie between the two screens goes to the lowest index.
        let tie = ScreenSet::new(vec![
            screen(1, 0, 0, 10, 10, 1.0),
            screen(2, 20, 0, 10, 10, 1.0),
        ])
        .unwrap();
        assert_eq!(tie.clamp_f(PointF::new(14.5, 5.0)), PointF::new(9.0, 5.0));
        // Non-finite input still lands on screen.
        assert!(s.contains_f(s.clamp_f(PointF::new(f64::NAN, f64::INFINITY))));
    }
}
