use super::screen::{Screen, Tone};
use super::*;
use crate::model::call::{CallDirection, CallOutcome, CallSignal};

pub(super) fn session(direction: CallDirection, video: bool) -> CallSession {
    CallSession::new("call".into(), "chat".into(), direction, video, 100)
}

fn actions(screen: &Screen) -> Vec<CallAction> {
    screen.buttons.iter().map(|button| button.action).collect()
}

pub(super) fn connected(direction: CallDirection, video: bool) -> CallSession {
    let mut call = session(direction, video);
    call.apply(
        match direction {
            CallDirection::Incoming => CallSignal::LocalAnswered,
            CallDirection::Outgoing => CallSignal::PeerAnswered,
        },
        100,
    );
    call.apply(CallSignal::MediaUp, 100);
    call
}

#[test]
fn a_ringing_call_offers_decline_and_answer() {
    let ringing = screen(&session(CallDirection::Incoming, false), "Ana", 100);
    assert_eq!(ringing.status, "Incoming voice call");
    assert_eq!(actions(&ringing), [CallAction::Decline, CallAction::Answer]);
    assert_eq!(ringing.buttons[0].tone, Tone::Danger);
    assert_eq!(ringing.buttons[1].tone, Tone::Success);
    let video = screen(&session(CallDirection::Incoming, true), "Ana", 100);
    assert_eq!(video.status, "Incoming video call");
}

#[test]
fn an_outgoing_call_can_only_be_cancelled_until_it_connects() {
    let calling = screen(&session(CallDirection::Outgoing, true), "Ana", 100);
    assert_eq!(calling.status, "Calling…");
    assert_eq!(calling.buttons.len(), 1);
    assert_eq!(calling.buttons[0].label, "Cancel call");
    assert_eq!(calling.buttons[0].action, CallAction::HangUp);
}

#[test]
fn a_voice_call_shows_the_timer_and_offers_mute_video_and_end() {
    let call = connected(CallDirection::Outgoing, false);
    let live = screen(&call, "Ana", 100 + 138);
    assert_eq!(live.status, "Connected · 02:18");
    assert_eq!(
        actions(&live),
        [
            CallAction::ToggleMute,
            CallAction::ToggleCamera,
            CallAction::HangUp
        ]
    );
    assert_eq!(live.buttons[0].label, "Mute");
    assert_eq!(live.buttons[1].label, "Start video");
}

#[test]
fn a_muted_call_says_so_and_offers_unmute() {
    let mut call = connected(CallDirection::Outgoing, false);
    call.muted = true;
    let live = screen(&call, "Ana", 100 + 138);
    assert_eq!(live.status, "Microphone muted · 02:18");
    assert_eq!(live.buttons[0].label, "Unmute");
}

#[test]
fn a_video_call_offers_the_camera_switch_by_its_state() {
    let mut call = connected(CallDirection::Outgoing, true);
    call.camera_on = true;
    assert_eq!(
        screen(&call, "Ana", 100).buttons[1].label,
        "Turn camera off"
    );
    call.camera_on = false;
    assert_eq!(screen(&call, "Ana", 100).buttons[1].label, "Turn camera on");
}

#[test]
fn the_camera_button_waits_while_the_camera_is_changing() {
    let mut call = connected(CallDirection::Outgoing, true);
    call.camera_busy = true;
    let live = screen(&call, "Ana", 100);
    let camera = live
        .buttons
        .iter()
        .find(|button| button.action == CallAction::ToggleCamera)
        .expect("the camera button stays");
    assert!(!camera.sensitive);
    assert!(
        live.buttons
            .iter()
            .filter(|button| button.action != CallAction::ToggleCamera)
            .all(|button| button.sensitive)
    );
    call.camera_busy = false;
    assert!(
        screen(&call, "Ana", 100)
            .buttons
            .iter()
            .all(|button| button.sensitive)
    );
}

#[test]
fn a_video_request_is_offered_for_acceptance_by_name() {
    let mut call = connected(CallDirection::Incoming, false);
    call.video_requested = true;
    let live = screen(&call, "Ana", 100);
    assert_eq!(live.detail.as_deref(), Some("Ana wants to switch to video"));
    assert_eq!(live.buttons[0].action, CallAction::AcceptVideo);
    assert!(!actions(&live).contains(&CallAction::ToggleCamera));
}

#[test]
fn a_finished_call_returns_to_the_chat_and_a_failed_one_can_retry() {
    let mut call = connected(CallDirection::Outgoing, false);
    call.apply(CallSignal::LocalHangup, 100 + 138);
    let ended = screen(&call, "Ana", 500);
    assert_eq!(ended.status, "Voice call ended · 02:18");
    assert_eq!(actions(&ended), [CallAction::BackToChat]);

    let mut failed = session(CallDirection::Outgoing, false);
    failed.apply(CallSignal::MediaFailed, 101);
    let failure = screen(&failed, "Ana", 101);
    assert_eq!(failure.status, "Couldn't connect the call");
    assert_eq!(
        actions(&failure),
        [CallAction::Retry, CallAction::BackToChat]
    );
    assert_eq!(failure.buttons[0].tone, Tone::Primary);

    let mut missed = session(CallDirection::Incoming, false);
    missed.apply(CallSignal::PeerTerminated, 101);
    assert_eq!(missed.phase, CallPhase::Ended(CallOutcome::Missed));
    assert_eq!(screen(&missed, "Ana", 101).status, "Missed voice call");
}

#[test]
fn history_rows_use_plain_words_for_the_call_kind() {
    assert_eq!(kind_label(CallKind::Missed), "Missed");
    assert_eq!(kind_label(CallKind::Outgoing), "Outgoing");
    assert_eq!(kind_label(CallKind::Incoming), "Incoming");
}

#[test]
fn a_chat_without_a_name_is_listed_by_its_number() {
    let chats = [crate::model::Chat::new(
        "1@s.whatsapp.net".into(),
        "Ana".into(),
    )];
    assert_eq!(chat_label(&chats, "1@s.whatsapp.net"), "Ana");
    assert!(!chat_label(&chats, "15551234567@s.whatsapp.net").is_empty());
}

#[test]
fn frames_with_the_wrong_size_never_become_pictures() {
    let frame = RgbaFrame {
        width: 2,
        height: 2,
        data: vec![0; 3],
    };
    assert!(texture(frame).is_none());
}
