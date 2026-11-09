use super::*;
use relm4::ComponentController;

const SECTIONS: [&str; 3] = ["chats", "archived", "calls"];
const CLICK_AND_ITS_TWO_SYNCS: usize = 3;

fn find_toggle_group(widget: &gtk::Widget) -> Option<adw::ToggleGroup> {
    if let Ok(group) = widget.clone().downcast::<adw::ToggleGroup>() {
        return Some(group);
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(group) = find_toggle_group(&current) {
            return Some(group);
        }
        child = current.next_sibling();
    }
    None
}

fn find_return_to_call(widget: &gtk::Widget) -> Option<gtk::Button> {
    if let Ok(button) = widget.clone().downcast::<gtk::Button>()
        && (button.label().as_deref() == Some("Return to call")
            || button.tooltip_text().as_deref() == Some("Return to call"))
    {
        return Some(button);
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(button) = find_return_to_call(&current) {
            return Some(button);
        }
        child = current.next_sibling();
    }
    None
}

fn spin(duration: std::time::Duration) {
    let context = gtk::glib::MainContext::default();
    let end = std::time::Instant::now() + duration;
    while std::time::Instant::now() < end {
        for _ in 0..500 {
            if !context.iteration(false) {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Run under a display, as the only display test of the process because it
/// registers the application:
/// `GDK_BACKEND=x11 GSETTINGS_BACKEND=memory xvfb-run -a cargo test --lib
/// every_sidebar_section_switch -- --ignored`.
#[test]
#[ignore = "needs a display"]
fn every_sidebar_section_switch_settles_on_the_chosen_section() {
    let data = tempfile::tempdir().unwrap();
    let _app: RelmApp<Input> = RelmApp::new("dev.luminusos.ZapTide.SidebarSections");
    let app = NativeApplication::builder().launch(Init {
        dirs: crate::paths::AppDirs::under(data.path()),
    });
    let window = app.widget().clone();
    window.present();
    spin(std::time::Duration::from_millis(1000));
    let group = find_toggle_group(window.upcast_ref()).expect("sidebar section toggle");
    let notifications = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let counter = notifications.clone();
    group.connect_active_name_notify(move |_| counter.set(counter.get() + 1));

    let switch_to = |section: &str, from: &str| {
        notifications.set(0);
        group.set_active_name(Some(section));
        spin(std::time::Duration::from_millis(300));
        let first_wave = notifications.get();
        spin(std::time::Duration::from_millis(200));
        assert!(
            first_wave <= CLICK_AND_ITS_TWO_SYNCS && notifications.get() == first_wave,
            "{from} to {section}: the toggle changed {first_wave} times, then {} in all",
            notifications.get()
        );
        assert_eq!(group.active_name().as_deref(), Some(section));
        let model = app.model();
        assert_eq!(model.calls.tab, section == "calls", "{from} to {section}");
        assert_eq!(
            model.showing_archived(),
            section == "archived",
            "{from} to {section}"
        );
    };
    let mut current = "chats";
    for from in SECTIONS {
        for to in SECTIONS {
            switch_to(from, current);
            switch_to(to, from);
            current = to;
        }
    }

    let mut call = crate::model::call::CallSession::new(
        "test-call".into(),
        "test-chat".into(),
        crate::model::call::CallDirection::Outgoing,
        false,
        100,
    );
    call.phase = crate::model::call::CallPhase::Connecting;
    app.emit(Input::TestCall(call.clone()));
    spin(std::time::Duration::from_millis(300));
    assert!(app.model().calls.showing_live);
    assert!(app.model().calls.live_view.is_mapped());
    assert_eq!(
        app.widgets().page_stack.visible_child_name().as_deref(),
        Some("chats")
    );

    app.emit(Input::ShowCalls(true));
    spin(std::time::Duration::from_millis(300));
    assert!(!app.model().calls.showing_live);
    assert!(!app.model().calls.live_view.is_mapped());
    let account = app.model().active_account;
    app.emit(Input::CallChoice(
        account,
        "stale-call".into(),
        crate::native_notifications::CallChoice::Show,
    ));
    spin(std::time::Duration::from_millis(100));
    assert!(!app.model().calls.showing_live);
    app.emit(Input::CallChoice(
        account,
        "test-call".into(),
        crate::native_notifications::CallChoice::Show,
    ));
    spin(std::time::Duration::from_millis(300));
    assert!(app.model().calls.showing_live);
    assert!(app.model().calls.live_view.is_mapped());

    app.emit(Input::ShowCalls(true));
    spin(std::time::Duration::from_millis(300));
    app.emit(Input::ShowLiveCall);
    spin(std::time::Duration::from_millis(300));
    assert!(app.model().calls.live_view.is_mapped());

    let return_button = find_return_to_call(window.upcast_ref()).expect("return to call button");
    assert!(return_button.is_visible());
    call.phase = crate::model::call::CallPhase::Ended(crate::model::call::CallOutcome::Completed);
    app.emit(Input::TestCall(call.clone()));
    spin(std::time::Duration::from_millis(300));
    assert!(
        !return_button.is_visible(),
        "finished calls must not offer to return"
    );

    call.phase = crate::model::call::CallPhase::Ended(crate::model::call::CallOutcome::Elsewhere);
    app.emit(Input::TestCall(call));
    spin(std::time::Duration::from_millis(300));
    assert!(!app.model().calls.has_live());
    assert!(app.model().calls.live_view.first_child().is_none());
}
