//! What the call window says and offers for each moment of a call.

use crate::i18n::gettext;
use crate::model::call::{
    CallPhase, CallSession, call_clock, call_summary, direction_label, with_detail,
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
    pub(super) fn class(self) -> &'static str {
        match self {
            Self::Neutral => "neutral",
            Self::Danger => "danger",
            Self::Success => "success",
            Self::Primary => "primary",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CallButton {
    pub(super) action: CallAction,
    pub(super) label: String,
    pub(super) icon: &'static str,
    pub(super) tone: Tone,
    pub(super) sensitive: bool,
}

fn button(action: CallAction, label: String, icon: &'static str, tone: Tone) -> CallButton {
    CallButton {
        action,
        label,
        icon,
        tone,
        sensitive: true,
    }
}

fn unless_busy(mut button: CallButton, session: &CallSession) -> CallButton {
    button.sensitive = !session.camera_busy;
    button
}

/// What the call window says and offers for one moment of a call.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Screen {
    pub(super) status: String,
    pub(super) detail: Option<String>,
    pub(super) buttons: Vec<CallButton>,
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

/// Adds the call that came in during this one: who it is, and what to do with it.
pub(super) fn with_waiting(mut screen: Screen, caller: &str, video: bool) -> Screen {
    let line = if video {
        gettext("%1$s is also calling, with video")
    } else {
        gettext("%1$s is also calling")
    }
    .replace("%1$s", caller);
    screen.detail = Some(match screen.detail {
        Some(detail) => format!("{detail}\n{line}"),
        None => line,
    });
    screen.buttons.push(button(
        CallAction::DeclineWaiting,
        gettext("Decline new call"),
        "call-stop-symbolic",
        Tone::Danger,
    ));
    screen.buttons.push(button(
        CallAction::EndAndAnswer,
        gettext("End and answer"),
        "call-start-symbolic",
        Tone::Success,
    ));
    screen
}

fn screen_of(status: String, buttons: Vec<CallButton>) -> Screen {
    Screen {
        status,
        detail: None,
        buttons,
    }
}

fn hang_up(label: String) -> CallButton {
    button(
        CallAction::HangUp,
        label,
        "call-stop-symbolic",
        Tone::Danger,
    )
}

fn back_to_chat() -> CallButton {
    button(
        CallAction::BackToChat,
        gettext("Back to chat"),
        "go-previous-symbolic",
        Tone::Neutral,
    )
}

pub(super) fn screen(session: &CallSession, name: &str, now: i64) -> Screen {
    match session.phase {
        CallPhase::Ringing => ringing(session),
        CallPhase::Calling => screen_of(gettext("Calling…"), vec![hang_up(gettext("Cancel call"))]),
        CallPhase::Connecting => {
            screen_of(gettext("Connecting…"), vec![hang_up(gettext("End call"))])
        }
        CallPhase::Connected { .. } => connected(session, name, now),
        CallPhase::Ended(outcome) => screen_of(
            call_summary(
                session.video,
                session.is_outgoing(),
                outcome,
                session.talk_seconds,
            ),
            vec![back_to_chat()],
        ),
        CallPhase::Failed => screen_of(
            gettext("Couldn't connect the call"),
            vec![
                button(
                    CallAction::Retry,
                    gettext("Try again"),
                    "view-refresh-symbolic",
                    Tone::Primary,
                ),
                back_to_chat(),
            ],
        ),
    }
}

fn ringing(session: &CallSession) -> Screen {
    screen_of(
        incoming_label(session.video),
        vec![
            button(
                CallAction::Decline,
                gettext("Decline"),
                "call-stop-symbolic",
                Tone::Danger,
            ),
            button(
                CallAction::Answer,
                gettext("Answer"),
                "call-start-symbolic",
                Tone::Success,
            ),
        ],
    )
}

fn mute_button(muted: bool) -> CallButton {
    if muted {
        button(
            CallAction::ToggleMute,
            gettext("Unmute"),
            "microphone-sensitivity-muted-symbolic",
            Tone::Neutral,
        )
    } else {
        button(
            CallAction::ToggleMute,
            gettext("Mute"),
            "microphone-sensitivity-high-symbolic",
            Tone::Neutral,
        )
    }
}

fn camera_button(session: &CallSession) -> CallButton {
    let (label, icon) = if session.camera_on {
        (gettext("Turn camera off"), "camera-disabled-symbolic")
    } else if session.video || session.peer_video {
        (gettext("Turn camera on"), "camera-video-symbolic")
    } else {
        (gettext("Start video"), "camera-video-symbolic")
    };
    unless_busy(
        button(CallAction::ToggleCamera, label, icon, Tone::Neutral),
        session,
    )
}

fn connected(session: &CallSession, name: &str, now: i64) -> Screen {
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
                Tone::Success,
            ),
            session,
        ));
    }
    buttons.push(mute_button(session.muted));
    if !session.video_requested {
        buttons.push(camera_button(session));
    }
    buttons.push(hang_up(gettext("End call")));
    Screen {
        status,
        detail,
        buttons,
    }
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
