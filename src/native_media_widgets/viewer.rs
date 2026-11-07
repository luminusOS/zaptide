mod details;
mod photo;
mod video;

pub(super) use video::show_video;
#[cfg(test)]
use video::{clock_text, seek_target, video_label, volume_icon};

use photo::PhotoState;
pub(super) use photo::show_photo;
#[cfg(test)]
use photo::{OPEN_PHOTO, ZoomKey, pan_key, zoom_key};

pub(super) use details::ViewerDetails;

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use super::{
    photo::{decode_photo, show_in_folder},
    video_stream::VideoStream,
    zoom::{self, Bounds, Level},
    zoom_canvas::ZoomCanvas,
    *,
};
use crate::i18n::gettext;

const AVATAR_SIZE: i32 = 28;
const UNLIMITED_DIALOG_SIZE: i32 = 10_000;
const MIN_VIEWER: (i32, i32) = (320, 240);
const NARROW_VIEWER: &str = "max-width: 480px";
/// Largest side the viewer decodes to show a photo fitted to the window; more
/// of it is decoded as the zoom needs it.
const FIT_EDGE: u32 = 3840;
/// Most pixels the viewer holds for one photo, whatever its real size.
const NATIVE_PIXELS: f64 = 64e6;
#[derive(Clone)]
pub(super) struct PhotoItem {
    pub(super) path: std::path::PathBuf,
    pub(super) details: ViewerDetails,
}

fn of_text(current: &str, total: &str) -> String {
    gettext("{current} of {total}")
        .replace("{current}", current)
        .replace("{total}", total)
}

fn counter_text(index: usize, total: usize) -> Option<String> {
    (total > 1).then(|| of_text(&(index + 1).to_string(), &total.to_string()))
}

fn step_target(current: usize, delta: isize, len: usize) -> Option<usize> {
    let target = current.checked_add_signed(delta)?;
    (delta != 0 && target < len && target != current).then_some(target)
}

fn key_step(key: gdk::Key, state: gdk::ModifierType, rtl: bool) -> Option<isize> {
    let held = gdk::ModifierType::SHIFT_MASK
        | gdk::ModifierType::CONTROL_MASK
        | gdk::ModifierType::ALT_MASK
        | gdk::ModifierType::SUPER_MASK
        | gdk::ModifierType::META_MASK;
    if state.intersects(held) {
        return None;
    }
    let delta = match key {
        gdk::Key::Left => -1,
        gdk::Key::Right => 1,
        _ => return None,
    };
    Some(if rtl { -delta } else { delta })
}

fn photo_label(name: &str) -> String {
    gettext("Photo from {name}").replace("{name}", name)
}

