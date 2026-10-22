//! Window and message actions installed on the main window.

use super::*;

pub(super) fn install_message_actions(
    window: &adw::ApplicationWindow,
    sender: &ComponentSender<NativeApplication>,
) {
    let group = gtk::gio::SimpleActionGroup::new();
    type MessageAction = (&'static str, fn() -> Input);
    let actions: [MessageAction; 12] = [
        ("copy", || Input::CopySelectedText),
        ("attachment", || Input::ActivateSelectedAttachment),
        ("folder", || Input::ShowSelectedInFolder),
        ("save", || Input::SaveSelectedAttachment),
        ("open-link", || Input::OpenSelectedUri),
        ("quoted", || Input::OpenQuoted),
        ("reply", || Input::ReplySelected),
        ("edit", || Input::EditSelected),
        ("forward", || Input::ShowForward),
        ("select", || Input::StartSelection),
        ("delete", || Input::DeleteSelected(false)),
        ("delete-everyone", || Input::DeleteSelected(true)),
    ];
    for (name, input) in actions {
        let action = gtk::gio::SimpleAction::new(name, None);
        let sender = sender.clone();
        action.connect_activate(move |_, _| sender.input(input()));
        group.add_action(&action);
    }
    let vote = gtk::gio::SimpleAction::new("vote", Some(gtk::glib::VariantTy::UINT32));
    let vote_sender = sender.clone();
    vote.connect_activate(move |_, choice| {
        if let Some(choice) = choice.and_then(gtk::glib::Variant::get::<u32>) {
            vote_sender.input(Input::VoteOption(choice as usize));
        }
    });
    group.add_action(&vote);
    window.insert_action_group("message", Some(&group));
}

pub(super) fn install_window_actions(
    window: &adw::ApplicationWindow,
    sender: &ComponentSender<NativeApplication>,
) {
    let add = |name: &str, activate: Box<dyn Fn()>| {
        let action = gtk::gio::SimpleAction::new(name, None);
        action.connect_activate(move |_, _| activate());
        window.add_action(&action);
    };
    let input = |input: fn() -> Input| -> Box<dyn Fn()> {
        let sender = sender.clone();
        Box::new(move || sender.input(input()))
    };
    add("preferences", input(|| Input::ShowPreferences));
    add("shortcuts", input(|| Input::ShowShortcuts));
    add("about", input(|| Input::ShowAbout));
    add("chat-info", input(|| Input::ShowChatInfo));
    add("chat-pin", input(|| Input::ToggleSelectedPin));
    add("chat-mute", input(|| Input::ToggleSelectedMute));
    add("chat-archive", input(|| Input::ToggleSelectedArchive));
    add("chat-close", input(|| Input::CloseChat));
    add("copy-transcript", input(|| Input::CopyTranscript));
    add("quit", input(|| Input::Quit));
    add("close", input(|| Input::Close));
    add("new-chat", input(|| Input::ShowNewChat));
    add("unlink", input(|| Input::ConfirmRemoveAccount));
}
