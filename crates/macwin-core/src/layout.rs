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
            rects(&peer).into_iter().map(|r| m.rect_to_local(r)).collect()
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
        let c = target.clamp_f(map.to_peer(probe));
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
        let on_screen = from.clamp_f(next);
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
