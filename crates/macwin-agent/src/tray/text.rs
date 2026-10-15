//! The texts the controller and the window-mode menu show, in Russian and
//! English. The classic menu of `render` stays English; the English notices
//! are the ones it always had.

use macwin_core::types::{RoleKind, Side};

use super::{Phase, TrayState};
use crate::config::{Language, side_name};

/// What the "paired" notice tells the user to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// The legacy "its screens are on the left/right".
    OnSide(Side),
    /// The screens are placed (or the primary computer places them).
    Shares,
    /// This machine has to place them and has not.
    NeedsPlacement,
}

/// Looks the texts up for one language.
#[derive(Debug, Clone, Copy)]
pub struct Tr(pub Language);

impl Tr {
    fn p(self, ru: &'static str, en: &'static str) -> &'static str {
        match self.0 {
            Language::Ru => ru,
            Language::En => en,
        }
    }

    fn pick(self, ru: String, en: String) -> String {
        match self.0 {
            Language::Ru => ru,
            Language::En => en,
        }
    }

    /// A name in quotation marks, as the window writes it.
    fn quoted(self, name: &str) -> String {
        match self.0 {
            Language::Ru => format!("«{name}»"),
            Language::En => format!("“{name}”"),
        }
    }

    /// A name in the quotation marks of the notices.
    fn notice_quoted(self, name: &str) -> String {
        match self.0 {
            Language::Ru => format!("«{name}»"),
            Language::En => format!("\"{name}\""),
        }
    }

    // ---- the window-mode menu ---------------------------------------

    pub fn open(self) -> &'static str {
        self.p("Открыть MacWin…", "Open MacWin…")
    }

    pub fn pause(self) -> &'static str {
        self.p("Пауза", "Pause")
    }

    pub fn resume(self) -> &'static str {
        self.p("Продолжить", "Resume")
    }

    pub fn quit(self) -> &'static str {
        self.p("Выйти из MacWin", "Quit MacWin")
    }

    /// The first line of the window-mode menu (and the tooltip).
    pub fn status(self, s: &TrayState) -> String {
        let peer = match &s.peer {
            Some(p) => self.quoted(&p.name),
            None => self
                .p("второй компьютер", "the other computer")
                .to_string(),
        };
        match &s.phase {
            Phase::Starting => self.p("Запуск…", "Starting…").into(),
            Phase::NotPaired => self
                .p("Компьютер не подключён", "No computer is paired")
                .into(),
            Phase::NeedsSide => self.p("Расставьте экраны", "Arrange the screens").into(),
            Phase::Listening => self.pick(format!("Ожидание {peer}"), format!("Waiting for {peer}")),
            Phase::Connecting => self.pick(
                format!("Подключение к {peer}…"),
                format!("Connecting to {peer}…"),
            ),
            Phase::Retrying(_) => self.pick(
                format!("Нет связи с {peer}, пробую снова…"),
                format!("Lost contact with {peer}, retrying…"),
            ),
            Phase::Connected(RoleKind::Idle) => self.pick(
                format!("Подключено к {peer}"),
                format!("Connected to {peer}"),
            ),
            Phase::Connected(RoleKind::Controlling) => self.pick(
                format!("Управляете {peer}"),
                format!("You are controlling {peer}"),
            ),
            Phase::Connected(RoleKind::Controlled) => self.pick(
                format!("{peer} управляет этим компьютером"),
                format!("{peer} is controlling this computer"),
            ),
            Phase::Paused => self.p("Пауза", "Paused").into(),
            Phase::EmergencyPaused => self.p("Аварийная пауза", "Emergency pause").into(),
            Phase::PairingWait { code: Some(code) } => {
                self.pick(format!("Код сопряжения: {code}"), format!("Pairing code: {code}"))
            }
            Phase::PairingWait { code: None } => self
                .p("Подготовка к сопряжению…", "Getting ready to pair…")
                .into(),
            Phase::PairingDial => self.p("Сопряжение…", "Pairing…").into(),
            Phase::Failed(_) => self.p("Ошибка", "Error").into(),
            Phase::Quitting => self.p("Завершение работы…", "Shutting down…").into(),
        }
    }

    // ---- notices ----------------------------------------------------

    pub fn sharing_stopped(self) -> &'static str {
        self.p("Работа остановлена", "Sharing stopped")
    }

    pub fn paused_title(self) -> &'static str {
        self.p("Пауза", "Sharing paused")
    }

    /// `window`: the window (and its menu) is the way to resume.
    pub fn paused_by_chord(self, window: bool) -> &'static str {
        match (self.0, window) {
            (Language::En, false) => {
                "The emergency chord stopped sharing. Choose \"Resume sharing\" in the macwin \
                 menu to continue."
            }
            (Language::En, true) => {
                "The emergency chord stopped sharing. Press “Resume” in the MacWin window or \
                 menu to continue."
            }
            (Language::Ru, false) => {
                "Аварийная комбинация остановила работу. Выберите «Resume sharing» в меню macwin, \
                 чтобы продолжить."
            }
            (Language::Ru, true) => {
                "Аварийная комбинация остановила работу. Нажмите «Продолжить» в окне или меню \
                 MacWin."
            }
        }
    }

    pub fn cannot_change_setting(self) -> &'static str {
        self.p("Не удалось изменить настройку", "Cannot change the setting")
    }

    pub fn autostart(self) -> &'static str {
        self.p("Автозапуск", "Start at login")
    }

    pub fn invalid_pairing_input(self) -> &'static str {
        self.p("Неверный код", "Invalid pairing input")
    }

    pub fn invalid_pairing_message(self, why: &str) -> String {
        self.pick(
            format!(
                "Введите 6-значный код или HOST[:PORT] и код, показанные на втором компьютере. \
                 {why}"
            ),
            format!(
                "Enter a 6-digit code, or HOST[:PORT] followed by the code shown on the other \
                 computer. {why}"
            ),
        )
    }

    pub fn pairing_failed(self) -> &'static str {
        self.p("Не удалось связать компьютеры", "Pairing failed")
    }

    pub fn cannot_save(self, why: &str) -> String {
        self.pick(
            format!("не удалось сохранить: {why}"),
            format!("cannot save it: {why}"),
        )
    }

    pub fn paired(self, name: &str) -> String {
        let name = self.notice_quoted(name);
        self.pick(format!("Сопряжено с {name}"), format!("Paired with {name}"))
    }

    pub fn paired_message(self, next: Next, window: bool) -> String {
        match (next, window) {
            (Next::OnSide(side), _) => {
                let side_ru = match side {
                    Side::Left => "слева",
                    Side::Right => "справа",
                };
                self.pick(
                    format!(
                        "Его экраны находятся {side_ru} от этого компьютера. MacWin передаёт ему \
                         клавиатуру, мышь и буфер обмена."
                    ),
                    format!(
                        "Its screens are on the {} of this computer. macwin now shares the \
                         keyboard, mouse and clipboard with it.",
                        side_name(side)
                    ),
                )
            }
            (Next::Shares, _) => self
                .p(
                    "Клавиатура, мышь и буфер обмена теперь общие.",
                    "macwin now shares the keyboard, mouse and clipboard with it.",
                )
                .into(),
            (Next::NeedsPlacement, false) => self
                .p(
                    "Укажите, где его экраны: «Other computer is on the left» или «… on the \
                     right» в меню macwin.",
                    "Choose where its screens are: \"Other computer is on the left\" or \"… on \
                     the right\" in the macwin menu.",
                )
                .into(),
            (Next::NeedsPlacement, true) => self
                .p(
                    "Расставьте экраны в окне MacWin.",
                    "Arrange its screens in the MacWin window.",
                )
                .into(),
        }
    }

    /// The notice that shows a code (the classic menu; the window shows the
    /// code itself).
    pub fn pairing_code_title(self, code: &str, replacement: bool) -> String {
        match (self.0, replacement) {
            (Language::En, false) => format!("Pairing code {code}"),
            (Language::En, true) => format!("New pairing code {code}"),
            (Language::Ru, false) => format!("Код сопряжения {code}"),
            (Language::Ru, true) => format!("Новый код сопряжения {code}"),
        }
    }

    pub fn pairing_code_message(self, digits: &str, address: &str, replacement: bool) -> String {
        let prefix = match (self.0, replacement) {
            (_, false) => "",
            (Language::En, true) => {
                "The previous code is no longer valid; use this replacement code.\n"
            }
            (Language::Ru, true) => "Прежний код больше не действует; введите новый.\n",
        };
        let body = self.pick(
            format!(
                "На втором компьютере выберите в меню macwin «Enter pairing code…» и введите \
                 {digits} или выполните:\n  macwin pair --code {digits}\n\
                 Если компьютеры не находят друг друга в сети, введите {address} {digits} в этом \
                 меню или выполните:\n  macwin pair {address} --code {digits}\n\
                 Код действует один раз, 10 минут."
            ),
            format!(
                "On the other computer, choose Enter pairing code… in the macwin \
                 menu and type {digits}, or run:\n  macwin pair --code {digits}\n\
                 If LAN discovery fails, enter {address} {digits} in that menu or run:\n  \
                 macwin pair {address} --code {digits}\n\
                 The code works once, for 10 minutes."
            ),
        );
        format!("{prefix}{body}")
    }
}

