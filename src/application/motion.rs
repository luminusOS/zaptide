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

pub(super) fn cancel_auto_scroll() {
    SCROLL_GENERATION.set(SCROLL_GENERATION.get().wrapping_add(1));
    let previous = END_GLIDE.with_borrow_mut(|running| running.take());
    if let Some(previous) = previous {
        previous.pause();
    }
}

pub(super) fn reading_anchor(view: &gtk::ListView, ids: &[String]) -> Option<(String, f64)> {
    let scroller = view.ancestor(gtk::ScrolledWindow::static_type())?;
    let mut child = view.first_child();
    while let Some(item) = child {
        if let Some(root) = item.first_child()
            && root.is_mapped()
            && root.height() > 0
            && let Some(bounds) = root.compute_bounds(&scroller)
            && bounds.y() + bounds.height() > 0.0
            && bounds.y() < scroller.height() as f32
            && ids.iter().any(|id| id == root.widget_name().as_str())
        {
            return Some((root.widget_name().to_string(), f64::from(bounds.y())));
        }
        child = item.next_sibling();
    }
    None
}

pub(super) fn restore_reading_anchor(view: &gtk::ListView, position: u32, id: String, offset: f64) {
    cancel_auto_scroll();
    stop_following_end();
    let generation = SCROLL_GENERATION.get();
    let Some(model) = view.model() else {
        return;
    };
    let Some(expected) = model.item(position) else {
        return;
    };
    let frames = std::cell::Cell::new(0);
    let stable = std::cell::Cell::new(0);
    view.add_tick_callback(move |view, _| {
        if SCROLL_GENERATION.get() != generation
            || model.item(position).as_ref() != Some(&expected)
            || frames.get() >= 16
        {
            return gtk::glib::ControlFlow::Break;
        }
        frames.set(frames.get() + 1);
        let Some(scroller) = view.ancestor(gtk::ScrolledWindow::static_type()) else {
            return gtk::glib::ControlFlow::Break;
        };
        let mut child = view.first_child();
        while let Some(item) = child {
            if let Some(root) = item.first_child()
                && root.is_mapped()
                && root.height() > 0
                && root.widget_name() == id
                && let Some(bounds) = root.compute_bounds(&scroller)
                && let Some(adjustment) = view.vadjustment()
            {
                let delta = f64::from(bounds.y()) - offset;
                if delta.abs() <= 1.0 {
                    stable.set(stable.get() + 1);
                } else {
                    stable.set(0);
                    adjustment.set_value(adjustment.value() + delta);
                }
                return if stable.get() >= 3 {
                    gtk::glib::ControlFlow::Break
                } else {
                    gtk::glib::ControlFlow::Continue
                };
            }
            child = item.next_sibling();
        }
        view.scroll_to(position, gtk::ListScrollFlags::NONE, None);
        gtk::glib::ControlFlow::Continue
    });
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

/// Keeps a transcript that rests at its end there when content grows after
/// layout: photos and link previews decoding, the typing indicator or a taller
/// composer shrinking the viewport. GtkListView keeps its anchor row in place
/// instead, which opens a gap below the newest message. Resting is decided
/// from the previous position because `changed` is emitted before
/// `value-changed` with the new bounds.
pub(super) fn install_end_following(scroller: &gtk::ScrolledWindow) {
    let adjustment = scroller.vadjustment();
    // An unallocated or hidden list reports an empty page, which says nothing
    // about where the reader was.
    adjustment.connect_value_changed(|adjustment| {
        if adjustment.page_size() > 0.0 {
            FOLLOWING_END.set(rests_at_end(adjustment));
        }
    });
    adjustment.connect_changed(|adjustment| {
        if adjustment.page_size() <= 0.0 {
            return;
        }
        if FOLLOWING_END.get() && !gliding() && !rests_at_end(adjustment) {
            adjustment.set_value(adjustment.upper() - adjustment.page_size());
            // GtkListBase blocks its own value-changed handler while it
            // configures the adjustment, so the list never sees this value
            // and its next allocation would restore the old anchor. Announce
            // it again once the handler is unblocked, before the next layout.
            let pending = adjustment.downgrade();
            gtk::glib::idle_add_local_full(gtk::glib::Priority::HIGH, move || {
                if let Some(adjustment) = pending.upgrade()
                    && FOLLOWING_END.get()
                    && rests_at_end(&adjustment)
                {
                    adjustment.emit_by_name::<()>("value-changed", &[]);
                }
                gtk::glib::ControlFlow::Break
            });
        }
        // Bounds shrinking under a reader can leave them at the end too.
        FOLLOWING_END.set(rests_at_end(adjustment));
    });
}

fn rests_at_end(adjustment: &gtk::Adjustment) -> bool {
    adjustment.value() + adjustment.page_size() >= adjustment.upper() - 1.0
}

/// Stops following the end until the view next rests there. Called
/// synchronously by programmatic positioning, before the model change it
/// accompanies is laid out.
fn stop_following_end() {
    FOLLOWING_END.set(false);
}

thread_local! {
    static FOLLOWING_END: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
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
/// end of the list allows.
pub(super) fn scroll_to_unread(view: &gtk::ListView, position: u32, id: String) {
    place_row(
        view,
        position,
        id,
        RowPlace::Top(16.0),
        gtk::ListScrollFlags::NONE,
        |_| {},
    );
}

/// Where [`place_row`] puts a row in the viewport.
#[derive(Clone, Copy)]
pub(super) enum RowPlace {
    /// This many pixels below the top edge.
    Top(f64),
    /// A third of the way down, leaving the context above a quoted message.
    UpperThird,
}

/// Scrolls the row `id` at `position` to `place`, applying `flags` such as
/// focus to the item, then calls `placed` with its root.
///
/// GtkListView creates the row only once `scroll_to` has brought it near the
/// viewport, and heights estimated for rows not yet measured move it while
/// they settle, so the place is corrected each frame until it holds or the
/// frame budget runs out. User input cancels it like the other automatic
/// scrolls; `placed` then never runs.
pub(super) fn place_row(
    view: &gtk::ListView,
    position: u32,
    id: String,
    place: RowPlace,
    flags: gtk::ListScrollFlags,
    placed: impl FnOnce(gtk::Widget) + 'static,
) {
    const MAX_FRAMES: u32 = 30;
    cancel_auto_scroll();
    stop_following_end();
    let generation = SCROLL_GENERATION.get();
    let view = view.clone();
    let placed = std::cell::Cell::new(Some(placed));
    gtk::glib::idle_add_local_once(move || {
        if SCROLL_GENERATION.get() != generation {
            return;
        }
        view.scroll_to(position, flags, None);
        let frames = std::cell::Cell::new(0);
        let stable = std::cell::Cell::new(0);
        view.add_tick_callback(move |view, _| {
            if SCROLL_GENERATION.get() != generation {
                return gtk::glib::ControlFlow::Break;
            }
            if frames.get() >= MAX_FRAMES {
                // Media still settling: the row is near its place, so finish.
                if let Some(root) = mapped_row(view, &id)
                    && let Some(placed) = placed.take()
                {
                    placed(root);
                }
                return gtk::glib::ControlFlow::Break;
            }
            frames.set(frames.get() + 1);
            let Some(adjustment) = view.vadjustment() else {
                return gtk::glib::ControlFlow::Break;
            };
            let Some(root) = mapped_row(view, &id) else {
                view.scroll_to(position, flags, None);
                return gtk::glib::ControlFlow::Continue;
            };
            let Some(bounds) = root.compute_bounds(view) else {
                return gtk::glib::ControlFlow::Continue;
            };
            let offset = match place {
                RowPlace::Top(margin) => margin,
                RowPlace::UpperThird => adjustment.page_size() / 3.0,
            };
            let end = (adjustment.upper() - adjustment.page_size()).max(0.0);
            let target = (adjustment.value() + f64::from(bounds.y()) - offset).clamp(0.0, end);
            if (target - adjustment.value()).abs() <= 1.0 {
                stable.set(stable.get() + 1);
            } else {
                stable.set(0);
                adjustment.set_value(target);
            }
            if stable.get() < 2 {
                return gtk::glib::ControlFlow::Continue;
            }
            if let Some(placed) = placed.take() {
                placed(root);
            }
            gtk::glib::ControlFlow::Break
        });
    });
}

fn mapped_row(view: &gtk::ListView, id: &str) -> Option<gtk::Widget> {
    let mut child = view.first_child();
    while let Some(item) = child {
        if let Some(root) = item.first_child()
            && root.is_mapped()
            && root.height() > 0
            && root.widget_name() == id
        {
            return Some(root);
        }
        child = item.next_sibling();
    }
    None
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

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn transcript_at_the_end_follows_late_growth() {
        adw::init().unwrap();
        gtk::Settings::default()
            .unwrap()
            .set_gtk_enable_animations(false);
        let strings = gtk::StringList::new(&["Synthetic"; 80]);
        let rows = std::rc::Rc::new(std::cell::RefCell::new(Vec::<gtk::Box>::new()));
        let factory = gtk::SignalListItemFactory::new();
        let created = rows.clone();
        factory.connect_setup(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
            root.set_height_request(60);
            created.borrow_mut().push(root.clone());
            item.set_child(Some(&root));
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            item.child()
                .unwrap()
                .set_widget_name(&item.position().to_string());
        });
        let view = gtk::ListView::new(Some(gtk::NoSelection::new(Some(strings))), Some(factory));
        let scroller = gtk::ScrolledWindow::builder()
            .child(&view)
            .vexpand(true)
            .build();
        install_scroll_cancellation(&scroller, &view);
        install_end_following(&scroller);
        let typing = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::None)
            .child(
                &gtk::Label::builder()
                    .label("typing")
                    .height_request(40)
                    .build(),
            )
            .build();
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&scroller);
        column.append(&typing);
        let window = gtk::Window::builder()
            .default_width(400)
            .default_height(400)
            .child(&column)
            .build();
        window.present();
        let context = gtk::glib::MainContext::default();
        let spin = || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
            while std::time::Instant::now() < deadline {
                context.iteration(false);
            }
        };
        spin();
        scroll_to_end(&view);
        spin();
        let adjustment = scroller.vadjustment();
        let gap = || adjustment.upper() - adjustment.page_size() - adjustment.value();
        assert!(
            gap().abs() <= 1.0,
            "fixture starts at the end, gap {}",
            gap()
        );
        let last = rows
            .borrow()
            .iter()
            .find(|row| row.widget_name() == "79")
            .cloned()
            .expect("last row");
        last.set_height_request(220);
        spin();
        assert!(
            gap().abs() <= 1.0,
            "late row growth left a gap of {}",
            gap()
        );
        view.queue_allocate();
        spin();
        assert!(
            gap().abs() <= 1.0,
            "follow lost on the next allocation: {}",
            gap()
        );
        typing.set_reveal_child(true);
        spin();
        assert!(
            gap().abs() <= 1.0,
            "typing indicator left a gap of {}",
            gap()
        );
        view.queue_allocate();
        spin();
        assert!(
            gap().abs() <= 1.0,
            "follow lost on the next allocation: {}",
            gap()
        );
        adjustment.set_value(adjustment.value() - 500.0);
        spin();
        let reading = adjustment.value();
        last.set_height_request(320);
        typing.set_reveal_child(false);
        spin();
        assert_eq!(adjustment.value(), reading, "growth must not move a reader");
        window.set_visible(false);
        spin();
        window.present();
        spin();
        assert_eq!(adjustment.value(), reading, "hiding must not move a reader");
        window.destroy();
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn placed_rows_settle_where_requested_among_unmeasured_heights() {
        adw::init().unwrap();
        gtk::Settings::default()
            .unwrap()
            .set_gtk_enable_animations(false);
        let strings = gtk::StringList::new(&["Synthetic"; 600]);
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            item.set_child(Some(&gtk::Box::new(gtk::Orientation::Vertical, 0)));
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let root = item.child().unwrap();
            root.set_widget_name(&item.position().to_string());
            root.set_height_request(40 + (item.position() % 7) as i32 * 45);
        });
        let view = gtk::ListView::new(Some(gtk::NoSelection::new(Some(strings))), Some(factory));
        let scroller = gtk::ScrolledWindow::builder().child(&view).build();
        install_scroll_cancellation(&scroller, &view);
        install_end_following(&scroller);
        let window = gtk::Window::builder()
            .default_width(400)
            .default_height(600)
            .child(&scroller)
            .build();
        window.present();
        let context = gtk::glib::MainContext::default();
        let spin = |milliseconds| {
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_millis(milliseconds);
            while std::time::Instant::now() < deadline {
                context.iteration(false);
            }
        };
        spin(200);
        scroll_to_end(&view);
        spin(300);
        let row_y = |id: &str| {
            mapped_row(&view, id)
                .and_then(|root| root.compute_bounds(&view))
                .map(|bounds| f64::from(bounds.y()))
        };
        let page = scroller.vadjustment().page_size();
        for (position, place, expected) in [
            (120, RowPlace::Top(16.0), 16.0),
            (431, RowPlace::UpperThird, page / 3.0),
        ] {
            let done = std::rc::Rc::new(std::cell::Cell::new(false));
            let observed = done.clone();
            place_row(
                &view,
                position,
                position.to_string(),
                place,
                gtk::ListScrollFlags::NONE,
                move |_| observed.set(true),
            );
            spin(800);
            assert!(done.get(), "row {position} was never placed");
            let y = row_y(&position.to_string()).expect("placed row is shown");
            assert!(
                (y - expected).abs() <= 2.0,
                "row {position} at {y}, expected {expected}"
            );
        }
        let done = std::rc::Rc::new(std::cell::Cell::new(false));
        let observed = done.clone();
        place_row(
            &view,
            40,
            "40".into(),
            RowPlace::Top(16.0),
            gtk::ListScrollFlags::NONE,
            move |_| observed.set(true),
        );
        assert!(!wheel_controller(&scroller).emit_by_name::<bool>("scroll", &[&0.0f64, &1.0f64]));
        spin(500);
        assert!(!done.get(), "wheel input cancels a pending placement");
        window.destroy();
    }
}
