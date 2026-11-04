//! A call that comes in while another is live: it is announced, and can be
//! declined or answered by ending the first call. There is one media session
//! at a time, so the second call only rings until then.

use super::screen::with_waiting;
use super::*;
use crate::backend::Command;

pub(super) struct Waiting {
    pub(super) account: AccountId,
    pub(super) session: CallSession,
    pub(super) avatar: Option<gtk::gdk::Texture>,
}

/// What declining the waiting call asks of its account. When it belongs to
/// the account of the live call the worker holds it apart; otherwise it is
/// that account's own ringing call.
pub(super) fn decline_command(live: AccountId, waiting: AccountId) -> Command {
    if live == waiting {
        Command::DeclineWaitingCall
    } else {
        Command::DeclineCall
    }
}

/// What ending the live call and answering the waiting one asks of each
/// account. Within one account the worker does both in order; across two, the
/// first is hung up and the second answered.
pub(super) fn end_and_answer_commands(
    live: AccountId,
    waiting: AccountId,
) -> Vec<(AccountId, Command)> {
    if live == waiting {
        vec![(waiting, Command::EndAndAnswerWaiting)]
    } else {
        vec![(live, Command::HangUpCall), (waiting, Command::AnswerCall)]
    }
}

impl NativeApplication {
    /// The account of the call in the window, or of the waiting one when no
    /// call is in the window.
    fn live_account_or(&self, waiting: AccountId) -> AccountId {
        self.calls
            .live
            .as_ref()
            .filter(|live| live.is_active())
            .map_or(waiting, |live| live.account)
    }

    /// What the window shows for `session`, with the waiting call added.
    pub(super) fn full_screen(&self, session: &CallSession, name: &str) -> Screen {
        let screen = screen(session, name, crate::util::now());
        match &self.calls.waiting {
            Some(waiting) if !session.phase.is_finished() => with_waiting(
                screen,
                &chat_label(self.chats_of(waiting.account), &waiting.session.chat),
                waiting.session.video,
                name,
            ),
            _ => screen,
        }
    }

    pub(super) fn is_waiting(&self, account: AccountId, call_id: &str) -> bool {
        self.calls
            .waiting
            .as_ref()
            .is_some_and(|waiting| waiting.account == account && waiting.session.id == call_id)
    }

    /// The worker of the live call's account reports a call that came in
    /// during it, or that it is gone.
    pub(super) fn waiting_changed(&mut self, account: AccountId, session: Option<CallSession>) {
        match session {
            Some(session) if self.calls.waiting.is_none() => self.start_waiting(account, session),
            Some(_) => {}
            None if self
                .calls
                .waiting
                .as_ref()
                .is_some_and(|waiting| waiting.account == account) =>
            {
                self.clear_waiting();
            }
            None => {}
        }
    }

    /// Another account's call rings while this one is live.
    pub(super) fn other_account_call(&mut self, account: AccountId, session: CallSession) {
        let ringing = session.phase == CallPhase::Ringing;
        if self.is_waiting(account, &session.id) {
            if ringing {
                return;
            }
            self.clear_waiting();
        } else if ringing && self.calls.waiting.is_none() {
            self.start_waiting(account, session);
        }
    }

    fn start_waiting(&mut self, account: AccountId, session: CallSession) {
        let name = chat_label(self.chats_of(account), &session.chat);
        let avatar = self
            .avatars_of(account)
            .and_then(|avatars| avatars.get(&session.chat))
            .cloned();
        self.notify_call(
            account,
            &session,
            CallBanner::Waiting,
            &name,
            avatar.as_deref(),
        );
        let avatar = avatar_image(avatar.as_deref());
        self.calls.waiting = Some(Waiting {
            account,
            session,
            avatar,
        });
        self.call_tick();
    }

    pub(super) fn clear_waiting(&mut self) {
        if let Some(waiting) = self.calls.waiting.take() {
            self.notifications
                .clear_call(waiting.account, &waiting.session.id);
        }
        self.call_tick();
    }

    pub(super) fn decline_waiting(&mut self) {
        let Some(waiting) = self.calls.waiting.as_ref() else {
            return;
        };
        let account = waiting.account;
        let command = decline_command(self.live_account_or(account), account);
        self.clear_waiting();
        if let Some(backend) = self.call_backend(account) {
            backend.send(command);
        }
    }

    pub(super) fn end_and_answer(&mut self) {
        let Some(waiting) = self.calls.waiting.take() else {
            return;
        };
        self.notifications
            .clear_call(waiting.account, &waiting.session.id);
        let live_account = self.live_account_or(waiting.account);
        for (account, command) in end_and_answer_commands(live_account, waiting.account) {
            if let Some(backend) = self.call_backend(account) {
                backend.send(command);
            }
        }
        if let Some(live) = self.calls.live.take().filter(LiveCall::is_active) {
            self.calls.abandoned = Some((live.account, live.session.id.clone()));
        }
    }

    /// The call in the window ended: a waiting call of another account, which
    /// its own worker keeps ringing, takes the window. One of the same account
    /// is brought up by its worker.
    pub(super) fn promote_waiting(&mut self, ended: AccountId, sender: &ComponentSender<Self>) {
        if self.calls.in_call()
            || self
                .calls
                .waiting
                .as_ref()
                .is_none_or(|waiting| waiting.account == ended)
        {
            return;
        }
        if let Some(waiting) = self.calls.waiting.take() {
            self.notifications
                .clear_call(waiting.account, &waiting.session.id);
            self.apply_call_change(waiting.account, waiting.session, sender);
        }
    }
}
