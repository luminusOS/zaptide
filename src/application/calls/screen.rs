//! What the call window says and offers for each moment of a call.

use crate::i18n::gettext;
use crate::model::call::{
    CallOutcome, CallPhase, CallSession, call_clock, call_summary, direction_label, with_detail,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallAction {
    Answer,
    Decline,
    HangUp,
    ToggleMute,
    ToggleCamera,
    AcceptVideo,
    Retry,
    DeclineWaiting,
    EndAndAnswer,
    BackToChat,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    Neutral,
    Danger,
    Success,
    Primary,
}

impl Tone {
    pub(super) fn classes(self) -> &'static [&'static str] {
        match self {
            Self::Neutral => &[],
            Self::Danger => &["destructive-action"],
            Self::Success => &["suggested-action", "call-answer"],
            Self::Primary => &["suggested-action", "call-primary"],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ButtonKind {
    /// Icon and label; stacked in a narrow window.
    Pill,
    /// A pill of a side by side pair, which keeps its place in a narrow window.
    Choice,
    /// An icon alone, for the in-call switches.
    Round,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CallButton {
    pub(super) action: CallAction,
    /// The visible text of a pill; the name and tooltip of a round button.
    pub(super) label: String,
    pub(super) icon: &'static str,
    pub(super) tone: Tone,
    pub(super) kind: ButtonKind,
    /// A switch that is on shows its device as off, with a crossed-out icon.
    pub(super) checked: bool,
    pub(super) sensitive: bool,
}

fn button(
    action: CallAction,
    label: String,
    icon: &'static str,
    tone: Tone,
    kind: ButtonKind,
) -> CallButton {
    CallButton {
        action,
        label,
        icon,
        tone,
        kind,
        checked: false,
        sensitive: true,
    }
}

fn unless_busy(mut button: CallButton, session: &CallSession) -> CallButton {
    button.sensitive = !session.camera_busy;
    button
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stage {
    /// An incoming or outgoing call that has not been answered.
    Ringing,
    Voice,
    Failed,
    /// The other side's video fills the window.
    Video,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pip {
    Hidden,
    /// What the camera sends.
    Preview,
    /// The camera is off in a video call.
    CameraOff,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Notice {
    pub(super) icon: &'static str,
    pub(super) title: String,
    pub(super) body: String,
}

/// The call that came in during this one, with what to do about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WaitingCard {
    pub(super) caller: String,
    pub(super) title: String,
    pub(super) hint: String,
    pub(super) buttons: Vec<CallButton>,
}

/// What the call window says and offers for one moment of a call.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Screen {
    pub(super) title: String,
    pub(super) status: String,
    pub(super) detail: Option<String>,
    pub(super) stage: Stage,
    pub(super) pip: Pip,
    pub(super) notice: Option<Notice>,
    pub(super) buttons: Vec<CallButton>,
    pub(super) waiting: Option<WaitingCard>,
    pub(super) focus: Option<CallAction>,
}

/// One line of controls: indexes into the buttons it was made from.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Row {
    pub(super) buttons: Vec<usize>,
    pub(super) stretch: bool,
}

/// Where the controls go. Wide windows keep the switches and End call on one
/// line; narrow ones give End call a line of its own, as the pt_BR label does
/// not fit beside two round buttons in 360 px.
pub(super) fn arrange(buttons: &[CallButton], narrow: bool) -> Vec<Row> {
    let indexes = |wanted: fn(&CallButton) -> bool| -> Vec<usize> {
        (0..buttons.len())
            .filter(|&index| wanted(&buttons[index]))
            .collect()
    };
    let is_end = |button: &CallButton| button.action == CallAction::HangUp;
    let leads =
        indexes(|button| button.kind != ButtonKind::Round && button.action != CallAction::HangUp);
    let rounds = indexes(|button| button.kind == ButtonKind::Round);
    let end = (0..buttons.len()).find(|&index| is_end(&buttons[index]));

    let mut rows = Vec::new();
    let stack = narrow
        && leads.len() > 1
        && leads
            .iter()
            .any(|&index| buttons[index].kind == ButtonKind::Pill);
    if stack {
        rows.extend(leads.into_iter().map(|index| Row {
            buttons: vec![index],
            stretch: true,
        }));
    } else if !leads.is_empty() {
        rows.push(Row {
            buttons: leads,
            stretch: false,
        });
    }
    match end {
        Some(end) if rounds.is_empty() => rows.push(Row {
            buttons: vec![end],
            stretch: false,
        }),
        Some(end) if narrow => {
            rows.push(Row {
                buttons: rounds,
                stretch: false,
            });
            rows.push(Row {
                buttons: vec![end],
                stretch: true,
            });
        }
        Some(end) => rows.push(Row {
            buttons: rounds.into_iter().chain([end]).collect(),
            stretch: false,
        }),
        None if rounds.is_empty() => {}
        None => rows.push(Row {
            buttons: rounds,
            stretch: false,
        }),
    }
    rows
}

/// The waiting call's buttons in reading order: side by side in a wide window,
/// with the answer on top in a narrow one.
pub(super) fn waiting_order(buttons: &[CallButton], narrow: bool) -> Vec<usize> {
    let mut order: Vec<usize> = (0..buttons.len()).collect();
    if narrow {
        order.reverse();
    }
    order
}

/// The picture's width: smaller in a narrow window and beside a waiting call.
pub(super) fn avatar_size(narrow: bool, compact: bool) -> i32 {
    match (narrow, compact) {
        (false, false) => 160,
        (true, false) => 128,
        (false, true) => 120,
        (true, true) => 96,
    }
}

fn timed(label: String, seconds: Option<u32>) -> String {
    match seconds {
        Some(seconds) => with_detail(&label, &call_clock(seconds)),
        None => label,
    }
}

pub(super) fn incoming_label(video: bool) -> String {
    direction_label(false, video)
}

fn title_of(video: bool) -> String {
    if video {
        gettext("Video call")
    } else {
        gettext("Voice call")
    }
}

/// Adds the call that came in during this one: who it is, and what to do with it.
pub(super) fn with_waiting(mut screen: Screen, caller: &str, video: bool, current: &str) -> Screen {
    let title = if video {
        gettext("%1$s is calling, with video")
    } else {
        gettext("%1$s is calling")
    }
    .replace("%1$s", caller);
    screen.waiting = Some(WaitingCard {
        caller: caller.to_owned(),
        title,
        hint: gettext("Answering ends your call with %1$s").replace("%1$s", current),
        buttons: vec![
            button(
                CallAction::DeclineWaiting,
                gettext("Decline"),
                "call-stop-symbolic",
                Tone::Neutral,
                ButtonKind::Pill,
            ),
            button(
                CallAction::EndAndAnswer,
                gettext("End and answer"),
                "call-start-symbolic",
                Tone::Success,
                ButtonKind::Pill,
            ),
        ],
    });
    for other in &mut screen.buttons {
        if matches!(other.tone, Tone::Primary | Tone::Success) {
            other.tone = Tone::Neutral;
        }
    }
    screen.focus = Some(CallAction::EndAndAnswer);
    screen
}

fn hang_up(label: String) -> CallButton {
    button(
        CallAction::HangUp,
        label,
        "call-stop-symbolic",
        Tone::Danger,
        ButtonKind::Pill,
    )
}

fn back_to_chat() -> CallButton {
    button(
        CallAction::BackToChat,
        gettext("Back to chat"),
        "go-previous-symbolic",
        Tone::Neutral,
        ButtonKind::Pill,
    )
}

fn stage_of(session: &CallSession) -> Stage {
    match session.phase {
        CallPhase::Ringing | CallPhase::Calling => Stage::Ringing,
        CallPhase::Failed => Stage::Failed,
        CallPhase::Connected { .. } if session.peer_video => Stage::Video,
        _ => Stage::Voice,
    }
}

fn pip_of(session: &CallSession, stage: Stage) -> Pip {
    if session.phase.is_finished() || session.camera_unavailable {
        Pip::Hidden
    } else if session.camera_on {
        Pip::Preview
    } else if stage == Stage::Video {
        Pip::CameraOff
    } else {
        Pip::Hidden
    }
}

fn notice_of(session: &CallSession) -> Option<Notice> {
    let live = matches!(
        session.phase,
        CallPhase::Connecting | CallPhase::Connected { .. }
    );
    (live && session.video && session.camera_unavailable).then(|| Notice {
        icon: "camera-disabled-symbolic",
        title: gettext("No camera available"),
        body: gettext("Your video is not being sent"),
    })
}

fn focus_of(phase: CallPhase) -> Option<CallAction> {
    match phase {
        CallPhase::Ringing => Some(CallAction::Answer),
        CallPhase::Calling => Some(CallAction::HangUp),
        CallPhase::Ended(_) => Some(CallAction::BackToChat),
        CallPhase::Failed => Some(CallAction::Retry),
        CallPhase::Connecting | CallPhase::Connected { .. } => None,
    }
}

fn ended_status(session: &CallSession, outcome: CallOutcome) -> String {
    match (outcome, session.talk_seconds) {
        (CallOutcome::Completed, seconds) => timed(gettext("Call ended"), seconds),
        _ => call_summary(
            session.video,
            session.is_outgoing(),
            outcome,
            session.talk_seconds,
        ),
    }
}

pub(super) fn screen(session: &CallSession, name: &str, now: i64) -> Screen {
    let mut detail = None;
    let (status, buttons) = match session.phase {
        CallPhase::Ringing => (incoming_label(session.video), ringing()),
        CallPhase::Calling => (gettext("Calling…"), vec![hang_up(gettext("Cancel call"))]),
        CallPhase::Connecting => (gettext("Connecting…"), vec![hang_up(gettext("End call"))]),
        CallPhase::Connected { .. } => {
            let (status, line, buttons) = connected(session, name, now);
            detail = line;
            (status, buttons)
        }
        CallPhase::Ended(outcome) => (ended_status(session, outcome), vec![back_to_chat()]),
        CallPhase::Failed => {
            detail = Some(gettext("Check your connection and try again."));
            (
                gettext("Couldn't connect the call"),
                vec![
                    button(
                        CallAction::Retry,
                        gettext("Try again"),
                        "view-refresh-symbolic",
                        Tone::Primary,
                        ButtonKind::Pill,
                    ),
                    back_to_chat(),
                ],
            )
        }
    };
    let stage = stage_of(session);
    Screen {
        title: title_of(session.video),
        status,
        detail,
        stage,
        pip: pip_of(session, stage),
        notice: notice_of(session),
        buttons,
        waiting: None,
        focus: focus_of(session.phase),
    }
}

fn ringing() -> Vec<CallButton> {
    vec![
        button(
            CallAction::Decline,
            gettext("Decline"),
            "call-stop-symbolic",
            Tone::Danger,
            ButtonKind::Choice,
        ),
        button(
            CallAction::Answer,
            gettext("Answer"),
            "call-start-symbolic",
            Tone::Success,
            ButtonKind::Choice,
        ),
    ]
}

fn mute_button(muted: bool) -> CallButton {
    let (label, icon) = if muted {
        (gettext("Unmute"), "microphone-disabled-symbolic")
    } else {
        (gettext("Mute"), "audio-input-microphone-symbolic")
    };
    CallButton {
        checked: muted,
        ..button(
            CallAction::ToggleMute,
            label,
            icon,
            Tone::Neutral,
            ButtonKind::Round,
        )
    }
}

fn camera_button(session: &CallSession) -> CallButton {
    let in_video = session.video || session.peer_video;
    let (label, icon, checked) = if session.camera_unavailable {
        (
            gettext("No camera available"),
            "camera-disabled-symbolic",
            true,
        )
    } else if session.camera_on {
        (gettext("Turn camera off"), "camera-video-symbolic", false)
    } else if in_video {
        (gettext("Turn camera on"), "camera-disabled-symbolic", true)
    } else {
        (gettext("Start video"), "camera-video-symbolic", false)
    };
    let mut camera = unless_busy(
        CallButton {
            checked,
            ..button(
                CallAction::ToggleCamera,
                label,
                icon,
                Tone::Neutral,
                ButtonKind::Round,
            )
        },
        session,
    );
    camera.sensitive &= !session.camera_unavailable;
    camera
}

fn connected(
    session: &CallSession,
    name: &str,
    now: i64,
) -> (String, Option<String>, Vec<CallButton>) {
    let seconds = session.seconds(now);
    let status = if session.muted {
        timed(gettext("Microphone muted"), seconds)
    } else {
        timed(gettext("Connected"), seconds)
    };
    let mut detail = None;
    let mut buttons = Vec::new();
    if session.video_requested {
        detail = Some(gettext("%1$s wants to switch to video").replace("%1$s", name));
        buttons.push(unless_busy(
            button(
                CallAction::AcceptVideo,
                gettext("Accept video"),
                "camera-video-symbolic",
                Tone::Primary,
                ButtonKind::Pill,
            ),
            session,
        ));
    }
    buttons.push(mute_button(session.muted));
    if !session.video_requested {
        buttons.push(camera_button(session));
    }
    buttons.push(hang_up(gettext("End call")));
    (status, detail, buttons)
}

/// Whether the history row at `timestamp` opens a new day heading, given the
/// row listed just before it.
pub(super) fn starts_day(previous: Option<i64>, timestamp: i64) -> bool {
    previous.is_none_or(|previous| !crate::util::same_day(previous, timestamp))
}

pub(super) fn when(timestamp: i64) -> String {
    with_detail(
        &crate::util::day_label(timestamp),
        &crate::util::clock(timestamp),
    )
}
