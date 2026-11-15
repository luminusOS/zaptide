//! Scrolling and fade-in animations for lists.

use super::*;

pub(super) fn install_scroll_cancellation(scroller: &gtk::ScrolledWindow, view: &gtk::ListView) {
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    scroll.set_name(Some("zaptide-scroll-cancellation"));
    scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
    scroll.connect_scroll(|_, _, _| {
        cancel_auto_scroll();
        gtk::glib::Propagation::Proceed
    });
    scroller.add_controller(scroll);
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(|_, key, _, _| {
        use gtk::gdk::Key;
        if matches!(
            key,
            Key::Up | Key::Down | Key::Home | Key::End | Key::Page_Up | Key::Page_Down | Key::space
        ) {
            cancel_auto_scroll();
        }
        gtk::glib::Propagation::Proceed
    });
    scroller.add_controller(keys);
    let click = gtk::GestureClick::new();
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    click.connect_pressed(|_, _, _, _| cancel_auto_scroll());
    scroller.add_controller(click);
    view.connect_unmap(|_| cancel_auto_scroll());
}

fn cancel_auto_scroll() {
    SCROLL_GENERATION.set(SCROLL_GENERATION.get().wrapping_add(1));
    let previous = END_GLIDE.with_borrow_mut(|running| running.take());
    if let Some(previous) = previous {
        previous.pause();
    }
}

/// Moves a list to its end once rows are laid out. `ListView::scroll_to`
/// leaves blank space with rows of varying height, and the height estimate
/// only settles after the newly shown rows are measured, hence the second pass.
pub(super) fn scroll_to_end(view: &gtk::ListView) {
    cancel_auto_scroll();
    let generation = SCROLL_GENERATION.get();
    let Some(adjustment) = view.vadjustment() else {
        return;
    };
    let end = move || {
        if SCROLL_GENERATION.get() == generation {
            adjustment.set_value(adjustment.upper() - adjustment.page_size());
        }
    };
    let settle = end.clone();
    gtk::glib::idle_add_local_once(end);
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(150), settle);
}

thread_local! {
    static SCROLL_GENERATION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static END_GLIDE: std::cell::RefCell<Option<adw::TimedAnimation>> =
        const { std::cell::RefCell::new(None) };
}

pub(super) fn gliding() -> bool {
    END_GLIDE.with_borrow(|running| {
        running
            .as_ref()
            .is_some_and(|animation| animation.state() == adw::AnimationState::Playing)
    })
}

/// Scrolls smoothly to the end for new messages. Each frame aims at the
/// current end, so row heights settling mid-way bend the path instead of
/// jumping. Reduced motion skips straight to the end.
pub(super) fn glide_to_end(view: &gtk::ListView) {
    cancel_auto_scroll();
    let generation = SCROLL_GENERATION.get();
    let Some(adjustment) = view.vadjustment() else {
        return;
    };
    let start = adjustment.value();
    let target = adw::CallbackAnimationTarget::new(move |progress| {
        if SCROLL_GENERATION.get() != generation {
            return;
        }
        let end = adjustment.upper() - adjustment.page_size();
        adjustment.set_value(start + (end - start) * progress);
    });
    let animation = adw::TimedAnimation::new(view, 0.0, 1.0, 250, target);
    animation.set_easing(adw::Easing::EaseOutCubic);
    END_GLIDE.set(Some(animation.clone()));
    animation.play();
}

thread_local! {
    static ARRIVING: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Names the message whose row should fade in when the list first binds it.
/// Binding happens during layout, before the row's first frame, so nothing
/// flashes at full opacity. The mark expires in case the row is never shown.
pub(super) fn mark_arriving(id: String) {
    ARRIVING.set(Some(id.clone()));
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
        ARRIVING.with_borrow_mut(|arriving| {
            if arriving.as_deref() == Some(id.as_str()) {
                *arriving = None;
            }
        });
    });
}

/// Fades `row` in if its message was marked as arriving; otherwise makes sure a
/// recycled widget is fully opaque. Adwaita animations honour reduced motion by
/// jumping straight to the end value.
pub(super) fn fade_in_if_arriving(row: &gtk::Box, id: &str) {
    if ARRIVING
        .with_borrow_mut(|arriving| arriving.take_if(|marked| marked == id))
        .is_none()
    {
        row.set_opacity(1.0);
        return;
    }
    row.set_opacity(0.0);
    // An animation on an unmapped widget jumps to its end, and a freshly bound
    // row is not mapped yet; mapping still happens before its first frame.
    if row.is_mapped() {
        fade_in(row);
        return;
    }
    let id = id.to_owned();
    let handler = std::rc::Rc::new(std::cell::Cell::new(None));
    let pending = handler.clone();
    handler.set(Some(row.connect_map(move |row| {
        if let Some(handler) = pending.take() {
            row.disconnect(handler);
        }
        // The widget may have been recycled for another message meanwhile.
        if row.widget_name() == id.as_str() {
            fade_in(row);
        }
    })));
}

pub(super) fn fade_in(row: &gtk::Box) {
    let target = adw::PropertyAnimationTarget::new(row, "opacity");
    let animation = adw::TimedAnimation::new(row, 0.0, 1.0, 220, target);
    animation.set_easing(adw::Easing::EaseOutCubic);
    animation.play();
}

