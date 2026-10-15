//! Where the cursor crosses between my screens and the peer's.
//!
//! A [`PeerMap`] puts the peer's screens into my coordinate space (the
//! primary computer's placement, resolved for this session by
//! [`crate::arrange::resolve`]). The cursor crosses only where one of my
//! screens really touches one of the peer's: probes half a unit beyond an
//! edge find the screen on the other side, and a small touch tolerance
//! absorbs the rounding between the two coordinate spaces.

use crate::arrange::{Edge, PeerMap, RectF};
use crate::geometry::ScreenSet;
use crate::types::{Point, PointF, Rect};

/// How far (native units) a cursor returned to my screens lands inside the
/// edge, so the next local event does not immediately re-enter the peer.
pub const EDGE_REENTRY_INSET: i32 = 2;

/// How far past an edge (in the native units of the space the probe lives
/// in) a probe looks for the screen on the other side.
const PROBE: f64 = 0.5;
/// How far apart (native units) a probe and a screen may be and still touch.
const TOUCH_TOLERANCE: f64 = 1.0;

/// Result of a successful edge crossing from my screens onto the peer's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crossing {
    /// Virtual cursor start, peer native coordinates.
    pub entry: PointF,
    /// Where my cursor left (parked), my native coordinates.
    pub exit: Point,
    /// `native_per_logical` of my screen containing `exit`.
    pub npl_src: f64,
    /// The side of my screen the cursor left through.
    pub edge: Edge,
}

/// One virtual-cursor step on the peer's screens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    /// The virtual cursor moved; the point is on a peer screen.
    Moved(PointF),
    /// The cursor crossed back onto my screens; warp my cursor here.
    Exited { warp_to: Point },
}

/// Static mapping for one session.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    local: ScreenSet,
    peer: ScreenSet,
    map: Option<PeerMap>,
    /// My screens as float rectangles (my native space).
    local_rects: Vec<RectF>,
    /// The peer's screens in my native space, in `peer` order (empty
    /// without a map).
    peer_in_local: Vec<RectF>,
}

impl Layout {
    /// `map` takes peer native coordinates to mine; `None` means the peer
    /// cannot be entered from here (`local` still clamps what the peer
    /// injects).
    pub fn new(local: ScreenSet, peer: ScreenSet, map: Option<PeerMap>) -> Layout {
        let rects = |set: &ScreenSet| -> Vec<RectF> {
            set.screens()
                .iter()
                .map(|s| RectF::from_rect(s.rect))
                .collect()
        };
        let local_rects = rects(&local);
        let peer_in_local = map.map_or_else(Vec::new, |m| {
            rects(&peer)
                .into_iter()
                .map(|r| m.rect_to_local(r))
                .collect()
        });
        Layout {
            local,
            peer,
            map,
            local_rects,
            peer_in_local,
        }
    }

    pub fn local(&self) -> &ScreenSet {
        &self.local
    }

    pub fn peer(&self) -> &ScreenSet {
        &self.peer
    }

    /// Peer native to my native coordinates; `None` when the peer cannot be
    /// entered from here.
    pub fn map(&self) -> Option<PeerMap> {
        self.map
    }

    /// If the local cursor at `pos` sits on the last pixel row or column of
    /// one of my screens where a peer screen touches, the crossing onto it.
    /// At a corner the edge `delta` pushes hardest against wins. Other
    /// preconditions (session, capabilities, buttons) are the Engine's
    /// business.
    pub fn try_enter(&self, pos: PointF, delta: PointF) -> Option<Crossing> {
        let map = self.map?;
        let p = self.local.clamp_f(pos);
        let exit = p.floor();
        let screen = *self.local.screen_for(p);
        let rect = RectF::from_rect(screen.rect);
        let centre = PointF::new(f64::from(exit.x) + 0.5, f64::from(exit.y) + 0.5);
        let mut best: Option<(f64, Edge, usize, PointF)> = None;
        for edge in Edge::ALL {
            let line = rect.edge_pos(edge);
            let probe = beyond(with_across(centre, edge, line), edge, PROBE);
            let on_last_pixel = (across(centre, edge) - line).abs() <= PROBE;
            if !on_last_pixel || self.local.contains_f(probe) {
                continue;
            }
            let Some(i) = nearest(&self.peer_in_local, probe) else {
                continue;
            };
            let push = outward(edge, delta);
            if best.is_none_or(|(b, ..)| push > b) {
                best = Some((push, edge, i, probe));
            }
        }
        let (_, edge, i, probe) = best?;
        let target = self.peer.screens()[i].rect;
        let c = clamp_into(target, map.to_peer(probe));
        // Pinned to the pixel line that faces me.
        let entry = with_across(c, edge, f64::from(edge_pixel(target, edge.opposite())));
        Some(Crossing {
            entry,
            exit,
            npl_src: screen.native_per_logical,
            edge,
        })
    }

