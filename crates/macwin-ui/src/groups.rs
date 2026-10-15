//! Which computer's screens stand still in the arrangement editor and which
//! are moved, with their labels.
//!
//! A placement always describes where the secondary's screens sit next to the
//! primary's. The primary draws its own screens fixed and drags the peer; the
//! secondary draws the primary's screens fixed and shows itself in place.

use macwin_core::arrange::{LogicalScreen, RectF};
use macwin_core::types::Os;

use crate::i18n::Tr;
use crate::model::{Role, ScreenView, UiState};

/// The screens of one computer, ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// The screens in that computer's logical space.
    pub screens: Vec<LogicalScreen>,
    /// One label per screen.
    pub labels: Vec<String>,
    /// The computer's OS.
    pub os: Os,
    /// The computer's name.
    pub name: String,
    /// Made up because the real screens are not known yet.
    pub placeholder: bool,
}

/// Both groups of the editor.
#[derive(Debug, Clone, PartialEq)]
pub struct Groups {
    /// The primary computer's screens: they stay put.
    pub anchor: Group,
    /// The secondary computer's screens: they are placed next to the anchor.
    pub moving: Group,
    /// The moving group is this computer (the secondary's view).
    pub moving_is_this: bool,
}

fn logical(s: &ScreenView) -> LogicalScreen {
    LogicalScreen {
        id: s.id,
        rect: s.rect,
        primary: s.primary,
    }
}

fn opposite(os: Os) -> Os {
    match os {
        Os::MacOs => Os::Windows,
        Os::Windows => Os::MacOs,
    }
}

/// A laptop of the given OS with a typical logical resolution.
fn placeholder(os: Os, tr: Tr) -> Group {
    let (w, h, name) = match os {
        Os::Windows => (1536.0, 864.0, tr.placeholder_windows()),
        Os::MacOs => (1470.0, 956.0, tr.placeholder_mac()),
    };
    Group {
        screens: vec![LogicalScreen {
            id: 1,
            rect: RectF::new(0.0, 0.0, w, h),
            primary: true,
        }],
        labels: vec![name.to_owned()],
        os,
        name: name.to_owned(),
        placeholder: true,
    }
}

/// The label of the screen at `i`: its name, or a generic one.
fn screen_label(s: &ScreenView, i: usize, tr: Tr) -> String {
    match s.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => n.to_owned(),
        None if s.builtin => tr.builtin_display().to_owned(),
        None => tr.display_n(i + 1),
    }
}

/// A group from real screens. With one screen the label is the computer's
/// name; with several, the main one carries it.
fn group(screens: &[ScreenView], os: Os, name: &str, name_the_screen: bool, tr: Tr) -> Group {
    let single = screens.len() == 1;
    let main = screens.iter().position(|s| s.primary).unwrap_or(0);
    let labels = screens
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if name_the_screen && (single || i == main) {
                name.to_owned()
            } else {
                screen_label(s, i, tr)
            }
        })
        .collect();
    Group {
        screens: screens.iter().map(logical).collect(),
        labels,
        os,
        name: name.to_owned(),
        placeholder: false,
    }
}

