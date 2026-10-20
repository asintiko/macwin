//! System shortcuts the controller sends for input that has no key on the
//! peer: a tap of the Mac's Fn / 🌐 key and trackpad gestures. They are
//! written in the peer's own keys (the Ctrl <-> Cmd remap does not apply) as
//! the peer's OS defines them by default.

use crate::keys::KeySet;
use crate::msg::{InputMsg, KeyMsg, WheelMsg};
use crate::types::{Gesture, GesturePhase, Hid, ModifierMask, Os, WheelUnit};

/// Pinch magnification per zoom step (one Ctrl+wheel notch on Windows).
pub const ZOOM_STEP: f64 = 0.12;
/// One magnify event never counts for more than this: a full doubling.
const MAX_MAGNIFY: f64 = 1.0;
const NOTCH: i32 = 120;

/// Switches the input language: Win+Space on Windows, Control+Space
/// ("Select the previous input source") on macOS.
pub fn language_switch(peer: Os) -> &'static [Hid] {
    match peer {
        Os::Windows => &[Hid::LEFT_GUI, Hid::SPACE],
        Os::MacOs => &[Hid::LEFT_CTRL, Hid::SPACE],
    }
}

/// The peer's equivalent of a swipe: on Windows Task View and the next /
/// previous virtual desktop, on macOS Mission Control and moving a Space.
/// Mission Control and App Exposé both open the overview: the vertical
/// direction is not reliable enough to tell them apart.
pub fn gesture_keys(peer: Os, g: Gesture) -> Option<&'static [Hid]> {
    match (peer, g) {
        (Os::Windows, Gesture::Overview) => Some(&[Hid::LEFT_GUI, Hid::TAB]),
        (Os::Windows, Gesture::SwitchSpace { next: true }) => {
            Some(&[Hid::LEFT_CTRL, Hid::LEFT_GUI, Hid::RIGHT_ARROW])
        }
        (Os::Windows, Gesture::SwitchSpace { next: false }) => {
            Some(&[Hid::LEFT_CTRL, Hid::LEFT_GUI, Hid::LEFT_ARROW])
        }
        (Os::MacOs, Gesture::Overview) => Some(&[Hid::LEFT_CTRL, Hid::UP_ARROW]),
        (Os::MacOs, Gesture::SwitchSpace { next: true }) => {
            Some(&[Hid::LEFT_CTRL, Hid::RIGHT_ARROW])
        }
        (Os::MacOs, Gesture::SwitchSpace { next: false }) => {
            Some(&[Hid::LEFT_CTRL, Hid::LEFT_ARROW])
        }
        _ => None,
    }
}

fn key(hid: Hid, down: bool) -> InputMsg {
    InputMsg::Key(KeyMsg {
        hid,
        down,
        repeat: false,
    })
}

/// Presses `keys` in order and releases them in reverse. Modifiers the peer
/// already holds (`held`, in the peer's keys) are neither pressed nor
/// released, so the user's own hold survives the shortcut.
pub fn chord(keys: &[Hid], held: ModifierMask) -> Vec<InputMsg> {
    chord_preserving_keys(keys, held, &KeySet::default())
}

/// As [`chord`], but repeats held main keys without releasing them.
pub(crate) fn chord_preserving_keys(
    keys: &[Hid],
    held: ModifierMask,
    held_keys: &KeySet,
) -> Vec<InputMsg> {
    let pressed: Vec<Hid> = keys
        .iter()
        .copied()
        .filter(|&k| !held.contains(k))
        .collect();
    pressed
        .iter()
        .map(|&k| key(k, true))
        .chain(
            pressed
                .iter()
                .rev()
                .filter(|&&k| !held_keys.contains(k))
                .map(|&k| key(k, false)),
        )
        .collect()
}

/// Turns pinch magnification into whole zoom steps.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ZoomSteps {
    accum: f64,
}

impl ZoomSteps {
    /// Steps for one magnify event: > 0 zooms in, < 0 out. A new pinch
    /// starts from zero and the remainder of an ended one is dropped.
    pub fn on_magnify(&mut self, delta: f64, phase: GesturePhase) -> i32 {
        if phase == GesturePhase::Began {
            self.accum = 0.0;
        }
        if delta.is_finite() {
            self.accum += delta.clamp(-MAX_MAGNIFY, MAX_MAGNIFY);
        }
        let steps = (self.accum / ZOOM_STEP).trunc();
        self.accum -= steps * ZOOM_STEP;
        if phase == GesturePhase::Ended {
            self.accum = 0.0;
        }
        steps as i32
    }
}

