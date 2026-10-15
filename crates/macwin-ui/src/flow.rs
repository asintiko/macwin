//! The first-run wizard as a small state machine. It is pure: the views feed
//! it the backend state and user choices, and it answers with the commands to
//! send, so it is tested without a window.

use macwin_core::arrange::Placement;

use crate::model::{Role, Status, UiCommand, UiState};

/// A page of the wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Choose primary or secondary.
    Role,
    /// Primary: place the second computer.
    Arrangement,
    /// Primary: enter the code. Secondary: show the code.
    Code,
    /// The computers are linked.
    Done,
}

/// The wizard: whether it is showing, and where.
#[derive(Debug, Clone, PartialEq)]
pub struct Flow {
    active: bool,
    rerun: bool,
    step: Step,
    draft: Option<Placement>,
}

/// The page a computer in `st` starts on.
fn default_step(st: &UiState) -> Step {
    match st.role {
        None => Step::Role,
        Some(Role::Secondary) => Step::Code,
        Some(Role::Primary) => {
            if st.placement.is_none() || st.status == Status::NeedsArrangement {
                Step::Arrangement
            } else {
                Step::Code
            }
        }
    }
}

impl Flow {
    /// The wizard for the state at startup. Commands for the first page
    /// come out of the first [`Flow::sync`].
    pub fn new() -> Flow {
        Flow {
            active: false,
            rerun: false,
            step: Step::Role,
            draft: None,
        }
    }

    /// The wizard is showing instead of the main window.
    pub fn active(&self) -> bool {
        self.active
    }

    /// The current page.
    pub fn step(&self) -> Step {
        self.step
    }

    /// The user asked to set everything up again.
    pub fn rerun(&self) -> bool {
        self.rerun
    }

    /// The arrangement chosen in the wizard and not yet confirmed.
    pub fn draft(&self) -> Option<Placement> {
        self.draft
    }

    /// Remembers the arrangement chosen in the editor.
    pub fn set_draft(&mut self, p: Placement) {
        self.draft = Some(p);
    }

    /// `(this page, pages)` for the progress indicator.
    pub fn progress(&self, role: Option<Role>) -> (usize, usize) {
        match (role, self.step) {
            (Some(Role::Secondary), Step::Role) => (1, 2),
            (Some(Role::Secondary), _) => (2, 2),
            (_, Step::Role) => (1, 3),
            (_, Step::Arrangement) => (2, 3),
            (_, _) => (3, 3),
        }
    }

    /// Brings the wizard in line with the backend: it opens when there is no
    /// role or no peer, and jumps to the last page when the peer appears.
    pub fn sync(&mut self, st: &UiState) -> Vec<UiCommand> {
        let mut out = Vec::new();
        if st.role.is_none() {
            self.active = true;
            self.rerun = false;
            self.step = Step::Role;
            self.draft = None;
            return out;
        }
        if !self.active {
            let settled = !matches!(st.status, Status::Starting | Status::Quitting);
            if st.peer.is_none() && settled {
                self.active = true;
                self.rerun = false;
                self.draft = None;
                self.goto(default_step(st), st, &mut out);
            }
            return out;
        }
        if !self.rerun && self.step != Step::Done && st.is_paired() {
            self.step = Step::Done;
        }
        out
    }

    /// Moves to `step`, asking a secondary to show a code on arrival.
    fn goto(&mut self, step: Step, st: &UiState, out: &mut Vec<UiCommand>) {
        self.step = step;
        if step == Step::Code
            && st.role == Some(Role::Secondary)
            && !matches!(st.status, Status::PairingWait { .. })
        {
            out.push(UiCommand::StartPairing);
        }
    }

    /// The user picked a role card.
    pub fn choose_role(&mut self, st: &UiState, role: Role) -> Vec<UiCommand> {
        let mut out = Vec::new();
        if st.role != Some(role) {
            out.push(UiCommand::SetRole(role));
            self.draft = None;
        }
        let paired_rerun = self.rerun && st.is_paired();
        match role {
            Role::Primary => self.step = Step::Arrangement,
            Role::Secondary if paired_rerun => self.step = Step::Done,
            Role::Secondary => {
                // The role is not in `st` yet; the code page must still ask.
                self.step = Step::Code;
                out.push(UiCommand::StartPairing);
            }
        }
        out
    }

    /// "Далее" on the arrangement page: keep `placement`, go on.
    pub fn confirm_arrangement(&mut self, st: &UiState, placement: Placement) -> Vec<UiCommand> {
        self.draft = Some(placement);
        self.step = if st.is_paired() { Step::Done } else { Step::Code };
        vec![UiCommand::SetPlacement(placement)]
    }

