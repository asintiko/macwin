//! Pure geometry of the arrangement editor: the logical <-> canvas transform
//! and the drag and snap logic. Nothing here draws or needs a display.

use eframe::egui::{Pos2, Rect, pos2};
use macwin_core::arrange::{
    self, Contact, LogicalScreen, Placement, RectF, bounds, contacts as shared_edges,
};
use macwin_core::types::PointF;

/// Maps logical units to canvas pixels: `canvas = logical * scale + (tx, ty)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    /// Pixels per logical unit.
    pub scale: f64,
    /// Canvas x of logical x = 0.
    pub tx: f64,
    /// Canvas y of logical y = 0.
    pub ty: f64,
}

impl Transform {
    /// Fits `content` into `canvas` shrunk by `margin` pixels on every side,
    /// keeping the aspect ratio and centring the content.
    pub fn fit(content: RectF, canvas: Rect, margin: f32) -> Transform {
        let avail_w = f64::from((canvas.width() - 2.0 * margin).max(1.0));
        let avail_h = f64::from((canvas.height() - 2.0 * margin).max(1.0));
        let scale = if content.w > 0.0 && content.h > 0.0 {
            (avail_w / content.w).min(avail_h / content.h)
        } else {
            1.0
        };
        let centre = canvas.center();
        let c = content.center();
        Transform {
            scale,
            tx: f64::from(centre.x) - c.x * scale,
            ty: f64::from(centre.y) - c.y * scale,
        }
    }

    /// The canvas position of a logical point.
    pub fn to_canvas(self, p: PointF) -> Pos2 {
        pos2(
            (p.x * self.scale + self.tx) as f32,
            (p.y * self.scale + self.ty) as f32,
        )
    }

    /// The logical point under a canvas position.
    pub fn to_logical(self, p: Pos2) -> PointF {
        PointF::new(
            (f64::from(p.x) - self.tx) / self.scale,
            (f64::from(p.y) - self.ty) / self.scale,
        )
    }

    /// A logical rectangle in canvas pixels.
    pub fn rect_to_canvas(self, r: RectF) -> Rect {
        Rect::from_min_max(
            self.to_canvas(PointF::new(r.x, r.y)),
            self.to_canvas(PointF::new(r.right(), r.bottom())),
        )
    }
}

/// Everything the peer's box can reach: my screens' bounding box widened by
/// the peer's size on every side, so it can be dropped on any edge. The view
/// fits this, so it stays put while the placement changes.
pub fn reachable_bounds(local: &[RectF], peer: (f64, f64)) -> RectF {
    let b = bounds(local.iter().copied());
    RectF::new(
        b.x - peer.0,
        b.y - peer.1,
        b.w + 2.0 * peer.0,
        b.h + 2.0 * peer.1,
    )
}

/// What an idle view shows: my screens and the peer's box, with some air
/// around them.
pub fn tight_bounds(local: &[RectF], group: RectF) -> RectF {
    let b = bounds(local.iter().copied().chain([group]));
    let m = 0.07 * b.w.max(b.h);
    RectF::new(b.x - m, b.y - m, b.w + 2.0 * m, b.h + 2.0 * m)
}

/// The box of `screens`, as its size.
pub fn group_size(screens: &[LogicalScreen]) -> (f64, f64) {
    let b = bounds(screens.iter().map(|s| s.rect));
    (b.w, b.h)
}

/// `screens` moved so that their bounding box has its top-left at `origin`.
pub fn group_at(screens: &[LogicalScreen], origin: PointF) -> Vec<LogicalScreen> {
    let b = bounds(screens.iter().map(|s| s.rect));
    let (dx, dy) = (origin.x - b.x, origin.y - b.y);
    screens
        .iter()
        .map(|s| LogicalScreen {
            rect: s.rect.translate(dx, dy),
            ..*s
        })
        .collect()
}

/// A drag of the peer group in progress. The grab point is kept as a
/// fraction of the group's box, so the pointer stays on the same spot of the
/// group even while the view zooms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drag {
    /// Where the group was grabbed: 0..=1 across and down its box.
    pub frac: (f64, f64),
}

impl Drag {
    /// A drag that grabbed the group whose box is `group` at `grab`.
    pub fn new(group: RectF, grab: PointF) -> Drag {
        let f = |v: f64, start: f64, len: f64| {
            if len > 0.0 {
                ((v - start) / len).clamp(0.0, 1.0)
            } else {
                0.5
            }
        };
        Drag {
            frac: (f(grab.x, group.x, group.w), f(grab.y, group.y, group.h)),
        }
    }

    /// Where the group's top-left is for the pointer at `pointer`, given the
    /// size of the group's box.
    pub fn want(&self, pointer: PointF, size: (f64, f64)) -> PointF {
        PointF::new(
            pointer.x - self.frac.0 * size.0,
            pointer.y - self.frac.1 * size.1,
        )
    }
}

