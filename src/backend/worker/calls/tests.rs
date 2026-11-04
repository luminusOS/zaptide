use super::runtime::{media_notice, peer_video_notice};
use super::*;
use crate::backend::worker::tests::worker;
use whatsapp_rust::types::call::MissedCall;
use whatsapp_rust::voip::{CallEvent, VideoState};
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
async fn a_second_offer_while_busy_is_left_alone_and_filed_when_the_library_reports_it_missed() {
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
    assert_eq!(waiting_ids(&events), [Some("SECOND".to_owned())]);
    assert!(
        worker.calls.rejected().is_empty(),
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
    assert_eq!(waiting_ids(&events), [None]);
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

const OTHER: &str = "15550003333@s.whatsapp.net";

/// The waiting-call events so far: the id of the call that waits, or `None`
/// when no call waits any more. Leaves the other events alone.
fn waiting_ids(events: &std::sync::mpsc::Receiver<Event>) -> Vec<Option<String>> {
    events
        .try_iter()
        .filter_map(|event| match event {
            Event::CallWaiting(session) => Some(session.map(|session| session.id)),
            _ => None,
        })
        .collect()
}

fn from_other(mut call: IncomingCall) -> IncomingCall {
    call.from = jid(OTHER);
    call
}

/// A call in progress (`FIRST`, answered) with `SECOND` waiting behind it.
fn first_call_with_second_waiting(worker: &mut Worker, events: &std::sync::mpsc::Receiver<Event>) {
    worker.on_call_signal(&offer("FIRST", false));
    answer_in_place(worker);
    worker.on_call_signal(&from_other(offer("SECOND", true)));
    assert_eq!(waiting_ids(events), [Some("SECOND".to_owned())]);
}

fn active_id(worker: &Worker) -> Option<&str> {
    worker
        .calls
        .active
        .as_ref()
        .map(|call| call.session.id.as_str())
}

#[tokio::test]
async fn declining_the_waiting_call_turns_it_down_and_leaves_the_first_alone() {
    let (mut worker, events, _, _) = worker();
    first_call_with_second_waiting(&mut worker, &events);

    worker.decline_waiting_call();

    assert_eq!(worker.calls.rejected(), ["SECOND"]);
    assert_eq!(waiting_ids(&events), [None]);
    assert_eq!(active_id(&worker), Some("FIRST"));
    let stored = worker.archive.message(OTHER, "SECOND").unwrap().unwrap();
    assert!(matches!(
        stored.content,
        Content::Call {
            video: true,
            outcome: CallOutcome::Declined,
            ..
        }
    ));
    assert_eq!(worker.archive.chat(OTHER).unwrap().unwrap().unread, 0);
}

#[tokio::test]
async fn ending_the_first_call_brings_the_waiting_one_up_to_ring() {
    let (mut worker, events, _, _) = worker();
    first_call_with_second_waiting(&mut worker, &events);

    worker.hang_up_call();

    assert_eq!(active_id(&worker), Some("SECOND"));
    assert_eq!(waiting_ids(&events), [None]);
    let call = worker.calls.active.as_ref().unwrap();
    assert_eq!(call.session.phase, CallPhase::Ringing);
    assert!(call.session.video);
    assert!(call.offer.is_some());
    assert_eq!(worker.calls.rejected(), ["FIRST"]);
}

#[tokio::test]
async fn end_and_answer_hangs_up_the_first_call_and_goes_on_to_answer_the_second() {
    let (mut worker, events, _, _) = worker();
    first_call_with_second_waiting(&mut worker, &events);

    worker.end_and_answer_waiting();

    assert_eq!(active_id(&worker), Some("SECOND"));
    assert!(worker.calls.waiting.is_none());
    assert_eq!(worker.calls.rejected(), ["FIRST"]);
    let shown: Vec<Event> = events.try_iter().collect();
    assert!(
        shown
            .iter()
            .any(|event| matches!(event, Event::CallWaiting(None)))
    );
    assert!(
        shown.iter().any(
            |event| matches!(event, Event::Info(text) if text == "The call could not be answered")
        ),
        "the answer was attempted; a worker without a connection cannot place it"
    );
}

#[tokio::test]
async fn end_and_answer_does_nothing_without_a_waiting_call() {
    let (mut worker, events, _, _) = worker();
    worker.on_call_signal(&offer("ONLY", false));
    answer_in_place(&mut worker);
    let _ = calls_in(&events);

    worker.end_and_answer_waiting();

    assert_eq!(active_id(&worker), Some("ONLY"));
    assert!(worker.calls.rejected().is_empty());
}

#[tokio::test]
async fn a_waiting_caller_giving_up_clears_the_wait_and_files_a_missed_call() {
    let (mut worker, events, _, _) = worker();
    first_call_with_second_waiting(&mut worker, &events);

    worker.on_call_signal(&from_other(terminate("SECOND", None)));
    assert_eq!(waiting_ids(&events), [None]);
    assert_eq!(active_id(&worker), Some("FIRST"));
    let missed = MissedCall::new(
        jid(OTHER),
        "SECOND".into(),
        whatsapp_rust::wacore::chrono::Utc::now(),
        whatsapp_rust::types::call::MissedReason::Remote,
    );
    worker.on_missed_call(&missed);

    assert!(
        waiting_ids(&events).is_empty(),
        "nothing waits to be cleared twice"
    );
    let stored = worker.archive.message(OTHER, "SECOND").unwrap().unwrap();
    assert!(matches!(
        stored.content,
        Content::Call {
            outcome: CallOutcome::Missed,
            ..
        }
    ));
    assert!(worker.calls.rejected().is_empty());
}

#[tokio::test]
async fn a_waiting_call_answered_on_another_device_is_dismissed_without_a_record() {
    let (mut worker, events, _, _) = worker();
    first_call_with_second_waiting(&mut worker, &events);

    worker.on_call_signal(&from_other(terminate("SECOND", Some("accepted_elsewhere"))));

    assert_eq!(waiting_ids(&events), [None]);
    assert!(worker.archive.message(OTHER, "SECOND").unwrap().is_none());
}

#[tokio::test]
async fn only_one_call_waits_and_a_repeated_offer_changes_nothing() {
    let (mut worker, events, _, _) = worker();
    first_call_with_second_waiting(&mut worker, &events);

    worker.on_call_signal(&from_other(offer("SECOND", true)));
    let mut third = offer("THIRD", false);
    third.from = jid("15550004444@s.whatsapp.net");
    worker.on_call_signal(&third);

    assert!(waiting_ids(&events).is_empty());
    assert_eq!(
        worker
            .calls
            .waiting
            .as_ref()
            .map(|waiting| waiting.session.id.as_str()),
        Some("SECOND")
    );
    assert!(worker.calls.rejected().is_empty(), "the third rings out");
}

#[tokio::test]
async fn a_waiting_call_does_not_survive_a_logout_or_a_shutdown() {
    let (mut worker, events, _, _) = worker();
    first_call_with_second_waiting(&mut worker, &events);
    worker.abandon_call();
    assert!(worker.calls.waiting.is_none());
    assert!(
        events
            .try_iter()
            .any(|event| matches!(event, Event::CallWaiting(None)))
    );
    assert!(
        worker.calls.active.is_none(),
        "no waiting call is brought up when the account goes"
    );

    let (mut worker, events, _, _) = self::worker();
    first_call_with_second_waiting(&mut worker, &events);
    worker.end_call_for_shutdown().await;
    assert!(worker.calls.waiting.is_none());
    assert!(worker.calls.active.is_none());
    assert_eq!(worker.calls.rejected(), ["FIRST"]);
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

    assert_eq!(worker.calls.rejected(), ["CONNECTING"]);
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
    assert_eq!(worker.calls.rejected(), ["CLOSING"]);

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
    assert_eq!(worker.calls.rejected(), ["CLOSING", "CLOSING2"]);
}

#[tokio::test]
async fn a_peer_ending_the_call_never_rejects_it() {
    let (mut worker, events, _, _) = worker();
    worker.on_call_signal(&offer("PEER", false));
    let _ = calls_in(&events);
    answer_in_place(&mut worker);
    worker.on_call_signal(&terminate("PEER", None));
    assert!(worker.calls.rejected().is_empty());
    let _ = calls_in(&events);
    worker.on_call_signal(&offer("ELSEWHERE", false));
    worker.on_call_signal(&terminate("ELSEWHERE", Some("accepted_elsewhere")));
    assert!(worker.calls.rejected().is_empty());
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
        worker.calls.rejected().is_empty(),
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

    assert_eq!(worker.calls.rejected(), ["SHUTDOWN"]);
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
async fn a_video_call_without_a_camera_says_so_until_the_camera_comes_on() {
    let (mut worker, events, _, _) = worker();
    worker.on_call_signal(&offer("NOCAM", true));
    let _ = calls_in(&events);
    answer_in_place(&mut worker);
    worker.on_call_notice(1, CallNotice::MediaUp);
    let _ = calls_in(&events);
    worker.on_call_notice(1, CallNotice::Camera(false));
    worker.on_call_notice(1, CallNotice::CameraUnavailable);
    let session = calls_in(&events).pop().expect("the call is reported");
    assert!(session.camera_unavailable && !session.camera_on);
    worker.on_call_notice(1, CallNotice::Camera(true));
    let session = calls_in(&events).pop().expect("the call is reported");
    assert!(!session.camera_unavailable && session.camera_on);
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