/// The groups for `role` from `st`.
pub fn build(st: &UiState, role: Role, tr: Tr) -> Groups {
    let (peer_os, peer_name) = match &st.peer {
        Some(p) => (p.os, Some(p.name.as_str())),
        None => (opposite(st.this_os), None),
    };
    let peer_group = |name_it: bool| {
        if st.peer_screens.is_empty() {
            let mut g = placeholder(peer_os, tr);
            if let Some(n) = peer_name {
                g.name = n.to_owned();
                g.labels = vec![n.to_owned()];
            }
            g
        } else {
            let name = peer_name.map_or_else(
                || match peer_os {
                    Os::Windows => tr.placeholder_windows().to_owned(),
                    Os::MacOs => tr.placeholder_mac().to_owned(),
                },
                str::to_owned,
            );
            group(&st.peer_screens, peer_os, &name, name_it, tr)
        }
    };
    match role {
        Role::Primary => Groups {
            anchor: group(&st.local_screens, st.this_os, &st.device_name, false, tr),
            moving: peer_group(true),
            moving_is_this: false,
        },
        Role::Secondary => {
            let mut anchor = peer_group(false);
            if !anchor.placeholder && anchor.screens.len() == 1 {
                anchor.labels = vec![anchor.name.clone()];
            }
            let mut moving = group(&st.local_screens, st.this_os, &st.device_name, true, tr);
            moving.name = tr.this_computer().to_owned();
            if moving.screens.len() == 1 {
                moving.labels = vec![tr.this_computer().to_owned()];
            }
            Groups {
                anchor,
                moving,
                moving_is_this: true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Lang;
    use crate::model::fixtures::desk_state;

    #[test]
    fn primary_keeps_its_own_screens_fixed() {
        let st = desk_state();
        let g = build(&st, Role::Primary, Tr(Lang::Ru));
        assert_eq!(g.anchor.screens.len(), 3);
        assert_eq!(g.anchor.os, Os::MacOs);
        assert_eq!(
            g.anchor.labels,
            vec!["Встроенный дисплей", "LG UltraFine", "Dell U2720Q"]
        );
        assert_eq!(g.moving.os, Os::Windows);
        assert_eq!(g.moving.labels, vec!["Dmitriy-PC"]);
        assert!(!g.moving.placeholder && !g.moving_is_this);
    }

    #[test]
    fn primary_without_peer_screens_gets_a_placeholder_of_the_other_os() {
        let mut st = desk_state();
        st.peer = None;
        st.peer_screens.clear();
        let g = build(&st, Role::Primary, Tr(Lang::Ru));
        assert!(g.moving.placeholder);
        assert_eq!(g.moving.os, Os::Windows);
        assert_eq!(g.moving.name, "Windows-ноутбук");
        assert_eq!(
            g.moving.screens[0].rect,
            RectF::new(0.0, 0.0, 1536.0, 864.0)
        );

        st.this_os = Os::Windows;
        let g = build(&st, Role::Primary, Tr(Lang::En));
        assert_eq!(g.moving.os, Os::MacOs);
        assert_eq!(g.moving.name, "MacBook");
        assert_eq!(
            g.moving.screens[0].rect,
            RectF::new(0.0, 0.0, 1470.0, 956.0)
        );
    }

    #[test]
    fn a_known_peer_names_the_placeholder() {
        let mut st = desk_state();
        st.peer_screens.clear();
        let g = build(&st, Role::Primary, Tr(Lang::Ru));
        assert!(g.moving.placeholder);
        assert_eq!(g.moving.name, "Dmitriy-PC");
    }

    #[test]
    fn secondary_draws_from_the_primarys_point_of_view() {
        let mut st = desk_state();
        st.this_os = Os::Windows;
        st.device_name = "Dmitriy-PC".into();
        st.role = Some(Role::Secondary);
        st.local_screens = vec![ScreenView {
            id: 1,
            rect: RectF::new(0.0, 0.0, 1536.0, 864.0),
            primary: true,
            builtin: true,
            name: None,
        }];
        st.peer = Some(crate::model::PeerView {
            name: "MacBook Pro".into(),
            os: Os::MacOs,
        });
        st.peer_screens = crate::model::fixtures::desk_screens();
        let g = build(&st, Role::Secondary, Tr(Lang::Ru));
        // The primary's three screens stay; this computer is the moving one.
        assert_eq!(g.anchor.screens.len(), 3);
        assert_eq!(g.anchor.os, Os::MacOs);
        assert_eq!(g.moving.os, Os::Windows);
        assert_eq!(g.moving.labels, vec!["Этот компьютер"]);
        assert!(g.moving_is_this);
    }

    #[test]
    fn secondary_without_primary_screens_gets_a_placeholder_primary() {
        let mut st = desk_state();
        st.this_os = Os::Windows;
        st.peer = None;
        st.peer_screens.clear();
        let g = build(&st, Role::Secondary, Tr(Lang::En));
        assert!(g.anchor.placeholder);
        assert_eq!(g.anchor.os, Os::MacOs);
    }

    #[test]
    fn unnamed_external_screens_are_numbered() {
        let screens = vec![
            ScreenView {
                id: 1,
                rect: RectF::new(0.0, 0.0, 100.0, 100.0),
                primary: true,
                builtin: false,
                name: Some("  ".into()),
            },
            ScreenView {
                id: 2,
                rect: RectF::new(100.0, 0.0, 100.0, 100.0),
                primary: false,
                builtin: false,
                name: None,
            },
        ];
        let g = group(&screens, Os::MacOs, "Mac", false, Tr(Lang::En));
        assert_eq!(g.labels, vec!["Display 1", "Display 2"]);
        let g = group(&screens, Os::MacOs, "Mac", true, Tr(Lang::En));
        assert_eq!(g.labels, vec!["Mac", "Display 2"]);
    }
}
