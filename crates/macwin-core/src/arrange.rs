//! Screen arrangement: where the peer's screens sit next to mine.
//!
//! The primary computer attaches the peer's screens to one of its own
//! screens: a [`Placement`] names that screen, the edge (any of the four) and
//! the offset along the edge, like the macOS Displays arrangement. Placements
//! are kept in *logical* units (macOS points, Windows DIPs), so they survive
//! a change of the peer's resolution, and are resolved per session into a
//! [`PeerMap`] between the two machines' native spaces. The secondary
//! resolves the primary's placement the same way and inverts the map, so
//! both cursors cross at the same shared edge.
//!
//! A machine is converted to logical units with one factor (the scale of its
//! primary screen), which keeps adjacent monitors adjacent.

use crate::geometry::ScreenSet;
use crate::types::{PointF, Rect, Side};

/// The least length (logical units) of edge a placement shares with its
/// screen, so the peer can always be reached.
pub const MIN_CONTACT: f64 = 40.0;
/// Snapping lines the peer up with a screen edge closer than this (logical
/// units).
pub const SNAP_ALIGN: f64 = 24.0;
/// Edges closer than this (logical units) count as touching.
const TOUCH_EPS: f64 = 0.5;

/// One of the four sides of a screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub const ALL: [Edge; 4] = [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom];

    pub const fn opposite(self) -> Edge {
        match self {
            Edge::Left => Edge::Right,
            Edge::Right => Edge::Left,
            Edge::Top => Edge::Bottom,
            Edge::Bottom => Edge::Top,
        }
    }

    /// True for Left and Right, whose edges run along y.
    pub const fn is_vertical(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }

    pub const fn from_side(side: Side) -> Edge {
        match side {
            Side::Left => Edge::Left,
            Side::Right => Edge::Right,
        }
    }
}

/// A rectangle `[x, x + w) x [y, y + h)` with f64 coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RectF {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl RectF {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> RectF {
        RectF { x, y, w, h }
    }

    pub fn from_rect(r: Rect) -> RectF {
        RectF::new(
            f64::from(r.left()),
            f64::from(r.top()),
            f64::from(r.width()),
            f64::from(r.height()),
        )
    }

    pub fn right(self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(self) -> f64 {
        self.y + self.h
    }

    pub fn center(self) -> PointF {
        PointF::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn contains(self, p: PointF) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }

    /// True when the interiors intersect; touching edges do not count.
    pub fn overlaps(self, o: RectF) -> bool {
        const EPS: f64 = 1e-6;
        self.right().min(o.right()) - self.x.max(o.x) > EPS
            && self.bottom().min(o.bottom()) - self.y.max(o.y) > EPS
    }

    pub fn translate(self, dx: f64, dy: f64) -> RectF {
        RectF::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// Every coordinate multiplied by `k`.
    pub fn scale(self, k: f64) -> RectF {
        RectF::new(self.x * k, self.y * k, self.w * k, self.h * k)
    }

    pub fn expand(self, by: f64) -> RectF {
        RectF::new(
            self.x - by,
            self.y - by,
            self.w + 2.0 * by,
            self.h + 2.0 * by,
        )
    }

    /// Where the line of `edge` is (x for Left/Right, y for Top/Bottom).
    pub fn edge_pos(self, edge: Edge) -> f64 {
        match edge {
            Edge::Left => self.x,
            Edge::Right => self.right(),
            Edge::Top => self.y,
            Edge::Bottom => self.bottom(),
        }
    }

    /// The extent along `edge`: `(top, bottom)` for Left/Right, `(left,
    /// right)` for Top/Bottom.
    pub fn span(self, edge: Edge) -> (f64, f64) {
        if edge.is_vertical() {
            (self.y, self.bottom())
        } else {
            (self.x, self.right())
        }
    }
}

/// The bounding box of `rects` (empty at the origin when there are none).
pub fn bounds(rects: impl IntoIterator<Item = RectF>) -> RectF {
    let mut it = rects.into_iter();
    let Some(first) = it.next() else {
        return RectF::default();
    };
    let (mut l, mut t, mut r, mut b) = (first.x, first.y, first.right(), first.bottom());
    for s in it {
        l = l.min(s.x);
        t = t.min(s.y);
        r = r.max(s.right());
        b = b.max(s.bottom());
    }
    RectF::new(l, t, r - l, b - t)
}

/// A screen in logical units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogicalScreen {
    pub id: u32,
    pub rect: RectF,
    pub primary: bool,
}