    /// Moves the virtual cursor `v` (on a peer screen) by the local delta
    /// `delta`, normalising speed by the two screens' scales. Leaving is
    /// suppressed while `buttons_held` (no leaving mid-drag).
    pub fn step(&self, v: PointF, delta: PointF, npl_src: f64, buttons_held: bool) -> Step {
        let v = self.peer.clamp_f(v);
        let s = *self.peer.screen_for(v);
        let npl_tgt = s.native_per_logical;
        // d_t = d_s / npl_src * npl_tgt; exact identity for equal scales and
        // multiply-then-divide otherwise (3 px at 1.5 -> exactly 2 points).
        let d = if !delta.is_finite() {
            PointF::default()
        } else if npl_tgt == npl_src {
            delta
        } else {
            PointF::new(delta.x * npl_tgt / npl_src, delta.y * npl_tgt / npl_src)
        };
        let next = PointF::new(v.x + d.x, v.y + d.y);
        if self.peer.contains_f(next) {
            return Step::Moved(next);
        }
        if !buttons_held
            && let Some(map) = self.map
            && let Some(warp_to) = self.leave(map, s.rect, next)
        {
            return Step::Exited { warp_to };
        }
        Step::Moved(self.peer.clamp_f(next))
    }

    /// Where my cursor reappears when the virtual cursor leaves the peer
    /// screen `from` towards `next` onto one of my screens.
    fn leave(&self, map: PeerMap, from: Rect, next: PointF) -> Option<Point> {
        let rect = RectF::from_rect(from);
        let on_screen = clamp_into(from, next);
        let horizontal = if next.x < rect.x {
            Some((Edge::Left, rect.x - next.x))
        } else if next.x >= rect.right() {
            Some((Edge::Right, next.x - rect.right()))
        } else {
            None
        };
        let vertical = if next.y < rect.y {
            Some((Edge::Top, rect.y - next.y))
        } else if next.y >= rect.bottom() {
            Some((Edge::Bottom, next.y - rect.bottom()))
        } else {
            None
        };
        let mut edges = [horizontal, vertical];
        if matches!((horizontal, vertical), (Some(h), Some(v)) if v.1 > h.1) {
            edges.swap(0, 1);
        }
        for (edge, _) in edges.into_iter().flatten() {
            let leaving = with_across(on_screen, edge, rect.edge_pos(edge));
            // Another peer screen continues here.
            if self.peer.contains_f(beyond(leaving, edge, PROBE)) {
                continue;
            }
            let probe = beyond(map.to_local(leaving), edge, PROBE);
            let Some(i) = nearest(&self.local_rects, probe) else {
                continue;
            };
            let to = self.local.screens()[i].rect;
            let facing = edge.opposite();
            let at = with_across(probe, facing, f64::from(edge_pixel(to, facing)));
            return Some(inset_into(to, facing, at.floor()));
        }
        None
    }

    /// `exit` moved `EDGE_REENTRY_INSET` away from `edge` (the side of my
    /// screen it left through), clamped into its screen: the warp target
    /// when a grab ends for any reason other than crossing back.
    pub fn inset(&self, exit: Point, edge: Edge) -> Point {
        let exit = self.local.clamp(exit);
        inset_into(self.local.screen_for(exit.to_f()).rect, edge, exit)
    }
}

/// `p` if its pixel is on `r`, else the nearest point of its closed pixel box.
fn clamp_into(r: Rect, p: PointF) -> PointF {
    if r.contains(p.floor()) { p } else { r.clamp_f(p) }
}

/// `p` moved `EDGE_REENTRY_INSET` inward from `edge`, clamped into `r`.
fn inset_into(r: Rect, edge: Edge, p: Point) -> Point {
    let k = EDGE_REENTRY_INSET;
    r.clamp(match edge {
        Edge::Left => Point::new(p.x.saturating_add(k), p.y),
        Edge::Right => Point::new(p.x.saturating_sub(k), p.y),
        Edge::Top => Point::new(p.x, p.y.saturating_add(k)),
        Edge::Bottom => Point::new(p.x, p.y.saturating_sub(k)),
    })
}

/// The pixel column (Left/Right) or row (Top/Bottom) of `r` along `edge`.
fn edge_pixel(r: Rect, edge: Edge) -> i32 {
    match edge {
        Edge::Left => r.left(),
        Edge::Right => r.right() - 1,
        Edge::Top => r.top(),
        Edge::Bottom => r.bottom() - 1,
    }
}

/// The coordinate of `p` across `edge`: x for Left/Right, y for Top/Bottom.
fn across(p: PointF, edge: Edge) -> f64 {
    if edge.is_vertical() { p.x } else { p.y }
}

/// `p` with its coordinate across `edge` replaced by `c`.
fn with_across(p: PointF, edge: Edge, c: f64) -> PointF {
    if edge.is_vertical() {
        PointF::new(c, p.y)
    } else {
        PointF::new(p.x, c)
    }
}

