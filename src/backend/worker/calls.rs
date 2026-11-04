//! Voice and video calls through whatsapp-rust's VoIP runtime.
//!
//! The worker owns the one call in progress: its state machine, its archive
//! event, and the tasks that drive the runtime. Audio and video devices live in
//! `crate::calls`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Mutex, MutexGuard};

use whatsapp_rust::types::call::{CallAction, CallEndedElsewhere, IncomingCall, MissedCall};
use whatsapp_rust::voip::{CallEvent, CallHandle, KeyframeUrgency, VideoState, VideoUpgradeToken};
use whatsapp_rust::wacore::stanza::call::{
    TERMINATE_REASON_ACCEPTED_ELSEWHERE, TERMINATE_REASON_REJECTED_ELSEWHERE,
};

use super::*;
use crate::backend::CallNotice;
use crate::calls::{
    CallControls, CallMedia, Fault, FaultHandler, MediaError, VideoEndpoints, VideoSlots,
};
use crate::i18n::gettext;
use crate::model::call::{CallDirection, CallOutcome, CallPhase, CallSession, CallSignal};

const CALL_HISTORY_LIMIT: usize = 200;
const CALLS_SEEN_KEY: &str = "calls_seen_at";
const PENDING_ID: &str = "pending-";
/// Missed calls replayed after a reconnect are only worth a notification while fresh.
const FRESH_MISSED_CALL: i64 = 10 * 60;
const SHUTDOWN_HANGUP: Duration = Duration::from_secs(2);
/// How long ending a call may wait on the network before giving up on it.
const TERMINATE_TIMEOUT: Duration = Duration::from_secs(5);
const KNOWN_OFFERS: usize = 32;

#[derive(Default)]
pub(super) struct Calls {
    active: Option<ActiveCall>,
    generations: u64,
    history_dirty: bool,
    /// Offers seen lately, so a missed-call event, which carries only the
    /// caller and the id, can be told from a group call and given its kind.
    offers: VecDeque<(String, Offer)>,
    #[cfg(test)]
    rejected: Vec<String>,
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
}

struct ActiveCall {
    generation: u64,
    session: CallSession,
    offer: Option<Box<IncomingCall>>,
    shared: Arc<CallShared>,
    slots: Arc<VideoSlots>,
}

/// What the tasks driving a call share with the worker.
#[derive(Default)]
struct CallShared {
    handle: Mutex<Option<CallHandle>>,
    media: Mutex<Option<CallMedia>>,
    controls: Mutex<Option<CallControls>>,
    upgrade: Mutex<Option<VideoUpgradeToken>>,
    cancelled: AtomicBool,
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl CallShared {
    fn handle(&self) -> Option<CallHandle> {
        guard(&self.handle).clone()
    }

    fn controls(&self) -> Option<CallControls> {
        guard(&self.controls).clone()
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(AtomicOrdering::Acquire)
    }

    /// Keeps the call's devices and their controls.
    fn store_media(&self, media: CallMedia) {
        *guard(&self.controls) = Some(media.controls());
        *guard(&self.media) = Some(media);
    }

    /// Releases the devices off the async runtime, so their lights go out.
    async fn release_media(self: &Arc<Self>) {
        let shared = self.clone();
        let _ = tokio::task::spawn_blocking(move || {
            *guard(&shared.controls) = None;
            drop(guard(&shared.media).take());
        })
        .await;
    }

    fn keyframe(&self) {
        if let Some(controls) = self.controls() {
            tokio::task::spawn_blocking(move || controls.request_keyframe());
        }
    }

    fn apply_mute(&self, muted: bool) {
        if let Some(controls) = self.controls() {
            tokio::task::spawn_blocking(move || controls.set_muted(muted));
        }
        if let Some(handle) = self.handle() {
            tokio::spawn(async move {
                if handle.set_muted(muted).await.is_err() {
                    log::debug!("mute state not announced");
                }
            });
        }
    }
}

/// Lets the tasks driving a call talk back to the worker and the window.
#[derive(Clone)]
struct Reporter {
    events: std::sync::mpsc::Sender<Event>,
    waker: Arc<dyn Wake>,
    commands: mpsc::UnboundedSender<Command>,
    generation: u64,
}

impl Reporter {
    fn notice(&self, notice: CallNotice) {
        let _ = self.commands.send(Command::CallNotice {
            generation: self.generation,
            notice,
        });
    }

    /// Tells the person in words that are always safe to show.
    fn error(&self, text: String) {
        let _ = self.events.send(Event::Info(text));
        self.waker.wake();
    }