    /// "Назад".
    pub fn back(&mut self, st: &UiState) -> Vec<UiCommand> {
        let mut out = Vec::new();
        match (self.step, st.role) {
            (Step::Arrangement, _) => self.step = Step::Role,
            (Step::Code, Some(Role::Primary)) => {
                if st.status == Status::PairingDial {
                    out.push(UiCommand::CancelPairing);
                }
                self.step = Step::Arrangement;
            }
            (Step::Code, _) => {
                out.push(UiCommand::CancelPairing);
                self.step = Step::Role;
            }
            (Step::Role, _) if self.rerun => self.finish(),
            _ => {}
        }
        out
    }

    /// "Отмена" while waiting for a code or dialing.
    pub fn cancel_pairing(&mut self, st: &UiState) -> Vec<UiCommand> {
        let mut out = vec![UiCommand::CancelPairing];
        if st.role == Some(Role::Secondary) {
            self.step = Step::Role;
        }
        out.shrink_to_fit();
        out
    }

    /// "Открыть главное окно", or leaving the wizard started by "Настроить заново".
    pub fn finish(&mut self) {
        self.active = false;
        self.rerun = false;
        self.step = Step::Role;
        self.draft = None;
    }

    /// "Настроить заново": the wizard again from the first page.
    pub fn restart(&mut self) {
        self.active = true;
        self.rerun = true;
        self.step = Step::Role;
        self.draft = None;
    }
}

