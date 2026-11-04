//! Voice and video calls through whatsapp-rust's VoIP runtime.
//!
//! The worker owns the one call in progress: its state machine, its archive
//! event, and the tasks that drive the runtime. Audio and video devices live in
//! `crate::calls`.

use std::collections::VecDeque;

use whatsapp_rust::types::call::{CallAction, CallEndedElsewhere, IncomingCall};
use whatsapp_rust::voip::KeyframeUrgency;
use whatsapp_rust::wacore::stanza::call::{
    TERMINATE_REASON_ACCEPTED_ELSEWHERE, TERMINATE_REASON_REJECTED_ELSEWHERE,
};

use super::*;
use crate::backend::CallNotice;
use crate::calls::VideoSlots;
use crate::i18n::gettext;
use crate::model::call::{CallDirection, CallOutcome, CallPhase, CallSession, CallSignal};

mod history;
mod runtime;
#[cfg(test)]
mod tests;

use runtime::{
    CallShared, Dial, Reporter, accept_video, hang_up, run_call, stop_camera, switch_camera_on,
    teardown,
};

/// Prefix of the id an outgoing call carries until the runtime assigns the
/// real one; signals for it are matched by peer instead.
const PENDING_ID: &str = "pending-";
/// How long closing the app waits for a hang-up. It is shorter than
/// `TERMINATE_TIMEOUT` because the window is waiting to close.
const SHUTDOWN_HANGUP: Duration = Duration::from_secs(2);
const KNOWN_OFFERS: usize = 32;

#[derive(Default)]
pub(super) struct Calls {
    active: Option<ActiveCall>,
    /// Counts calls begun. Notices carry the number of the call they belong
    /// to, so one from an earlier call cannot touch the current one.
    generations: u64,
    history_dirty: bool,
    /// Offers seen lately, so a missed-call event, which carries only the
    /// caller and the id, can be told from a group call and given its kind.
    offers: VecDeque<(String, Offer)>,
    #[cfg(test)]
    rejected: std::sync::Mutex<Vec<String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Offer {
    Group,
    Direct { video: bool },
}

impl Calls {
    fn remember_offer(&mut self, id: &str, offer: Offer) {
        self.offers.retain(|(known, _)| known != id);
        if self.offers.len() == KNOWN_OFFERS {
            self.offers.pop_front();
        }
        self.offers.push_back((id.to_owned(), offer));
    }

    fn offer(&self, id: &str) -> Option<Offer> {
        self.offers
            .iter()
            .find(|(known, _)| known == id)
            .map(|(_, offer)| *offer)
    }