    fn faults(&self) -> FaultHandler {
        let reporter = self.clone();
        Arc::new(move |fault| match fault {
            Fault::Audio => reporter.notice(CallNotice::MediaFailed),
            Fault::Camera => reporter.notice(CallNotice::CameraLost),
            Fault::Decoder => reporter.notice(CallNotice::DecoderLost),
        })
    }
}

struct Prepared {
    media: CallMedia,
    video: Option<VideoEndpoints>,
    camera: bool,
}

/// Opens devices off the async runtime. `camera_required` fails the call when
/// the camera cannot start; otherwise the call goes ahead without a picture.
async fn prepare_media(
    slots: Arc<VideoSlots>,
    faults: FaultHandler,
    video: bool,
    camera_required: bool,
) -> Result<Prepared, MediaError> {
    tokio::task::spawn_blocking(move || {
        if video {
            CallMedia::check_video()?;
        }
        let mut media = CallMedia::start(slots, faults)?;
        if !video {
            return Ok(Prepared {
                media,
                video: None,
                camera: false,
            });
        }
        match media.start_video(true) {
            Ok(endpoints) => Ok(Prepared {
                media,
                video: Some(endpoints),
                camera: true,
            }),
            Err(MediaError::NoCamera) if !camera_required => {
                let endpoints = media.start_video(false)?;
                Ok(Prepared {
                    media,
                    video: Some(endpoints),
                    camera: false,
                })
            }
            Err(error) => Err(error),
        }
    })
    .await
    .unwrap_or_else(|_| Err(MediaError::Unavailable("media task failed".to_owned())))
}

fn media_notice(event: &CallEvent) -> Option<CallNotice> {
    match event {
        CallEvent::RelayAllocated => Some(CallNotice::MediaUp),
        CallEvent::RelayAllocateFailed(_)
        | CallEvent::RelayAllocateTimedOut
        | CallEvent::RelayReconnectTimedOut
        | CallEvent::MediaSetupFailed(_)
        | CallEvent::AudioFormatMismatch { .. } => Some(CallNotice::MediaFailed),
        _ => None,
    }
}

fn peer_video_notice(state: VideoState) -> Option<CallNotice> {
    match state {
        VideoState::UpgradeRequest | VideoState::UpgradeRequestV2 => {
            Some(CallNotice::VideoRequested)
        }
        VideoState::Enabled | VideoState::UpgradeAccept => Some(CallNotice::PeerVideo(true)),
        VideoState::Disabled | VideoState::Paused | VideoState::Stopped | VideoState::Error => {
            Some(CallNotice::PeerVideo(false))
        }
        VideoState::UpgradeCancel
        | VideoState::UpgradeCancelByTimeout
        | VideoState::UpgradeRejectByTimeout => Some(CallNotice::VideoRequestEnded),
        _ => None,
    }
}

fn forward(event: &CallEvent, shared: &CallShared, reporter: &Reporter) {
    match event {
        CallEvent::PeerVideoStateChanged {
            state,
            upgrade_token,
            ..
        } => {
            if let Some(token) = upgrade_token {
                *guard(&shared.upgrade) = Some(*token);
            }
            if let Some(notice) = peer_video_notice(*state) {
                reporter.notice(notice);
            }
        }
        CallEvent::VideoKeyframeNeeded => shared.keyframe(),
        CallEvent::MediaSetupFailed(_) => {
            log::debug!("call media setup failed");
            reporter.notice(CallNotice::MediaFailed);
        }
        other => {
            if let Some(notice) = media_notice(other) {
                reporter.notice(notice);
            }
        }
    }
}

/// Keeps the handle, and follows the runtime's events until the call ends.
async fn adopt(handle: CallHandle, shared: Arc<CallShared>, reporter: Reporter) {
    *guard(&shared.handle) = Some(handle.clone());
    reporter.notice(CallNotice::Placed(handle.call_id().to_owned()));
    if shared.is_cancelled() {
        end_handle(&handle).await;
        return;
    }
    tokio::spawn(async move {
        let events = handle.events();
        let ended = handle.wait_ended();
        tokio::pin!(ended);
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Ok(event) => forward(&event, &shared, &reporter),
                    Err(_) => break,
                },
                () = &mut ended => break,
            }
        }
        reporter.notice(CallNotice::Ended);
    });
}

/// Ends the runtime side of a call, without waiting on a bad network forever.
async fn end_handle(handle: &CallHandle) {
    if tokio::time::timeout(TERMINATE_TIMEOUT, handle.terminate())
        .await
        .is_err()
    {
        log::debug!("call end not confirmed in time");
    }
}

/// Ends the runtime side of a call and releases its devices at once. Devices
/// are freed alongside the network exchange, never after it, so the microphone
/// and camera go quiet even when the network is slow. `reject` is the offer to
/// turn down when the call never got a handle to terminate.
fn teardown(
    shared: Arc<CallShared>,
    client: Option<Arc<Client>>,
    reject: Option<Box<IncomingCall>>,
) {
    shared.cancelled.store(true, AtomicOrdering::Release);
    tokio::spawn(async move {
        let release = shared.release_media();
        let hangup = async {
            if let Some(handle) = shared.handle() {
                end_handle(&handle).await;
            } else if let (Some(client), Some(offer)) = (client, reject) {
                reject_offer(&client, &offer).await;
            }
        };
        tokio::join!(release, hangup);
    });
}

async fn reject_offer(client: &Client, offer: &IncomingCall) {
    if tokio::time::timeout(TERMINATE_TIMEOUT, client.voip().reject(offer))
        .await
        .is_err()
    {
        log::debug!("call rejection not confirmed in time");
    }
}

/// Keeps the prepared devices, unless the call was ended while they opened, in
/// which case they are released here, because nothing else knows about them.
async fn adopt_media(shared: &Arc<CallShared>, media: CallMedia) -> bool {
    shared.store_media(media);
    if shared.is_cancelled() {
        shared.release_media().await;
        return false;
    }
    true
}

