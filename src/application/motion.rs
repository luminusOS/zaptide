//! Scrolling and fade-in animations for lists.

use super::*;

/// Moves a list to its end once rows are laid out. `ListView::scroll_to`
/// leaves blank space with rows of varying height, and the height estimate
/// only settles after the newly shown rows are measured, hence the second pass.
pub(super) fn scroll_to_end(view: &gtk::ListView) {
    let Some(adjustment) = view.vadjustment() else {
        return;
    };
    let end = move || adjustment.set_value(adjustment.upper() - adjustment.page_size());
    let settle = end.clone();
    gtk::glib::idle_add_local_once(end);
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(150), settle);
}

thread_local! {
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
    let Some(adjustment) = view.vadjustment() else {
        return;
    };
    let start = adjustment.value();
    let target = adw::CallbackAnimationTarget::new(move |progress| {
        let end = adjustment.upper() - adjustment.page_size();
        adjustment.set_value(start + (end - start) * progress);
    });
    let animation = adw::TimedAnimation::new(view, 0.0, 1.0, 250, target);
    animation.set_easing(adw::Easing::EaseOutCubic);
    END_GLIDE.with_borrow_mut(|running| {
        if let Some(previous) = running.replace(animation.clone()) {
            previous.pause();
        }
    });
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
    let view = view.clone();
    gtk::glib::idle_add_local_once(move || {
        view.scroll_to(position, gtk::ListScrollFlags::NONE, None);
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
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