/// Zooms the peer by `steps`: on Windows Ctrl + wheel notches, which is what a
/// precision touchpad's pinch does in most apps. macOS peers get nothing.
pub fn zoom(peer: Os, steps: i32, held: ModifierMask) -> Vec<InputMsg> {
    if steps == 0 || peer != Os::Windows {
        return Vec::new();
    }
    let press_ctrl = !held.intersects(ModifierMask::CTRL);
    let dy = if steps > 0 { NOTCH } else { -NOTCH };
    let mut out = Vec::new();
    if press_ctrl {
        out.push(key(Hid::LEFT_CTRL, true));
    }
    // Separate notches: many apps zoom one step per message whatever its size.
    for _ in 0..steps.unsigned_abs() {
        out.push(InputMsg::Wheel(WheelMsg {
            dx: 0,
            dy,
            unit: WheelUnit::Notch120,
        }));
    }
    if press_ctrl {
        out.push(key(Hid::LEFT_CTRL, false));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(msgs: &[InputMsg]) -> Vec<(Hid, bool)> {
        msgs.iter()
            .map(|m| match m {
                InputMsg::Key(k) => (k.hid, k.down),
                other => panic!("not a key: {other:?}"),
            })
            .collect()
    }

    fn mask(keys: &[Hid]) -> ModifierMask {
        let mut m = ModifierMask::EMPTY;
        for &k in keys {
            m.insert(k);
        }
        m
    }

    #[test]
    fn the_language_switch_is_the_peers_default_shortcut() {
        assert_eq!(
            keys(&chord(language_switch(Os::Windows), ModifierMask::EMPTY)),
            vec![
                (Hid::LEFT_GUI, true),
                (Hid::SPACE, true),
                (Hid::SPACE, false),
                (Hid::LEFT_GUI, false),
            ]
        );
        assert_eq!(language_switch(Os::MacOs), &[Hid::LEFT_CTRL, Hid::SPACE]);
    }

    #[test]
    fn a_chord_leaves_modifiers_the_user_holds_alone() {
        // The user holds Cmd on the Mac: Ctrl is down on the PC.
        let held = mask(&[Hid::LEFT_CTRL]);
        let next = gesture_keys(Os::Windows, Gesture::SwitchSpace { next: true }).unwrap();
        assert_eq!(
            keys(&chord(next, held)),
            vec![
                (Hid::LEFT_GUI, true),
                (Hid::RIGHT_ARROW, true),
                (Hid::RIGHT_ARROW, false),
                (Hid::LEFT_GUI, false),
            ]
        );
    }

    #[test]
    fn a_chord_repeats_a_held_main_key_without_releasing_it() {
        let held: KeySet = [Hid::SPACE].into_iter().collect();
        assert_eq!(
            keys(&chord_preserving_keys(
                language_switch(Os::Windows),
                ModifierMask::EMPTY,
                &held,
            )),
            vec![
                (Hid::LEFT_GUI, true),
                (Hid::SPACE, true),
                (Hid::LEFT_GUI, false),
            ]
        );
    }

    #[test]
    fn swipes_map_to_task_view_desktops_and_spaces() {
        let w = |g| gesture_keys(Os::Windows, g).unwrap().to_vec();
        assert_eq!(w(Gesture::Overview), vec![Hid::LEFT_GUI, Hid::TAB]);
        assert_eq!(
            w(Gesture::SwitchSpace { next: false }),
            vec![Hid::LEFT_CTRL, Hid::LEFT_GUI, Hid::LEFT_ARROW]
        );
        let m = |g| gesture_keys(Os::MacOs, g).unwrap().to_vec();
        assert_eq!(m(Gesture::Overview), vec![Hid::LEFT_CTRL, Hid::UP_ARROW]);
        assert_eq!(
            m(Gesture::SwitchSpace { next: true }),
            vec![Hid::LEFT_CTRL, Hid::RIGHT_ARROW]
        );
        assert_eq!(gesture_keys(Os::Windows, Gesture::Other), None);
        let pinch = Gesture::Magnify {
            delta: 0.5,
            phase: GesturePhase::Changed,
        };
        assert_eq!(gesture_keys(Os::Windows, pinch), None);
    }

    #[test]
    fn pinching_counts_whole_steps_per_gesture() {
        let mut z = ZoomSteps::default();
        assert_eq!(z.on_magnify(0.05, GesturePhase::Began), 0);
        assert_eq!(z.on_magnify(0.05, GesturePhase::Changed), 0);
        // 0.15 in total: one step, 0.03 left.
        assert_eq!(z.on_magnify(0.05, GesturePhase::Changed), 1);
        assert_eq!(z.on_magnify(0.3, GesturePhase::Changed), 2);
        // Back the other way.
        assert_eq!(z.on_magnify(-0.5, GesturePhase::Changed), -3);
        // The remainder of an ended pinch does not leak into the next one.
        assert_eq!(z.on_magnify(-0.02, GesturePhase::Ended), 0);
        assert_eq!(z.on_magnify(0.1, GesturePhase::Began), 0);
        assert_eq!(z.on_magnify(0.03, GesturePhase::Changed), 1);
        // Bogus values are bounded or ignored.
        assert_eq!(z.on_magnify(1e9, GesturePhase::Changed), 8);
        assert_eq!(z.on_magnify(f64::NAN, GesturePhase::Changed), 0);
    }

    #[test]
    fn zoom_is_ctrl_and_wheel_notches_on_windows_only() {
        let msgs = zoom(Os::Windows, -2, ModifierMask::EMPTY);
        let wheel = |dy| {
            InputMsg::Wheel(WheelMsg {
                dx: 0,
                dy,
                unit: WheelUnit::Notch120,
            })
        };
        assert_eq!(
            msgs,
            vec![
                key(Hid::LEFT_CTRL, true),
                wheel(-120),
                wheel(-120),
                key(Hid::LEFT_CTRL, false),
            ]
        );
        // Ctrl already held on the PC: only the notches.
        assert_eq!(
            zoom(Os::Windows, 1, mask(&[Hid::RIGHT_CTRL])),
            vec![wheel(120)]
        );
        assert!(zoom(Os::Windows, 0, ModifierMask::EMPTY).is_empty());
        assert!(zoom(Os::MacOs, 3, ModifierMask::EMPTY).is_empty());
    }
}
