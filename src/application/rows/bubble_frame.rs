use std::cell::OnceCell;

use relm4::gtk::{self, glib, prelude::*, subclass::prelude::*};

use crate::native_media_widgets::text_bubble::TextBubble;

#[cfg(test)]
mod tests;

pub(super) const MAX_WIDTH: i32 = 480;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct BubbleFrame {
        pub(super) bubble: OnceCell<gtk::Box>,
        pub(super) body: OnceCell<TextBubble>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BubbleFrame {
        const NAME: &'static str = "ZapTideBubbleFrame";
        type Type = super::BubbleFrame;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for BubbleFrame {
        fn dispose(&self) {
            if let Some(bubble) = self.bubble.get() {
                bubble.unparent();
            }
        }
    }

    impl WidgetImpl for BubbleFrame {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let bubble = self.bubble.get().expect("bubble initialized");
            if orientation == gtk::Orientation::Horizontal {
                let (minimum, _, min_baseline, nat_baseline) =
                    bubble.measure(orientation, for_size);
                return (
                    minimum,
                    self.obj().fitted_width(MAX_WIDTH).max(minimum),
                    min_baseline,
                    nat_baseline,
                );
            }
            let width = self.obj().fitted_width(for_size);
            let (minimum, natural, min_baseline, nat_baseline) = bubble.measure(orientation, width);
            (minimum, natural.max(minimum), min_baseline, nat_baseline)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let bubble = self.bubble.get().expect("bubble initialized");
            let fitted = self.obj().fitted_width(width);
            let x = if bubble.halign() == gtk::Align::End {
                width - fitted
            } else {
                0
            };
            let transform =
                gtk::gsk::Transform::new().translate(&gtk::graphene::Point::new(x as f32, 0.0));
            bubble.allocate(fitted, height, baseline, Some(transform));
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(bubble) = self.bubble.get() {
                self.obj().snapshot_child(bubble, snapshot);
            }
        }
    }
}

glib::wrapper! {
    pub struct BubbleFrame(ObjectSubclass<imp::BubbleFrame>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl BubbleFrame {
    pub(super) fn new(bubble: &gtk::Box, body: &TextBubble) -> Self {
        let frame: Self = glib::Object::new();
        frame.imp().bubble.set(bubble.clone()).unwrap();
        frame.imp().body.set(body.clone()).unwrap();
        bubble.set_parent(&frame);
        frame
    }

    fn fitted_width(&self, available: i32) -> i32 {
        let bubble = self.imp().bubble.get().expect("bubble initialized");
        let body = self.imp().body.get().expect("body initialized");
        let (minimum, natural, _, _) = bubble.measure(gtk::Orientation::Horizontal, -1);
        let mut widest = 0;
        let mut metadata = 0;
        let mut child = bubble.first_child();
        while let Some(widget) = child {
            if widget.is_visible() {
                let (_, width, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
                widest = widest.max(width);
                if widget != *body.upcast_ref::<gtk::Widget>() {
                    metadata = metadata.max(width);
                }
            }
            child = widget.next_sibling();
        }
        // Include themed padding without duplicating CSS pixel values here.
        let padding = natural - widest;
        let available = if available < 0 { MAX_WIDTH } else { available }
            .clamp(minimum, MAX_WIDTH.max(minimum));
        let text = if body.is_visible() {
            body.wrapped_width((available - padding).max(1))
        } else {
            0
        };
        (text.max(metadata) + padding).clamp(minimum, available)
    }
}
