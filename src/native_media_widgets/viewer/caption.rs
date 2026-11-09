use super::*;

#[derive(Clone)]
pub(super) struct Caption {
    pub(super) widget: gtk::Box,
    preview: gtk::Label,
    full: gtk::Label,
    open: gtk::MenuButton,
    scroller: gtk::ScrolledWindow,
}

impl Caption {
    pub(super) fn new(metadata: &gtk::Label) -> Self {
        let preview = gtk::Label::builder()
            .hexpand(true)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .lines(2)
            .width_chars(1)
            .selectable(true)
            .css_classes(["zaptide-viewer-caption-preview"])
            .build();
        let full = gtk::Label::builder()
            .xalign(0.0)
            .yalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .width_chars(1)
            .max_width_chars(40)
            .selectable(true)
            .css_classes(["zaptide-viewer-caption-full"])
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .propagate_natural_width(true)
            .min_content_width(240)
            .max_content_height(240)
            .max_content_width(280)
            .child(&full)
            .build();
        let popover = gtk::Popover::builder()
            .position(gtk::PositionType::Top)
            .child(&scroller)
            .build();
        popover.connect_show({
            let full = full.clone();
            move |_| full.select_region(0, 0)
        });
        let open = gtk::MenuButton::builder()
            .label(gettext("View Caption"))
            .direction(gtk::ArrowType::Up)
            .popover(&popover)
            .css_classes(["flat"])
            .build();
        let details = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        details.append(&open);
        details.append(metadata);
        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .css_classes(["zaptide-viewer-caption"])
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        content.append(&preview);
        content.append(&details);
        widget.append(&content);
        Self {
            widget,
            preview,
            full,
            open,
            scroller,
        }
    }

    pub(super) fn show(&self, text: Option<&str>) {
        self.open.popdown();
        self.scroller.vadjustment().set_value(0.0);
        let text = text.unwrap_or_default();
        let has_caption = !text.trim().is_empty();
        self.preview
            .set_label(&text.split_whitespace().collect::<Vec<_>>().join(" "));
        self.full.set_label(text);
        self.preview.set_visible(has_caption);
        self.open.set_visible(has_caption);
    }
}