impl Default for Flow {
    fn default() -> Self {
        Flow::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixtures::desk_state;
    use crate::model::{Activity, PeerView};
    use macwin_core::arrange::Edge;
    use macwin_core::types::Os;

    fn fresh() -> UiState {
        let mut st = desk_state();
        st.role = None;
        st.peer = None;
        st.peer_screens.clear();
        st.placement = None;
        st.status = Status::NotPaired;
        st
    }

    fn left() -> Placement {
        Placement {
            display: Some(1),
            edge: Edge::Left,
            offset: Some(30),
        }
    }

    fn peer() -> PeerView {
        PeerView {
            name: "Dmitriy-PC".into(),
            os: Os::Windows,
        }
    }

    #[test]
    fn a_fresh_install_opens_on_the_role_page() {
        let mut f = Flow::new();
        assert!(f.sync(&fresh()).is_empty());
        assert!(f.active());
        assert_eq!(f.step(), Step::Role);
    }

    #[test]
    fn primary_goes_role_arrangement_code_done() {
        let mut st = fresh();
        let mut f = Flow::new();
        f.sync(&st);

        assert_eq!(f.choose_role(&st, Role::Primary), vec![UiCommand::SetRole(Role::Primary)]);
        assert_eq!(f.step(), Step::Arrangement);
        st.role = Some(Role::Primary);
        st.status = Status::NeedsArrangement;
        assert!(f.sync(&st).is_empty());
        assert_eq!(f.step(), Step::Arrangement);

        assert_eq!(f.confirm_arrangement(&st, left()), vec![UiCommand::SetPlacement(left())]);
        assert_eq!(f.step(), Step::Code);
        assert_eq!(f.draft(), Some(left()));
        st.placement = Some(left());
        st.status = Status::PairingDial;
        f.sync(&st);
        assert_eq!(f.step(), Step::Code);

        st.peer = Some(peer());
        st.status = Status::Connected(Activity::Idle);
        f.sync(&st);
        assert_eq!(f.step(), Step::Done);

        f.finish();
        assert!(!f.active());
        assert!(f.sync(&st).is_empty());
        assert!(!f.active());
    }

    #[test]
    fn secondary_asks_for_a_code_when_it_reaches_the_code_page() {
        let st = fresh();
        let mut f = Flow::new();
        f.sync(&st);
        assert_eq!(
            f.choose_role(&st, Role::Secondary),
            vec![UiCommand::SetRole(Role::Secondary), UiCommand::StartPairing]
        );
        assert_eq!(f.step(), Step::Code);
    }

    #[test]
    fn reopening_mid_setup_continues_where_it_belongs() {
        // Secondary already showing a code: no second StartPairing.
        let mut st = fresh();
        st.role = Some(Role::Secondary);
        st.status = Status::PairingWait {
            code: Some("482-913".into()),
        };
        let mut f = Flow::new();
        assert!(f.sync(&st).is_empty());
        assert_eq!((f.active(), f.step()), (true, Step::Code));

        // Secondary with nothing running: ask for a code.
        st.status = Status::NotPaired;
        let mut f = Flow::new();
        assert_eq!(f.sync(&st), vec![UiCommand::StartPairing]);

        // Primary with an arrangement goes straight to the code; without, to the arrangement.
        let mut st = fresh();
        st.role = Some(Role::Primary);
        st.placement = Some(left());
        let mut f = Flow::new();
        f.sync(&st);
        assert_eq!(f.step(), Step::Code);
        st.placement = None;
        let mut f = Flow::new();
        f.sync(&st);
        assert_eq!(f.step(), Step::Arrangement);
    }

    #[test]
    fn going_back_from_a_secondary_code_page_cancels_pairing() {
        let mut st = fresh();
        st.role = Some(Role::Secondary);
        st.status = Status::PairingWait { code: None };
        let mut f = Flow::new();
        f.sync(&st);
        assert_eq!(f.back(&st), vec![UiCommand::CancelPairing]);
        assert_eq!(f.step(), Step::Role);
        // Nothing to go back to from the first page of a first run.
        assert!(f.back(&st).is_empty());
        assert_eq!(f.step(), Step::Role);
    }

    #[test]
    fn going_back_from_the_primary_code_page_keeps_the_arrangement() {
        let mut st = fresh();
        st.role = Some(Role::Primary);
        st.placement = Some(left());
        let mut f = Flow::new();
        f.sync(&st);
        assert!(f.back(&st).is_empty());
        assert_eq!(f.step(), Step::Arrangement);
        f.confirm_arrangement(&st, left());
        st.status = Status::PairingDial;
        assert_eq!(f.back(&st), vec![UiCommand::CancelPairing]);
    }

    #[test]
    fn a_paired_app_opens_the_main_window_and_unpairing_reopens_the_wizard() {
        let mut st = desk_state();
        st.placement = Some(left());
        let mut f = Flow::new();
        assert!(f.sync(&st).is_empty());
        assert!(!f.active());

        st.peer = None;
        st.peer_screens.clear();
        st.status = Status::NotPaired;
        f.sync(&st);
        assert!(f.active());
        assert_eq!(f.step(), Step::Code);
    }

    #[test]
    fn starting_does_not_open_the_wizard() {
        let mut st = desk_state();
        st.peer = None;
        st.status = Status::Starting;
        let mut f = Flow::new();
        f.sync(&st);
        assert!(!f.active());
        st.status = Status::NotPaired;
        f.sync(&st);
        assert!(f.active());
    }

    #[test]
    fn setting_up_again_while_paired_keeps_the_pairing() {
        let mut st = desk_state();
        st.placement = Some(left());
        let mut f = Flow::new();
        f.sync(&st);
        f.restart();
        assert!(f.active() && f.rerun());
        // Paired: the wizard does not jump to the end by itself.
        f.sync(&st);
        assert_eq!(f.step(), Step::Role);

        // Same role: no SetRole; the arrangement page, then straight to the end.
        assert!(f.choose_role(&st, Role::Primary).is_empty());
        assert_eq!(f.step(), Step::Arrangement);
        assert_eq!(f.confirm_arrangement(&st, left()), vec![UiCommand::SetPlacement(left())]);
        assert_eq!(f.step(), Step::Done);
        f.finish();
        assert!(!f.active() && !f.rerun());
    }

    #[test]
    fn cancel_on_the_role_page_leaves_a_rerun() {
        let mut st = desk_state();
        st.placement = Some(left());
        let mut f = Flow::new();
        f.sync(&st);
        f.restart();
        assert!(f.back(&st).is_empty());
        assert!(!f.active());
    }

    #[test]
    fn changing_the_role_while_paired_sends_it() {
        let mut st = desk_state();
        st.placement = Some(left());
        let mut f = Flow::new();
        f.sync(&st);
        f.restart();
        assert_eq!(
            f.choose_role(&st, Role::Secondary),
            vec![UiCommand::SetRole(Role::Secondary), UiCommand::StartPairing]
        );
    }

    #[test]
    fn progress_counts_the_pages_of_each_role() {
        let mut f = Flow::new();
        assert_eq!(f.progress(None), (1, 3));
        f.step = Step::Arrangement;
        assert_eq!(f.progress(Some(Role::Primary)), (2, 3));
        f.step = Step::Code;
        assert_eq!(f.progress(Some(Role::Primary)), (3, 3));
        assert_eq!(f.progress(Some(Role::Secondary)), (2, 2));
        f.step = Step::Role;
        assert_eq!(f.progress(Some(Role::Secondary)), (1, 2));
    }
}