/// The point `by` further out of `edge` than `p`.
fn beyond(p: PointF, edge: Edge, by: f64) -> PointF {
    match edge {
        Edge::Left => PointF::new(p.x - by, p.y),
        Edge::Right => PointF::new(p.x + by, p.y),
        Edge::Top => PointF::new(p.x, p.y - by),
        Edge::Bottom => PointF::new(p.x, p.y + by),
    }
}

/// How far `delta` points out of `edge`; non-finite movement counts as none.
fn outward(edge: Edge, delta: PointF) -> f64 {
    let v = match edge {
        Edge::Left => -delta.x,
        Edge::Right => delta.x,
        Edge::Top => -delta.y,
        Edge::Bottom => delta.y,
    };
    if v.is_finite() { v } else { 0.0 }
}

/// The index of the rectangle nearest to `p` when within
/// [`TOUCH_TOLERANCE`] of it (ties to the lowest index).
fn nearest(rects: &[RectF], p: PointF) -> Option<usize> {
    let gap = |r: &RectF| {
        let dx = (r.x - p.x).max(p.x - r.right()).max(0.0);
        let dy = (r.y - p.y).max(p.y - r.bottom()).max(0.0);
        dx.hypot(dy)
    };
    rects
        .iter()
        .enumerate()
        .map(|(i, r)| (i, gap(r)))
        .filter(|&(_, g)| g <= TOUCH_TOLERANCE)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::arrange::{Placement, bounds, contacts, logical_screens, resolve, snap};
    use crate::geometry::tests::screen;
    use crate::types::{ScreenInfo, Side};

    fn mac() -> ScreenSet {
        ScreenSet::new(vec![screen(1, 0, 0, 1440, 900, 1.0)]).unwrap()
    }

    fn win(npl: f64) -> ScreenSet {
        ScreenSet::new(vec![screen(7, 0, 0, 1920, 1080, npl)]).unwrap()
    }

    /// The legacy "the peer is on my left/right" arrangement.
    fn beside(local: ScreenSet, peer: ScreenSet, side: Side) -> Layout {
        let map = resolve(&local, &peer, &Placement::side(side));
        Layout::new(local, peer, Some(map))
    }

    fn pt(x: f64, y: f64) -> PointF {
        PointF::new(x, y)
    }

    #[track_caller]
    fn assert_near(p: PointF, x: f64, y: f64) {
        assert!(
            (p.x - x).abs() < 1e-6 && (p.y - y).abs() < 1e-6,
            "{p:?} is not ({x}, {y})"
        );
    }

    #[track_caller]
    fn assert_within(p: Point, x: i32, y: i32, slack: i32) {
        assert!(
            (p.x - x).abs() <= slack && (p.y - y).abs() <= slack,
            "{p:?} is not within {slack} of ({x}, {y})"
        );
    }

    #[test]
    fn enter_right_and_left() {
        // The Windows screen is centred on the Mac's: y 450 <-> 540.
        let a = beside(mac(), win(1.0), Side::Right);
        let c = a.try_enter(pt(1439.5, 450.0), pt(5.0, 0.0)).unwrap();
        assert_near(c.entry, 0.0, 540.5);
        assert_eq!(c.exit, Point::new(1439, 450));
        assert_eq!((c.npl_src, c.edge), (1.0, Edge::Right));
        // Unclamped OS positions past the edge are clamped first.
        assert_eq!(
            a.try_enter(pt(1500.0, 450.0), PointF::default())
                .unwrap()
                .exit,
            Point::new(1439, 450)
        );
        assert!(a.try_enter(pt(1438.0, 450.0), pt(5.0, 0.0)).is_none());

        let b = beside(win(1.0), mac(), Side::Left);
        let c = b.try_enter(pt(0.0, 540.0), pt(-5.0, 0.0)).unwrap();
        assert_near(c.entry, 1439.0, 450.5);
        assert_eq!(c.exit, Point::new(0, 540));
        assert_eq!(c.edge, Edge::Left);
        assert!(b.try_enter(pt(1919.0, 540.0), pt(5.0, 0.0)).is_none());
        // The Mac is shorter, so only part of the Windows edge touches it.
        assert!(b.try_enter(pt(0.0, 100.0), pt(-5.0, 0.0)).is_some());
        assert!(b.try_enter(pt(0.0, 20.0), pt(-5.0, 0.0)).is_none());
        assert!(b.try_enter(pt(0.0, 1000.0), pt(-5.0, 0.0)).is_none());
    }

    #[test]
    fn step_moves_normalises_and_exits() {
        // Windows at 1.5 controlling a Mac at 1.0: 3 px -> 2 points.
        let l = beside(win(1.5), mac(), Side::Left);
        let v = pt(1439.0, 450.0);
        assert_eq!(
            l.step(v, pt(-3.0, 0.0), 1.5, false),
            Step::Moved(pt(1437.0, 450.0))
        );
        // Crossing back over the facing (right) edge of the Mac.
        match l.step(v, pt(3.0, 0.0), 1.5, false) {
            Step::Exited { warp_to } => assert_eq!(warp_to, Point::new(2, 540)),
            other => panic!("expected exit, got {other:?}"),
        }
        // Held button: clamped instead of leaving.
        assert_eq!(
            l.step(v, pt(3.0, 0.0), 1.5, true),
            Step::Moved(pt(1439.0, 450.0))
        );
        // Other edges clamp.
        assert_eq!(
            l.step(pt(10.0, 5.0), pt(-100.0, -100.0), 1.5, false),
            Step::Moved(pt(0.0, 0.0))
        );
        // Non-finite movement is none.
        assert_eq!(
            l.step(v, pt(f64::NAN, f64::INFINITY), 1.5, false),
            Step::Moved(v)
        );
    }

    #[test]
    fn inset_moves_away_from_the_edge_that_was_left() {
        let l = beside(mac(), win(1.0), Side::Right);
        assert_eq!(
            l.inset(Point::new(1439, 450), Edge::Right),
            Point::new(1437, 450)
        );
        assert_eq!(l.inset(Point::new(0, 450), Edge::Left), Point::new(2, 450));
        assert_eq!(l.inset(Point::new(700, 0), Edge::Top), Point::new(700, 2));
        assert_eq!(
            l.inset(Point::new(700, 899), Edge::Bottom),
            Point::new(700, 897)
        );
        // Clamped onto the screen it belongs to.
        assert_eq!(l.inset(Point::new(1, 450), Edge::Right), Point::new(0, 450));
        assert_eq!(
            l.inset(Point::new(5000, 450), Edge::Right),
            Point::new(1437, 450)
        );
    }

    /// A MacBook (primary) under a wide monitor that sticks out to its left,
    /// a second monitor right of the wide one.
    fn mac_desk() -> ScreenSet {
        ScreenSet::new(vec![
            screen(1, 0, 0, 1512, 982, 1.0),
            screen(2, -524, -1440, 2560, 1440, 1.0),
            screen(3, 2036, -1440, 1920, 1080, 1.0),
        ])
        .unwrap()
    }

    /// A Windows laptop: 2304x1296 pixels at 150 %.
    fn win_laptop() -> ScreenSet {
        ScreenSet::new(vec![screen(1, 0, 0, 2304, 1296, 1.5)]).unwrap()
    }

    /// The laptop to the left of the MacBook, top edges level.
    fn desk_placement() -> Placement {
        Placement {
            display: Some(1),
            edge: Edge::Left,
            offset: Some(0),
        }
    }

    fn desk() -> Layout {
        let map = resolve(&mac_desk(), &win_laptop(), &desk_placement());
        Layout::new(mac_desk(), win_laptop(), Some(map))
    }

    #[test]
    fn the_desk_crosses_where_the_laptop_touches() {
        let l = desk();
        // The MacBook's left edge.
        let c = l.try_enter(pt(0.0, 500.0), pt(-5.0, 0.0)).unwrap();
        assert_near(c.entry, 2303.0, 750.75);
        assert_eq!((c.exit, c.edge), (Point::new(0, 500), Edge::Left));
        // The wide monitor's bottom edge, above the laptop.
        let c = l.try_enter(pt(-300.0, -1.0), pt(0.0, 5.0)).unwrap();
        assert_near(c.entry, 1854.75, 0.0);
        assert_eq!((c.exit, c.edge), (Point::new(-300, -1), Edge::Bottom));
        assert_eq!(c.npl_src, 1.0);
        // Edges that touch nothing of the laptop's.
        for (x, y) in [
            (-524.0, -700.0), // the wide monitor's free left edge
            (500.0, 981.0),   // the MacBook's bottom
            (0.0, 950.0),     // the MacBook's left edge below the laptop
            (500.0, 0.0),     // the MacBook's top: the wide monitor is there
            (2035.0, -700.0), // the wide monitor's right: the other monitor
        ] {
            assert!(
                l.try_enter(pt(x, y), pt(-5.0, 5.0)).is_none(),
                "({x}, {y}) must not cross"
            );
        }
        // The contact ends where the laptop does.
        assert!(l.try_enter(pt(0.0, 863.0), pt(-5.0, 0.0)).is_some());
        assert!(l.try_enter(pt(0.0, 866.0), pt(-5.0, 0.0)).is_none());
    }

    #[test]
    fn the_desk_returns_to_the_screen_that_touches() {
        let l = desk();
        let right_edge = pt(2303.0, 750.0);
        match l.step(right_edge, pt(5.0, 0.0), 1.0, false) {
            Step::Exited { warp_to } => assert_within(warp_to, 2, 500, 1),
            other => panic!("expected exit, got {other:?}"),
        }
        // Up through the laptop's top edge, onto the wide monitor.
        let top_edge = pt(1854.0, 0.0);
        match l.step(top_edge, pt(0.0, -5.0), 1.0, false) {
            Step::Exited { warp_to } => assert_within(warp_to, -300, -3, 1),
            other => panic!("expected exit, got {other:?}"),
        }
        // Not while a button is held.
        assert_eq!(
            l.step(top_edge, pt(0.0, -5.0), 1.0, true),
            Step::Moved(top_edge)
        );
        // The laptop's other edges stay in.
        assert_eq!(
            l.step(pt(5.0, 400.0), pt(-20.0, 0.0), 1.0, false),
            Step::Moved(pt(0.0, 400.0))
        );
        assert_eq!(
            l.step(pt(500.0, 1295.0), pt(0.0, 20.0), 1.0, false),
            Step::Moved(pt(500.0, 1295.0))
        );
    }

    #[test]
    fn the_secondary_mirrors_the_primarys_placement() {
        let map = resolve(&mac_desk(), &win_laptop(), &desk_placement()).inverse();
        let l = Layout::new(win_laptop(), mac_desk(), Some(map));
        // Out of the laptop's right edge into the MacBook's left edge.
        let c = l.try_enter(pt(2303.0, 750.0), pt(5.0, 0.0)).unwrap();
        assert_near(c.entry, 0.0, 500.0 + 1.0 / 3.0);
        assert_eq!(
            (c.exit, c.edge, c.npl_src),
            (Point::new(2303, 750), Edge::Right, 1.5)
        );
        // Out of the laptop's top edge into the wide monitor's bottom row.
        let c = l.try_enter(pt(1854.0, 0.0), pt(0.0, -5.0)).unwrap();
        assert_near(c.entry, -300.0 + 1.0 / 3.0, -1.0);
        assert_eq!(c.edge, Edge::Top);
        // The laptop's bottom edge touches nothing.
        assert!(l.try_enter(pt(1000.0, 1295.0), pt(0.0, 5.0)).is_none());

        // Back out of the MacBook's left edge (laptop pixels: 6 -> 4 points).
        match l.step(pt(0.5, 500.0), pt(-6.0, 0.0), 1.5, false) {
            Step::Exited { warp_to } => assert_within(warp_to, 2301, 750, 1),
            other => panic!("expected exit, got {other:?}"),
        }
        // And out of the wide monitor's bottom edge into the laptop's top.
        match l.step(pt(-300.0, -1.0), pt(0.0, 6.0), 1.5, false) {
            Step::Exited { warp_to } => assert_within(warp_to, 1854, 2, 1),
            other => panic!("expected exit, got {other:?}"),
        }
        // The MacBook's left edge below the laptop leads nowhere.
        assert_eq!(
            l.step(pt(5.0, 950.0), pt(-30.0, 0.0), 1.5, false),
            Step::Moved(pt(0.0, 950.0))
        );
    }

    #[test]
    fn a_corner_goes_where_the_cursor_pushes() {
        // One screen with a peer screen to its right and another below it;
        // an identity map keeps the peer where it sits next to mine.
        let local = ScreenSet::new(vec![screen(1, 0, 0, 1000, 800, 1.0)]).unwrap();
        let peer = ScreenSet::new(vec![
            screen(1, 1000, 0, 800, 800, 1.0),
            screen(2, 0, 800, 1000, 600, 1.0),
        ])
        .unwrap();
        let identity = PeerMap {
            scale: 1.0,
            dx: 0.0,
            dy: 0.0,
        };
        let l = Layout::new(local, peer, Some(identity));
        let corner = pt(999.5, 799.5);
        let right = l.try_enter(corner, pt(10.0, 1.0)).unwrap();
        assert_eq!(right.edge, Edge::Right);
        assert_near(right.entry, 1000.0, 799.5);
        let down = l.try_enter(corner, pt(1.0, 10.0)).unwrap();
        assert_eq!(down.edge, Edge::Bottom);
        assert_near(down.entry, 999.5, 800.0);
        // Without a push the first edge in Left, Right, Top, Bottom order.
        assert_eq!(
            l.try_enter(corner, PointF::default()).unwrap().edge,
            Edge::Right
        );
        let nan = pt(f64::NAN, f64::NEG_INFINITY);
        assert_eq!(l.try_enter(corner, nan).unwrap().edge, Edge::Right);
        // Away from the corner only one edge is touching.
        assert_eq!(
            l.try_enter(pt(500.0, 799.5), pt(10.0, 1.0)).unwrap().edge,
            Edge::Bottom
        );
    }

    #[test]
    fn dominant_overshoot_picks_the_edge_to_leave_by() {
        // The peer's only screen has one of mine to its right and one below.
        let local = ScreenSet::new(vec![
            screen(1, 100, 0, 100, 100, 1.0),
            screen(2, 0, 100, 100, 100, 1.0),
        ])
        .unwrap();
        let peer = ScreenSet::new(vec![screen(1, 0, 0, 100, 100, 1.0)]).unwrap();
        let identity = PeerMap {
            scale: 1.0,
            dx: 0.0,
            dy: 0.0,
        };
        let l = Layout::new(local, peer, Some(identity));
        let v = pt(95.0, 95.0);
        match l.step(v, pt(20.0, 10.0), 1.0, false) {
            Step::Exited { warp_to } => assert_eq!(warp_to, Point::new(102, 99)),
            other => panic!("expected exit, got {other:?}"),
        }
        match l.step(v, pt(10.0, 20.0), 1.0, false) {
            Step::Exited { warp_to } => assert_eq!(warp_to, Point::new(99, 102)),
            other => panic!("expected exit, got {other:?}"),
        }
    }

    #[test]
    fn no_exit_where_another_peer_screen_continues() {
        // P1 and a shorter P2 to its right; one of my screens sits where P2
        // is. Running right along P1's upper half goes on to P2, not home.
        let local = ScreenSet::new(vec![screen(1, 100, 0, 100, 100, 1.0)]).unwrap();
        let peer = ScreenSet::new(vec![
            screen(1, 0, 0, 100, 100, 1.0),
            screen(2, 100, 0, 100, 60, 1.0),
        ])
        .unwrap();
        let identity = PeerMap {
            scale: 1.0,
            dx: 0.0,
            dy: 0.0,
        };
        let l = Layout::new(local, peer, Some(identity));
        assert_eq!(
            l.step(pt(99.0, 30.0), pt(151.0, 0.0), 1.0, false),
            Step::Moved(pt(199.0, 30.0))
        );
        // Below P2 the edge of P1 is free, and one of mine is there.
        assert!(matches!(
            l.step(pt(99.0, 80.0), pt(30.0, 0.0), 1.0, false),
            Step::Exited { .. }
        ));
    }

    #[test]
    fn two_contacts_of_one_peer() {
        // The peer sits on top of the external monitor and also touches the
        // laptop's right edge above it.
        let local = ScreenSet::new(vec![
            screen(1, 0, 0, 1440, 900, 1.0),
            screen(2, 1440, 300, 1920, 1080, 1.0),
        ])
        .unwrap();
        let p = Placement {
            display: Some(2),
            edge: Edge::Top,
            offset: Some(0),
        };
        let map = resolve(&local, &win(1.0), &p);
        let l = Layout::new(local, win(1.0), Some(map));
        // The laptop's right edge above the external monitor.
        let c = l.try_enter(pt(1439.0, 100.0), pt(5.0, 0.0)).unwrap();
        assert_eq!(c.edge, Edge::Right);
        assert_near(c.entry, 0.0, 1080.0 - 300.0 + 100.5);
        // Below it the external monitor is in the way.
        assert!(l.try_enter(pt(1439.0, 500.0), pt(5.0, 0.0)).is_none());
        // The external monitor's top edge.
        let c = l.try_enter(pt(2000.0, 300.0), pt(0.0, -5.0)).unwrap();
        assert_eq!(c.edge, Edge::Top);
        assert_near(c.entry, 2000.5 - 1440.0, 1079.0);
        // Its free edges touch nothing.
        assert!(l.try_enter(pt(3359.0, 800.0), pt(5.0, 0.0)).is_none());
    }

    #[test]
    fn top_and_bottom_placements() {
        // Windows (1.5) above a Mac, then below it.
        let above = Placement {
            display: None,
            edge: Edge::Top,
            offset: Some(100),
        };
        let l = Layout::new(mac(), win(1.5), Some(resolve(&mac(), &win(1.5), &above)));
        // 1920x1080 pixels at 1.5 is 1280x720 points, 100 from the left.
        let c = l.try_enter(pt(500.0, 0.0), pt(0.0, -5.0)).unwrap();
        assert_eq!(c.edge, Edge::Top);
        assert_near(c.entry, (500.5 - 100.0) * 1.5, 1079.0);
        assert!(l.try_enter(pt(50.0, 0.0), pt(0.0, -5.0)).is_none());
        assert!(l.try_enter(pt(500.0, 899.0), pt(0.0, 5.0)).is_none());
        match l.step(pt(600.0, 0.0), pt(0.0, 5.0), 1.0, false) {
            Step::Moved(v) => assert_near(v, 600.0, 7.5),
            other => panic!("expected a move, got {other:?}"),
        }
        match l.step(pt(600.0, 1079.0), pt(0.0, 5.0), 1.0, false) {
            Step::Exited { warp_to } => assert_within(warp_to, 100 + 400, 2, 1),
            other => panic!("expected exit, got {other:?}"),
        }

        let below = Placement {
            display: None,
            edge: Edge::Bottom,
            offset: None,
        };
        let l = Layout::new(mac(), win(1.5), Some(resolve(&mac(), &win(1.5), &below)));
        // Centred: (1440 - 1280) / 2 = 80 points from the left.
        let c = l.try_enter(pt(500.0, 899.0), pt(0.0, 5.0)).unwrap();
        assert_eq!(c.edge, Edge::Bottom);
        assert_near(c.entry, (500.5 - 80.0) * 1.5, 0.0);
        assert!(l.try_enter(pt(500.0, 0.0), pt(0.0, -5.0)).is_none());
        match l.step(pt(600.0, 0.0), pt(0.0, -5.0), 1.0, false) {
            Step::Exited { warp_to } => assert_within(warp_to, 80 + 400, 897, 1),
            other => panic!("expected exit, got {other:?}"),
        }
    }

    #[test]
    fn a_layout_without_a_map_never_crosses() {
        let l = Layout::new(mac(), win(1.0), None);
        assert_eq!(l.map(), None);
        assert!(l.try_enter(pt(1439.0, 450.0), pt(5.0, 0.0)).is_none());
        assert_eq!(
            l.step(pt(1919.0, 540.0), pt(50.0, 0.0), 1.0, false),
            Step::Moved(pt(1919.0, 540.0))
        );
        // It still clamps what the peer injects.
        assert_eq!(l.local().clamp(Point::new(5000, -5)), Point::new(1439, 0));
        assert_eq!(l.peer().screens().len(), 1);
    }

    // ---- property tests (design §5.3) ----

    /// 1-4 screens tiled on a coarse grid (cells 400x300, sizes 100..=400 x
    /// 100..=300), unique ids, random scales.
    fn arb_screens() -> impl Strategy<Value = ScreenSet> {
        prop::collection::btree_set((0i32..4, 0i32..3), 1..=4)
            .prop_flat_map(|cells| {
                let n = cells.len();
                (
                    Just(cells),
                    prop::collection::vec(
                        (
                            100i32..=400,
                            100i32..=300,
                            prop::sample::select(vec![1.0, 1.25, 1.5, 2.0]),
                        ),
                        n,
                    ),
                )
            })
            .prop_map(|(cells, dims)| {
                let screens: Vec<ScreenInfo> = cells
                    .into_iter()
                    .zip(dims)
                    .enumerate()
                    .map(|(i, ((cx, cy), (w, h, npl)))| {
                        screen(i as u32 + 1, cx * 400, cy * 300, w, h, npl)
                    })
                    .collect();
                ScreenSet::new(screens).unwrap()
            })
    }

    fn arb_placement() -> impl Strategy<Value = Placement> {
        (
            prop::option::of(1u32..=4),
            prop::sample::select(Edge::ALL.to_vec()),
            prop::option::of(-2000i32..2000),
        )
            .prop_map(|(display, edge, offset)| Placement {
                display,
                edge,
                offset,
            })
    }

    fn arb_point() -> impl Strategy<Value = PointF> {
        (-2000.0f64..3000.0, -2000.0f64..3000.0).prop_map(|(x, y)| PointF::new(x, y))
    }

    fn layout_of(local: &ScreenSet, peer: &ScreenSet, p: &Placement) -> Layout {
        Layout::new(local.clone(), peer.clone(), Some(resolve(local, peer, p)))
    }

    /// A point on the last pixel row/column of `r` towards `edge`, `t` of the
    /// way along it.
    fn on_edge(r: crate::types::Rect, edge: Edge, t: f64) -> PointF {
        let x = f64::from(r.left()) + t * f64::from(r.width() - 1);
        let y = f64::from(r.top()) + t * f64::from(r.height() - 1);
        match edge {
            Edge::Left => PointF::new(f64::from(r.left()), y),
            Edge::Right => PointF::new(f64::from(r.right() - 1) + 0.5, y),
            Edge::Top => PointF::new(x, f64::from(r.top())),
            Edge::Bottom => PointF::new(x, f64::from(r.bottom() - 1) + 0.5),
        }
    }

    fn unit(edge: Edge) -> PointF {
        beyond(PointF::default(), edge, 1.0)
    }

    proptest! {
        #[test]
        fn prop_entry_is_on_a_peer_screen_facing_me(
            local in arb_screens(),
            peer in arb_screens(),
            p in arb_placement(),
            t in 0.0f64..1.0,
            push in -20.0f64..20.0,
        ) {
            let l = layout_of(&local, &peer, &p);
            for s in local.screens() {
                for edge in Edge::ALL {
                    let pos = on_edge(s.rect, edge, t);
                    let Some(c) = l.try_enter(pos, PointF::new(push, -push)) else {
                        continue;
                    };
                    prop_assert!(peer.contains_f(c.entry), "entry {:?} off screen", c.entry);
                    let target = peer.screen_for(c.entry).rect;
                    prop_assert_eq!(
                        across(c.entry, c.edge),
                        f64::from(edge_pixel(target, c.edge.opposite())),
                        "entry {:?} is not on the facing line of {:?}", c.entry, target
                    );
                    prop_assert!(local.contains(c.exit));
                    prop_assert_eq!(c.npl_src, local.screen_for(c.exit.to_f()).native_per_logical);
                }
            }
        }

        #[test]
        fn prop_exit_lands_on_my_screens(
            local in arb_screens(),
            peer in arb_screens(),
            p in arb_placement(),
            v in arb_point(),
            dx in -400.0f64..400.0,
            dy in -400.0f64..400.0,
            npl in prop::sample::select(vec![1.0, 1.25, 1.5, 2.0]),
            held in any::<bool>(),
        ) {
            let l = layout_of(&local, &peer, &p);
            let v = peer.clamp_f(v);
            match l.step(v, PointF::new(dx, dy), npl, held) {
                Step::Moved(n) => prop_assert!(peer.contains_f(n), "moved off screen to {:?}", n),
                Step::Exited { warp_to } => {
                    prop_assert!(!held, "left while a button was held");
                    prop_assert!(local.contains(warp_to), "warp target {:?} off screen", warp_to);
                }
            }
        }

        /// Wherever the UI draws a contact the cursor crosses, and it can
        /// come straight back.
        #[test]
        fn prop_contacts_cross_and_return(
            local in arb_screens(),
            peer in arb_screens(),
            wx in -3000.0f64..3000.0,
            wy in -3000.0f64..3000.0,
        ) {
            let pb = bounds(logical_screens(&peer).iter().map(|s| s.rect));
            let (p, _) = snap(&logical_screens(&local), (pb.w, pb.h), pt(wx, wy)).unwrap();
            let l = layout_of(&local, &peer, &p);
            let mine: Vec<RectF> = local.screens().iter().map(|s| RectF::from_rect(s.rect)).collect();
            prop_assume!(l.peer_in_local.iter().all(|q| mine.iter().all(|m| !m.overlaps(*q))));
            for k in contacts(&mine, &l.peer_in_local) {
                let (len, mid) = if k.edge.is_vertical() {
                    (k.to.y - k.from.y, pt(k.from.x, (k.from.y + k.to.y) / 2.0))
                } else {
                    (k.to.x - k.from.x, pt((k.from.x + k.to.x) / 2.0, k.from.y))
                };
                if len < 3.0 {
                    continue;
                }
                // The last pixel of my screen at this edge.
                let pos = beyond(mid, k.edge, -0.25);
                let c = l.try_enter(pos, unit(k.edge)).expect("a contact must cross");
                prop_assert_eq!(c.edge, k.edge);
                // The virtual cursor walks straight back out and lands next
                // to where it left, on the same screen.
                let back = unit(k.edge.opposite());
                match l.step(c.entry, PointF::new(back.x * 8.0, back.y * 8.0), c.npl_src, false) {
                    Step::Exited { warp_to } => {
                        prop_assert_eq!(
                            local.screen_for(warp_to.to_f()).id,
                            local.screen_for(c.exit.to_f()).id
                        );
                        let scale = l.map().unwrap().scale;
                        let slack = 2.0 + scale.max(1.0 / scale);
                        let (a, b) = if k.edge.is_vertical() { (warp_to.y, c.exit.y) } else { (warp_to.x, c.exit.x) };
                        prop_assert!(f64::from(a - b).abs() <= slack, "{:?} vs {:?}", warp_to, c.exit);
                        let r = local.screen_for(c.exit.to_f()).rect;
                        let line = edge_pixel(r, k.edge);
                        let (across_warp, inward) = if k.edge.is_vertical() { (warp_to.x, warp_to.x - line) } else { (warp_to.y, warp_to.y - line) };
                        let _ = across_warp;
                        prop_assert!(inward.abs() <= EDGE_REENTRY_INSET, "warped {} from the edge", inward);
                    }
                    other => prop_assert!(false, "expected exit, got {:?}", other),
                }
            }
        }

        #[test]
        fn prop_clamp_idempotent_and_on_screen(s in arb_screens(), p in arb_point()) {
            let c = s.clamp_f(p);
            prop_assert!(s.contains_f(c));
            prop_assert_eq!(s.clamp_f(c), c);
            let ci = s.clamp(p.floor());
            prop_assert!(s.contains(ci));
            prop_assert_eq!(s.clamp(ci), ci);
        }

        #[test]
        fn prop_inset_stays_on_screen(
            local in arb_screens(),
            p in arb_point(),
            edge in prop::sample::select(Edge::ALL.to_vec()),
        ) {
            let l = Layout::new(local.clone(), mac(), None);
            prop_assert!(local.contains(l.inset(p.floor(), edge)));
        }

        #[test]
        fn prop_normalisation_identity_for_equal_scales(npl in prop::sample::select(vec![1.0, 1.25, 1.5, 2.0]), dx in -50.0f64..50.0, dy in -50.0f64..50.0) {
            let local = ScreenSet::new(vec![screen(1, 0, 0, 1000, 1000, npl)]).unwrap();
            let peer = ScreenSet::new(vec![screen(2, 0, 0, 1000, 1000, npl)]).unwrap();
            let l = beside(local, peer, Side::Right);
            let v = PointF::new(500.0, 500.0);
            prop_assert_eq!(l.step(v, PointF::new(dx, dy), npl, false), Step::Moved(PointF::new(500.0 + dx, 500.0 + dy)));
        }
    }
}