/// Scrolls so the row `id` at `position` sits near the top, or as high as the
/// end of the list allows. The row only exists once `scroll_to` has realized
/// it, so its place is read in a second pass.
pub(super) fn scroll_to_unread(view: &gtk::ListView, position: u32, id: String) {
    cancel_auto_scroll();
    let generation = SCROLL_GENERATION.get();
    let view = view.clone();
    gtk::glib::idle_add_local_once(move || {
        if SCROLL_GENERATION.get() != generation {
            return;
        }
        view.scroll_to(position, gtk::ListScrollFlags::NONE, None);
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
            if SCROLL_GENERATION.get() != generation {
                return;
            }
            let Some(adjustment) = view.vadjustment() else {
                return;
            };
            let mut child = view.first_child();
            while let Some(item) = child {
                if let Some(root) = item.first_child()
                    && root.widget_name() == id
                    && let Some(bounds) = root.compute_bounds(&view)
                {
                    let top = adjustment.value() + f64::from(bounds.y()) - 16.0;
                    adjustment
                        .set_value(top.clamp(0.0, adjustment.upper() - adjustment.page_size()));
                    return;
                }
                child = item.next_sibling();
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wheel_controller(scroller: &gtk::ScrolledWindow) -> gtk::EventControllerScroll {
        let controllers = scroller.observe_controllers();
        (0..controllers.n_items())
            .filter_map(|index| {
                controllers
                    .item(index)?
                    .downcast::<gtk::EventControllerScroll>()
                    .ok()
            })
            .find(|controller| controller.name().as_deref() == Some("zaptide-scroll-cancellation"))
            .expect("transcript scroll cancellation controller")
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn wheel_input_cancels_delayed_scroll_to_end() {
        adw::init().unwrap();
        let view = gtk::ListView::new(None::<gtk::SelectionModel>, None::<gtk::ListItemFactory>);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_child(Some(&view));
        install_scroll_cancellation(&scroller, &view);
        let adjustment = scroller.vadjustment();
        let context = gtk::glib::MainContext::default();
        for after_first_pass in [false, true] {
            adjustment.configure(700.0, 0.0, 1000.0, 10.0, 100.0, 200.0);
            scroll_to_end(&view);
            if after_first_pass {
                while context.pending() {
                    context.iteration(false);
                }
                assert_eq!(adjustment.value(), 800.0);
            }
            assert!(
                !wheel_controller(&scroller).emit_by_name::<bool>("scroll", &[&0.0f64, &-1.0f64])
            );
            adjustment.set_value(300.0);
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
            while std::time::Instant::now() < deadline {
                context.iteration(false);
            }
            assert_eq!(
                adjustment.value(),
                300.0,
                "pending follow must not undo wheel input (first pass={after_first_pass})"
            );
        }
    }

    #[test]
    #[ignore = "requires private Xvfb, Python, X11 and XTest"]
    fn wheel_input_stops_an_active_glide() {
        adw::init().unwrap();
        gtk::Settings::default()
            .unwrap()
            .set_gtk_enable_animations(true);
        let strings = gtk::StringList::new(&["Synthetic"; 100]);
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            item.set_child(Some(
                &gtk::Label::builder()
                    .label("Synthetic")
                    .height_request(32)
                    .build(),
            ));
        });
        let view = gtk::ListView::new(Some(gtk::NoSelection::new(Some(strings))), Some(factory));
        let scroller = gtk::ScrolledWindow::builder().child(&view).build();
        install_scroll_cancellation(&scroller, &view);
        let window = gtk::Window::builder()
            .title("ZapTide Transcript Input Fixture")
            .default_width(400)
            .default_height(300)
            .child(&scroller)
            .build();
        window.present();
        let context = gtk::glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
        while std::time::Instant::now() < deadline {
            context.iteration(false);
        }
        let adjustment = scroller.vadjustment();
        let active_at_input = std::rc::Rc::new(std::cell::Cell::new(false));
        let observed = active_at_input.clone();
        let capture = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        capture.set_propagation_phase(gtk::PropagationPhase::Capture);
        capture.connect_scroll(move |_, _, _| {
            observed.set(gliding());
            gtk::glib::Propagation::Proceed
        });
        window.add_controller(capture);
        adjustment.set_value(300.0);
        glide_to_end(&view);
        assert!(gliding());
        let at_input = std::rc::Rc::new(std::cell::Cell::new(None));
        let observed = at_input.clone();
        let position = adjustment.downgrade();
        wheel_controller(&scroller).connect_scroll(move |_, _, _| {
            observed.set(position.upgrade().map(|adjustment| adjustment.value()));
            gtk::glib::Propagation::Proceed
        });
        super::super::sidebar_runtime_tests::native_x11_input(&window, &scroller, "wheel-up");
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(350);
        while std::time::Instant::now() < deadline {
            context.iteration(false);
        }
        assert!(!gliding());
        assert!(
            active_at_input.get(),
            "wheel must interrupt a running glide"
        );
        let before = at_input.get().expect("native wheel reached the transcript");
        let moved = before - adjustment.value();
        assert!(
            moved > 0.0 && moved < adjustment.page_size(),
            "native wheel must move upward without the glide taking over: before={before}, after={}",
            adjustment.value()
        );
        window.destroy();
    }
}