/// Native units per logical unit for a whole machine: the scale of its
/// primary screen (of the first screen when none is marked primary).
pub fn machine_scale(set: &ScreenSet) -> f64 {
    let screens = set.screens();
    screens
        .iter()
        .find(|s| s.primary)
        .unwrap_or(&screens[0])
        .native_per_logical
}

/// The screens of `set` in logical units.
pub fn logical_screens(set: &ScreenSet) -> Vec<LogicalScreen> {
    let k = 1.0 / machine_scale(set);
    set.screens()
        .iter()
        .map(|s| LogicalScreen {
            id: s.id,
            rect: RectF::from_rect(s.rect).scale(k),
            primary: s.primary,
        })
        .collect()
}

/// Where the peer's screens are attached on the primary computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Placement {
    /// The id of the primary's screen the peer is attached to; `None` (or a
    /// screen that is gone) picks the outermost screen towards `edge`.
    pub display: Option<u32>,
    /// The side of that screen the peer is on.
    pub edge: Edge,
    /// Logical units from that screen's top (Left/Right) or left (Top/Bottom)
    /// to the top or left of the peer's screens; `None` centres them.
    pub offset: Option<i32>,
}

impl Placement {
    /// The legacy "the peer is on my left/right": centred on my outermost
    /// screen on that side.
    pub const fn side(side: Side) -> Placement {
        Placement {
            display: None,
            edge: Edge::from_side(side),
            offset: None,
        }
    }
}

/// The peer's box attached to `anchor` on `edge` at `offset`.
fn attach(anchor: RectF, edge: Edge, offset: f64, (w, h): (f64, f64)) -> RectF {
    match edge {
        Edge::Left => RectF::new(anchor.x - w, anchor.y + offset, w, h),
        Edge::Right => RectF::new(anchor.right(), anchor.y + offset, w, h),
        Edge::Top => RectF::new(anchor.x + offset, anchor.y - h, w, h),
        Edge::Bottom => RectF::new(anchor.x + offset, anchor.bottom(), w, h),
    }
}

/// The offsets that keep at least [`MIN_CONTACT`] (or the shorter of the two
/// extents) of `edge` shared.
fn offset_range(anchor: RectF, edge: Edge, (w, h): (f64, f64)) -> (f64, f64) {
    let (a_len, p_len) = if edge.is_vertical() {
        (anchor.h, h)
    } else {
        (anchor.w, w)
    };
    let c = MIN_CONTACT.min(a_len).min(p_len);
    (c - p_len, a_len - c)
}

/// The length of edge `anchor` shares with `peer` attached on `edge`.
fn shared_len(anchor: RectF, edge: Edge, peer: RectF) -> f64 {
    let (a0, a1) = anchor.span(edge);
    let (p0, p1) = peer.span(edge);
    (a1.min(p1) - a0.max(p0)).max(0.0)
}

/// The index of the screen farthest towards `edge`, ties to the primary
/// screen, then to the lowest index.
fn outermost(local: &[LogicalScreen], edge: Edge) -> usize {
    let key = |s: &LogicalScreen| match edge {
        Edge::Left => -s.rect.x,
        Edge::Right => s.rect.right(),
        Edge::Top => -s.rect.y,
        Edge::Bottom => s.rect.bottom(),
    };
    let mut best = 0;
    for (i, s) in local.iter().enumerate().skip(1) {
        let (k, kb) = (key(s), key(&local[best]));
        if k > kb || (k == kb && s.primary && !local[best].primary) {
            best = i;
        }
    }
    best
}

/// Where the peer's bounding box of size `peer` (logical) goes for `p`: its
/// top-left corner in my logical space and the index of the screen it is
/// attached to. `None` only when `local` is empty.
pub fn place(local: &[LogicalScreen], peer: (f64, f64), p: &Placement) -> Option<(PointF, usize)> {
    if local.is_empty() {
        return None;
    }
    let i = p
        .display
        .and_then(|id| local.iter().position(|s| s.id == id))
        .unwrap_or_else(|| outermost(local, p.edge));
    let a = local[i].rect;
    let (lo, hi) = offset_range(a, p.edge, peer);
    let centred = if p.edge.is_vertical() {
        (a.h - peer.1) / 2.0
    } else {
        (a.w - peer.0) / 2.0
    };
    let offset = p.offset.map_or(centred, f64::from).clamp(lo, hi);
    let r = attach(a, p.edge, offset, peer);
    Some((PointF::new(r.x, r.y), i))
}