async fn place_call(
    client: Arc<Client>,
    peer: Jid,
    video: bool,
    slots: Arc<VideoSlots>,
    shared: Arc<CallShared>,
    reporter: Reporter,
) -> Result<(), String> {
    let prepared = prepare_media(slots, reporter.faults(), video, true)
        .await
        .map_err(|error| error.to_string());
    if shared.is_cancelled() {
        if let Ok(prepared) = prepared {
            drop_off_runtime(prepared.media).await;
        }
        return Ok(());
    }
    let prepared = prepared?;
    let (microphone, speaker) = (prepared.media.microphone(), prepared.media.speaker());
    let camera = prepared.camera;
    if !adopt_media(&shared, prepared.media).await {
        return Ok(());
    }
    reporter.notice(CallNotice::Camera(camera));
    let voip = client.voip();
    let mut builder = voip.call(&peer).audio(microphone, speaker);
    if let Some(endpoints) = prepared.video {
        builder = builder.video(endpoints.source, endpoints.sink);
    }
    match builder.start().await {
        Ok(handle) => {
            adopt(handle, shared, reporter).await;
            Ok(())
        }
        Err(_) if shared.is_cancelled() => Ok(()),
        Err(_) => {
            log::warn!("call offer failed");
            Err(gettext("The call could not be placed"))
        }
    }
}

async fn answer_offer(
    client: Arc<Client>,
    offer: Box<IncomingCall>,
    video: bool,
    slots: Arc<VideoSlots>,
    shared: Arc<CallShared>,
    reporter: Reporter,
) -> Result<(), String> {
    let prepared = prepare_media(slots, reporter.faults(), video, false)
        .await
        .map_err(|error| error.to_string());
    if shared.is_cancelled() {
        if let Ok(prepared) = prepared {
            drop_off_runtime(prepared.media).await;
        }
        return Ok(());
    }
    let prepared = prepared?;
    let (microphone, speaker) = (prepared.media.microphone(), prepared.media.speaker());
    let camera = prepared.camera;
    if !adopt_media(&shared, prepared.media).await {
        return Ok(());
    }
    reporter.notice(CallNotice::Camera(camera));
    let voip = client.voip();
    let mut builder = voip.accept(&offer).audio(microphone, speaker);
    if let Some(endpoints) = prepared.video {
        builder = builder.video(endpoints.source, endpoints.sink);
    }
    match builder.start().await {
        Ok(handle) => {
            adopt(handle, shared, reporter).await;
            Ok(())
        }
        Err(_) if shared.is_cancelled() => Ok(()),
        Err(_) => {
            log::warn!("call answer failed");
            Err(gettext("The call could not be answered"))
        }
    }
}

async fn drop_off_runtime(media: CallMedia) {
    let _ = tokio::task::spawn_blocking(move || drop(media)).await;
}

fn is_pending(id: &str) -> bool {
    id.starts_with(PENDING_ID)
}

impl Worker {
    fn call_reporter(&self, generation: u64) -> Reporter {
        Reporter {
            events: self.events.clone(),
            waker: self.waker.clone(),
            commands: self.commands.clone(),
            generation,
        }
    }

    fn new_video_slots(&self) -> Arc<VideoSlots> {
        let events = self.events.clone();
        let waker = self.waker.clone();
        Arc::new(VideoSlots::new(move || {
            let _ = events.send(Event::CallFrames);
            waker.wake();
        }))
    }

    pub(super) fn emit_calls(&self) {
        match self.archive.calls(CALL_HISTORY_LIMIT) {
            Ok(entries) => self.emit(Event::Calls {
                entries,
                seen_at: self.calls_seen_at(),
            }),
            Err(error) => log::warn!("could not list calls: {error}"),
        }
    }

    fn calls_seen_at(&self) -> i64 {
        if let Ok(Some(stored)) = self.archive.meta(CALLS_SEEN_KEY)
            && let Ok(seen_at) = stored.parse()
        {
            return seen_at;
        }
        let now = crate::util::now();
        let _ = self.archive.set_meta(CALLS_SEEN_KEY, &now.to_string());
        now
    }

    pub(super) fn mark_calls_seen(&self) {
        let _ = self
            .archive
            .set_meta(CALLS_SEEN_KEY, &crate::util::now().to_string());
        self.emit_calls();
    }

    pub(super) fn flush_call_history(&mut self) {
        if std::mem::take(&mut self.calls.history_dirty) {
            self.emit_calls();
        }
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
        let rejects = call.offer.is_some()
            && call.shared.handle().is_none()
            && matches!(
                signal,
                CallSignal::LocalDeclined | CallSignal::LocalHangup | CallSignal::MediaFailed
            );
        self.leave_call(&call, rejects);
        self.record_finished(&call);
    }

    /// Releases a call's devices and tells the network, rejecting the offer
    /// when we turned it down before the runtime had a call to end.
    fn leave_call(&mut self, call: &ActiveCall, reject: bool) {
        let offer = call.offer.clone().filter(|_| reject);
        #[cfg(test)]
        if offer.is_some() {
            self.calls.rejected.push(call.session.id.clone());
        }
        teardown(call.shared.clone(), self.client.clone(), offer);
    }