    #[cfg(test)]
    fn rejected(&self) -> Vec<String> {
        self.rejected
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

struct ActiveCall {
    generation: u64,
    session: CallSession,
    /// Set only for an incoming call: the offer to answer or reject.
    offer: Option<Box<IncomingCall>>,
    shared: Arc<CallShared>,
    slots: Arc<VideoSlots>,
}

impl ActiveCall {
    /// The offer nobody answered with a runtime call yet: the only one that
    /// still needs rejecting, because there is no handle to terminate.
    fn unanswered_offer(&self) -> Option<Box<IncomingCall>> {
        self.offer
            .clone()
            .filter(|_| self.shared.handle().is_none())
    }
}

/// Whether ending a call with `signal` is our decision, so a call that never
/// got a handle has to be turned down rather than left to ring.
fn turns_offer_down(signal: CallSignal) -> bool {
    matches!(
        signal,
        CallSignal::LocalDeclined | CallSignal::LocalHangup | CallSignal::MediaFailed
    )
}

fn is_pending(id: &str) -> bool {
    id.starts_with(PENDING_ID)
}

impl Worker {
    fn call_reporter(&self, generation: u64) -> Reporter {
        Reporter::new(
            self.events.clone(),
            self.waker.clone(),
            self.commands.clone(),
            generation,
        )
    }

    fn new_video_slots(&self) -> Arc<VideoSlots> {
        let events = self.events.clone();
        let waker = self.waker.clone();
        Arc::new(VideoSlots::new(move || {
            let _ = events.send(Event::CallFrames);
            waker.wake();
        }))
    }

    fn emit_call(&self) {
        if let Some(call) = &self.calls.active {
            self.emit(Event::Call(call.session.clone()));
        }
    }

    pub(super) fn emit_active_call(&self) {
        if let Some(call) = &self.calls.active {
            self.emit(Event::CallVideo(call.slots.clone()));
            self.emit(Event::Call(call.session.clone()));
        }
    }

    /// The call a signal addresses. An outgoing call has no id until the
    /// runtime returns its handle, so until then it answers to its peer.
    fn addressed_call(&self, id: &str, from: &Jid) -> bool {
        let Some(call) = &self.calls.active else {
            return false;
        };
        call.session.id == id
            || (is_pending(&call.session.id)
                && call.session.is_outgoing()
                && self.canonical(from) == call.session.chat)
    }

    fn begin_call(&mut self, session: CallSession, offer: Option<Box<IncomingCall>>) {
        self.calls.generations += 1;
        let slots = self.new_video_slots();
        self.calls.active = Some(ActiveCall {
            generation: self.calls.generations,
            session,
            offer,
            shared: Arc::new(CallShared::default()),
            slots: slots.clone(),
        });
        self.emit(Event::CallVideo(slots));
        self.emit_call();
    }

    /// Applies a signal; a call that finished is filed and forgotten.
    fn signal_call(&mut self, signal: CallSignal) {
        let Some(call) = self.calls.active.as_mut() else {
            return;
        };
        let was_connected = matches!(call.session.phase, CallPhase::Connected { .. });
        if !call.session.apply(signal, crate::util::now()) {
            return;
        }
        if call.session.phase.is_finished() {
            self.finish_call(signal);
            return;
        }
        let connected = matches!(call.session.phase, CallPhase::Connected { .. });
        if connected && !was_connected && call.session.muted {
            call.shared.apply_mute(true);
        }
        self.emit_call();
    }

    fn finish_call(&mut self, signal: CallSignal) {
        let Some(call) = self.calls.active.take() else {
            return;
        };
        self.emit(Event::Call(call.session.clone()));
        let offer = self.rejection_of(&call, turns_offer_down(signal));
        teardown(call.shared.clone(), self.client.clone(), offer);
        self.record_finished(&call);
    }

    /// The offer to turn down when `reject` and the call has no handle yet.
    fn rejection_of(&self, call: &ActiveCall, reject: bool) -> Option<Box<IncomingCall>> {
        let offer = call.unanswered_offer().filter(|_| reject);
        #[cfg(test)]
        if offer.is_some() {
            self.calls
                .rejected
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(call.session.id.clone());
        }
        offer
    }

    pub(super) fn on_call_signal(&mut self, call: &IncomingCall) {
        match &call.action {
            CallAction::Offer {
                call_id,
                is_video,
                group_jid,
                ..
            } => self.on_call_offer(call, call_id, *is_video, group_jid.is_some()),
            CallAction::Accept { call_id, .. } if self.addressed_call(call_id, &call.from) => {
                self.signal_call(CallSignal::PeerAnswered);
            }
            CallAction::Reject {
                call_id, reason, ..
            } if reason.is_none() && self.addressed_call(call_id, &call.from) => {
                self.signal_call(CallSignal::PeerDeclined);
            }
            CallAction::Terminate {
                call_id, reason, ..
            } if self.addressed_call(call_id, &call.from) => {
                let elsewhere = matches!(
                    reason.as_deref(),
                    Some(TERMINATE_REASON_ACCEPTED_ELSEWHERE | TERMINATE_REASON_REJECTED_ELSEWHERE)
                );
                self.signal_call(if elsewhere {
                    CallSignal::HandledElsewhere
                } else {
                    CallSignal::PeerTerminated
                });
            }
            _ => {}
        }
    }

    fn on_call_offer(&mut self, call: &IncomingCall, call_id: &str, video: bool, group: bool) {
        if group || call.group.is_some() {
            self.calls.remember_offer(call_id, Offer::Group);
            log::debug!("group call ignored");
            return;
        }
        self.calls.remember_offer(call_id, Offer::Direct { video });
        if self
            .calls
            .active
            .as_ref()
            .is_some_and(|active| active.session.id == call_id)
        {
            return;
        }
        if self.calls.active.is_some() {
            // Left to ring: turning it down would tell the caller, and every
            // other device of ours, that the call was declined. The library
            // reports it as missed when the caller gives up.
            log::debug!("call offer while busy");
            return;
        }
        let chat = self.canonical(&call.from);
        self.ensure_chat(&chat, call.notify.as_deref());
        let session = CallSession::new(
            call_id.to_owned(),
            chat,
            CallDirection::Incoming,
            video,
            call.timestamp.timestamp(),
        );
        self.begin_call(session, Some(Box::new(call.clone())));
    }
    pub(super) fn on_call_elsewhere(&mut self, ended: &CallEndedElsewhere) {
        if self.addressed_call(&ended.call_id, &ended.from) {
            self.signal_call(CallSignal::HandledElsewhere);
        }
    }

    pub(super) fn start_call(&mut self, chat: ChatId, video: bool) {
        let Some(client) = self.client.clone().filter(|_| self.status.is_connected()) else {
            self.emit(Event::Info(gettext("Connect to WhatsApp to place a call")));
            return;
        };
        if self.calls.active.is_some() {
            self.emit(Event::Info(gettext("You are already in a call")));
            return;
        }
        let peer = Self::jid_of(&chat)
            .filter(|_| ChatKind::from_id(&chat) == ChatKind::Direct && !self.is_me(&chat));
        let Some(peer) = peer else {
            self.emit(Event::Info(gettext(
                "Calls are only available in one-to-one chats",
            )));
            return;
        };
        let session = CallSession::new(
            format!("{PENDING_ID}{}", self.calls.generations + 1),
            chat,
            CallDirection::Outgoing,
            video,
            crate::util::now(),
        );
        self.begin_call(session, None);
        let Some(call) = &self.calls.active else {
            return;
        };
        let (shared, generation, slots) =
            (call.shared.clone(), call.generation, call.slots.clone());
        let reporter = self.call_reporter(generation);
        tokio::spawn(async move {
            let dial = Dial::Place(peer);
            if let Err(message) =
                run_call(client, dial, video, slots, shared, reporter.clone()).await
            {
                reporter.failed(message);
            }
        });
    }

    pub(super) fn answer_call(&mut self) {
        let Some(client) = self.client.clone() else {
            self.emit(Event::Info(gettext("The call could not be answered")));
            return;
        };
        let Some(call) = self.calls.active.as_mut() else {
            return;
        };
        let Some(offer) = call.offer.clone() else {
            return;
        };
        if !call
            .session
            .apply(CallSignal::LocalAnswered, crate::util::now())
        {
            return;
        }
        let (shared, generation, video, slots) = (
            call.shared.clone(),
            call.generation,
            call.session.video,
            call.slots.clone(),
        );
        self.emit_call();
        let reporter = self.call_reporter(generation);
        tokio::spawn(async move {
            let dial = Dial::Answer(offer);
            if let Err(message) =
                run_call(client, dial, video, slots, shared, reporter.clone()).await
            {
                reporter.failed(message);
            }
        });
    }

    pub(super) fn decline_call(&mut self) {
        if self
            .calls
            .active
            .as_ref()
            .is_some_and(|call| call.session.phase == CallPhase::Ringing)
        {
            self.signal_call(CallSignal::LocalDeclined);
        }
    }

    /// Ends whatever the call is doing now: declines a ringing call, cancels a
    /// calling one, hangs up a connected one. The caller need not know which.
    pub(super) fn hang_up_call(&mut self) {
        match self.calls.active.as_ref().map(|call| call.session.phase) {
            Some(CallPhase::Ringing) => self.decline_call(),
            Some(_) => self.signal_call(CallSignal::LocalHangup),
            None => {}
        }
    }

    pub(super) fn set_call_muted(&mut self, muted: bool) {
        let Some(call) = self.calls.active.as_mut() else {
            return;
        };
        if call.session.phase.is_finished() || call.session.muted == muted {
            return;
        }
        call.session.muted = muted;
        call.shared.apply_mute(muted);
        self.emit_call();
    }

    /// Marks the camera as changing, unless it already is; returns the call's
    /// parts for the task that does the work.
    fn begin_camera_change(&mut self) -> Option<(Arc<CallShared>, Reporter)> {
        let call = self.calls.active.as_mut()?;
        if !matches!(call.session.phase, CallPhase::Connected { .. }) || call.session.camera_busy {
            return None;
        }
        call.session.camera_busy = true;
        let (shared, generation) = (call.shared.clone(), call.generation);
        self.emit_call();
        Some((shared, self.call_reporter(generation)))
    }

    pub(super) fn set_call_camera(&mut self, on: bool) {
        if self
            .calls
            .active
            .as_ref()
            .is_none_or(|call| call.session.camera_on == on)
        {
            return;
        }
        let Some((shared, reporter)) = self.begin_camera_change() else {
            return;
        };
        tokio::spawn(async move {
            if on {
                switch_camera_on(&shared, &reporter).await;
            } else {
                stop_camera(&shared).await;
                reporter.notice(CallNotice::Camera(false));
            }
            reporter.notice(CallNotice::CameraBusy(false));
        });
    }

    pub(super) fn accept_call_video(&mut self) {
        if !self
            .calls
            .active
            .as_ref()
            .is_some_and(|call| call.session.video_requested)
        {
            return;
        }
        let Some((shared, reporter)) = self.begin_camera_change() else {
            return;
        };
        tokio::spawn(async move {
            accept_video(&shared, &reporter).await;
            reporter.notice(CallNotice::CameraBusy(false));
        });
    }

    pub(super) fn on_call_notice(&mut self, generation: u64, notice: CallNotice) {
        let Some(call) = self
            .calls
            .active
            .as_mut()
            .filter(|call| call.generation == generation)
        else {
            return;
        };
        match notice {
            CallNotice::Placed(id) => {
                call.session.id = id;
                if call.session.muted {
                    call.shared.apply_mute(true);
                }
                self.emit_call();
            }
            CallNotice::MediaUp => {
                if let Some(controls) = call.shared.controls() {
                    controls.drain_microphone();
                }
                self.signal_call(CallSignal::MediaUp);
            }
            CallNotice::MediaFailed => self.signal_call(CallSignal::MediaFailed),
            CallNotice::Ended => self.signal_call(CallSignal::PeerTerminated),
            CallNotice::Camera(on) => {
                call.session.camera_on = on;
                call.session.video |= on;
                self.emit_call();
            }
            CallNotice::CameraBusy(busy) => {
                call.session.camera_busy = busy;
                self.emit_call();
            }
            CallNotice::CameraLost => {
                if call.session.camera_on {
                    call.session.camera_on = false;
                    self.emit(Event::Info(gettext("The camera stopped working")));
                    self.emit_call();
                }
            }
            CallNotice::DecoderLost => {
                if let Some(handle) = call.shared.handle() {
                    handle.request_peer_keyframe(KeyframeUrgency::Immediate);
                }
            }
            CallNotice::VideoStarted => {
                call.session.video = true;
                call.session.video_requested = false;
                self.emit_call();
            }
            CallNotice::PeerVideo(on) => {
                call.session.peer_video = on;
                call.session.video |= on;
                call.session.video_requested = false;
                self.emit_call();
            }
            CallNotice::VideoRequested => {
                call.session.video_requested = true;
                self.emit_call();
            }
            CallNotice::VideoRequestEnded => {
                call.session.video_requested = false;
                self.emit_call();
            }
        }
    }

    /// Ends the call for a logout, without waiting on the network.
    pub(super) fn abandon_call(&mut self) {
        self.signal_call(CallSignal::MediaFailed);
    }

    /// Hangs up before the connection closes with the window.
    pub(super) async fn end_call_for_shutdown(&mut self) {
        let Some(call) = self.calls.active.take() else {
            return;
        };
        call.shared.cancel();
        let offer = self.rejection_of(&call, true);
        let client = self.client.clone();
        let hangup = hang_up(&call.shared, client.as_deref(), offer.as_deref());
        let _ = tokio::time::timeout(SHUTDOWN_HANGUP, hangup).await;
    }
}