/// The placement closest to dropping the peer's box (size `peer`) with its
/// top-left corner at `want`: attached to one of my screens, overlapping
/// none, lined up with a nearby screen edge. Returns it with the box's
/// top-left corner. `None` only when `local` is empty.
pub fn snap(local: &[LogicalScreen], peer: (f64, f64), want: PointF) -> Option<(Placement, PointF)> {
    // (valid, distance², shared length, placement, origin)
    let mut best: Option<(bool, f64, f64, Placement, PointF)> = None;
    for (i, anchor) in local.iter().enumerate() {
        for edge in Edge::ALL {
            let a = anchor.rect;
            let (lo, hi) = offset_range(a, edge, peer);
            let (start, p_len) = if edge.is_vertical() {
                (a.y, peer.1)
            } else {
                (a.x, peer.0)
            };
            let want_off = if edge.is_vertical() { want.y } else { want.x } - start;
            // Line up with the edges of the screens, and slide clear of them.
            let mut lines = Vec::with_capacity(4 * local.len());
            for o in local {
                let (o0, o1) = o.rect.span(edge);
                lines.extend([o0 - start, o1 - p_len - start]);
            }
            let mut slides = Vec::with_capacity(2 * local.len());
            for o in local {
                let (o0, o1) = o.rect.span(edge);
                slides.extend([o1 - start, o0 - p_len - start]);
            }
            let aligned = lines
                .iter()
                .copied()
                .filter(|l| (l - want_off).abs() < SNAP_ALIGN)
                .min_by(|a, b| (a - want_off).abs().total_cmp(&(b - want_off).abs()))
                .unwrap_or(want_off);
            for off in std::iter::once(aligned).chain(slides) {
                let offset = off.clamp(lo, hi).round() as i32;
                let p = Placement {
                    display: Some(anchor.id),
                    edge,
                    offset: Some(offset),
                };
                let Some((origin, _)) = place(local, peer, &p) else {
                    continue;
                };
                let r = RectF::new(origin.x, origin.y, peer.0, peer.1);
                let valid = !local.iter().any(|s| s.rect.overlaps(r));
                let (dx, dy) = (origin.x - want.x, origin.y - want.y);
                let d2 = dx * dx + dy * dy;
                let shared = shared_len(a, edge, r);
                let better = match &best {
                    None => true,
                    Some((bv, bd, bs, ..)) => {
                        (valid && !bv)
                            || (valid == *bv
                                && (d2 < bd - 1e-6 || ((d2 - bd).abs() <= 1e-6 && shared > *bs)))
                    }
                };
                if better {
                    best = Some((valid, d2, shared, p, origin));
                }
                let _ = i;
            }
        }
    }
    best.map(|(.., p, origin)| (p, origin))
}

/// A stretch of edge one of my screens shares with one of the peer's: where
/// the cursor can cross, in my logical space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contact {
    pub from: PointF,
    pub to: PointF,
    /// The side of my screen.
    pub edge: Edge,
}

/// Every stretch of edge shared by a screen of `local` and one of `peer`
/// (both in the same space).
pub fn contacts(local: &[RectF], peer: &[RectF]) -> Vec<Contact> {
    let mut out = Vec::new();
    for l in local {
        for p in peer {
            for edge in Edge::ALL {
                let line = l.edge_pos(edge);
                if (line - p.edge_pos(edge.opposite())).abs() > TOUCH_EPS {
                    continue;
                }
                let (l0, l1) = l.span(edge);
                let (p0, p1) = p.span(edge);
                let (s, t) = (l0.max(p0), l1.min(p1));
                if t - s <= TOUCH_EPS {
                    continue;
                }
                let (from, to) = if edge.is_vertical() {
                    (PointF::new(line, s), PointF::new(line, t))
                } else {
                    (PointF::new(s, line), PointF::new(t, line))
                };
                out.push(Contact { from, to, edge });
            }
        }
    }
    out
}

/// Both machines in my logical space, ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    pub local: Vec<LogicalScreen>,
    /// The peer's screens moved into place.
    pub peer: Vec<LogicalScreen>,
    /// Index into `local` of the screen the peer is attached to.
    pub anchor: usize,
    pub contacts: Vec<Contact>,
}

