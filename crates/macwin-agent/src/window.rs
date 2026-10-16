//! The app window's view of the controller: [`WindowBackend`] implements
//! [`macwin_ui::Backend`] on top of the tray's shared state, so the window
//! and the tray menu show the same state and drive the same controller.

use std::collections::HashMap;
use std::sync::Arc;

use macwin_core::types::{Os, RoleKind};
use macwin_platform::tray::TrayApp;
use macwin_ui::{
    Activity, Backend, Lang, Notice, PeerView, Permissions, Role, ScreenView, Settings, Status,
    UiCommand, UiState,
};

use crate::config::{Language, Role as ConfigRole};
use crate::tray::{Phase, ScreenDesc, TrayShared, TrayState};

/// Display names by screen id; only the UI thread may ask on macOS.
pub type DisplayNames = Box<dyn Fn() -> HashMap<u32, String> + Send + Sync>;
/// Opens the System Settings page of a permission.
pub type OpenSettings = Box<dyn Fn(macwin_ui::Permission) + Send + Sync>;

/// The controller as the window sees it.
pub struct WindowBackend {
    shared: Arc<TrayShared>,
    this_os: Os,
    device_name: String,
    names: DisplayNames,
    open_settings: OpenSettings,
}

impl WindowBackend {
    pub fn new(
        shared: Arc<TrayShared>,
        this_os: Os,
        device_name: String,
        names: DisplayNames,
        open_settings: OpenSettings,
    ) -> WindowBackend {
        WindowBackend {
            shared,
            this_os,
            device_name,
            names,
            open_settings,
        }
    }
}

impl Backend for WindowBackend {
    fn version(&self) -> u64 {
        self.shared.version()
    }

    fn state(&self) -> UiState {
        ui_state(
            &self.shared.state(),
            self.this_os,
            &self.device_name,
            &(self.names)(),
        )
    }

    fn send(&self, cmd: UiCommand) {
        match cmd {
            UiCommand::OpenPermissionSettings(p) => (self.open_settings)(p),
            cmd => self.shared.send_window(cmd),
        }
    }

    fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>) {
        self.shared.set_waker(waker);
    }

    fn finished(&self) -> bool {
        self.shared.finished()
    }
}

fn status(phase: &Phase) -> Status {
    match phase {
        Phase::Starting => Status::Starting,
        Phase::NotPaired => Status::NotPaired,
        Phase::NeedsSide => Status::NeedsArrangement,
        Phase::Listening => Status::Listening,
        Phase::Connecting => Status::Connecting,
        Phase::Retrying(e) => Status::Retrying(e.clone()),
        Phase::Connected(role) => Status::Connected(match role {
            RoleKind::Idle => Activity::Idle,
            RoleKind::Controlling => Activity::Controlling,
            RoleKind::Controlled => Activity::Controlled,
        }),
        Phase::Paused => Status::Paused,
        Phase::EmergencyPaused => Status::EmergencyPaused,
        Phase::PairingWait { code } => Status::PairingWait {
            code: code.as_ref().map(|c| c.to_string()),
        },
        Phase::PairingDial => Status::PairingDial,
        Phase::Failed(e) => Status::Failed(e.clone()),
        Phase::Quitting => Status::Quitting,
    }
}

fn screens(screens: &[ScreenDesc], names: &HashMap<u32, String>) -> Vec<ScreenView> {
    screens
        .iter()
        .map(|s| ScreenView {
            id: s.id,
            rect: s.rect,
            primary: s.primary,
            builtin: s.builtin,
            name: names.get(&s.id).cloned(),
        })
        .collect()
}

/// What the window shows for the controller's state `s`; `names` are the
/// display names of this computer's screens.
pub fn ui_state(
    s: &TrayState,
    this_os: Os,
    device_name: &str,
    names: &HashMap<u32, String>,
) -> UiState {
    UiState {
        lang: match s.language {
            Language::Ru => Lang::Ru,
            Language::En => Lang::En,
        },
        this_os,
        device_name: device_name.to_owned(),
        role: s.role.map(|r| match r {
            ConfigRole::Primary => Role::Primary,
            ConfigRole::Secondary => Role::Secondary,
        }),
        status: status(&s.phase),
        peer: s.peer.as_ref().map(|p| PeerView {
            name: p.name.clone(),
            os: p.os,
        }),
        local_screens: screens(&s.local_screens, names),
        peer_screens: screens(&s.peer_screens, &HashMap::new()),
        placement: s.placement,
        settings: Settings {
            clipboard: s.clipboard,
            images: s.options.images,
            files: s.options.files,
            remap: s.options.remap,
            discovery: s.options.discovery,
            autostart: s.autostart,
        },
        permissions: s.permissions.map(|p| Permissions {
            accessibility: p.accessibility,
            input_monitoring: p.input_monitoring,
        }),
        notice: s.notice.as_ref().map(|n| Notice {
            id: n.id,
            title: n.title.clone(),
            message: n.message.clone(),
            error: s.notice_error,
        }),
        app_version: env!("CARGO_PKG_VERSION").into(),
    }
}

