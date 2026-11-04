//! The tasks that drive one call: opening its devices, following the VoIP
//! runtime's events, and ending it.

use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Mutex, MutexGuard};

use whatsapp_rust::types::call::IncomingCall;
use whatsapp_rust::voip::{CallEvent, CallHandle, VideoState, VideoUpgradeToken};

use super::super::*;
use crate::backend::CallNotice;
use crate::calls::video::H264Encoder;
use crate::calls::{
    CallControls, CallMedia, CameraNeed, Fault, FaultHandler, MediaError, VideoEndpoints,
    VideoSlots, no_camera,
};
use crate::i18n::gettext;

/// How long ending a call may wait on the network before giving up on it.
const TERMINATE_TIMEOUT: Duration = Duration::from_secs(5);

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What the tasks driving a call share with the worker.
#[derive(Default)]
pub(super) struct CallShared {
    handle: Mutex<Option<CallHandle>>,
    media: Mutex<Option<CallMedia>>,
    controls: Mutex<Option<CallControls>>,
    upgrade: Mutex<Option<VideoUpgradeToken>>,
    /// Set before a call is torn down, so tasks still setting it up notice and
    /// release the devices they opened, which nothing else knows about.
    cancelled: AtomicBool,
}

impl CallShared {
    pub(super) fn handle(&self) -> Option<CallHandle> {
        guard(&self.handle).clone()
    }

    pub(super) fn controls(&self) -> Option<CallControls> {
        guard(&self.controls).clone()
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.cancelled.load(AtomicOrdering::Acquire)
    }