/// Lays out `peer` (logical, in its own space) next to `local` per `p`.
/// `None` when either side has no screens.
pub fn scene(local: &[LogicalScreen], peer: &[LogicalScreen], p: &Placement) -> Option<Scene> {
    if peer.is_empty() {
        return None;
    }
    let pb = bounds(peer.iter().map(|s| s.rect));
    let (origin, anchor) = place(local, (pb.w, pb.h), p)?;
    let (dx, dy) = (origin.x - pb.x, origin.y - pb.y);
    let placed: Vec<LogicalScreen> = peer
        .iter()
        .map(|s| LogicalScreen {
            rect: s.rect.translate(dx, dy),
            ..*s
        })
        .collect();
    let contacts = contacts(
        &local.iter().map(|s| s.rect).collect::<Vec<_>>(),
        &placed.iter().map(|s| s.rect).collect::<Vec<_>>(),
    );
    Some(Scene {
        local: local.to_vec(),
        peer: placed,
        anchor,
        contacts,
    })
}

/// Maps the peer's native coordinates to mine: `p * scale + (dx, dy)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeerMap {
    pub scale: f64,
    pub dx: f64,
    pub dy: f64,
}

impl PeerMap {
    pub fn to_local(self, p: PointF) -> PointF {
        PointF::new(p.x * self.scale + self.dx, p.y * self.scale + self.dy)
    }

    pub fn to_peer(self, p: PointF) -> PointF {
        PointF::new((p.x - self.dx) / self.scale, (p.y - self.dy) / self.scale)
    }

    pub fn rect_to_local(self, r: RectF) -> RectF {
        r.scale(self.scale).translate(self.dx, self.dy)
    }

    pub fn rect_to_peer(self, r: RectF) -> RectF {
        r.translate(-self.dx, -self.dy).scale(1.0 / self.scale)
    }

    /// The map in the other direction (mine to the peer's).
    pub fn inverse(self) -> PeerMap {
        PeerMap {
            scale: 1.0 / self.scale,
            dx: -self.dx / self.scale,
            dy: -self.dy / self.scale,
        }
    }
}

