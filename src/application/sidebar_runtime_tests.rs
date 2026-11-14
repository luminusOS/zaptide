use super::*;
use relm4::ComponentController;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

fn wait_until(description: &str, mut ready: impl FnMut() -> bool) {
    let context = gtk::glib::MainContext::default();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}"
        );
        context.iteration(false);
    }
}

fn wait_for_frames(widget: &impl IsA<gtk::Widget>, count: u32) {
    let frames = Rc::new(Cell::new(0));
    let observed = frames.clone();
    widget.add_tick_callback(move |_, _| {
        let next = observed.get() + 1;
        observed.set(next);
        if next >= count {
            gtk::glib::ControlFlow::Break
        } else {
            gtk::glib::ControlFlow::Continue
        }
    });
    widget.queue_draw();
    wait_until("frame clock", || frames.get() >= count);
}

fn fixture_chats() -> Vec<crate::model::Chat> {
    (0..300)
        .map(|index| {
            let mut chat = crate::model::Chat::new(
                format!("fixture-{index:03}@s.whatsapp.net"),
                format!("Fixture {index:03}"),
            );
            chat.last_activity = 300 - i64::from(index);
            chat
        })
        .collect()
}

fn send_event(events: &mpsc::Sender<Event>, notifier: &EventNotifier, event: Event) {
    events.send(event).expect("detached backend event receiver");
    crate::backend::Wake::wake(notifier);
}

fn row_ids(model: &NativeApplication) -> Vec<String> {
    (0..model.chats.len())
        .map(|position| {
            model
                .chats
                .get_visible(position)
                .expect("chat model row")
                .borrow()
                .id
                .clone()
        })
        .collect()
}

fn row_wrapper(list: &gtk::ListView, id: &str) -> Option<gtk::Widget> {
    fn find(widget: &gtk::Widget, id: &str) -> Option<gtk::Widget> {
        if widget.widget_name() == id && widget.has_css_class("zaptide-chat-item") {
            return widget.parent();
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(found) = find(&current, id) {
                return Some(found);
            }
            child = current.next_sibling();
        }
        None
    }
    find(list.upcast_ref(), id)
}

fn wrapper_y(list: &gtk::ListView, scroller: &gtk::ScrolledWindow, id: &str) -> Option<f32> {
    let wrapper = row_wrapper(list, id)?;
    if !wrapper.is_mapped() || wrapper.height() <= 0 {
        return None;
    }
    Some(wrapper.compute_bounds(scroller)?.y())
}

fn first_visible_row(
    list: &gtk::ListView,
    scroller: &gtk::ScrolledWindow,
    ids: &[String],
) -> Option<(String, f32)> {
    ids.iter()
        .filter_map(|id| {
            let wrapper = row_wrapper(list, id)?;
            if !wrapper.is_mapped() || wrapper.height() <= 0 {
                return None;
            }
            let bounds = wrapper.compute_bounds(scroller)?;
            (bounds.y() + bounds.height() > 0.0 && bounds.y() < scroller.height() as f32)
                .then(|| (id.clone(), bounds.y()))
        })
        .min_by(|left, right| left.1.total_cmp(&right.1))
}

fn first_model_id(list: &gtk::ListView) -> Option<String> {
    let item = list.model()?.item(0)?;
    let item = item.downcast::<gtk::glib::BoxedAnyObject>().ok()?;
    let id = item.borrow::<ChatRow>().id.clone();
    Some(id)
}

fn middle_visible_row(
    list: &gtk::ListView,
    scroller: &gtk::ScrolledWindow,
    ids: &[String],
) -> (String, f32) {
    let midpoint = scroller.height() as f32 / 2.0;
    ids.iter()
        .filter_map(|id| wrapper_y(list, scroller, id).map(|y| (id.clone(), y)))
        .filter(|(_, y)| *y > 100.0 && *y < scroller.height() as f32 - 100.0)
        .min_by(|left, right| {
            (left.1 - midpoint)
                .abs()
                .total_cmp(&(right.1 - midpoint).abs())
        })
        .expect("visible middle row")
}

fn copy_control_rows(
    control: &mut TypedListView<ChatRow, gtk::SingleSelection>,
    model: &NativeApplication,
    ids: &[String],
) {
    control.clear();
    control.extend_from_iter(ids.iter().map(|id| {
        let position = (0..model.chats.len())
            .find(|&position| model.chats.get_visible(position).unwrap().borrow().id == *id)
            .unwrap();
        let item = model.chats.get_visible(position).unwrap();
        let row = item.borrow();
        ChatRow {
            id: row.id.clone(),
            last_activity: row.last_activity,
            name: row.name.clone(),
            preview: row.preview.clone(),
            unread: row.unread.clone(),
            avatar: row.avatar.clone(),
            pinned: row.pinned,
            muted: row.muted,
            quiet: row.quiet,
            delivery: row.delivery,
            open: row.open,
        }
    }));
    control
        .selection_model
        .set_selected(model.chats.selection_model.selected());
}