/// What dropping the group with its top-left at `want` would do: the
/// placement it snaps to and the group's top-left there.
pub fn drop_target(
    local: &[LogicalScreen],
    peer_size: (f64, f64),
    want: PointF,
) -> Option<(Placement, PointF)> {
    arrange::snap(local, peer_size, want)
}

/// Where the cursor crosses for the peer `group` (already in place).
pub fn crossings(local: &[LogicalScreen], group: &[LogicalScreen]) -> Vec<Contact> {
    let l: Vec<RectF> = local.iter().map(|s| s.rect).collect();
    let g: Vec<RectF> = group.iter().map(|s| s.rect).collect();
    shared_edges(&l, &g)
}

/// True when `p` is over one of `rects` (canvas pixels), allowing `slop`.
pub fn hit(rects: &[Rect], p: Pos2, slop: f32) -> bool {
    rects.iter().any(|r| r.expand(slop).contains(p))
}

/// Eases a settling move: `t` from 0 to 1 gives 0 to 1, fast then slow.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// The point `t` of the way from `a` to `b`.
pub fn lerp_point(a: PointF, b: PointF, t: f64) -> PointF {
    PointF::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::vec2;
    use macwin_core::arrange::Edge;

    fn ls(id: u32, x: f64, y: f64, w: f64, h: f64) -> LogicalScreen {
        LogicalScreen {
            id,
            rect: RectF::new(x, y, w, h),
            primary: id == 1,
        }
    }

    fn desk() -> Vec<LogicalScreen> {
        vec![
            ls(1, 0.0, 0.0, 1512.0, 982.0),
            ls(2, -524.0, -1440.0, 2560.0, 1440.0),
            ls(3, 2036.0, -1440.0, 1920.0, 1080.0),
        ]
    }

    const LAPTOP: (f64, f64) = (1536.0, 864.0);

    fn canvas() -> Rect {
        Rect::from_min_size(pos2(20.0, 100.0), vec2(800.0, 400.0))
    }

    fn desk_rects() -> Vec<RectF> {
        desk().iter().map(|s| s.rect).collect()
    }

    #[test]
    fn fit_centres_and_keeps_the_aspect_ratio() {
        let content = RectF::new(-100.0, -50.0, 400.0, 100.0);
        let tf = Transform::fit(content, canvas(), 10.0);
        // Width-bound: 780 px for 400 units.
        assert!((tf.scale - 780.0 / 400.0).abs() < 1e-9);
        let r = tf.rect_to_canvas(content);
        assert!((r.width() / r.height() - 4.0).abs() < 1e-4);
        assert!((r.center().x - canvas().center().x).abs() < 1e-3);
        assert!((r.center().y - canvas().center().y).abs() < 1e-3);
        assert!(canvas().shrink(10.0 - 1e-3).contains_rect(r));
    }

    #[test]
    fn fit_is_height_bound_for_tall_content() {
        let tf = Transform::fit(RectF::new(0.0, 0.0, 100.0, 1000.0), canvas(), 0.0);
        assert!((tf.scale - 0.4).abs() < 1e-9);
    }

    #[test]
    fn fit_survives_degenerate_input() {
        let tf = Transform::fit(RectF::default(), canvas(), 10.0);
        assert!(tf.scale.is_finite() && tf.scale > 0.0);
        let tiny = Rect::from_min_size(pos2(0.0, 0.0), vec2(5.0, 5.0));
        let tf = Transform::fit(RectF::new(0.0, 0.0, 10.0, 10.0), tiny, 20.0);
        assert!(tf.scale.is_finite() && tf.scale > 0.0);
    }

    #[test]
    fn transform_round_trips() {
        let tf = Transform::fit(RectF::new(-524.0, -1440.0, 4480.0, 2422.0), canvas(), 12.0);
        for p in [
            PointF::new(0.0, 0.0),
            PointF::new(-524.0, -1440.0),
            PointF::new(3956.0, 982.0),
            PointF::new(123.4, -567.8),
        ] {
            let q = tf.to_logical(tf.to_canvas(p));
            assert!(
                (q.x - p.x).abs() < 0.05 && (q.y - p.y).abs() < 0.05,
                "{p:?} -> {q:?}"
            );
        }
    }

    #[test]
    fn reachable_bounds_leave_room_for_the_peer_on_every_side() {
        let b = reachable_bounds(&desk_rects(), LAPTOP);
        assert_eq!(
            b,
            RectF::new(
                -524.0 - 1536.0,
                -1440.0 - 864.0,
                4480.0 + 3072.0,
                2422.0 + 1728.0
            )
        );
        // The same box whatever the placement: the view does not jump.
        assert_eq!(b, reachable_bounds(&desk_rects(), LAPTOP));
    }

    #[test]
    fn group_helpers_keep_the_arrangement_inside_the_group() {
        let g = vec![
            ls(1, 10.0, 20.0, 100.0, 50.0),
            ls(2, 110.0, 40.0, 80.0, 60.0),
        ];
        assert_eq!(group_size(&g), (180.0, 80.0));
        let moved = group_at(&g, PointF::new(-5.0, 0.0));
        let b = bounds(moved.iter().map(|s| s.rect));
        assert_eq!((b.x, b.y), (-5.0, 0.0));
        assert_eq!(moved[1].rect.x - moved[0].rect.x, 100.0);
        assert_eq!(moved[1].rect.y - moved[0].rect.y, 20.0);
        assert_eq!(moved[1].id, 2);
    }

    #[test]
    fn dragging_the_laptop_left_of_the_macbook_snaps_to_display_1_left() {
        let local = desk();
        let tf = Transform::fit(reachable_bounds(&desk_rects(), LAPTOP), canvas(), 8.0);
        // Start: right of the Dell monitor (the laptop's box at x 3956, y -1440),
        // grabbed at its centre.
        let start = RectF::new(3956.0, -1440.0, LAPTOP.0, LAPTOP.1);
        let grab_canvas = tf.to_canvas(start.center());
        let drag = Drag::new(start, tf.to_logical(grab_canvas));
        // The user drops it left of the MacBook, a little below its top
        // edge: the box's top-left lands at (-1546, 30).
        let drop_canvas = tf.to_canvas(PointF::new(-1546.0 + 768.0, 30.0 + 432.0));
        let want = drag.want(tf.to_logical(drop_canvas), LAPTOP);
        assert!((want.x + 1546.0).abs() < 0.05 && (want.y - 30.0).abs() < 0.05);

        let (p, at) = drop_target(&local, LAPTOP, want).unwrap();
        assert_eq!(
            p,
            Placement {
                display: Some(1),
                edge: Edge::Left,
                offset: Some(30)
            }
        );
        assert_eq!(at, PointF::new(-1536.0, 30.0));

        let group = group_at(&[ls(1, 0.0, 0.0, LAPTOP.0, LAPTOP.1)], at);
        let c = crossings(&local, &group);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].edge, Edge::Left);
        assert_eq!(
            (c[0].from, c[0].to),
            (PointF::new(0.0, 30.0), PointF::new(0.0, 894.0))
        );
    }

    #[test]
    fn the_pointer_stays_on_the_same_spot_of_the_group_when_the_view_zooms() {
        let group = RectF::new(100.0, 100.0, 400.0, 200.0);
        let drag = Drag::new(group, PointF::new(400.0, 150.0));
        assert_eq!(drag.frac, (0.75, 0.25));
        let pointer = pos2(333.0, 222.0);
        for scale in [0.05_f64, 0.12, 0.4] {
            let tf = Transform {
                scale,
                tx: 40.0,
                ty: 10.0,
            };
            let want = drag.want(tf.to_logical(pointer), (group.w, group.h));
            let top_left = tf.to_canvas(want);
            let under = pos2(
                top_left.x + (0.75 * group.w * scale) as f32,
                top_left.y + (0.25 * group.h * scale) as f32,
            );
            assert!((under.x - pointer.x).abs() < 0.01 && (under.y - pointer.y).abs() < 0.01);
        }
        // A grab outside the box is clamped onto it.
        assert_eq!(Drag::new(group, PointF::new(0.0, 900.0)).frac, (0.0, 1.0));
        assert_eq!(
            Drag::new(RectF::default(), PointF::new(5.0, 5.0)).frac,
            (0.5, 0.5)
        );
    }

    #[test]
    fn the_idle_view_is_tighter_than_the_reachable_one() {
        let laptop = RectF::new(-1536.0, 30.0, LAPTOP.0, LAPTOP.1);
        let tight = tight_bounds(&desk_rects(), laptop);
        let reach = reachable_bounds(&desk_rects(), LAPTOP);
        assert!(tight.w < reach.w && tight.h < reach.h);
        // Everything is inside, with air around it.
        for r in desk_rects().into_iter().chain([laptop]) {
            assert!(tight.x < r.x && tight.right() > r.right());
            assert!(tight.y < r.y && tight.bottom() > r.bottom());
        }
    }

    #[test]
    fn dragging_far_from_everything_still_lands_on_an_edge() {
        let (p, _) = drop_target(&desk(), LAPTOP, PointF::new(9000.0, 5000.0)).unwrap();
        assert!(p.display.is_some());
        assert!(drop_target(&[], LAPTOP, PointF::new(0.0, 0.0)).is_none());
    }

    #[test]
    fn hit_allows_a_little_slop() {
        let r = [Rect::from_min_size(pos2(100.0, 100.0), vec2(50.0, 30.0))];
        assert!(hit(&r, pos2(120.0, 110.0), 0.0));
        assert!(!hit(&r, pos2(95.0, 110.0), 0.0));
        assert!(hit(&r, pos2(95.0, 110.0), 6.0));
        assert!(!hit(&[], pos2(0.0, 0.0), 6.0));
    }

    #[test]
    fn easing_runs_from_zero_to_one() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert!(ease_out(0.5) > 0.5);
        assert_eq!(ease_out(7.0), 1.0);
        let m = lerp_point(PointF::new(0.0, 10.0), PointF::new(10.0, 20.0), 0.5);
        assert_eq!(m, PointF::new(5.0, 15.0));
    }
}