/// Resolves `p`, a placement made on `local`, into the native map from
/// `peer` to `local`. The secondary calls this with the primary's screens as
/// `local` and takes the [`PeerMap::inverse`].
pub fn resolve(local: &ScreenSet, peer: &ScreenSet, p: &Placement) -> PeerMap {
    let (sl, sp) = (machine_scale(local), machine_scale(peer));
    let pb = bounds(logical_screens(peer).iter().map(|s| s.rect));
    let (origin, _) =
        place(&logical_screens(local), (pb.w, pb.h), p).unwrap_or((PointF::new(pb.x, pb.y), 0));
    PeerMap {
        scale: sl / sp,
        dx: sl * (origin.x - pb.x),
        dy: sl * (origin.y - pb.y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::tests::screen;
    use proptest::prelude::*;

    fn ls(id: u32, x: f64, y: f64, w: f64, h: f64) -> LogicalScreen {
        LogicalScreen {
            id,
            rect: RectF::new(x, y, w, h),
            primary: id == 1,
        }
    }

    /// The user's desk: a MacBook (primary) under a wide monitor that sticks
    /// out to the left, a second monitor right of the wide one.
    fn desk() -> Vec<LogicalScreen> {
        vec![
            ls(1, 0.0, 0.0, 1512.0, 982.0),
            ls(2, -524.0, -1440.0, 2560.0, 1440.0),
            ls(3, 2036.0, -1440.0, 1920.0, 1080.0),
        ]
    }

    fn rects(s: &[LogicalScreen]) -> Vec<RectF> {
        s.iter().map(|s| s.rect).collect()
    }

    /// A Windows laptop: 2304x1296 pixels at 150 %.
    const LAPTOP: (f64, f64) = (1536.0, 864.0);

    #[test]
    fn snaps_left_of_the_macbook_under_the_wide_monitor() {
        let (p, at) = snap(&desk(), LAPTOP, PointF::new(-1546.0, 30.0)).unwrap();
        assert_eq!(
            p,
            Placement {
                display: Some(1),
                edge: Edge::Left,
                offset: Some(30)
            }
        );
        assert_eq!(at, PointF::new(-1536.0, 30.0));
        let peer = [RectF::new(at.x, at.y, LAPTOP.0, LAPTOP.1)];
        assert_eq!(
            contacts(&rects(&desk()), &peer),
            vec![Contact {
                from: PointF::new(0.0, 30.0),
                to: PointF::new(0.0, 894.0),
                edge: Edge::Left
            }]
        );
    }

    #[test]
    fn slides_clear_of_the_wide_monitor_and_touches_both() {
        // Dropped a little too high it would overlap the wide monitor.
        let (p, at) = snap(&desk(), LAPTOP, PointF::new(-1536.0, -60.0)).unwrap();
        assert_eq!(
            p,
            Placement {
                display: Some(1),
                edge: Edge::Left,
                offset: Some(0)
            }
        );
        assert_eq!(at, PointF::new(-1536.0, 0.0));
        let peer = [RectF::new(at.x, at.y, LAPTOP.0, LAPTOP.1)];
        let c = contacts(&rects(&desk()), &peer);
        assert!(c.contains(&Contact {
            from: PointF::new(0.0, 0.0),
            to: PointF::new(0.0, 864.0),
            edge: Edge::Left
        }));
        assert!(c.contains(&Contact {
            from: PointF::new(-524.0, 0.0),
            to: PointF::new(0.0, 0.0),
            edge: Edge::Bottom
        }));
    }

    #[test]
    fn lines_up_with_a_nearby_edge() {
        let (p, _) = snap(&desk(), LAPTOP, PointF::new(-1540.0, 12.0)).unwrap();
        assert_eq!(p.offset, Some(0));
        // Far from any edge: kept where it was dropped.
        let (p, _) = snap(&desk(), LAPTOP, PointF::new(-1540.0, 70.0)).unwrap();
        assert_eq!(p.offset, Some(70));
    }

    #[test]
    fn snaps_above_and_below() {
        let (p, at) = snap(&desk(), LAPTOP, PointF::new(200.0, 1000.0)).unwrap();
        assert_eq!((p.display, p.edge), (Some(1), Edge::Bottom));
        assert_eq!(at, PointF::new(200.0, 982.0));
        let (p, at) = snap(&desk(), LAPTOP, PointF::new(2200.0, -2400.0)).unwrap();
        assert_eq!((p.display, p.edge), (Some(3), Edge::Top));
        assert_eq!(at, PointF::new(2200.0, -2304.0));
    }

    #[test]
    fn place_keeps_contact_and_centres_by_default() {
        let d = desk();
        let far = Placement {
            display: Some(1),
            edge: Edge::Left,
            offset: Some(5000),
        };
        let (at, i) = place(&d, LAPTOP, &far).unwrap();
        assert_eq!(i, 0);
        assert_eq!(at, PointF::new(-1536.0, 982.0 - MIN_CONTACT));
        // Legacy "peer on my left": the outermost screen on the left, centred.
        let (at, i) = place(&d, LAPTOP, &Placement::side(Side::Left)).unwrap();
        assert_eq!(i, 1);
        assert_eq!(at, PointF::new(-524.0 - 1536.0, -1440.0 + 288.0));
        // A screen that is gone falls back the same way.
        let gone = Placement {
            display: Some(77),
            edge: Edge::Right,
            offset: Some(0),
        };
        assert_eq!(place(&d, LAPTOP, &gone).unwrap(), (PointF::new(3956.0, -1440.0), 2));
        assert_eq!(place(&[], LAPTOP, &gone), None);
    }

    fn mac_desk() -> ScreenSet {
        ScreenSet::new(vec![
            screen(1, 0, 0, 1512, 982, 1.0),
            screen(2, -524, -1440, 2560, 1440, 1.0),
            screen(3, 2036, -1440, 1920, 1080, 1.0),
        ])
        .unwrap()
    }

    fn win_laptop() -> ScreenSet {
        ScreenSet::new(vec![screen(1, 0, 0, 2304, 1296, 1.5)]).unwrap()
    }

    #[test]
    fn resolves_both_ways() {
        let p = Placement {
            display: Some(1),
            edge: Edge::Left,
            offset: Some(30),
        };
        let mac = resolve(&mac_desk(), &win_laptop(), &p);
        assert_eq!(
            mac,
            PeerMap {
                scale: 1.0 / 1.5,
                dx: -1536.0,
                dy: 30.0
            }
        );
        // The laptop's right edge lands on the MacBook's left edge.
        let r = mac.rect_to_local(RectF::new(0.0, 0.0, 2304.0, 1296.0));
        assert!((r.right() - 0.0).abs() < 1e-9 && (r.y - 30.0).abs() < 1e-9);
        // The secondary resolves the primary's placement and inverts it: the
        // MacBook's left edge is just past the laptop's right edge.
        let win = resolve(&mac_desk(), &win_laptop(), &p).inverse();
        let q = win.to_local(PointF::new(0.0, 500.0));
        assert!((q.x - 2304.0).abs() < 1e-9 && (q.y - 705.0).abs() < 1e-9);
    }

    #[test]
    fn scene_moves_the_peer_into_place() {
        let peer = vec![ls(1, 0.0, 0.0, 1536.0, 864.0), ls(2, 1536.0, -200.0, 1920.0, 1080.0)];
        let p = Placement {
            display: Some(1),
            edge: Edge::Left,
            offset: Some(0),
        };
        let s = scene(&desk(), &peer, &p).unwrap();
        // The peer's bounding box is 3456 wide, its top-left at (0, -200).
        assert_eq!(s.peer[0].rect, RectF::new(-3456.0, 200.0, 1536.0, 864.0));
        assert_eq!(s.peer[1].rect, RectF::new(-1920.0, 0.0, 1920.0, 1080.0));
        assert_eq!(s.anchor, 0);
        assert!(!s.contacts.is_empty());
        assert_eq!(scene(&desk(), &[], &p), None);
    }

    /// 1-4 screens, each attached to an earlier one without overlap, like a
    /// real desk.
    fn arb_desk() -> impl Strategy<Value = Vec<LogicalScreen>> {
        (
            200u32..3000,
            200u32..2000,
            prop::collection::vec((0usize..4, 0usize..4, 0.0f64..=1.0, 200u32..3000, 200u32..2000), 0..4),
        )
            .prop_map(|(w0, h0, steps)| {
                let mut v = vec![ls(1, 0.0, 0.0, f64::from(w0), f64::from(h0))];
                for (k, (ai, ei, t, w, h)) in steps.into_iter().enumerate() {
                    let a = v[ai % v.len()].rect;
                    let edge = Edge::ALL[ei];
                    let size = (f64::from(w), f64::from(h));
                    let (lo, hi) = offset_range(a, edge, size);
                    let r = attach(a, edge, (lo + (hi - lo) * t).round().clamp(lo, hi), size);
                    if v.iter().all(|s| !s.rect.overlaps(r)) {
                        v.push(ls(k as u32 + 2, r.x, r.y, r.w, r.h));
                    }
                }
                v
            })
    }

    proptest! {
        #[test]
        fn prop_snap_touches_and_never_overlaps(
            desk in arb_desk(),
            w in 100u32..4000,
            h in 100u32..3000,
            x in -8000.0f64..8000.0,
            y in -8000.0f64..8000.0,
        ) {
            let size = (f64::from(w), f64::from(h));
            let (p, at) = snap(&desk, size, PointF::new(x, y)).unwrap();
            let r = RectF::new(at.x, at.y, size.0, size.1);
            prop_assert!(desk.iter().all(|s| !s.rect.overlaps(r)));
            let (again, i) = place(&desk, size, &p).unwrap();
            prop_assert_eq!(again, at);
            prop_assert_eq!(Some(desk[i].id), p.display);
            let c = MIN_CONTACT.min(size.0).min(size.1).min(desk[i].rect.w).min(desk[i].rect.h);
            prop_assert!(shared_len(desk[i].rect, p.edge, r) >= c - 1e-6);
            prop_assert!(!contacts(&rects(&desk), &[r]).is_empty());
        }

        #[test]
        fn prop_resolve_round_trips(
            off in -3000i32..3000,
            edge in 0usize..4,
            npl in prop::sample::select(vec![1.0, 1.25, 1.5, 2.0]),
            x in -5000.0f64..5000.0,
            y in -5000.0f64..5000.0,
        ) {
            let peer = ScreenSet::new(vec![screen(1, -300, 0, 2304, 1296, npl)]).unwrap();
            let p = Placement { display: Some(1), edge: Edge::ALL[edge], offset: Some(off) };
            let m = resolve(&mac_desk(), &peer, &p);
            let q = m.inverse().to_local(m.to_local(PointF::new(x, y)));
            prop_assert!((q.x - x).abs() < 1e-6 && (q.y - y).abs() < 1e-6);
            // The peer always shares an edge with the desk.
            let placed = m.rect_to_local(RectF::from_rect(peer.screens()[0].rect));
            let local = rects(&logical_screens(&mac_desk()));
            prop_assert!(!contacts(&local, &[placed]).is_empty());
        }
    }
}