fn header_button(icon: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(label)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

/// What the zoom pill shows.
struct ZoomView {
    percent: u32,
    at_fit: bool,
    can_shrink: bool,
    can_grow: bool,
    ready: bool,
}

#[derive(Clone)]
struct ZoomPill {
    root: gtk::Box,
    shrink: gtk::Button,
    level: gtk::Button,
    percent: gtk::Label,
    grow: gtk::Button,
}

struct ZoomPillWeak {
    shrink: glib::WeakRef<gtk::Button>,
    level: glib::WeakRef<gtk::Button>,
    percent: glib::WeakRef<gtk::Label>,
    grow: glib::WeakRef<gtk::Button>,
}

fn zoom_level_text(percent: u32) -> String {
    gettext("Zoom level {percent} percent").replace("{percent}", &percent.to_string())
}

impl ZoomPill {
    fn new() -> Self {
        let shrink = header_button("zoom-out-symbolic", &gettext("Zoom Out"));
        let grow = header_button("zoom-in-symbolic", &gettext("Zoom In"));
        let percent = gtk::Label::builder()
            .width_chars(4)
            .css_classes(["numeric"])
            .build();
        let level = gtk::Button::builder()
            .child(&percent)
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let root = gtk::Box::builder()
            .valign(gtk::Align::Center)
            .css_classes(["zaptide-viewer-zoom"])
            .build();
        root.append(&shrink);
        root.append(&level);
        root.append(&grow);
        let pill = Self {
            root,
            shrink,
            level,
            percent,
            grow,
        };
        pill.downgrade().show(&ZoomView {
            percent: 100,
            at_fit: true,
            can_shrink: false,
            can_grow: false,
            ready: false,
        });
        pill
    }

    fn downgrade(&self) -> ZoomPillWeak {
        ZoomPillWeak {
            shrink: self.shrink.downgrade(),
            level: self.level.downgrade(),
            percent: self.percent.downgrade(),
            grow: self.grow.downgrade(),
        }
    }

    fn connect(&self, photo: &Rc<PhotoState>) {
        self.shrink.connect_clicked({
            let photo = photo.clone();
            move |_| photo.zoom_by(false)
        });
        self.grow.connect_clicked({
            let photo = photo.clone();
            move |_| photo.zoom_by(true)
        });
        self.level.connect_clicked({
            let photo = photo.clone();
            move |_| photo.toggle_fit(None)
        });
    }
}

impl ZoomPillWeak {
    fn show(&self, view: &ZoomView) {
        if let (Some(shrink), Some(level), Some(percent), Some(grow)) = (
            self.shrink.upgrade(),
            self.level.upgrade(),
            self.percent.upgrade(),
            self.grow.upgrade(),
        ) {
            let text = format!("{}%", view.percent);
            if percent.label().as_str() != text {
                percent.set_label(&text);
                level.update_property(&[gtk::accessible::Property::Label(&zoom_level_text(
                    view.percent,
                ))]);
            }
            let action = if view.at_fit {
                gettext("Actual Size")
            } else {
                gettext("Fit to Window")
            };
            if level.tooltip_text().as_deref() != Some(action.as_str()) {
                level.set_tooltip_text(Some(&action));
                level.update_property(&[gtk::accessible::Property::Description(&action)]);
            }
            level.set_sensitive(view.ready);
            shrink.set_sensitive(view.ready && view.can_shrink);
            grow.set_sensitive(view.ready && view.can_grow);
        }
    }
}

#[derive(Clone)]
struct ViewerParts {
    dialog: glib::WeakRef<adw::Dialog>,
    avatar: adw::Avatar,
    name: gtk::Label,
    time: gtk::Label,
    counter: gtk::Label,
    caption: gtk::Label,
    metadata: gtk::Label,
}

impl ViewerParts {
    fn show(&self, details: &ViewerDetails, position: Option<(usize, usize)>) {
        if let Some(dialog) = self.dialog.upgrade() {
            dialog.set_title(&details.title);
        }
        self.avatar.set_text(Some(&details.title));
        self.name.set_label(&details.title);
        self.time.set_label(&details.subtitle);
        let counter = position.and_then(|(index, total)| counter_text(index, total));
        self.counter.set_visible(counter.is_some());
        self.counter
            .set_label(counter.as_deref().unwrap_or_default());
        self.caption
            .set_label(details.caption.as_deref().unwrap_or_default());
        self.metadata.set_label(&details.metadata);
    }
}

struct Viewer {
    dialog: adw::Dialog,
    view: adw::ToolbarView,
    parts: ViewerParts,
}

/// The files shown are kept from the downloads cleanup until the dialog closes.
fn keep_while_open(dialog: &adw::Dialog, paths: impl IntoIterator<Item = std::path::PathBuf>) {
    let hold = std::cell::RefCell::new(Some(crate::media_retention::hold(paths)));
    dialog.connect_closed(move |_| drop(hold.take()));
}

fn viewer_size(parent: &impl IsA<gtk::Widget>) -> (i32, i32) {
    let (width, height) = parent
        .as_ref()
        .root()
        .map_or((900, 700), |root| (root.width(), root.height()));
    ((width * 9 / 10).max(360), (height * 9 / 10).max(360))
}

fn label_with(classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::builder().valign(gtk::Align::Center).build();
    label.set_css_classes(classes);
    label
}

/// A dark, near-full-window dialog around `content`: the sender, an optional
/// zoom pill and the `actions` (left to right, before a close button) in a
/// 56 px header, and the caption with the file summary in a bar below. Below
/// 480 px the time, the counter and the pill's zoom steps give way so the
/// header still fits; the percentage stays.
fn media_viewer(
    parent: &impl IsA<gtk::Widget>,
    details: &ViewerDetails,
    content: &impl IsA<gtk::Widget>,
    zoom: Option<&ZoomPill>,
    actions: &[gtk::Widget],
) -> Viewer {
    let (width, height) = viewer_size(parent);
    let dialog = adw::Dialog::builder()
        .title(&details.title)
        .content_width(width)
        .content_height(height)
        .width_request(MIN_VIEWER.0)
        .height_request(MIN_VIEWER.1)
        .build();

    let avatar = adw::Avatar::new(AVATAR_SIZE, Some(&details.title), true);
    avatar.set_valign(gtk::Align::Center);
    let name = label_with(&["heading", "zaptide-viewer-name"]);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.set_max_width_chars(24);
    let time = label_with(&["caption", "dim-label", "zaptide-viewer-time"]);
    let identity = gtk::Box::builder().spacing(8).margin_start(6).build();
    identity.append(&avatar);
    identity.append(&name);
    identity.append(&time);

    let counter = label_with(&["caption", "zaptide-viewer-pill"]);
    let counter_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    counter_slot.append(&counter);
    let centre = gtk::Box::builder()
        .spacing(8)
        .halign(gtk::Align::Center)
        .build();
    centre.append(&counter_slot);
    if let Some(zoom) = zoom {
        centre.append(&zoom.root);
    }

    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .title_widget(&centre)
        .build();
    header.pack_start(&identity);
    let close = header_button("window-close-symbolic", &gettext("Close"));
    close.set_margin_end(6);
    close.connect_clicked({
        let dialog = dialog.downgrade();
        move |_| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
        }
    });
    header.pack_end(&close);
    for action in actions.iter().rev() {
        header.pack_end(action);
    }

    if let Ok(condition) = adw::BreakpointCondition::parse(NARROW_VIEWER) {
        let narrow = adw::Breakpoint::new(condition);
        let hidden = false.to_value();
        narrow.add_setter(&time, "visible", Some(&hidden));
        narrow.add_setter(&counter_slot, "visible", Some(&hidden));
        if let Some(zoom) = zoom {
            narrow.add_setter(&zoom.shrink, "visible", Some(&hidden));
            narrow.add_setter(&zoom.grow, "visible", Some(&hidden));
        }
        dialog.add_breakpoint(narrow);
    }

    let caption = gtk::Label::builder()
        .hexpand(true)
        .xalign(0.0)
        .valign(gtk::Align::Center)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .lines(2)
        .selectable(true)
        .build();
    let metadata = label_with(&["caption", "dim-label", "numeric", "zaptide-viewer-metadata"]);
    metadata.set_halign(gtk::Align::End);
    let bar = gtk::Box::builder()
        .spacing(16)
        .css_classes(["zaptide-viewer-caption"])
        .build();
    bar.append(&caption);
    bar.append(&metadata);

    let view = adw::ToolbarView::builder()
        .content(content)
        .top_bar_style(adw::ToolbarStyle::Flat)
        .bottom_bar_style(adw::ToolbarStyle::Flat)
        .css_classes(["zaptide-viewer"])
        .build();
    view.add_top_bar(&header);
    view.add_bottom_bar(&bar);
    dialog.set_child(Some(&view));

    let parts = ViewerParts {
        dialog: dialog.downgrade(),
        avatar,
        name,
        time,
        counter,
        caption,
        metadata,
    };
    parts.show(details, None);
    Viewer {
        dialog,
        view,
        parts,
    }
}

#[cfg(test)]
mod tests;