#[cfg(test)]
mod tests {
    use macwin_core::arrange::{Edge, Placement, RectF};
    use macwin_net::pairing::PairingCode;
    use macwin_platform::tray::Notice as TrayNotice;

    use super::*;
    use crate::tray::{Options, PeerSummary, PermissionState};

    #[test]
    fn every_phase_maps_to_a_window_status() {
        let code = PairingCode::parse("123456").unwrap();
        let cases = [
            (Phase::Starting, Status::Starting),
            (Phase::NotPaired, Status::NotPaired),
            (Phase::NeedsSide, Status::NeedsArrangement),
            (Phase::Listening, Status::Listening),
            (Phase::Connecting, Status::Connecting),
            (Phase::Retrying("e".into()), Status::Retrying("e".into())),
            (
                Phase::Connected(RoleKind::Controlled),
                Status::Connected(Activity::Controlled),
            ),
            (Phase::Paused, Status::Paused),
            (Phase::EmergencyPaused, Status::EmergencyPaused),
            (
                Phase::PairingWait { code: Some(code) },
                Status::PairingWait {
                    code: Some("123-456".into()),
                },
            ),
            (
                Phase::PairingWait { code: None },
                Status::PairingWait { code: None },
            ),
            (Phase::PairingDial, Status::PairingDial),
            (Phase::Failed("e".into()), Status::Failed("e".into())),
            (Phase::Quitting, Status::Quitting),
        ];
        for (phase, want) in cases {
            assert_eq!(status(&phase), want, "{phase:?}");
        }
    }

    #[test]
    fn the_window_state_carries_role_screens_settings_and_notice() {
        let placement = Placement {
            display: Some(1),
            edge: Edge::Left,
            offset: Some(120),
        };
        let s = TrayState {
            phase: Phase::Connected(RoleKind::Idle),
            peer: Some(PeerSummary {
                name: "PC".into(),
                os: Os::Windows,
                side: None,
            }),
            clipboard: true,
            autostart: Some(true),
            notice: Some(TrayNotice {
                id: 7,
                title: "t".into(),
                message: "m".into(),
            }),
            notice_error: true,
            role: Some(ConfigRole::Primary),
            language: Language::En,
            placement: Some(placement),
            options: Options {
                images: false,
                files: true,
                remap: false,
                discovery: true,
            },
            local_screens: vec![ScreenDesc {
                id: 1,
                rect: RectF::new(0.0, 0.0, 1512.0, 982.0),
                primary: true,
                builtin: true,
            }],
            peer_screens: vec![ScreenDesc {
                id: 1,
                rect: RectF::new(0.0, 0.0, 1536.0, 864.0),
                primary: true,
                builtin: false,
            }],
            permissions: Some(PermissionState {
                accessibility: true,
                input_monitoring: false,
            }),
        };
        let names = HashMap::from([(1, "Built-in Retina Display".to_string())]);
        let ui = ui_state(&s, Os::MacOs, "MacBook", &names);
        assert_eq!(ui.lang, Lang::En);
        assert_eq!(ui.role, Some(Role::Primary));
        assert!(ui.is_paired());
        assert_eq!(ui.placement, Some(placement));
        assert_eq!(
            ui.local_screens[0].name.as_deref(),
            Some("Built-in Retina Display")
        );
        assert!(ui.local_screens[0].builtin);
        assert_eq!(ui.peer_screens[0].name, None, "names are this computer's");
        assert!(!ui.settings.images && !ui.settings.remap && ui.settings.files);
        assert_eq!(ui.settings.autostart, Some(true));
        let p = ui.permissions.unwrap();
        assert!(p.accessibility && !p.input_monitoring);
        let n = ui.notice.unwrap();
        assert!(n.error && n.id == 7);
        assert_eq!(ui.device_name, "MacBook");
    }
}
