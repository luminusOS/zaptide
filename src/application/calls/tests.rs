use super::screen::{
    ButtonKind, Pip, Row, Screen, Stage, Tone, arrange, avatar_size, waiting_order, with_waiting,
};
use super::waiting::{decline_command, end_and_answer_commands};
use super::*;
use crate::calls::ringtone::Ring;
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
    assert_eq!(ringing.title, "Voice call");
    assert_eq!(actions(&ringing), [CallAction::Decline, CallAction::Answer]);
    assert_eq!(ringing.buttons[0].tone, Tone::Danger);
    assert_eq!(ringing.buttons[1].tone, Tone::Success);
    assert!(
        ringing
            .buttons
            .iter()
            .all(|button| button.kind == ButtonKind::Choice)
    );
    assert_eq!(ringing.stage, Stage::Ringing);
    assert_eq!(ringing.focus, Some(CallAction::Answer));
    let video = screen(&session(CallDirection::Incoming, true), "Ana", 100);
    assert_eq!(video.status, "Incoming video call");
    assert_eq!(video.title, "Video call");
}

#[test]
fn an_outgoing_call_can_only_be_cancelled_until_it_connects() {
    let calling = screen(&session(CallDirection::Outgoing, true), "Ana", 100);
    assert_eq!(calling.status, "Calling…");
    assert_eq!(calling.buttons.len(), 1);
    assert_eq!(calling.buttons[0].label, "Cancel call");
    assert_eq!(calling.buttons[0].action, CallAction::HangUp);
    assert_eq!(calling.buttons[0].tone, Tone::Danger);
    assert_eq!(calling.stage, Stage::Ringing);
    assert_eq!(calling.pip, Pip::Preview);
    assert_eq!(calling.focus, Some(CallAction::HangUp));
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
    assert_eq!(live.buttons[0].icon, "audio-input-microphone-symbolic");
    assert_eq!(live.buttons[1].label, "Start video");
    assert!(
        live.buttons
            .iter()
            .take(2)
            .all(|b| b.kind == ButtonKind::Round && !b.checked)
    );
    assert_eq!(live.buttons[2].kind, ButtonKind::Pill);
    assert_eq!(live.buttons[2].tone, Tone::Danger);
    assert_eq!(live.focus, None, "a stray key must not hang up");
    assert_eq!(live.stage, Stage::Voice);
}

#[test]
fn a_muted_call_says_so_and_offers_unmute() {
    let mut call = connected(CallDirection::Outgoing, false);
    call.muted = true;
    let live = screen(&call, "Ana", 100 + 138);
    assert_eq!(live.status, "Microphone muted · 02:18");
    assert_eq!(live.buttons[0].label, "Unmute");
    assert_eq!(live.buttons[0].icon, "microphone-disabled-symbolic");
    assert!(live.buttons[0].checked);
}

#[test]
fn a_video_call_offers_the_camera_switch_by_its_state() {
    let mut call = connected(CallDirection::Outgoing, true);
    call.camera_on = true;
    let on = screen(&call, "Ana", 100);
    assert_eq!(on.buttons[1].label, "Turn camera off");
    assert_eq!(on.buttons[1].icon, "camera-video-symbolic");
    assert!(!on.buttons[1].checked);
    call.camera_on = false;
    let off = screen(&call, "Ana", 100);
    assert_eq!(off.buttons[1].label, "Turn camera on");
    assert_eq!(off.buttons[1].icon, "camera-disabled-symbolic");
    assert!(off.buttons[1].checked);
}

#[test]
fn the_remote_picture_fills_the_window_and_my_camera_becomes_a_corner_preview() {
    let mut call = connected(CallDirection::Outgoing, true);
    call.peer_video = true;
    call.camera_on = true;
    let video = screen(&call, "Ana", 100);
    assert_eq!((video.stage, video.pip), (Stage::Video, Pip::Preview));
    call.camera_on = false;
    assert_eq!(screen(&call, "Ana", 100).pip, Pip::CameraOff);
    call.peer_video = false;
    let voice = screen(&call, "Ana", 100);
    assert_eq!((voice.stage, voice.pip), (Stage::Voice, Pip::Hidden));
}

#[test]
fn a_video_call_without_a_camera_says_so_and_disables_the_switch() {
    let mut call = connected(CallDirection::Incoming, true);
    call.camera_unavailable = true;
    let live = screen(&call, "Ana", 100);
    let notice = live.notice.expect("the person is told");
    assert_eq!(notice.title, "No camera available");
    assert_eq!(notice.body, "Your video is not being sent");
    assert_eq!(live.pip, Pip::Hidden);
    let camera = &live.buttons[1];
    assert_eq!(camera.label, "No camera available");
    assert_eq!(camera.icon, "camera-disabled-symbolic");
    assert!(!camera.sensitive);
    call.video = false;
    assert!(screen(&call, "Ana", 100).notice.is_none());
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
    assert_eq!(ended.status, "Call ended · 02:18");
    assert_eq!(ended.focus, Some(CallAction::BackToChat));
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
    assert_eq!(failure.buttons[1].tone, Tone::Neutral);
    assert_eq!(failure.stage, Stage::Failed);
    assert_eq!(failure.focus, Some(CallAction::Retry));
    assert_eq!(
        failure.detail.as_deref(),
        Some("Check your connection and try again.")
    );

    let mut missed = session(CallDirection::Incoming, false);
    missed.apply(CallSignal::PeerTerminated, 101);
    assert_eq!(missed.phase, CallPhase::Ended(CallOutcome::Missed));
    assert_eq!(screen(&missed, "Ana", 101).status, "Missed voice call");
}