    pub(super) fn cancel(&self) {
        self.cancelled.store(true, AtomicOrdering::Release);
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

    pub(super) fn apply_mute(&self, muted: bool) {
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
pub(super) struct Reporter {
    events: std::sync::mpsc::Sender<Event>,
    waker: Arc<dyn Wake>,
    commands: mpsc::UnboundedSender<Command>,
    generation: u64,
}

impl Reporter {
    pub(super) fn new(
        events: std::sync::mpsc::Sender<Event>,
        waker: Arc<dyn Wake>,
        commands: mpsc::UnboundedSender<Command>,
        generation: u64,
    ) -> Self {
        Self {
            events,
            waker,
            commands,
            generation,
        }
    }

    pub(super) fn notice(&self, notice: CallNotice) {
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

    /// Shows `text` and ends the call.
    pub(super) fn failed(&self, text: String) {
        self.error(text);
        self.notice(CallNotice::MediaFailed);
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
}

/// Opens devices off the async runtime. `video` is `None` for a voice call;
/// otherwise it says whether the call can go on without the camera.
async fn prepare_media(
    slots: Arc<VideoSlots>,
    faults: FaultHandler,
    video: Option<CameraNeed>,
) -> Result<Prepared, MediaError> {
    tokio::task::spawn_blocking(move || {
        let video = video
            .map(|need| CallMedia::check_video().map(|encoder| (encoder, need)))
            .transpose()?;
        let mut media = CallMedia::start(slots, faults)?;
        let video = video
            .map(|(encoder, need)| open_endpoints(&mut media, encoder, need))
            .transpose()?;
        Ok(Prepared { media, video })
    })
    .await
    .unwrap_or(Err(MediaError::Unavailable))
}

fn open_endpoints(
    media: &mut CallMedia,
    encoder: H264Encoder,
    need: CameraNeed,
) -> Result<VideoEndpoints, MediaError> {
    let plan = media.plan_video(encoder);
    let sink = media.attach_decoder(plan.open_decoder()?);
    let camera = plan.open_camera(need)?;
    let has_camera = camera.is_some();
    let source = camera.map_or_else(no_camera, |camera| media.attach_camera(camera));
    Ok(VideoEndpoints {
        source,
        sink,
        camera: has_camera,
    })
}

pub(super) fn media_notice(event: &CallEvent) -> Option<CallNotice> {
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

pub(super) fn peer_video_notice(state: VideoState) -> Option<CallNotice> {
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

async fn reject_offer(client: &Client, offer: &IncomingCall) {
    if tokio::time::timeout(TERMINATE_TIMEOUT, client.voip().reject(offer))
        .await
        .is_err()
    {
        log::debug!("call rejection not confirmed in time");
    }
}

/// Ends a call on the network and releases its devices at once. Devices are
/// freed alongside the network exchange, never after it, so the microphone and
/// camera go quiet even when the network is slow. `offer` is the one to turn
/// down when the call never got a handle to terminate.
pub(super) async fn hang_up(
    shared: &Arc<CallShared>,
    client: Option<&Client>,
    offer: Option<&IncomingCall>,
) {
    let network = async {
        if let Some(handle) = shared.handle() {
            end_handle(&handle).await;
        } else if let (Some(client), Some(offer)) = (client, offer) {
            reject_offer(client, offer).await;
        }
    };
    tokio::join!(shared.release_media(), network);
}

/// Cancels the call's setup tasks and hangs up in the background.
pub(super) fn teardown(
    shared: Arc<CallShared>,
    client: Option<Arc<Client>>,
    offer: Option<Box<IncomingCall>>,
) {
    shared.cancel();
    tokio::spawn(async move { hang_up(&shared, client.as_deref(), offer.as_deref()).await });
}

/// Turns an offer down without holding up the worker.
pub(super) fn reject_in_background(client: Arc<Client>, offer: Box<IncomingCall>) {
    tokio::spawn(async move { reject_offer(&client, &offer).await });
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

async fn drop_off_runtime(media: CallMedia) {
    let _ = tokio::task::spawn_blocking(move || drop(media)).await;
}

/// Which call to start with the VoIP runtime.
pub(super) enum Dial {
    Place(Jid),
    Answer(Box<IncomingCall>),
}

impl Dial {
    /// A call you place needs its camera; one you answer goes on without.
    fn camera_need(&self) -> CameraNeed {
        match self {
            Self::Place(_) => CameraNeed::Required,
            Self::Answer(_) => CameraNeed::Optional,
        }
    }

    fn failure(&self) -> String {
        match self {
            Self::Place(_) => gettext("The call could not be placed"),
            Self::Answer(_) => gettext("The call could not be answered"),
        }
    }

    async fn start(
        &self,
        client: &Client,
        microphone: async_channel::Receiver<Vec<i16>>,
        speaker: async_channel::Sender<Vec<i16>>,
        video: Option<VideoEndpoints>,
    ) -> Option<CallHandle> {
        let voip = client.voip();
        match self {
            Self::Place(peer) => {
                let mut builder = voip.call(peer).audio(microphone, speaker);
                if let Some(video) = video {
                    builder = builder.video(video.source, video.sink);
                }
                builder.start().await.ok()
            }
            Self::Answer(offer) => {
                let mut builder = voip.accept(offer).audio(microphone, speaker);
                if let Some(video) = video {
                    builder = builder.video(video.source, video.sink);
                }
                builder.start().await.ok()
            }
        }
    }
}

/// Opens the devices, then places or answers the call. A call cancelled at
/// any point along the way ends quietly, and releases whatever it opened.
pub(super) async fn run_call(
    client: Arc<Client>,
    dial: Dial,
    video: bool,
    slots: Arc<VideoSlots>,
    shared: Arc<CallShared>,
    reporter: Reporter,
) -> Result<(), String> {
    let need = video.then(|| dial.camera_need());
    let prepared = prepare_media(slots, reporter.faults(), need)
        .await
        .map_err(|error| error.to_string());
    if shared.is_cancelled() {
        if let Ok(prepared) = prepared {
            drop_off_runtime(prepared.media).await;
        }
        return Ok(());
    }
    let Prepared { media, video } = prepared?;
    let (microphone, speaker) = (media.microphone(), media.speaker());
    let has_camera = video.as_ref().is_some_and(|video| video.camera);
    let missing_camera = video.is_some() && !has_camera;
    if !adopt_media(&shared, media).await {
        return Ok(());
    }
    reporter.notice(CallNotice::Camera(has_camera));
    if missing_camera {
        reporter.notice(CallNotice::CameraUnavailable);
    }
    match dial.start(&client, microphone, speaker, video).await {
        Some(handle) => {
            adopt(handle, shared, reporter).await;
            Ok(())
        }
        None if shared.is_cancelled() => Ok(()),
        None => {
            log::warn!("call did not start");
            Err(dial.failure())
        }
    }
}

pub(super) async fn switch_camera_on(shared: &Arc<CallShared>, reporter: &Reporter) {
    match open_video(shared, CameraNeed::Required).await {
        Ok(endpoints) => {
            let Some(handle) = shared.handle() else {
                stop_camera(shared).await;
                return;
            };
            let VideoEndpoints { source, sink, .. } = endpoints;
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

pub(super) async fn accept_video(shared: &Arc<CallShared>, reporter: &Reporter) {
    let (Some(handle), Some(token)) = (shared.handle(), *guard(&shared.upgrade)) else {
        reporter.notice(CallNotice::VideoRequestEnded);
        return;
    };
    let endpoints = match open_video(shared, CameraNeed::Optional).await {
        Ok(endpoints) => endpoints,
        Err(error) => {
            reporter.error(error.to_string());
            return;
        }
    };
    let VideoEndpoints {
        source,
        sink,
        camera,
    } = endpoints;
    match handle.accept_video(token, source, sink).await {
        Ok(()) => {
            reporter.notice(CallNotice::Camera(camera));
            if !camera {
                reporter.notice(CallNotice::CameraUnavailable);
            }
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

/// Opens the decoder, and the camera when it can, without holding the call's
/// media while the devices start: the media lock is taken only to read what is
/// needed and to keep the result.
async fn open_video(
    shared: &Arc<CallShared>,
    need: CameraNeed,
) -> Result<VideoEndpoints, MediaError> {
    let shared = shared.clone();
    tokio::task::spawn_blocking(move || {
        let encoder = CallMedia::check_video()?;
        let (plan, sink) = {
            let media = guard(&shared.media);
            let media = media.as_ref().ok_or(MediaError::Unavailable)?;
            (media.plan_video(encoder), media.decoder_sender())
        };
        let sink = match sink {
            Some(sink) => sink,
            None => {
                let decoder = plan.open_decoder()?;
                guard(&shared.media)
                    .as_mut()
                    .ok_or(MediaError::Unavailable)?
                    .attach_decoder(decoder)
            }
        };
        let camera = plan.open_camera(need)?;
        let has_camera = camera.is_some();
        let source = match camera {
            Some(camera) => guard(&shared.media)
                .as_mut()
                .ok_or(MediaError::Unavailable)?
                .attach_camera(camera),
            None => no_camera(),
        };
        Ok(VideoEndpoints {
            source,
            sink,
            camera: has_camera,
        })
    })
    .await
    .unwrap_or(Err(MediaError::Unavailable))
}

pub(super) async fn stop_camera(shared: &Arc<CallShared>) {
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
