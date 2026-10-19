//! Interface strings in Russian and English.

use macwin_core::arrange::Edge;
use macwin_core::types::Os;

use crate::model::Lang;

/// Looks strings up for one language.
#[derive(Debug, Clone, Copy)]
pub struct Tr(pub Lang);

impl Tr {
    fn p(self, ru: &'static str, en: &'static str) -> &'static str {
        match self.0 {
            Lang::Ru => ru,
            Lang::En => en,
        }
    }

    /// A name in the language's quotation marks.
    pub fn quoted(self, name: &str) -> String {
        match self.0 {
            Lang::Ru => format!("«{name}»"),
            Lang::En => format!("“{name}”"),
        }
    }

    // ---- general ----------------------------------------------------

    pub fn back(self) -> &'static str {
        self.p("Назад", "Back")
    }
    pub fn next(self) -> &'static str {
        self.p("Далее", "Next")
    }
    pub fn cancel(self) -> &'static str {
        self.p("Отмена", "Cancel")
    }
    pub fn this_computer(self) -> &'static str {
        self.p("Этот компьютер", "This computer")
    }
    pub fn os_name(self, os: Os) -> &'static str {
        match os {
            Os::MacOs => "macOS",
            Os::Windows => "Windows",
        }
    }
    pub fn step_of(self, step: usize, total: usize) -> String {
        match self.0 {
            Lang::Ru => format!("Шаг {step} из {total}"),
            Lang::En => format!("Step {step} of {total}"),
        }
    }
    pub fn dismiss(self) -> &'static str {
        self.p("Закрыть", "Dismiss")
    }
    pub fn starting(self) -> &'static str {
        self.p("Запуск…", "Starting…")
    }
    pub fn quitting(self) -> &'static str {
        self.p("Завершение работы…", "Shutting down…")
    }

    // ---- wizard: role -----------------------------------------------

    pub fn role_title(self) -> &'static str {
        self.p("Каким будет этот компьютер?", "What will this computer be?")
    }
    pub fn role_subtitle(self, device: &str, os: Os) -> String {
        let os = self.os_name(os);
        match self.0 {
            Lang::Ru => format!("Этот компьютер: {device} · {os}"),
            Lang::En => format!("This computer: {device} · {os}"),
        }
    }
    pub fn primary(self) -> &'static str {
        self.p("Основной", "Primary")
    }
    pub fn secondary(self) -> &'static str {
        self.p("Вторичный", "Secondary")
    }
    pub fn primary_desc(self) -> &'static str {
        self.p(
            "Его клавиатура и мышь будут управлять вторым компьютером. Здесь вы расставите экраны и введёте код.",
            "Its keyboard and mouse will control the second computer. Here you arrange the screens and enter the code.",
        )
    }
    pub fn secondary_desc(self) -> &'static str {
        self.p(
            "Им будут управлять с основного компьютера. Здесь появится код для подключения.",
            "It will be controlled from the primary computer. A pairing code will appear here.",
        )
    }
    pub fn current_role(self) -> &'static str {
        self.p("Сейчас", "Current")
    }

    // ---- wizard: arrangement ----------------------------------------

    pub fn arrange_title(self) -> &'static str {
        self.p(
            "Где стоит второй компьютер?",
            "Where is the second computer?",
        )
    }
    pub fn arrange_hint_home(self) -> &'static str {
        self.p(
            "Перетащите второй компьютер туда, где он стоит на столе. Курсор переходит через подсвеченный край.",
            "Drag the second computer to where it sits on your desk. The cursor crosses at the highlighted edge.",
        )
    }
    pub fn arrange_hint(self) -> &'static str {
        self.p(
            "Перетащите его туда, где он стоит на столе. Курсор будет переходить через подсвеченный край.",
            "Drag it to where it sits on your desk. The cursor will cross at the highlighted edge.",
        )
    }

    // ---- wizard: code -----------------------------------------------

    pub fn code_title(self) -> &'static str {
        self.p(
            "Введите код со второго компьютера",
            "Enter the code from the second computer",
        )
    }
    pub fn code_note(self) -> &'static str {
        self.p(
            "На втором компьютере откройте MacWin и выберите «Вторичный»",
            "On the second computer, open MacWin and choose “Secondary”",
        )
    }
    pub fn connect(self) -> &'static str {
        self.p("Подключить", "Connect")
    }
    pub fn manual_address(self) -> &'static str {
        self.p("Указать адрес вручную", "Enter the address manually")
    }
    pub fn manual_address_hint(self) -> &'static str {
        self.p(
            "Адрес второго компьютера, например 192.168.1.20",
            "Address of the second computer, e.g. 192.168.1.20",
        )
    }
    pub fn dialing(self) -> &'static str {
        self.p(
            "Ищу второй компьютер в сети…",
            "Looking for the second computer on the network…",
        )
    }
    pub fn secondary_code_title(self) -> &'static str {
        self.p(
            "Введите этот код на основном компьютере",
            "Enter this code on the primary computer",
        )
    }
    pub fn code_validity(self) -> &'static str {
        self.p(
            "Код действует 10 минут. После неверной попытки появится новый код.",
            "The code is valid for 10 minutes. After a wrong attempt a new code appears.",
        )
    }
    pub fn waiting_primary(self) -> &'static str {
        self.p(
            "Ожидание основного компьютера…",
            "Waiting for the primary computer…",
        )
    }
    pub fn generating_code(self) -> &'static str {
        self.p("Создаю код…", "Generating a code…")
    }

    // ---- wizard: done -----------------------------------------------

    pub fn done_title(self) -> &'static str {
        self.p(
            "Готово! Компьютеры связаны.",
            "All set! The computers are linked.",
        )
    }
    pub fn done_hint_primary(self, edge: Edge, anchor: &str, peer: &str) -> String {
        let (anchor, peer) = (self.quoted(anchor), self.quoted(peer));
        let edge = self.edge_dative(edge);
        match self.0 {
            Lang::Ru => format!("Подведите курсор к {edge} {anchor}, чтобы перейти на {peer}"),
            Lang::En => format!("Move the cursor to the {edge} of {anchor} to cross to {peer}"),
        }
    }
    pub fn done_hint_secondary(self, peer: &str) -> String {
        let peer = self.quoted(peer);
        match self.0 {
            Lang::Ru => format!("Теперь этим компьютером можно управлять с {peer}."),
            Lang::En => format!("You can now control this computer from {peer}."),
        }
    }
    pub fn done_title_short(self) -> &'static str {
        self.p("Готово", "Done")
    }
    pub fn show_code(self) -> &'static str {
        self.p("Показать код", "Show a code")
    }
    pub fn save_arrangement(self) -> &'static str {
        self.p("Сохранить расстановку", "Save the arrangement")
    }
    pub fn open_main(self) -> &'static str {
        self.p("Открыть главное окно", "Open the main window")
    }

    // ---- edges ------------------------------------------------------

    /// "к ... краю": the edge in the dative case.
    pub fn edge_dative(self, edge: Edge) -> &'static str {
        match (self.0, edge) {
            (Lang::Ru, Edge::Left) => "левому краю",
            (Lang::Ru, Edge::Right) => "правому краю",
            (Lang::Ru, Edge::Top) => "верхнему краю",
            (Lang::Ru, Edge::Bottom) => "нижнему краю",
            (Lang::En, Edge::Left) => "left edge",
            (Lang::En, Edge::Right) => "right edge",
            (Lang::En, Edge::Top) => "top edge",
            (Lang::En, Edge::Bottom) => "bottom edge",
        }
    }
    /// "через ... край": the edge in the accusative case.
    pub fn edge_accusative(self, edge: Edge) -> &'static str {
        match (self.0, edge) {
            (Lang::Ru, Edge::Left) => "левый край",
            (Lang::Ru, Edge::Right) => "правый край",
            (Lang::Ru, Edge::Top) => "верхний край",
            (Lang::Ru, Edge::Bottom) => "нижний край",
            (Lang::En, Edge::Left) => "left edge",
            (Lang::En, Edge::Right) => "right edge",
            (Lang::En, Edge::Top) => "top edge",
            (Lang::En, Edge::Bottom) => "bottom edge",
        }
    }
    /// "слева от": where something stands relative to a screen.
    pub fn edge_beside(self, edge: Edge) -> &'static str {
        match (self.0, edge) {
            (Lang::Ru, Edge::Left) => "слева от",
            (Lang::Ru, Edge::Right) => "справа от",
            (Lang::Ru, Edge::Top) => "над",
            (Lang::Ru, Edge::Bottom) => "под",
            (Lang::En, Edge::Left) => "to the left of",
            (Lang::En, Edge::Right) => "to the right of",
            (Lang::En, Edge::Top) => "above",
            (Lang::En, Edge::Bottom) => "below",
        }
    }

    // ---- main window ------------------------------------------------

    pub fn role_badge(self, primary: bool) -> &'static str {
        match (self.0, primary) {
            (Lang::Ru, true) => "Этот компьютер: основной",
            (Lang::Ru, false) => "Этот компьютер: вторичный",
            (Lang::En, true) => "This computer: primary",
            (Lang::En, false) => "This computer: secondary",
        }
    }
    pub fn pause(self) -> &'static str {
        self.p("Пауза", "Pause")
    }
    pub fn resume(self) -> &'static str {
        self.p("Продолжить", "Resume")
    }
    pub fn tab_arrangement(self) -> &'static str {
        self.p("Расстановка", "Arrangement")
    }
    pub fn tab_clipboard(self) -> &'static str {
        self.p("Буфер обмена", "Clipboard")
    }
    pub fn tab_settings(self) -> &'static str {
        self.p("Настройки", "Settings")
    }
    pub fn tab_about(self) -> &'static str {
        self.p("О программе", "About")
    }

    // status line texts
    pub fn st_connected(self, peer: &str) -> String {
        let peer = self.quoted(peer);
        match self.0 {
            Lang::Ru => format!("Подключено к {peer}"),
            Lang::En => format!("Connected to {peer}"),
        }
    }
    pub fn st_controlling(self, peer: &str) -> String {
        let peer = self.quoted(peer);
        match self.0 {
            Lang::Ru => format!("Управляете {peer}"),
            Lang::En => format!("You are controlling {peer}"),
        }
    }
    pub fn st_controlled(self, peer: &str) -> String {
        let peer = self.quoted(peer);
        match self.0 {
            Lang::Ru => format!("{peer} управляет этим компьютером"),
            Lang::En => format!("{peer} is controlling this computer"),
        }
    }
    pub fn st_listening(self, peer: &str) -> String {
        let peer = self.quoted(peer);
        match self.0 {
            Lang::Ru => format!("Ожидание {peer}"),
            Lang::En => format!("Waiting for {peer}"),
        }
    }
    pub fn st_connecting(self, peer: &str) -> String {
        let peer = self.quoted(peer);
        match self.0 {
            Lang::Ru => format!("Подключение к {peer}…"),
            Lang::En => format!("Connecting to {peer}…"),
        }
    }
    pub fn st_retrying(self, peer: &str) -> String {
        let peer = self.quoted(peer);
        match self.0 {
            Lang::Ru => format!("Нет связи с {peer}, пробую снова…"),
            Lang::En => format!("Lost contact with {peer}, retrying…"),
        }
    }
    pub fn st_paused(self) -> &'static str {
        self.p("Пауза", "Paused")
    }
    pub fn st_paused_detail(self) -> &'static str {
        self.p(
            "Клавиатура и мышь работают только на этом компьютере.",
            "Keyboard and mouse stay on this computer.",
        )
    }
    pub fn st_emergency(self) -> &'static str {
        self.p("Аварийная пауза", "Emergency pause")
    }
    pub fn st_emergency_detail(self) -> &'static str {
        self.p(
            "Нажата аварийная комбинация. Нажмите «Продолжить», когда будете готовы.",
            "The emergency chord was pressed. Press “Resume” when you are ready.",
        )
    }
    pub fn st_failed(self) -> &'static str {
        self.p("Ошибка", "Error")
    }
    pub fn st_needs_arrangement(self) -> &'static str {
        self.p("Расставьте экраны", "Arrange the screens")
    }
    pub fn st_needs_arrangement_detail(self) -> &'static str {
        self.p(
            "Перетащите второй компьютер туда, где он стоит на столе.",
            "Drag the second computer to where it sits on your desk.",
        )
    }
    pub fn st_pairing(self) -> &'static str {
        self.p("Сопряжение…", "Pairing…")
    }
    pub fn st_not_paired(self) -> &'static str {
        self.p("Компьютер не подключён", "No computer is paired")
    }
    pub fn st_detail_primary_edge(self, edge: Edge, anchor: &str) -> String {
        let edge = self.edge_accusative(edge);
        let anchor = self.quoted(anchor);
        match self.0 {
            Lang::Ru => format!("Курсор переходит через {edge} {anchor}"),
            Lang::En => format!("The cursor crosses the {edge} of {anchor}"),
        }
    }

    // ---- arrangement tab --------------------------------------------

    pub fn read_only_note(self) -> &'static str {
        self.p(
            "Расстановку меняют на основном компьютере",
            "The arrangement is changed on the primary computer",
        )
    }
    pub fn no_arrangement_yet(self) -> &'static str {
        self.p(
            "Расстановка появится после первого подключения основного компьютера.",
            "The arrangement appears after the primary computer connects for the first time.",
        )
    }
    pub fn no_screens(self) -> &'static str {
        self.p("Нет данных о мониторах", "No screen information")
    }
    pub fn legend_cross(self) -> &'static str {
        self.p(
            "Здесь курсор переходит между компьютерами",
            "The cursor crosses between the computers here",
        )
    }
    pub fn legend_second(self) -> &'static str {
        self.p("Второй компьютер", "Second computer")
    }
    pub fn legend_primary(self) -> &'static str {
        self.p("Основной компьютер", "Primary computer")
    }
    pub fn no_contact(self) -> &'static str {
        self.p(
            "Экраны не соприкасаются — курсор не сможет перейти.",
            "The screens do not touch, so the cursor cannot cross.",
        )
    }
    pub fn placement_caption(self, peer: &str, edge: Edge, anchor: &str) -> String {
        let (peer, anchor) = (self.quoted(peer), self.quoted(anchor));
        let beside = self.edge_beside(edge);
        match self.0 {
            Lang::Ru => format!("{peer} — {beside} {anchor}"),
            Lang::En => format!("{peer} is {beside} {anchor}"),
        }
    }
    pub fn builtin_display(self) -> &'static str {
        self.p("Встроенный дисплей", "Built-in display")
    }
    pub fn display_n(self, n: usize) -> String {
        match self.0 {
            Lang::Ru => format!("Монитор {n}"),
            Lang::En => format!("Display {n}"),
        }
    }
    pub fn screen_main(self) -> &'static str {
        self.p("главный", "main")
    }
    pub fn placeholder_windows(self) -> &'static str {
        self.p("Windows-ноутбук", "Windows laptop")
    }
    pub fn placeholder_mac(self) -> &'static str {
        "MacBook"
    }

    // ---- clipboard tab ----------------------------------------------

    pub fn clipboard_shared(self) -> &'static str {
        self.p("Общий буфер обмена", "Shared clipboard")
    }
    pub fn clipboard_shared_desc(self) -> &'static str {
        self.p(
            "Скопируйте на одном компьютере — вставьте на другом.",
            "Copy on one computer, paste on the other.",
        )
    }
    pub fn clipboard_images(self) -> &'static str {
        self.p("Картинки", "Images")
    }
    pub fn clipboard_files(self) -> &'static str {
        self.p(
            "Файлы и папки (копировать → вставить)",
            "Files and folders (copy → paste)",
        )
    }
    pub fn clipboard_limits(self) -> &'static str {
        self.p(
            "Предельные размеры: текст — 4 МиБ, картинки — 64 МиБ, файлы — 512 МиБ.",
            "Size limits: text 4 MiB, pictures 64 MiB, files 512 MiB.",
        )
    }

    // ---- settings tab -----------------------------------------------

    pub fn set_autostart(self) -> &'static str {
        self.p("Запускать при входе в систему", "Start at login")
    }
    pub fn set_autostart_na(self) -> &'static str {
        self.p("Недоступно в этой сборке", "Not available in this build")
    }
    pub fn set_remap(self) -> &'static str {
        self.p("Менять местами Ctrl и Cmd/Win", "Swap Ctrl and Cmd/Win")
    }
    pub fn set_remap_desc(self) -> &'static str {
        self.p(
            "Копировать, вставить и другие сочетания работают одинаково на обоих компьютерах.",
            "Copy, paste and other shortcuts work the same on both computers.",
        )
    }
    pub fn set_discovery(self) -> &'static str {
        self.p(
            "Искать компьютер в локальной сети",
            "Look for the computer on the local network",
        )
    }
    pub fn set_discovery_desc(self) -> &'static str {
        self.p(
            "Находить второй компьютер автоматически и переподключаться после сна.",
            "Find the second computer automatically and reconnect after sleep.",
        )
    }
    pub fn set_language(self) -> &'static str {
        self.p("Язык", "Language")
    }
    pub fn permissions_title(self) -> &'static str {
        self.p("Разрешения macOS", "macOS permissions")
    }
    pub fn permissions_desc(self) -> &'static str {
        self.p(
            "Без них MacWin не сможет перехватывать и передавать ввод.",
            "Without them MacWin cannot capture and send input.",
        )
    }
    pub fn perm_accessibility(self) -> &'static str {
        self.p("Универсальный доступ", "Accessibility")
    }
    pub fn perm_input_monitoring(self) -> &'static str {
        self.p("Мониторинг ввода", "Input Monitoring")
    }
    pub fn perm_granted(self) -> &'static str {
        self.p("Выдано", "Granted")
    }
    pub fn perm_missing(self) -> &'static str {
        self.p("Не выдано", "Not granted")
    }
    pub fn open_settings(self) -> &'static str {
        self.p("Открыть настройки", "Open Settings")
    }
    pub fn reconfigure(self) -> &'static str {
        self.p("Настроить заново", "Set up again")
    }
    pub fn reconfigure_desc(self) -> &'static str {
        self.p(
            "Снова пройти мастер: роль, расстановка, подключение.",
            "Run the wizard again: role, arrangement, pairing.",
        )
    }
    pub fn unpair(self) -> &'static str {
        self.p("Отвязать компьютер", "Unpair this computer")
    }
    pub fn unpair_desc(self) -> &'static str {
        self.p(
            "Забыть второй компьютер. Чтобы связать снова, понадобится новый код.",
            "Forget the second computer. A new code is needed to link again.",
        )
    }
    pub fn unpair_confirm(self, peer: &str) -> String {
        let peer = self.quoted(peer);
        match self.0 {
            Lang::Ru => format!("Отвязать этот компьютер от {peer}?"),
            Lang::En => format!("Unpair this computer from {peer}?"),
        }
    }
    pub fn unpair_confirm_btn(self) -> &'static str {
        self.p("Отвязать", "Unpair")
    }

    // ---- about tab --------------------------------------------------

    pub fn version(self, v: &str) -> String {
        match self.0 {
            Lang::Ru => format!("Версия {v}"),
            Lang::En => format!("Version {v}"),
        }
    }
    pub fn about_tagline(self) -> &'static str {
        self.p(
            "Одна клавиатура, одна мышь и один буфер обмена на Mac и Windows.",
            "One keyboard, one mouse and one clipboard for a Mac and a Windows PC.",
        )
    }
    pub fn emergency_title(self) -> &'static str {
        self.p(
            "Аварийный возврат управления",
            "Emergency return of control",
        )
    }
    pub fn emergency_keys(self) -> &'static str {
        self.p(
            "Ctrl+Alt+Shift+Esc (на Mac — Control+Option+Shift+Esc)",
            "Ctrl+Alt+Shift+Esc (on a Mac: Control+Option+Shift+Esc)",
        )
    }
    pub fn how_title(self) -> &'static str {
        self.p("Как это работает", "How it works")
    }
    pub fn how_items(self) -> [&'static str; 3] {
        match self.0 {
            Lang::Ru => [
                "Курсор уходит за край экрана основного компьютера и появляется на втором, как у iPad рядом с Mac.",
                "Клавиатура, мышь и буфер обмена общие; файлы копируются и вставляются между компьютерами.",
                "Компьютеры связываются одноразовым кодом, а обмен идёт по локальной сети в зашифрованном виде.",
            ],
            Lang::En => [
                "The cursor leaves the edge of the primary computer's screen and appears on the second, like an iPad next to a Mac.",
                "Keyboard, mouse and clipboard are shared; files are copied and pasted between the computers.",
                "The computers are linked with a one-time code and talk over the local network, encrypted.",
            ],
        }
    }
    pub fn shortcuts_hint(self, cmd: &str) -> String {
        match self.0 {
            Lang::Ru => format!("Вкладки: {cmd}+1 … {cmd}+5"),
            Lang::En => format!("Tabs: {cmd}+1 … {cmd}+5"),
        }
    }

    // ---- diagnostics tab --------------------------------------------

    pub fn tab_diagnostics(self) -> &'static str {
        self.p("Диагностика", "Diagnostics")
    }
    pub fn diag_waiting(self) -> &'static str {
        self.p(
            "Цифры появятся, когда компьютеры подключатся друг к другу.",
            "The numbers appear once the computers are connected.",
        )
    }
    pub fn diag_link(self) -> &'static str {
        self.p("Связь", "Link")
    }
    pub fn diag_link_desc(self) -> &'static str {
        self.p(
            "Пинг до второго компьютера 4 раза в секунду, за последнюю минуту.",
            "A ping to the other computer 4 times a second, over the last minute.",
        )
    }
    pub fn diag_rtt(self) -> &'static str {
        self.p("Задержка туда и обратно", "Round trip")
    }
    pub fn diag_jitter(self) -> &'static str {
        self.p("Разброс (джиттер)", "Jitter")
    }
    pub fn diag_loss(self) -> &'static str {
        self.p("Потери", "Loss")
    }
    pub fn diag_lost_of(self, lost: u64, total: u64) -> String {
        match self.0 {
            Lang::Ru => format!("{lost} из {total}"),
            Lang::En => format!("{lost} of {total}"),
        }
    }
    pub fn diag_mouse(self) -> &'static str {
        self.p("Мышь", "Mouse")
    }
    pub fn diag_sent(self) -> &'static str {
        self.p("Отправлено движений", "Moves sent")
    }
    pub fn diag_applied(self) -> &'static str {
        self.p("Принято движений", "Moves applied")
    }
    pub fn diag_per_sec(self, n: u32) -> String {
        match self.0 {
            Lang::Ru => format!("{n} в секунду"),
            Lang::En => format!("{n} per second"),
        }
    }
    pub fn diag_skipped(self) -> &'static str {
        self.p(
            "Пропущено (потеряны или опоздали)",
            "Skipped (lost or late)",
        )
    }
    pub fn diag_added(self) -> &'static str {
        self.p("Задержка самого MacWin", "Time MacWin adds")
    }
    pub fn diag_added_desc(self) -> &'static str {
        self.p(
            "Медиана / 95 % / максимум за 1–2 минуты.",
            "Median / 95 % / maximum over 1–2 minutes.",
        )
    }
    pub fn diag_send(self) -> &'static str {
        self.p("Отправка (захват → сеть)", "Send (capture → network)")
    }
    pub fn diag_receive(self) -> &'static str {
        self.p("Приём (сеть → курсор)", "Receive (network → cursor)")
    }
    pub fn diag_loop(self) -> &'static str {
        self.p("Опоздание цикла сеанса", "Session loop lag")
    }
    pub fn diag_versions(self, peer: &str, version: &str) -> String {
        let peer = self.quoted(peer);
        match self.0 {
            Lang::Ru => format!("MacWin на {peer}: {version}"),
            Lang::En => format!("MacWin on {peer}: {version}"),
        }
    }
    pub fn diag_copy(self) -> &'static str {
        self.p("Скопировать отчёт", "Copy report")
    }
    pub fn diag_copied(self) -> &'static str {
        self.p("Отчёт скопирован", "Report copied")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_differ_and_are_not_empty() {
        let (ru, en) = (Tr(Lang::Ru), Tr(Lang::En));
        let pairs = [
            (ru.role_title(), en.role_title()),
            (ru.arrange_hint(), en.arrange_hint()),
            (ru.code_validity(), en.code_validity()),
            (ru.clipboard_limits(), en.clipboard_limits()),
            (ru.emergency_keys(), en.emergency_keys()),
            (ru.tab_diagnostics(), en.tab_diagnostics()),
            (ru.diag_waiting(), en.diag_waiting()),
            (ru.diag_added(), en.diag_added()),
        ];
        for (r, e) in pairs {
            assert!(!r.is_empty() && !e.is_empty());
            assert_ne!(r, e);
        }
    }

    #[test]
    fn done_hint_names_the_edge_and_both_computers() {
        let s = Tr(Lang::Ru).done_hint_primary(Edge::Left, "MacBook", "Windows-ноутбук");
        assert_eq!(
            s,
            "Подведите курсор к левому краю «MacBook», чтобы перейти на «Windows-ноутбук»"
        );
        let s = Tr(Lang::En).done_hint_primary(Edge::Top, "Mac", "PC");
        assert_eq!(
            s,
            "Move the cursor to the top edge of “Mac” to cross to “PC”"
        );
    }

    #[test]
    fn placement_caption_reads_naturally() {
        assert_eq!(
            Tr(Lang::Ru).placement_caption("Dmitriy-PC", Edge::Bottom, "LG"),
            "«Dmitriy-PC» — под «LG»"
        );
        assert_eq!(
            Tr(Lang::En).placement_caption("PC", Edge::Right, "Mac"),
            "“PC” is to the right of “Mac”"
        );
    }
}