#[test]
fn the_ringing_label_names_the_kind_of_call() {
    assert_eq!(incoming_label(false), "Incoming voice call");
    assert_eq!(incoming_label(true), "Incoming video call");
}

#[test]
fn history_rows_start_a_heading_when_the_day_changes() {
    let noon = crate::util::noon_today();
    let day = 24 * 3600;
    let stamps = [
        noon + 600,
        noon,
        noon - day,
        noon - day - 60,
        noon - 10 * day,
    ];
    let starts: Vec<bool> = (0..stamps.len())
        .map(|index| starts_day(index.checked_sub(1).map(|prev| stamps[prev]), stamps[index]))
        .collect();
    assert_eq!(starts, [true, false, true, false, true]);
}

#[test]
fn history_headings_name_today_yesterday_and_older_dates() {
    let noon = crate::util::noon_today();
    let day = 24 * 3600;
    assert_eq!(crate::util::day_label(noon), "Today");
    assert_eq!(crate::util::day_label(noon - day), "Yesterday");
    assert!(crate::util::day_label(noon - 30 * day).contains(','));
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

#[test]
fn a_ringing_call_rings_unless_quiet_and_a_call_being_placed_always_does() {
    let incoming = session(CallDirection::Incoming, false);
    assert_eq!(ring_for(&incoming, || true), Some(Ring::Incoming));
    assert_eq!(ring_for(&incoming, || false), None);
    let outgoing = session(CallDirection::Outgoing, false);
    assert_eq!(ring_for(&outgoing, || false), Some(Ring::Outgoing));
}

#[test]
fn the_ring_stops_once_the_call_is_answered_or_over() {
    let never = || -> bool { unreachable!("only a ringing call asks") };
    let mut call = session(CallDirection::Incoming, false);
    call.apply(CallSignal::LocalAnswered, 100);
    assert_eq!(ring_for(&call, never), None);
    call.apply(CallSignal::MediaUp, 100);
    assert_eq!(ring_for(&call, never), None);
    call.apply(CallSignal::LocalHangup, 101);
    assert_eq!(ring_for(&call, never), None);
    let mut outgoing = session(CallDirection::Outgoing, false);
    outgoing.apply(CallSignal::PeerAnswered, 100);
    assert_eq!(ring_for(&outgoing, never), None);
    let mut failed = session(CallDirection::Outgoing, false);
    failed.apply(CallSignal::MediaFailed, 100);
    assert_eq!(ring_for(&failed, never), None);
}

#[test]
fn notification_buttons_act_only_on_a_ringing_call() {
    use crate::backend::Command;
    assert!(matches!(
        choice_command(CallChoice::Answer, CallPhase::Ringing),
        Some(Command::AnswerCall)
    ));
    assert!(matches!(
        choice_command(CallChoice::Decline, CallPhase::Ringing),
        Some(Command::DeclineCall)
    ));
    let mut answered = session(CallDirection::Incoming, true);
    answered.apply(CallSignal::LocalAnswered, 100);
    for choice in [CallChoice::Answer, CallChoice::Decline, CallChoice::Show] {
        assert!(choice_command(choice, answered.phase).is_none());
    }
    assert!(choice_command(CallChoice::Show, CallPhase::Ringing).is_none());
}

#[test]
fn a_waiting_call_gets_a_card_with_two_ways_to_deal_with_it() {
    let live = screen(&connected(CallDirection::Outgoing, false), "Ana", 100);
    let before = actions(&live);
    let shown = with_waiting(live, "Bruno", false, "Ana");
    assert_eq!(actions(&shown), before, "the live call keeps its controls");
    let card = shown.waiting.as_ref().expect("the card");
    assert_eq!(card.title, "Bruno is calling");
    assert_eq!(card.hint, "Answering ends your call with Ana");
    let card_actions: Vec<_> = card.buttons.iter().map(|b| b.action).collect();
    assert_eq!(
        card_actions,
        [CallAction::DeclineWaiting, CallAction::EndAndAnswer]
    );
    assert_eq!(
        card.buttons[0].tone,
        Tone::Neutral,
        "End call is the red one"
    );
    assert_eq!(card.buttons[1].tone, Tone::Success);
    assert_eq!(shown.focus, Some(CallAction::EndAndAnswer));
    let with_video = with_waiting(
        screen(&connected(CallDirection::Outgoing, false), "Ana", 100),
        "Bruno",
        true,
        "Ana",
    );
    assert_eq!(
        with_video.waiting.unwrap().title,
        "Bruno is calling, with video"
    );

    let mut request = connected(CallDirection::Incoming, false);
    request.video_requested = true;
    let shown = with_waiting(screen(&request, "Ana", 100), "Bruno", true, "Ana");
    assert_eq!(
        shown.detail.as_deref(),
        Some("Ana wants to switch to video")
    );
    let suggested = shown
        .buttons
        .iter()
        .chain(&shown.waiting.unwrap().buttons)
        .filter(|button| matches!(button.tone, Tone::Success | Tone::Primary))
        .count();
    assert_eq!(suggested, 1, "one suggested action per state");
}

#[test]
fn no_state_has_more_than_one_suggested_or_one_destructive_action() {
    let mut muted = connected(CallDirection::Outgoing, false);
    muted.muted = true;
    let mut ended = connected(CallDirection::Outgoing, false);
    ended.apply(CallSignal::LocalHangup, 200);
    let mut failed = session(CallDirection::Outgoing, false);
    failed.apply(CallSignal::MediaFailed, 101);
    for call in [
        session(CallDirection::Incoming, true),
        session(CallDirection::Outgoing, true),
        connected(CallDirection::Outgoing, false),
        muted,
        ended,
        failed,
    ] {
        let shown = screen(&call, "Ana", 100);
        let count = |tones: &[Tone]| {
            shown
                .buttons
                .iter()
                .filter(|button| tones.contains(&button.tone))
                .count()
        };
        assert!(count(&[Tone::Success, Tone::Primary]) <= 1);
        assert!(count(&[Tone::Danger]) <= 2);
        assert!(
            shown
                .buttons
                .iter()
                .all(|button| button.kind != ButtonKind::Round || !button.label.is_empty())
        );
    }
}

fn rows(screen: &Screen, narrow: bool) -> Vec<(Vec<CallAction>, bool)> {
    arrange(&screen.buttons, narrow)
        .into_iter()
        .map(|Row { buttons, stretch }| {
            (
                buttons.iter().map(|&i| screen.buttons[i].action).collect(),
                stretch,
            )
        })
        .collect()
}

#[test]
fn the_switches_and_end_call_share_a_line_only_in_a_wide_window() {
    use CallAction::*;
    let live = screen(&connected(CallDirection::Outgoing, false), "Ana", 100);
    assert_eq!(
        rows(&live, false),
        [(vec![ToggleMute, ToggleCamera, HangUp], false)]
    );
    assert_eq!(
        rows(&live, true),
        [
            (vec![ToggleMute, ToggleCamera], false),
            (vec![HangUp], true)
        ]
    );
}

#[test]
fn choices_stay_side_by_side_and_other_pairs_stack_in_a_narrow_window() {
    use CallAction::*;
    let ringing = screen(&session(CallDirection::Incoming, false), "Ana", 100);
    assert_eq!(rows(&ringing, true), [(vec![Decline, Answer], false)]);
    let mut failed = session(CallDirection::Outgoing, false);
    failed.apply(CallSignal::MediaFailed, 101);
    let failure = screen(&failed, "Ana", 100);
    assert_eq!(rows(&failure, false), [(vec![Retry, BackToChat], false)]);
    assert_eq!(
        rows(&failure, true),
        [(vec![Retry], true), (vec![BackToChat], true)]
    );
    let calling = screen(&session(CallDirection::Outgoing, false), "Ana", 100);
    assert_eq!(rows(&calling, true), [(vec![HangUp], false)]);
    let mut request = connected(CallDirection::Incoming, false);
    request.video_requested = true;
    let asked = screen(&request, "Ana", 100);
    assert_eq!(
        rows(&asked, false),
        [
            (vec![AcceptVideo], false),
            (vec![ToggleMute, HangUp], false)
        ]
    );
}

#[test]
fn the_waiting_card_puts_the_answer_on_top_when_stacked() {
    let card = with_waiting(
        screen(&connected(CallDirection::Outgoing, false), "Ana", 100),
        "Bruno",
        false,
        "Ana",
    )
    .waiting
    .unwrap();
    assert_eq!(waiting_order(&card.buttons, false), [0, 1]);
    assert_eq!(waiting_order(&card.buttons, true), [1, 0]);
}

#[test]
fn the_picture_shrinks_in_a_narrow_window_and_beside_a_waiting_call() {
    assert_eq!(avatar_size(false, false), 160);
    assert_eq!(avatar_size(true, false), 128);
    assert_eq!(avatar_size(false, true), 120);
    assert_eq!(avatar_size(true, true), 96);
}

#[test]
fn deciding_about_a_waiting_call_reaches_the_account_that_holds_it() {
    use crate::backend::Command;
    let (one, two) = (AccountId(1), AccountId(2));
    assert!(matches!(
        decline_command(one, one),
        Command::DeclineWaitingCall
    ));
    assert!(matches!(decline_command(one, two), Command::DeclineCall));

    let within = end_and_answer_commands(one, one);
    assert!(matches!(
        within.as_slice(),
        [(account, Command::EndAndAnswerWaiting)] if *account == one
    ));
    let across = end_and_answer_commands(one, two);
    assert!(matches!(
        across.as_slice(),
        [(first, Command::HangUpCall), (second, Command::AnswerCall)]
            if *first == one && *second == two
    ));
}
