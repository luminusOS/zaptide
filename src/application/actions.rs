//! Window and message actions installed on the main window.

use super::*;

pub(super) const QUIT_ACCELERATOR: &str = "<Control>q";
pub(super) const CLOSE_ACCELERATOR: &str = "<Control>w";

const ACCELERATORS: [(&str, &str); 2] = [
    ("app.quit", QUIT_ACCELERATOR),
    ("win.close", CLOSE_ACCELERATOR),
];

pub(super) fn install_accelerators(application: &impl IsA<gtk::Application>) {
    for (action, accelerator) in ACCELERATORS {
        application.set_accels_for_action(action, &[accelerator]);
    }
}

/// Ctrl+W belongs to the dialog in front of the window, so it never closes
/// the window underneath an open dialog.
fn dismiss_visible_dialog(window: &adw::ApplicationWindow) -> bool {
    let Some(dialog) = window.visible_dialog() else {
        return false;
    };
    dialog.close();
    true
}

pub(super) fn install_message_actions(
    window: &adw::ApplicationWindow,
    sender: &ComponentSender<NativeApplication>,
) {
    let group = gtk::gio::SimpleActionGroup::new();
    type MessageAction = (&'static str, fn() -> Input);
    let actions: [MessageAction; 14] = [
        ("copy", || Input::CopySelectedText),
        ("copy-image", || Input::CopySelectedImage),
        ("attachment", || Input::ActivateSelectedAttachment),
        ("folder", || Input::ShowSelectedInFolder),
        ("save", || Input::SaveSelectedAttachment),
        ("open-link", || Input::OpenSelectedUri),
        ("quoted", || Input::OpenQuoted),
        ("reply", || Input::ReplySelected),
        ("edit", || Input::EditSelected),
        ("forward", || Input::ShowForward),
        ("select", || Input::StartSelection),
        ("star", || Input::ToggleSelectedStar),
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
    let quit = gtk::gio::SimpleAction::new("quit", None);
    let quit_sender = sender.clone();
    quit.connect_activate(move |_, _| quit_sender.input(Input::Quit));
    relm4::main_application().add_action(&quit);
    add("preferences", input(|| Input::ShowPreferences));
    add("shortcuts", input(|| Input::ShowShortcuts));
    add("about", input(|| Input::ShowAbout));
    add("chat-info", input(|| Input::ShowChatInfo));
    add("chat-pin", input(|| Input::ToggleSelectedPin));
    add("chat-mute", input(|| Input::ToggleSelectedMute));
    add("chat-archive", input(|| Input::ToggleSelectedArchive));
    add("chat-close", input(|| Input::CloseChat));
    add("copy-transcript", input(|| Input::CopyTranscript));
    add("close", {
        let sender = sender.clone();
        let window = window.downgrade();
        Box::new(move || {
            if !window
                .upgrade()
                .is_some_and(|window| dismiss_visible_dialog(&window))
            {
                sender.input(Input::Close);
            }
        })
    });
    add("new-chat", input(|| Input::ShowNewChat));
    add("unlink", input(|| Input::ConfirmRemoveAccount));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quit_and_close_have_distinct_accelerators() {
        let accelerators: std::collections::HashSet<_> = ACCELERATORS
            .iter()
            .map(|(_, accelerator)| accelerator)
            .collect();
        assert_eq!(accelerators.len(), ACCELERATORS.len());
        assert!(ACCELERATORS.contains(&("app.quit", "<Control>q")));
        assert!(ACCELERATORS.contains(&("win.close", "<Control>w")));
    }

    #[test]
    #[ignore = "needs a display"]
    fn accelerators_are_installed_and_close_only_dismisses_the_open_dialog() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("dev.luminusos.ZapTide.AcceleratorFixture")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        install_accelerators(&app);
        assert_eq!(app.accels_for_action("app.quit"), ["<Control>q"]);
        assert_eq!(app.accels_for_action("win.close"), ["<Control>w"]);

        let window = adw::ApplicationWindow::new(&app);
        window.present();
        assert!(!dismiss_visible_dialog(&window));

        let dialog = adw::AlertDialog::new(Some("Fixture"), None);
        dialog.present(Some(&window));
        assert!(dismiss_visible_dialog(&window));
        let context = gtk::glib::MainContext::default();
        let end = std::time::Instant::now() + std::time::Duration::from_millis(600);
        while std::time::Instant::now() < end {
            context.iteration(false);
        }
        assert!(window.visible_dialog().is_none());
        assert!(window.is_visible());
    }
}