fn native_x11_input(window: &gtk::Window, scroller: &gtk::ScrolledWindow, action: &str) {
    assert_eq!(
        std::env::var("ZAPTIDE_TEST_PRIVATE_XVFB").as_deref(),
        Ok("1")
    );
    let bounds = scroller.compute_bounds(window).expect("scroller bounds");
    let x = (bounds.x() + bounds.width() / 2.0) as i32;
    let y = (bounds.y() + bounds.height() / 2.0) as i32;
    let script = r#"
import ctypes as c, sys
x = c.CDLL('libX11.so.6'); t = c.CDLL('libXtst.so.6')
x.XOpenDisplay.argtypes=[c.c_char_p]; x.XOpenDisplay.restype=c.c_void_p
x.XDefaultRootWindow.argtypes=[c.c_void_p]; x.XDefaultRootWindow.restype=c.c_ulong
x.XQueryTree.argtypes=[c.c_void_p,c.c_ulong,c.POINTER(c.c_ulong),c.POINTER(c.c_ulong),c.POINTER(c.POINTER(c.c_ulong)),c.POINTER(c.c_uint)]
x.XFetchName.argtypes=[c.c_void_p,c.c_ulong,c.POINTER(c.c_void_p)]
x.XTranslateCoordinates.argtypes=[c.c_void_p,c.c_ulong,c.c_ulong,c.c_int,c.c_int,c.POINTER(c.c_int),c.POINTER(c.c_int),c.POINTER(c.c_ulong)]
x.XStringToKeysym.argtypes=[c.c_char_p]; x.XStringToKeysym.restype=c.c_ulong
x.XKeysymToKeycode.argtypes=[c.c_void_p,c.c_ulong]; x.XKeysymToKeycode.restype=c.c_ubyte
x.XFree.argtypes=[c.c_void_p]; x.XFlush.argtypes=[c.c_void_p]
x.XSetInputFocus.argtypes=[c.c_void_p,c.c_ulong,c.c_int,c.c_ulong]
t.XTestFakeMotionEvent.argtypes=[c.c_void_p,c.c_int,c.c_int,c.c_int,c.c_ulong]
t.XTestFakeButtonEvent.argtypes=[c.c_void_p,c.c_uint,c.c_int,c.c_ulong]
t.XTestFakeKeyEvent.argtypes=[c.c_void_p,c.c_uint,c.c_int,c.c_ulong]
d=x.XOpenDisplay(None)
if not d: raise RuntimeError('cannot open private display')
root=x.XDefaultRootWindow(d)
def find(w):
    name=c.c_void_p()
    if x.XFetchName(d,w,c.byref(name)) and name.value:
        value=c.string_at(name.value); x.XFree(name)
        if value == sys.argv[4].encode(): return w
    r=c.c_ulong(); p=c.c_ulong(); children=c.POINTER(c.c_ulong)(); n=c.c_uint()
    if not x.XQueryTree(d,w,c.byref(r),c.byref(p),c.byref(children),c.byref(n)): return None
    result=None
    for child in children[:n.value]:
        result=find(child)
        if result: break
    if children: x.XFree(children)
    return result
w=find(root)
if not w: raise RuntimeError('fixture window not found')
rx=c.c_int(); ry=c.c_int(); child=c.c_ulong()
if not x.XTranslateCoordinates(d,w,root,int(sys.argv[1]),int(sys.argv[2]),c.byref(rx),c.byref(ry),c.byref(child)): raise RuntimeError('cannot translate coordinates')
if not t.XTestFakeMotionEvent(d,-1,rx.value,ry.value,0): raise RuntimeError('cannot move pointer')
action=sys.argv[3]
if action == 'wheel-down':
    for pressed in (1,0):
        if not t.XTestFakeButtonEvent(d,5,pressed,0): raise RuntimeError('cannot send wheel')
else:
    x.XSetInputFocus(d,w,1,0)
    shifted=action.startswith('Shift+')
    key=x.XKeysymToKeycode(d,x.XStringToKeysym(action.removeprefix('Shift+').encode()))
    shift=x.XKeysymToKeycode(d,x.XStringToKeysym(b'Shift_L')) if shifted else None
    if not key or (shifted and not shift): raise RuntimeError('unknown key')
    try:
        if shift and not t.XTestFakeKeyEvent(d,shift,1,0): raise RuntimeError('cannot press shift')
        for pressed in (1,0):
            if not t.XTestFakeKeyEvent(d,key,pressed,0): raise RuntimeError('cannot send key')
    finally:
        if shift: t.XTestFakeKeyEvent(d,shift,0,0)
x.XFlush(d)
"#;
    let output = std::process::Command::new("python3")
        .args([
            "-c",
            script,
            &x.to_string(),
            &y.to_string(),
            action,
            window.title().as_deref().expect("fixture title"),
        ])
        .output()
        .expect("run X11 test input");
    assert!(
        output.status.success(),
        "X11 fixture input failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn next_snapshot_y(
    bridge: &super::sidebar_coordinates::SidebarCoordinates,
    scroller: &gtk::ScrolledWindow,
    id: &str,
) -> Rc<RefCell<Option<Option<f32>>>> {
    let result = Rc::new(RefCell::new(None));
    let observed = result.clone();
    let scroller = scroller.downgrade();
    let id = id.to_owned();
    bridge.observe_snapshot(move |list| {
        if observed.borrow().is_none() {
            *observed.borrow_mut() = Some(
                scroller
                    .upgrade()
                    .and_then(|scroller| wrapper_y(list, &scroller, &id)),
            );
        }
    });
    result
}

fn assert_snapshot_y(result: &RefCell<Option<Option<f32>>>, expected: f32) {
    wait_until("first snapshot after cancellation", || {
        result.borrow().is_some()
    });
    let actual = result
        .borrow()
        .expect("snapshot observed")
        .expect("reference row materialized");
    assert!(
        (actual - expected).abs() <= 2.0,
        "first snapshot {actual} differs from native {expected}"
    );
}

fn native_cancel_reference(
    control: &mut TypedListView<ChatRow, gtk::SingleSelection>,
    control_window: &gtk::Window,
    control_scroller: &gtk::ScrolledWindow,
    app: &relm4::component::Connector<NativeApplication>,
    final_ids: &[String],
    real: &gtk::Adjustment,
    unmap: bool,
) -> (String, f32) {
    let real_value = real.value();
    let page_size = real.page_size();
    let old_ids = row_ids(&app.model());
    copy_control_rows(control, &app.model(), &old_ids);
    control_window.present();
    gtk::prelude::GtkWindowExt::set_focus(control_window, None::<&gtk::Widget>);
    wait_for_frames(control_window, 2);
    let adjustment = control_scroller.vadjustment();
    control_window.set_default_size(
        control_window.width(),
        control_window.height() + (page_size - adjustment.page_size()) as i32,
    );
    wait_for_frames(control_window, 2);
    assert_eq!(adjustment.page_size(), page_size);
    adjustment.set_value(real_value);
    wait_for_frames(control_window, 2);
    assert!((adjustment.value() - real_value).abs() <= 2.0);
    let anchor = old_ids
        .iter()
        .filter_map(|id| {
            wrapper_y(&control.view, control_scroller, id)
                .filter(|y| *y >= 0.0)
                .map(|y| (id.clone(), y))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .expect("native baseline anchor")
        .0;
    let selected = control.selection_model.selected();
    let selected_id = old_ids.get(selected as usize);
    copy_control_rows(control, &app.model(), final_ids);
    control.selection_model.set_selected(
        selected_id
            .and_then(|id| final_ids.iter().position(|item| item == id))
            .map_or(gtk::INVALID_LIST_POSITION, |position| position as u32),
    );
    let position = final_ids.iter().position(|id| id == &anchor).unwrap() as u32;
    control
        .view
        .scroll_to(position, gtk::ListScrollFlags::NONE, None);
    if unmap {
        control_window.set_visible(false);
    }
    let value = adjustment.value();
    let delta = if value + 5.0 <= adjustment.upper() - adjustment.page_size() {
        5.0
    } else {
        -5.0
    };
    adjustment.set_value(value + delta);
    control.view.scroll_to(
        0,
        gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT,
        None,
    );
    adjustment.set_value(0.0);
    let result = Rc::new(RefCell::new(None));
    let observed = result.clone();
    let view = control.view.downgrade();
    let scroller = control_scroller.downgrade();
    let ids = final_ids.to_vec();
    let clock = control_window.frame_clock().unwrap();
    let handler = clock.connect_after_paint(move |_| {
        if observed.borrow().is_none() {
            let view = view.upgrade().unwrap();
            let scroller = scroller.upgrade().unwrap();
            *observed.borrow_mut() =
                Some(first_visible_row(&view, &scroller, &ids).expect("native first painted row"));
        }
    });
    if unmap {
        control_window.present();
    }
    control_window.queue_draw();
    wait_until("native cancellation first frame", || {
        result.borrow().is_some()
    });
    clock.disconnect(handler);
    let reference = result.borrow().clone().unwrap();
    control_window.set_visible(false);
    reference
}

#[test]
#[ignore = "requires private Xvfb, Python, X11 and XTest"]
fn native_wheel_and_keyboard_survive_backend_reorder() {
    let data = tempfile::tempdir().expect("private fixture directory");
    let _relm: RelmApp<Input> = RelmApp::new("dev.luminusos.ZapTide.SidebarNativeInputTest");
    relm4::main_application()
        .register(None::<&gtk::gio::Cancellable>)
        .expect("register test application");
    gtk::Settings::default()
        .unwrap()
        .set_gtk_enable_animations(false);
    let (backend, events) = Backend::detached();
    TEST_BACKEND.with_borrow_mut(|slot| assert!(slot.replace(backend).is_none()));
    let app = NativeApplication::builder().launch(Init {
        dirs: AppDirs::under(data.path()),
    });
    let notifier = app.model().notifier.clone();
    let window = app.widget().clone();
    window.set_default_size(1100, 760);
    window.set_title(Some("ZapTide Sidebar Input Fixture"));
    window.present();
    send_event(&events, &notifier, Event::Link(LinkStatus::Connected));
    let mut chats = fixture_chats();
    send_event(&events, &notifier, Event::Chats(chats.clone()));
    wait_until("backend chats", || app.model().chats.len() == 300);
    let list = app.model().chats.view.clone();
    let bridge = list
        .parent()
        .and_downcast::<super::sidebar_coordinates::SidebarCoordinates>()
        .unwrap();
    let scroller = bridge
        .ancestor(gtk::ScrolledWindow::static_type())
        .unwrap()
        .downcast::<gtk::ScrolledWindow>()
        .unwrap();
    wait_for_frames(&window, 2);
    wait_until("mapped sidebar", || {
        list.is_mapped() && scroller.vadjustment().upper() > scroller.vadjustment().page_size()
    });
    let adjustment = scroller.vadjustment();
    adjustment.set_value((adjustment.upper() - adjustment.page_size()) * 0.35);
    wait_for_frames(&window, 2);
    let scroll_events = Rc::new(RefCell::new(Vec::new()));
    let observed = scroll_events.clone();
    let controller = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    controller.connect_scroll(move |_, _, dy| {
        observed.borrow_mut().push(dy);
        gtk::glib::Propagation::Proceed
    });
    scroller.add_controller(controller);
    let mut control = TypedListView::<ChatRow, gtk::SingleSelection>::new();
    control.view.action_set_enabled("list.select-item", false);
    control.view.add_css_class("navigation-sidebar");
    control.view.add_css_class("zaptide-chat-list");
    copy_control_rows(&mut control, &app.model(), &row_ids(&app.model()));
    let control_scroller = gtk::ScrolledWindow::new();
    control_scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    control_scroller.set_child(Some(&control.view));
    let control_window = gtk::Window::builder()
        .title("ZapTide Native Control Fixture")
        .default_width(scroller.width())
        .default_height(scroller.height())
        .child(&control_scroller)
        .build();
    control_window.present();
    wait_for_frames(&control_window, 2);
    let control_adjustment = control_scroller.vadjustment();
    wait_until("mapped native control", || {
        control.view.is_mapped() && control_adjustment.upper() > control_adjustment.page_size()
    });
    let height_correction = (control_adjustment.page_size() - adjustment.page_size()) as i32;
    control_window.set_default_size(scroller.width(), scroller.height() - height_correction);
    wait_for_frames(&control_window, 2);
    assert_eq!(control_adjustment.page_size(), adjustment.page_size());
    assert_eq!(control.view.height(), list.height());
    control_adjustment
        .set_value((control_adjustment.upper() - control_adjustment.page_size()) * 0.35);
    wait_for_frames(&control_window, 2);
    let control_events = Rc::new(RefCell::new(Vec::new()));
    let observed = control_events.clone();
    let controller = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    controller.connect_scroll(move |_, _, dy| {
        observed.borrow_mut().push(dy);
        gtk::glib::Propagation::Proceed
    });
    control_scroller.add_controller(controller);
    let native_before = control_adjustment.value();
    native_x11_input(&control_window, &control_scroller, "wheel-down");
    wait_until("native control wheel event", || {
        !control_events.borrow().is_empty()
    });
    wait_for_frames(&control_window, 3);
    let native_delta = control_adjustment.value() - native_before;
    assert!(native_delta > 0.0);
    assert_eq!(control_events.borrow().len(), 1);
    assert!(control_events.borrow()[0] > 0.0);
    control_window.set_visible(false);
    window.present();
    wait_for_frames(&window, 2);
    let real = list.vadjustment().unwrap();
    let ids = row_ids(&app.model());
    let (first_id, first_y) = middle_visible_row(&list, &scroller, &ids);
    let real_before = real.value();
    let before = adjustment.value();
    native_x11_input(window.upcast_ref(), &scroller, "wheel-down");
    wait_until("native wheel event", || !scroll_events.borrow().is_empty());
    wait_for_frames(&window, 3);
    let first_delta = adjustment.value() - before;
    let first_real_delta = real.value() - real_before;
    let first_after_y = wrapper_y(&list, &scroller, &first_id).expect("first wheel anchor");
    assert!(first_delta > 0.0, "wheel did not scroll: {first_delta}");
    assert!(
        (first_real_delta - native_delta).abs() <= 2.0,
        "candidate first wheel {first_real_delta} differs from native control {native_delta}"
    );
    assert!((first_delta - first_real_delta).abs() <= 0.01);
    assert!((f64::from(first_y - first_after_y) - first_real_delta).abs() <= 2.0);
    assert!(scroll_events.borrow().iter().all(|dy| *dy > 0.0));
    let first_events = scroll_events.borrow().len();
    assert_eq!(first_events, 1);
    assert_eq!(scroll_events.borrow()[0], control_events.borrow()[0]);
    let mut promoted = chats.remove(250);
    promoted.last_activity = 1000;
    chats.insert(0, promoted);
    let expected: Vec<_> = chats.iter().map(|chat| chat.id.clone()).collect();
    send_event(&events, &notifier, Event::Chats(chats.clone()));
    wait_until("reordered chats", || row_ids(&app.model()) == expected);
    wait_for_frames(&window, 2);
    let (second_id, second_y) = middle_visible_row(&list, &scroller, &expected);
    let real_before = real.value();
    let before = adjustment.value();
    native_x11_input(window.upcast_ref(), &scroller, "wheel-down");
    wait_until("second native wheel event", || {
        scroll_events.borrow().len() > first_events
    });
    wait_for_frames(&window, 3);
    let second_delta = adjustment.value() - before;
    let second_real_delta = real.value() - real_before;
    let second_after_y = wrapper_y(&list, &scroller, &second_id).expect("second wheel anchor");
    eprintln!(
        "native wheel: first={first_delta} second={second_delta} events={:?}",
        *scroll_events.borrow()
    );
    assert!(second_delta > 0.0 && (second_delta - first_delta).abs() <= 2.0);
    assert!(
        (second_real_delta - native_delta).abs() <= 2.0,
        "candidate second wheel {second_real_delta} differs from native control {native_delta}"
    );
    assert_eq!(scroll_events.borrow().len(), first_events + 1);
    assert_eq!(scroll_events.borrow()[1], control_events.borrow()[0]);
    assert!((second_delta - second_real_delta).abs() <= 0.01);
    assert!((f64::from(second_y - second_after_y) - second_real_delta).abs() <= 2.0);
    assert!(adjustment.value() < adjustment.upper() - adjustment.page_size() - 100.0);
    let account = app.model().active_account;
    chats.reverse();
    for (index, chat) in chats.iter_mut().enumerate() {
        chat.last_activity = 1000 - index as i64;
    }
    let expected: Vec<_> = chats.iter().map(|chat| chat.id.clone()).collect();
    send_event(&events, &notifier, Event::Chats(chats.clone()));
    wait_until("pending backend reorder", || {
        row_ids(&app.model()) == expected && bridge.restore_pending()
    });
    let selected_id = expected[150].clone();
    app.emit(Input::SelectChat(account, selected_id.clone()));
    wait_until("selected chat", || {
        app.model().open_chat.active_chat.as_deref() == Some(selected_id.as_str())
    });
    assert!(
        !bridge.restore_pending(),
        "explicit chat navigation cancels the pending restore"
    );
    let before = adjustment.value();
    let seen = scroll_events.borrow().len();
    native_x11_input(window.upcast_ref(), &scroller, "wheel-down");
    wait_until("native wheel after navigation", || {
        scroll_events.borrow().len() > seen
    });
    wait_for_frames(&window, 3);
    assert!(
        adjustment.value() > before,
        "wheel must work after cancellation"
    );
    assert!(list.grab_focus());
    native_x11_input(window.upcast_ref(), &scroller, "Down");
    wait_until("native Down selection", || {
        app.model().chats.selection_model.selected() == 151
    });
    native_x11_input(window.upcast_ref(), &scroller, "Up");
    wait_until("native Up selection", || {
        app.model().chats.selection_model.selected() == 150
    });
    native_x11_input(window.upcast_ref(), &scroller, "Down");
    wait_until("native Down selection again", || {
        app.model().chats.selection_model.selected() == 151
    });
    native_x11_input(window.upcast_ref(), &scroller, "Return");
    wait_until("native Enter opens chat", || {
        app.model().open_chat.active_chat.as_deref() == Some(expected[151].as_str())
    });
    wait_for_frames(&window, 2);
    assert!(list.grab_focus());
    let original_focus = gtk::prelude::GtkWindowExt::focus(&window).expect("initial focus");
    native_x11_input(window.upcast_ref(), &scroller, "Tab");
    wait_until("Tab moves focus", || {
        gtk::prelude::GtkWindowExt::focus(&window).is_some_and(|focus| focus != original_focus)
    });
    native_x11_input(window.upcast_ref(), &scroller, "Shift+Tab");
    wait_until("Shift-Tab restores focus", || {
        gtk::prelude::GtkWindowExt::focus(&window).as_ref() == Some(&original_focus)
    });
    copy_control_rows(&mut control, &app.model(), &expected);
    control_window.present();
    wait_for_frames(&control_window, 2);
    control_window.set_default_size(
        control_window.width(),
        control_window.height()
            + (scroller.vadjustment().page_size() - control_adjustment.page_size()) as i32,
    );
    wait_for_frames(&control_window, 2);
    assert_eq!(
        control_adjustment.page_size(),
        scroller.vadjustment().page_size()
    );
    control_window.set_visible(false);
    let mut keyboard_errors = Vec::new();
    for key in ["Home", "End", "Prior", "Next"] {
        control_window.present();
        control.view.scroll_to(
            150,
            gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT,
            None,
        );
        wait_for_frames(&control_window, 2);
        native_x11_input(&control_window, &control_scroller, key);
        wait_for_frames(&control_window, 3);
        let native_selected = control.selection_model.selected();
        let (native_id, native_y) = first_visible_row(&control.view, &control_scroller, &expected)
            .expect("native keyboard visible row");
        control_window.set_visible(false);
        window.present();
        list.scroll_to(
            150,
            gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT,
            None,
        );
        assert!(list.grab_focus());
        wait_for_frames(&window, 2);
        native_x11_input(window.upcast_ref(), &scroller, key);
        wait_for_frames(&window, 3);
        let selected = app.model().chats.selection_model.selected();
        let actual_y = wrapper_y(&list, &scroller, &native_id);
        if selected != native_selected || actual_y.is_none_or(|y| (y - native_y).abs() > 2.0) {
            keyboard_errors.push(format!(
                "{key}: selected {selected}/{native_selected}, y={actual_y:?}/{native_y}"
            ));
        }
    }
    adjustment.set_value((adjustment.upper() - adjustment.page_size()) * 0.35);
    wait_for_frames(&window, 2);
    chats.reverse();
    for (index, chat) in chats.iter_mut().enumerate() {
        chat.last_activity = 1000 - index as i64;
    }
    let expected: Vec<_> = chats.iter().map(|chat| chat.id.clone()).collect();
    let (swap_id, swap_y) = native_cancel_reference(
        &mut control,
        &control_window,
        &control_scroller,
        &app,
        &expected,
        &real,
        false,
    );
    window.present();
    wait_for_frames(&window, 2);
    send_event(&events, &notifier, Event::Chats(chats.clone()));
    wait_until("pending reorder before adjustment replacement", || {
        row_ids(&app.model()) == expected && bridge.restore_pending()
    });
    let old_proxy = scroller.vadjustment();
    let replacement = gtk::Adjustment::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    scroller.set_vadjustment(Some(&replacement));
    assert!(!bridge.restore_pending());
    let real_before = real.value();
    let old_before = old_proxy.value();
    let old_delta = if old_before + 5.0 <= old_proxy.upper() - old_proxy.page_size() {
        5.0
    } else {
        -5.0
    };
    old_proxy.set_value(old_before + old_delta);
    assert_eq!(old_proxy.value(), old_before + old_delta);
    assert_eq!(
        real.value(),
        real_before,
        "old adjustment must be disconnected"
    );
    let proxy_before = replacement.value();
    let delta = if proxy_before + 5.0 <= replacement.upper() - replacement.page_size() {
        5.0
    } else {
        -5.0
    };
    replacement.set_value(proxy_before + delta);
    assert_eq!(replacement.value(), proxy_before + delta);
    assert_eq!(real.value(), real_before + delta);
    list.scroll_to(
        0,
        gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT,
        None,
    );
    real.set_value(0.0);
    assert_eq!(real.value(), 0.0);
    let snapshot = next_snapshot_y(&bridge, &scroller, &swap_id);
    window.queue_draw();
    assert_snapshot_y(&snapshot, swap_y);
    chats.reverse();
    for (index, chat) in chats.iter_mut().enumerate() {
        chat.last_activity = 1000 - index as i64;
    }
    let expected: Vec<_> = chats.iter().map(|chat| chat.id.clone()).collect();
    replacement
        .set_value((replacement.upper() - replacement.page_size() + replacement.lower()) / 2.0);
    wait_for_frames(&window, 2);
    let (remap_id, remap_y) = native_cancel_reference(
        &mut control,
        &control_window,
        &control_scroller,
        &app,
        &expected,
        &real,
        true,
    );
    window.present();
    wait_for_frames(&window, 2);
    send_event(&events, &notifier, Event::Chats(chats));
    wait_until("pending reorder before unmap", || {
        row_ids(&app.model()) == expected && bridge.restore_pending()
    });
    window.set_visible(false);
    wait_until("sidebar unmap", || !list.is_mapped());
    assert!(!bridge.restore_pending());
    let real_before = real.value();
    let proxy_before = replacement.value();
    let delta = if proxy_before + 5.0 <= replacement.upper() - replacement.page_size() {
        5.0
    } else {
        -5.0
    };
    replacement.set_value(proxy_before + delta);
    assert_eq!(replacement.value(), proxy_before + delta);
    assert_eq!(real.value(), real_before + delta);
    list.scroll_to(
        0,
        gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT,
        None,
    );
    real.set_value(0.0);
    assert_eq!(real.value(), 0.0);
    let snapshot = next_snapshot_y(&bridge, &scroller, &remap_id);
    window.present();
    assert_snapshot_y(&snapshot, remap_y);
    assert!(keyboard_errors.is_empty(), "{}", keyboard_errors.join("; "));
    control_window.destroy();
    window.destroy();
    drop(app);
}

#[test]
#[ignore = "requires a display and a dedicated test process"]
fn backend_reorder_preserves_the_first_snapshot_and_open_chat() {
    let data = tempfile::tempdir().expect("private fixture directory");
    let _relm: RelmApp<Input> = RelmApp::new("dev.luminusos.ZapTide.SidebarRuntimeTest");
    let application = relm4::main_application();
    application
        .register(None::<&gtk::gio::Cancellable>)
        .expect("register test application");
    gtk::Settings::default()
        .expect("GTK settings")
        .set_gtk_enable_animations(false);
    let (backend, events) = Backend::detached();
    TEST_BACKEND.with_borrow_mut(|slot| {
        assert!(
            slot.replace(backend).is_none(),
            "test backend slot is occupied"
        );
    });
    let app = NativeApplication::builder().launch(Init {
        dirs: AppDirs::under(data.path()),
    });
    TEST_BACKEND.with_borrow(|slot| assert!(slot.is_none(), "fixture backend was not consumed"));
    let notifier = app.model().notifier.clone();
    let window = app.widget().clone();
    window.set_default_size(1100, 760);
    window.present();
    send_event(&events, &notifier, Event::Link(LinkStatus::Connected));
    wait_until("connected link", || app.model().link.is_connected());
    let mut chats = fixture_chats();
    let initial_ids: Vec<_> = chats.iter().map(|chat| chat.id.clone()).collect();
    send_event(&events, &notifier, Event::Chats(chats.clone()));
    wait_until("initial chat projection", || {
        row_ids(&app.model()) == initial_ids
    });
    let account = app.model().active_account;
    let open_id = chats[150].id.clone();
    app.emit(Input::SelectChat(account, open_id.clone()));
    wait_until("open chat", || {
        app.model().open_chat.active_chat.as_deref() == Some(open_id.as_str())
    });
    let list = app.model().chats.view.clone();
    let bridge = list
        .parent()
        .and_downcast::<super::sidebar_coordinates::SidebarCoordinates>()
        .expect("chat list coordinate bridge");
    let scroller = bridge
        .ancestor(gtk::ScrolledWindow::static_type())
        .expect("chat list scroll window")
        .downcast::<gtk::ScrolledWindow>()
        .expect("scrolled window type");
    wait_for_frames(&window, 2);
    eprintln!(
        "fixture window={}x{} list_mapped={} list_visible={} scroller_visible={} scroller={}x{} upper={} page={} linked={}",
        window.width(),
        window.height(),
        list.is_mapped(),
        list.is_visible(),
        scroller.is_visible(),
        scroller.width(),
        scroller.height(),
        scroller.vadjustment().upper(),
        scroller.vadjustment().page_size(),
        app.model().is_linked()
    );
    wait_until("mapped scrollable list", || {
        list.is_mapped()
            && scroller.height() > 0
            && scroller.vadjustment().upper() > scroller.vadjustment().page_size()
    });
    let adjustment = scroller.vadjustment();
    let scrollable = adjustment.upper() - adjustment.page_size() - adjustment.lower();
    adjustment.set_value(adjustment.lower() + scrollable * 0.35);
    wait_for_frames(&list, 2);
    let mut anchor = None;
    wait_until("visible off-top wrapper", || {
        anchor = first_visible_row(&list, &scroller, &initial_ids);
        adjustment.value() > adjustment.lower()
            && anchor.as_ref().is_some_and(|(id, _)| id != &initial_ids[0])
    });
    let (anchor_id, before_y) = anchor.expect("mapped anchor wrapper");
    let selected_before = app.model().chats.selection_model.selected();
    assert_ne!(selected_before, gtk::INVALID_LIST_POSITION);
    assert_eq!(
        app.model()
            .chats
            .get_visible(selected_before)
            .expect("selected chat")
            .borrow()
            .id,
        open_id
    );
    let mut promoted = chats.remove(250);
    promoted.last_activity = 1000;
    let promoted_id = promoted.id.clone();
    chats.insert(0, promoted);
    let expected: Vec<_> = chats.iter().map(|chat| chat.id.clone()).collect();
    let first_snapshot: Rc<RefCell<Option<Option<f32>>>> = Rc::new(RefCell::new(None));
    let observed = first_snapshot.clone();
    let observed_anchor = anchor_id.clone();
    let observed_scroller = scroller.downgrade();
    bridge.observe_snapshot(move |list| {
        if observed.borrow().is_none()
            && first_model_id(list).as_deref() == Some(promoted_id.as_str())
        {
            *observed.borrow_mut() = Some(
                observed_scroller
                    .upgrade()
                    .and_then(|scroller| wrapper_y(list, &scroller, &observed_anchor)),
            );
        }
    });
    send_event(&events, &notifier, Event::Chats(chats));
    wait_until("reordered backend projection", || {
        row_ids(&app.model()) == expected
    });
    wait_until("first snapshot after projection", || {
        first_snapshot.borrow().is_some()
    });
    let first_y = first_snapshot
        .borrow()
        .expect("snapshot observer ran")
        .expect("anchor wrapper missing from first snapshot");
    eprintln!("first snapshot: anchor={anchor_id} before={before_y} after={first_y}");
    assert!(
        (first_y - before_y).abs() <= 2.0,
        "anchor moved from {before_y} to {first_y} in first snapshot"
    );
    {
        let model = app.model();
        assert_eq!(row_ids(&model), expected);
        assert_eq!(
            model.open_chat.active_chat.as_deref(),
            Some(open_id.as_str())
        );
        for (position, id) in expected.iter().enumerate() {
            let position = u32::try_from(position).expect("fixture position fits u32");
            assert_eq!(
                model.chat_targets.borrow().get(position),
                Some((account, id.clone()))
            );
            assert_eq!(
                model
                    .chats
                    .get_visible(position)
                    .expect("projected chat row")
                    .borrow()
                    .open,
                id == &open_id
            );
        }
        let selected = model.chats.selection_model.selected();
        assert_ne!(selected, gtk::INVALID_LIST_POSITION);
        assert_eq!(
            model
                .chats
                .get_visible(selected)
                .expect("selected row after reorder")
                .borrow()
                .id,
            open_id
        );
    }
    window.destroy();
    drop(app);
}

#[test]
#[ignore = "requires a display and a dedicated test process"]
fn logout_and_account_switch_cancel_pending_restore() {
    let data = tempfile::tempdir().expect("private fixture directory");
    let _relm: RelmApp<Input> = RelmApp::new("dev.luminusos.ZapTide.SidebarLifecycleTest");
    relm4::main_application()
        .register(None::<&gtk::gio::Cancellable>)
        .unwrap();
    gtk::Settings::default()
        .unwrap()
        .set_gtk_enable_animations(false);
    let (backend, events) = Backend::detached();
    TEST_BACKEND.with_borrow_mut(|slot| assert!(slot.replace(backend).is_none()));
    let app = NativeApplication::builder().launch(Init {
        dirs: AppDirs::under(data.path()),
    });
    let notifier = app.model().notifier.clone();
    let window = app.widget().clone();
    window.set_default_size(1100, 760);
    window.present();
    send_event(&events, &notifier, Event::Link(LinkStatus::Connected));
    let mut chats = fixture_chats();
    send_event(&events, &notifier, Event::Chats(chats.clone()));
    wait_until("initial chats", || app.model().chats.len() == 300);
    let account = app.model().active_account;
    let open = chats[150].id.clone();
    app.emit(Input::SelectChat(account, open.clone()));
    wait_until("open initial chat", || {
        app.model().open_chat.active_chat.as_deref() == Some(&open)
    });
    let list = app.model().chats.view.clone();
    let bridge = list
        .parent()
        .and_downcast::<super::sidebar_coordinates::SidebarCoordinates>()
        .unwrap();
    let scroller = bridge
        .ancestor(gtk::ScrolledWindow::static_type())
        .unwrap()
        .downcast::<gtk::ScrolledWindow>()
        .unwrap();
    wait_for_frames(&window, 2);
    let adjustment = scroller.vadjustment();
    adjustment.set_value((adjustment.upper() - adjustment.page_size()) * 0.35);
    wait_for_frames(&window, 2);
    let mut promoted = chats.remove(250);
    promoted.last_activity = 1000;
    chats.insert(0, promoted);
    let expected: Vec<_> = chats.iter().map(|chat| chat.id.clone()).collect();
    send_event(&events, &notifier, Event::Chats(chats.clone()));
    wait_until("pending before logout", || {
        row_ids(&app.model()) == expected && bridge.restore_pending()
    });
    send_event(&events, &notifier, Event::Link(LinkStatus::LoggedOut));
    wait_until("logout applied", || {
        matches!(app.model().link, LinkStatus::LoggedOut)
    });
    assert!(app.model().chats.is_empty());
    assert!(app.model().open_chat.active_chat.is_none());
    assert!(!bridge.restore_pending());
    send_event(&events, &notifier, Event::Link(LinkStatus::Connected));
    send_event(&events, &notifier, Event::Chats(chats.clone()));
    wait_until("chats after reconnect", || {
        row_ids(&app.model()) == expected
    });
    wait_for_frames(&window, 2);
    adjustment.set_value((adjustment.upper() - adjustment.page_size()) * 0.35);
    wait_for_frames(&window, 2);
    let mut promoted = chats.remove(240);
    promoted.last_activity = 2000;
    chats.insert(0, promoted);
    let expected: Vec<_> = chats.iter().map(|chat| chat.id.clone()).collect();
    send_event(&events, &notifier, Event::Chats(chats.clone()));
    wait_until("new restore after logout", || {
        row_ids(&app.model()) == expected && bridge.restore_pending()
    });
    wait_for_frames(&window, 2);
    assert_eq!(row_ids(&app.model()), expected);
    let (new_backend, new_events) = Backend::detached();
    let (new_notifier, drain) = EventNotifier::new();
    let mut incoming = fixture_chats();
    for chat in &mut incoming {
        chat.name = format!("Second account {}", chat.name);
    }
    incoming.reverse();
    for (index, chat) in incoming.iter_mut().enumerate() {
        chat.last_activity = 1000 - index as i64;
    }
    let incoming_ids: Vec<_> = incoming.iter().map(|chat| chat.id.clone()).collect();
    let new_account;
    {
        let mut state = app.state().get_mut();
        let model = &mut state.model;
        new_account = model.registry.add();
        let entry = model.registry.entry_mut(new_account).unwrap();
        entry.linked = true;
        entry.name = Some("Synthetic account".into());
        let mut session = super::accounts::AccountSession::detached_for_tests(new_account);
        session.dirs = AppDirs::under(&data.path().join("second"));
        session.dirs.ensure().unwrap();
        session.backend = Some(new_backend);
        session.notifier = new_notifier.clone();
        session.link = LinkStatus::Connected;
        session.chat_snapshots = incoming.iter().cloned().map(Rc::new).collect();
        let sender = app.sender().clone();
        session.drain = Some(GlibEventDrain::install(
            &new_notifier,
            drain,
            gtk::glib::MainContext::default(),
            move || {
                let _ = sender.send(Input::BackendReady(new_account));
            },
        ));
        model.accounts.insert(new_account, session);
    }
    let mut promoted = chats.remove(230);
    promoted.last_activity = 3000;
    chats.insert(0, promoted);
    let expected: Vec<_> = chats.iter().map(|chat| chat.id.clone()).collect();
    send_event(&events, &notifier, Event::Chats(chats));
    wait_until("pending before account switch", || {
        row_ids(&app.model()) == expected && bridge.restore_pending()
    });
    app.emit(Input::SwitchAccount(new_account));
    wait_until("account switched", || {
        app.model().active_account == new_account
    });
    assert_eq!(row_ids(&app.model()), incoming_ids);
    assert!(app.model().open_chat.active_chat.is_none());
    assert!(
        !bridge.restore_pending(),
        "old account restore must be cancelled at switch"
    );
    wait_for_frames(&window, 2);
    assert_eq!(row_ids(&app.model()), incoming_ids);
    let mut promoted = incoming.remove(250);
    promoted.last_activity = 4000;
    incoming.insert(0, promoted);
    let incoming_ids: Vec<_> = incoming.iter().map(|chat| chat.id.clone()).collect();
    adjustment.set_value((adjustment.upper() - adjustment.page_size()) * 0.35);
    wait_for_frames(&window, 2);
    send_event(&new_events, &new_notifier, Event::Chats(incoming));
    wait_until("new account notifier and restore", || {
        row_ids(&app.model()) == incoming_ids && bridge.restore_pending()
    });
    let mut hidden_update = fixture_chats();
    hidden_update[0].name = "Old account update".into();
    send_event(&events, &notifier, Event::Chats(hidden_update));
    wait_until("old account notifier delivered to hidden session", || {
        app.model().accounts.get(&account).is_some_and(|session| {
            session
                .chat_snapshots
                .first()
                .is_some_and(|chat| chat.name == "Old account update")
        })
    });
    assert_eq!(row_ids(&app.model()), incoming_ids);
    assert!(
        bridge.restore_pending(),
        "old account callback cancelled new restore"
    );
    wait_for_frames(&window, 2);
    assert_eq!(row_ids(&app.model()), incoming_ids);
    assert_eq!(app.model().active_account, new_account);
    window.destroy();
    drop(app);
}