#[cfg(test)]
mod tests {
    use macwin_core::types::Os;

    use super::*;
    use crate::tray::PeerSummary;

    fn state(phase: Phase) -> TrayState {
        TrayState {
            phase,
            peer: Some(PeerSummary {
                name: "DESKTOP-1".into(),
                os: Os::Windows,
                side: None,
            }),
            ..TrayState::default()
        }
    }

    #[test]
    fn the_status_line_follows_the_language() {
        let connected = state(Phase::Connected(RoleKind::Controlling));
        assert_eq!(Tr(Language::Ru).status(&connected), "Управляете «DESKTOP-1»");
        assert_eq!(
            Tr(Language::En).status(&connected),
            "You are controlling “DESKTOP-1”"
        );
        let lost = Tr(Language::Ru).status(&state(Phase::Retrying("x".into())));
        assert_eq!(lost, "Нет связи с «DESKTOP-1», пробую снова…");
        let alone = TrayState {
            phase: Phase::Listening,
            ..TrayState::default()
        };
        assert_eq!(Tr(Language::En).status(&alone), "Waiting for the other computer");
        assert_eq!(Tr(Language::Ru).status(&alone), "Ожидание второй компьютер");
    }

    #[test]
    fn every_phase_has_a_text_in_both_languages() {
        let code = macwin_net::pairing::PairingCode::parse("123456").unwrap();
        for phase in [
            Phase::Starting,
            Phase::NotPaired,
            Phase::NeedsSide,
            Phase::Listening,
            Phase::Connecting,
            Phase::Retrying("e".into()),
            Phase::Connected(RoleKind::Idle),
            Phase::Connected(RoleKind::Controlling),
            Phase::Connected(RoleKind::Controlled),
            Phase::Paused,
            Phase::EmergencyPaused,
            Phase::PairingWait { code: Some(code) },
            Phase::PairingWait { code: None },
            Phase::PairingDial,
            Phase::Failed("e".into()),
            Phase::Quitting,
        ] {
            let s = state(phase.clone());
            let (ru, en) = (Tr(Language::Ru).status(&s), Tr(Language::En).status(&s));
            assert!(!ru.is_empty() && !en.is_empty(), "{phase:?}");
            assert_ne!(ru, en, "{phase:?}");
        }
    }