    fn record_finished(&mut self, call: &ActiveCall) {
        let session = &call.session;
        let outcome = match session.phase {
            CallPhase::Ended(outcome) => outcome,
            CallPhase::Failed if session.talk_seconds.is_some() => CallOutcome::Completed,
            CallPhase::Failed => CallOutcome::Failed,
            _ => return,
        };
        if !outcome.is_recordable() {
            return;
        }
        let mut session = session.clone();
        if is_pending(&session.id) {
            if outcome != CallOutcome::Failed {
                return;
            }
            session.id = format!("local-{}-{}", session.started_at, call.generation);
        }
        self.record_call(&session, outcome);
    }

    fn record_call(&mut self, session: &CallSession, outcome: CallOutcome) {
        let message = self.call_message(session, outcome);
        self.file_call(
            message,
            outcome == CallOutcome::Missed && !session.is_outgoing(),
        );
    }

    /// Files a call event in its chat. Only a fresh missed call counts as
    /// unread and notifies; history stays quiet.
    fn file_call(&mut self, message: Message, notify: bool) {
        if notify {
            self.store_message(message, None, None);
        } else {
            let chat = message.chat.clone();
            self.ensure_chat(&chat, None);
            if let Err(error) = self.archive.insert_call(&message) {
                log::warn!("could not store a call: {error}");
                return;
            }
            let mut stored = self
                .archive
                .message(&chat, &message.id)
                .ok()
                .flatten()
                .unwrap_or(message);
            self.polish(&mut stored);
            self.emit(Event::Messages {
                chat: chat.clone(),
                messages: vec![stored],
                older: false,
                complete: false,
            });
            self.emit_chat(&chat);
        }
        self.calls.history_dirty = true;
        if !self.syncing {
            self.flush_call_history();
        }
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

    pub(super) fn on_missed_call(&mut self, missed: &MissedCall) {
        if self.addressed_call(&missed.call_id, &missed.from) {
            self.signal_call(CallSignal::PeerTerminated);
            return;
        }
        let chat = self.canonical(&missed.from);
        let video = match self.calls.offer(&missed.call_id) {
            Some(Offer::Group) => return,
            Some(Offer::Direct { video }) => video,
            None => false,
        };
        if ChatKind::from_id(&chat) != ChatKind::Direct {
            return;
        }
        self.ensure_chat(&chat, None);
        let timestamp = missed.timestamp.timestamp();
        let mut session = CallSession::new(
            missed.call_id.clone(),
            chat,
            CallDirection::Incoming,
            video,
            timestamp,
        );
        session.apply(CallSignal::PeerTerminated, timestamp);
        let fresh = crate::util::now() - timestamp < FRESH_MISSED_CALL;
        let message = self.call_message(&session, CallOutcome::Missed);
        self.file_call(message, fresh);
    }

    fn call_message(&self, session: &CallSession, outcome: CallOutcome) -> Message {
        let outgoing = session.is_outgoing();
        Message {
            id: session.id.clone(),
            chat: session.chat.clone(),
            sender: if outgoing {
                self.me()
            } else {
                session.chat.clone()
            },
            sender_name: None,
            from_me: outgoing,
            timestamp: session.started_at,
            content: Content::Call {
                video: session.video,
                outgoing,
                outcome,
                seconds: session.talk_seconds,
            },
            status: Delivery::None,
            delivered_at: None,
            read_at: None,
            quoted: None,
            reactions: Vec::new(),
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: None,
        }
    }

    pub(super) fn on_call_elsewhere(&mut self, ended: &CallEndedElsewhere) {
        if self.addressed_call(&ended.call_id, &ended.from) {
            self.signal_call(CallSignal::HandledElsewhere);
        }
    }

    /// Files a call the phone placed or took, from its synced call history.
    pub(super) fn on_call_log(&mut self, sync: &wa_events::CallLogSync) {
        use wa::call_log_record::CallResult;
        let record = &sync.record;
        if record.group_jid.is_some() || record.is_call_link == Some(true) {
            return;
        }
        let outcome = match record.call_result {
            Some(CallResult::CONNECTED) => CallOutcome::Completed,
            Some(CallResult::REJECTED) => CallOutcome::Declined,
            Some(CallResult::CANCELLED) => CallOutcome::Cancelled,
            Some(CallResult::MISSED) => CallOutcome::Missed,
            Some(CallResult::FAILED) => CallOutcome::Failed,
            _ => return,
        };
        let peer = if sync.from_me {
            record
                .participants
                .iter()
                .filter_map(|participant| participant.user_jid.as_deref())
                .filter_map(|jid| jid.parse::<Jid>().ok())
                .map(|jid| self.canonical(&jid))
                .find(|id| !self.is_me(id))
        } else {
            Some(self.canonical(&sync.call_creator_jid))
        };
        let Some(chat) =
            peer.filter(|chat| !self.is_me(chat) && ChatKind::from_id(chat) == ChatKind::Direct)
        else {
            return;
        };
        let started_at = record
            .start_time
            .filter(|time| *time > 0)
            .unwrap_or_else(|| sync.timestamp.timestamp());
        let mut session = CallSession::new(
            sync.call_id.clone(),
            chat,
            if sync.from_me {
                CallDirection::Outgoing
            } else {
                CallDirection::Incoming
            },
            record.is_video.unwrap_or(false),
            started_at,
        );
        session.talk_seconds = (outcome == CallOutcome::Completed)
            .then(|| record.duration.unwrap_or(0).clamp(0, i64::from(u32::MAX)) as u32);
        let message = self.call_message(&session, outcome);
        self.file_call(message, false);
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
            if let Err(message) =
                place_call(client, peer, video, slots, shared, reporter.clone()).await
            {
                reporter.error(message);
                reporter.notice(CallNotice::MediaFailed);
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
            if let Err(message) =
                answer_offer(client, offer, video, slots, shared, reporter.clone()).await
            {
                reporter.error(message);
                reporter.notice(CallNotice::MediaFailed);
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
        call.shared.cancelled.store(true, AtomicOrdering::Release);
        let rejects = call.offer.is_some() && call.shared.handle().is_none();
        #[cfg(test)]
        if rejects {
            self.calls.rejected.push(call.session.id.clone());
        }
        let client = self.client.clone();
        let release = call.shared.release_media();
        let hangup = async {
            if let Some(handle) = call.shared.handle() {
                end_handle(&handle).await;
            } else if let (Some(client), Some(offer), true) =
                (client, call.offer.as_deref(), rejects)
            {
                reject_offer(&client, offer).await;
            }
        };
        let _ =
            tokio::time::timeout(SHUTDOWN_HANGUP, async { tokio::join!(release, hangup) }).await;
    }
}

async fn switch_camera_on(shared: &Arc<CallShared>, reporter: &Reporter) {
    match open_video(shared, true).await {
        Ok(endpoints) => {
            let Some(handle) = shared.handle() else {
                stop_camera(shared).await;
                return;
            };
            let VideoEndpoints { source, sink } = endpoints;
            let result = match handle.resume_video(source.clone(), sink.clone()).await {
                Ok(()) => Ok(()),
                Err(_) => handle.start_video(source, sink).await,
            };
            match result {
                Ok(()) => reporter.notice(CallNotice::Camera(true)),
                Err(_) => {
                    log::warn!("video not started");
                    stop_camera(shared).await;
                    reporter.error(gettext("Video could not be started"));
                }
            }
        }
        Err(error) => reporter.error(error.to_string()),
    }
}

async fn accept_video(shared: &Arc<CallShared>, reporter: &Reporter) {
    let (Some(handle), Some(token)) = (shared.handle(), *guard(&shared.upgrade)) else {
        reporter.notice(CallNotice::VideoRequestEnded);
        return;
    };
    let (endpoints, camera) = match open_video(shared, true).await {
        Ok(endpoints) => (endpoints, true),
        Err(_) => match open_video(shared, false).await {
            Ok(endpoints) => (endpoints, false),
            Err(error) => {
                reporter.error(error.to_string());
                return;
            }
        },
    };
    let VideoEndpoints { source, sink } = endpoints;
    match handle.accept_video(token, source, sink).await {
        Ok(()) => {
            reporter.notice(CallNotice::Camera(camera));
            reporter.notice(CallNotice::VideoStarted);
        }
        Err(_) => {
            log::warn!("video not accepted");
            stop_camera(shared).await;
            reporter.notice(CallNotice::VideoRequestEnded);
            reporter.error(gettext("Video could not be started"));
        }
    }
}

/// Opens the decoder, and the camera when asked, without holding the call's
/// media while the devices start: the media lock is taken only to read what is
/// needed and to keep the result.
async fn open_video(shared: &Arc<CallShared>, camera: bool) -> Result<VideoEndpoints, MediaError> {
    let shared = shared.clone();
    tokio::task::spawn_blocking(move || {
        let gone = || MediaError::Unavailable("no call media".to_owned());
        let (plan, sink) = {
            let mut media = guard(&shared.media);
            let media = media.as_mut().ok_or_else(gone)?;
            (media.plan_video()?, media.decoder_sender())
        };
        let sink = match sink {
            Some(sink) => sink,
            None => {
                let decoder = plan.open_decoder()?;
                guard(&shared.media)
                    .as_mut()
                    .ok_or_else(gone)?
                    .attach_decoder(decoder)
            }
        };
        let source = if camera {
            let opened = plan.open_camera()?;
            guard(&shared.media)
                .as_mut()
                .ok_or_else(gone)?
                .attach_camera(opened)
        } else {
            async_channel::bounded(1).1
        };
        Ok(VideoEndpoints { source, sink })
    })
    .await
    .unwrap_or_else(|_| Err(MediaError::Unavailable("media task failed".to_owned())))
}

async fn stop_camera(shared: &Arc<CallShared>) {
    {
        let shared = shared.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Some(media) = guard(&shared.media).as_mut() {
                media.stop_camera();
            }
        })
        .await;
    }
    if let Some(handle) = shared.handle() {
        let _ = handle.stop_video().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::worker::tests::worker;
    use whatsapp_rust::wacore::types::events::CallLogSync;

    const PEER: &str = "15550002222@s.whatsapp.net";

    fn jid(id: &str) -> Jid {
        id.parse().unwrap()
    }

    fn incoming(action: CallAction) -> IncomingCall {
        IncomingCall::new_for_test(
            jid(PEER),
            "stanza".into(),
            whatsapp_rust::wacore::chrono::Utc::now(),
            action,
        )
    }

    fn offer(id: &str, video: bool) -> IncomingCall {
        incoming(CallAction::Offer {
            call_id: id.into(),
            call_creator: jid(PEER),
            caller_pn: None,
            caller_country_code: None,
            device_class: None,
            joinable: false,
            is_video: video,
            audio: Vec::new(),
            group_jid: None,
        })
    }

    fn terminate(id: &str, reason: Option<&str>) -> IncomingCall {
        incoming(CallAction::Terminate {
            call_id: id.into(),
            call_creator: jid(PEER),
            reason: reason.map(str::to_owned),
            duration: None,
            audio_duration: None,
        })
    }

    fn calls_in(events: &std::sync::mpsc::Receiver<Event>) -> Vec<CallSession> {
        events
            .try_iter()
            .filter_map(|event| match event {
                Event::Call(session) => Some(session),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn an_incoming_offer_rings_and_an_unanswered_hangup_files_a_missed_call() {
        let (mut worker, events, _, _) = worker();
        worker.on_call_signal(&offer("CALL1", true));
        let ringing = calls_in(&events);
        assert_eq!(ringing.len(), 1);
        assert_eq!(ringing[0].phase, CallPhase::Ringing);
        assert_eq!(ringing[0].chat, PEER);
        assert!(ringing[0].video);

        worker.on_call_signal(&terminate("CALL1", None));
        let ended = calls_in(&events);
        assert_eq!(ended[0].phase, CallPhase::Ended(CallOutcome::Missed));
        assert!(worker.calls.active.is_none());

        let stored = worker.archive.message(PEER, "CALL1").unwrap().unwrap();
        assert!(!stored.from_me);
        assert_eq!(
            stored.content,
            Content::Call {
                video: true,
                outgoing: false,
                outcome: CallOutcome::Missed,
                seconds: None
            }
        );
        assert_eq!(worker.archive.chat(PEER).unwrap().unwrap().unread, 1);
        assert_eq!(worker.archive.calls(10).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn answering_elsewhere_dismisses_the_ring_without_filing_anything() {
        let (mut worker, events, _, _) = worker();
        worker.on_call_signal(&offer("CALL2", false));
        let _ = calls_in(&events);
        worker.on_call_signal(&terminate("CALL2", Some("accepted_elsewhere")));
        let ended = calls_in(&events);
        assert_eq!(ended[0].phase, CallPhase::Ended(CallOutcome::Elsewhere));
        assert!(worker.archive.message(PEER, "CALL2").unwrap().is_none());
    }

    #[tokio::test]
    async fn declining_files_a_declined_call_without_marking_the_chat_unread() {
        let (mut worker, events, _, _) = worker();
        worker.on_call_signal(&offer("CALL3", false));
        let _ = calls_in(&events);
        worker.decline_call();
        let ended = calls_in(&events);
        assert_eq!(ended[0].phase, CallPhase::Ended(CallOutcome::Declined));
        let stored = worker.archive.message(PEER, "CALL3").unwrap().unwrap();
        assert!(matches!(
            stored.content,
            Content::Call {
                outcome: CallOutcome::Declined,
                ..
            }
        ));
        assert_eq!(worker.archive.chat(PEER).unwrap().unwrap().unread, 0);
    }

    #[tokio::test]
    async fn a_second_offer_while_busy_is_left_alone_and_filed_when_the_library_reports_it_missed()
    {
        let (mut worker, events, _, _) = worker();
        worker.on_call_signal(&offer("FIRST", false));
        let _ = calls_in(&events);
        let other_peer = "15550003333@s.whatsapp.net";
        let mut other = offer("SECOND", true);
        other.from = jid(other_peer);
        worker.on_call_signal(&other);
        assert_eq!(
            worker
                .calls
                .active
                .as_ref()
                .map(|call| call.session.id.as_str()),
            Some("FIRST")
        );
        assert!(
            worker.calls.rejected.is_empty(),
            "turning it down would tell the caller it was declined"
        );
        assert!(
            worker
                .archive
                .message(other_peer, "SECOND")
                .unwrap()
                .is_none()
        );

        let missed = MissedCall::new(
            jid(other_peer),
            "SECOND".into(),
            whatsapp_rust::wacore::chrono::Utc::now(),
            whatsapp_rust::types::call::MissedReason::Remote,
        );
        worker.on_missed_call(&missed);
        let stored = worker
            .archive
            .message(other_peer, "SECOND")
            .unwrap()
            .unwrap();
        assert!(matches!(
            stored.content,
            Content::Call {
                video: true,
                outcome: CallOutcome::Missed,
                ..
            }
        ));
    }

    fn answer_in_place(worker: &mut Worker) {
        let call = worker.calls.active.as_mut().expect("a call is ringing");
        assert!(
            call.session
                .apply(CallSignal::LocalAnswered, crate::util::now())
        );
    }

    #[tokio::test]
    async fn hanging_up_while_the_answer_is_still_connecting_rejects_the_offer() {
        let (mut worker, events, _, _) = worker();
        worker.on_call_signal(&offer("CONNECTING", false));
        let _ = calls_in(&events);
        answer_in_place(&mut worker);
        let shared = worker.calls.active.as_ref().unwrap().shared.clone();

        worker.hang_up_call();

        assert_eq!(worker.calls.rejected, ["CONNECTING"]);
        assert!(shared.is_cancelled());
        assert!(worker.calls.active.is_none());
        let ended = calls_in(&events);
        assert_eq!(ended[0].phase, CallPhase::Ended(CallOutcome::Declined));
    }

    #[tokio::test]
    async fn closing_the_window_while_connecting_hangs_up_whatever_the_phase_is() {
        let (mut worker, events, _, _) = worker();
        worker.on_call_signal(&offer("CLOSING", false));
        let _ = calls_in(&events);
        worker.decline_call();
        let _ = calls_in(&events);
        assert_eq!(worker.calls.rejected, ["CLOSING"]);

        worker.on_call_signal(&offer("CLOSING2", false));
        let _ = calls_in(&events);
        answer_in_place(&mut worker);
        worker.decline_call();
        assert!(
            worker.calls.active.is_some(),
            "a stale decline does nothing once the call is answered"
        );
        worker.hang_up_call();
        assert!(worker.calls.active.is_none());
        assert_eq!(worker.calls.rejected, ["CLOSING", "CLOSING2"]);
    }

    #[tokio::test]
    async fn a_peer_ending_the_call_never_rejects_it() {
        let (mut worker, events, _, _) = worker();
        worker.on_call_signal(&offer("PEER", false));
        let _ = calls_in(&events);
        answer_in_place(&mut worker);
        worker.on_call_signal(&terminate("PEER", None));
        assert!(worker.calls.rejected.is_empty());
        let _ = calls_in(&events);
        worker.on_call_signal(&offer("ELSEWHERE", false));
        worker.on_call_signal(&terminate("ELSEWHERE", Some("accepted_elsewhere")));
        assert!(worker.calls.rejected.is_empty());
    }

    fn outgoing_pending(worker: &mut Worker) -> Arc<CallShared> {
        let session = CallSession::new(
            format!("{PENDING_ID}1"),
            PEER.into(),
            CallDirection::Outgoing,
            false,
            crate::util::now(),
        );
        worker.begin_call(session, None);
        worker.calls.active.as_ref().unwrap().shared.clone()
    }

    #[tokio::test]
    async fn cancelling_before_the_runtime_returns_a_handle_cancels_the_setup_quietly() {
        let (mut worker, events, _, _) = worker();
        let shared = outgoing_pending(&mut worker);
        let _ = calls_in(&events);

        worker.hang_up_call();

        assert!(
            shared.is_cancelled(),
            "the setup task sees the cancellation"
        );
        assert!(
            worker.calls.rejected.is_empty(),
            "there is no offer to reject"
        );
        let ended = calls_in(&events);
        assert_eq!(ended[0].phase, CallPhase::Ended(CallOutcome::Cancelled));
        assert!(worker.archive.calls(10).unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_call_that_failed_to_set_up_is_still_recorded() {
        let (mut worker, events, _, _) = worker();
        outgoing_pending(&mut worker);
        let _ = calls_in(&events);
        worker.on_call_notice(1, CallNotice::MediaFailed);
        let history = worker.archive.calls(10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].outcome, CallOutcome::Failed);
        assert!(history[0].outgoing);
    }

    #[tokio::test]
    async fn a_call_that_dropped_after_connecting_is_recorded_with_its_talk_time() {
        let (mut worker, events, _, _) = worker();
        worker.on_call_signal(&offer("DROPPED", false));
        let _ = calls_in(&events);
        answer_in_place(&mut worker);
        worker.on_call_notice(1, CallNotice::MediaUp);
        {
            let call = worker.calls.active.as_mut().unwrap();
            call.session.phase = CallPhase::Connected {
                since: crate::util::now() - 42,
            };
        }
        worker.on_call_notice(1, CallNotice::MediaFailed);
        let history = worker.archive.calls(10).unwrap();
        assert_eq!(history[0].outcome, CallOutcome::Completed);
        assert!(history[0].seconds.is_some_and(|seconds| seconds >= 42));
    }

    #[tokio::test]
    async fn shutting_down_while_connecting_rejects_the_offer_and_stops_the_setup() {
        let (mut worker, events, _, _) = worker();
        worker.on_call_signal(&offer("SHUTDOWN", false));
        let _ = calls_in(&events);
        answer_in_place(&mut worker);
        let shared = worker.calls.active.as_ref().unwrap().shared.clone();

        worker.end_call_for_shutdown().await;

        assert_eq!(worker.calls.rejected, ["SHUTDOWN"]);
        assert!(shared.is_cancelled());
        assert!(worker.calls.active.is_none());
    }

    #[tokio::test]
    async fn a_camera_change_is_refused_while_another_is_in_flight() {
        let (mut worker, events, _, _) = worker();
        worker.on_call_signal(&offer("CAMERA", true));
        let _ = calls_in(&events);
        answer_in_place(&mut worker);
        worker.on_call_notice(1, CallNotice::MediaUp);
        assert!(worker.begin_camera_change().is_some());
        assert!(
            worker.begin_camera_change().is_none(),
            "a second change waits for the first"
        );
        worker.on_call_notice(1, CallNotice::CameraBusy(false));
        assert!(worker.begin_camera_change().is_some());
    }

    #[tokio::test]
    async fn a_missed_event_for_a_group_call_is_not_filed_as_a_one_to_one_call() {
        let (mut worker, events, _, _) = worker();
        let mut group = offer("GROUPMISS", false);
        if let CallAction::Offer { group_jid, .. } = &mut group.action {
            *group_jid = Some(jid("120363000000000000@g.us"));
        }
        worker.on_call_signal(&group);
        let _ = calls_in(&events);
        let missed = MissedCall::new(
            jid(PEER),
            "GROUPMISS".into(),
            whatsapp_rust::wacore::chrono::Utc::now(),
            whatsapp_rust::types::call::MissedReason::Remote,
        );
        worker.on_missed_call(&missed);
        assert!(worker.archive.calls(10).unwrap().is_empty());
    }

    #[test]
    fn remembered_offers_are_bounded_and_replace_by_id() {
        let mut calls = Calls::default();
        calls.remember_offer("a", Offer::Direct { video: false });
        calls.remember_offer("a", Offer::Direct { video: true });
        assert_eq!(calls.offer("a"), Some(Offer::Direct { video: true }));
        for index in 0..KNOWN_OFFERS + 4 {
            calls.remember_offer(&format!("id{index}"), Offer::Group);
        }
        assert_eq!(calls.offers.len(), KNOWN_OFFERS);
        assert_eq!(calls.offer("a"), None);
    }

    #[tokio::test]
    async fn group_call_offers_do_not_ring() {
        let (mut worker, events, _, _) = worker();
        let mut group = offer("GROUP", false);
        if let CallAction::Offer { group_jid, .. } = &mut group.action {
            *group_jid = Some(jid("120363000000000000@g.us"));
        }
        worker.on_call_signal(&group);
        assert!(calls_in(&events).is_empty());
        assert!(worker.calls.active.is_none());
    }

    #[tokio::test]
    async fn an_offline_missed_call_is_filed_once_even_if_replayed() {
        let (mut worker, events, _, _) = worker();
        let missed = MissedCall::new(
            jid(PEER),
            "OLD".into(),
            whatsapp_rust::wacore::chrono::Utc::now()
                - whatsapp_rust::wacore::chrono::Duration::days(2),
            whatsapp_rust::types::call::MissedReason::Offline,
        );
        worker.on_missed_call(&missed);
        worker.on_missed_call(&missed);
        assert_eq!(worker.archive.calls(10).unwrap().len(), 1);
        let notified = events
            .try_iter()
            .filter(|event| matches!(event, Event::Incoming { .. }))
            .count();
        assert_eq!(notified, 0, "a two day old call does not notify");
    }

    #[tokio::test]
    async fn phone_call_history_is_filed_quietly_and_replaces_the_live_record() {
        use wa::call_log_record::CallResult;
        let (mut worker, events, _, _) = worker();
        let record = |result, incoming| wa::CallLogRecord {
            call_result: Some(result),
            duration: Some(138),
            start_time: Some(1_700_000_000),
            is_incoming: Some(incoming),
            is_video: Some(true),
            ..Default::default()
        };
        let sync = |id: &str, from_me: bool, result| {
            CallLogSync::builder()
                .call_creator_jid(jid(if from_me {
                    "15550001111@s.whatsapp.net"
                } else {
                    PEER
                }))
                .call_id(id.to_owned())
                .from_me(from_me)
                .timestamp(whatsapp_rust::wacore::chrono::Utc::now())
                .record(Box::new(record(result, !from_me)))
                .from_full_sync(true)
                .build()
        };
        worker.on_call_log(&sync("PHONE1", false, CallResult::CONNECTED));
        let stored = worker.archive.message(PEER, "PHONE1").unwrap().unwrap();
        assert_eq!(stored.timestamp, 1_700_000_000);
        assert_eq!(
            stored.content,
            Content::Call {
                video: true,
                outgoing: false,
                outcome: CallOutcome::Completed,
                seconds: Some(138)
            }
        );
        assert_eq!(worker.archive.chat(PEER).unwrap().unwrap().unread, 0);

        worker.on_call_log(&sync("PHONE2", false, CallResult::ACCEPTEDELSEWHERE));
        assert!(worker.archive.message(PEER, "PHONE2").unwrap().is_none());

        worker.on_call_log(&sync("PHONE1", false, CallResult::MISSED));
        let replaced = worker.archive.message(PEER, "PHONE1").unwrap().unwrap();
        assert!(matches!(
            replaced.content,
            Content::Call {
                outcome: CallOutcome::Missed,
                ..
            }
        ));
        assert_eq!(worker.archive.calls(10).unwrap().len(), 1);
        drop(events);
    }

    #[test]
    fn runtime_events_map_to_call_progress() {
        assert_eq!(
            media_notice(&CallEvent::RelayAllocated),
            Some(CallNotice::MediaUp)
        );
        assert_eq!(
            media_notice(&CallEvent::RelayAllocateFailed(403)),
            Some(CallNotice::MediaFailed)
        );
        assert_eq!(media_notice(&CallEvent::VideoKeyframeNeeded), None);
        assert_eq!(
            peer_video_notice(VideoState::UpgradeRequestV2),
            Some(CallNotice::VideoRequested)
        );
        assert_eq!(
            peer_video_notice(VideoState::Enabled),
            Some(CallNotice::PeerVideo(true))
        );
        assert_eq!(
            peer_video_notice(VideoState::Stopped),
            Some(CallNotice::PeerVideo(false))
        );
        assert_eq!(
            peer_video_notice(VideoState::UpgradeCancel),
            Some(CallNotice::VideoRequestEnded)
        );
    }
}