    #[test]
    fn the_pairing_code_notice_keeps_its_english_wording() {
        let en = Tr(Language::En);
        assert_eq!(en.pairing_code_title("123-456", false), "Pairing code 123-456");
        assert_eq!(en.pairing_code_title("123-456", true), "New pairing code 123-456");
        let m = en.pairing_code_message("123456", "10.0.0.2:47800", true);
        assert!(m.starts_with("The previous code is no longer valid"), "{m}");
        assert!(m.contains("10.0.0.2:47800 123456") && m.contains("macwin pair --code 123456"));
        let ru = Tr(Language::Ru).pairing_code_message("123456", "10.0.0.2:47800", false);
        assert!(ru.contains("10.0.0.2:47800 123456") && !ru.contains("The code works"), "{ru}");
    }

    #[test]
    fn paired_notices_name_the_peer_and_the_next_step() {
        assert_eq!(Tr(Language::En).paired("pc"), "Paired with \"pc\"");
        assert_eq!(Tr(Language::Ru).paired("pc"), "Сопряжено с «pc»");
        let en = Tr(Language::En);
        assert!(en.paired_message(Next::NeedsPlacement, false).contains("Choose where"));
        assert!(en.paired_message(Next::NeedsPlacement, true).contains("window"));
        assert!(en.paired_message(Next::OnSide(Side::Left), false).contains("on the left"));
        assert!(
            Tr(Language::Ru)
                .paired_message(Next::OnSide(Side::Right), true)
                .contains("справа")
        );
    }
}
