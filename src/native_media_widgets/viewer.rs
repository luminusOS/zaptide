mod caption;
mod details;
mod photo;
mod video;

pub(crate) use video::build_video;
#[cfg(test)]
use video::show_video;
#[cfg(test)]
use video::{clock_text, seek_target, video_label, volume_icon};

use photo::PhotoState;
pub(crate) use photo::build_photo;
#[cfg(test)]
use photo::show_photo;
#[cfg(test)]
use photo::{OPEN_PHOTO, ZoomKey, pan_key, zoom_key};

pub(crate) use details::ViewerDetails;

#[cfg(test)]
use adw::prelude::AdwWindowExt;
use adw::prelude::BreakpointBinExt;
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
const MIN_VIEWER: (i32, i32) = (320, 240);
const NARROW_VIEWER: &str = "max-width: 480px";
/// Largest side the viewer decodes to show a photo fitted to the window; more
/// of it is decoded as the zoom needs it.
const FIT_EDGE: u32 = 3840;
/// Most pixels the viewer holds for one photo, whatever its real size.
const NATIVE_PIXELS: f64 = 64e6;
#[derive(Clone)]
pub(crate) struct PhotoItem {
    pub(crate) path: std::path::PathBuf,
    pub(crate) details: ViewerDetails,
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
    avatar: adw::Avatar,
    name: gtk::Label,
    time: gtk::Label,
    counter: gtk::Label,
    caption: caption::Caption,
    metadata: gtk::Label,
}

impl ViewerParts {
    fn show(&self, details: &ViewerDetails, position: Option<(usize, usize)>) {
        self.avatar.set_text(Some(&details.title));
        self.name.set_label(&details.title);
        self.time.set_label(&details.subtitle);
        let counter = position.and_then(|(index, total)| counter_text(index, total));
        self.counter.set_visible(counter.is_some());
        self.counter
            .set_label(counter.as_deref().unwrap_or_default());
        self.caption.show(details.caption.as_deref());
        self.metadata.set_label(&details.metadata);
    }
}

struct Viewer {
    page: MediaViewer,
    view: adw::ToolbarView,
    parts: ViewerParts,
}

pub(crate) struct MediaViewer {
    pub(crate) widget: adw::BreakpointBin,
    lifetime: Rc<RefCell<ViewerLifetime>>,
    on_close: Rc<dyn Fn()>,
}

impl Drop for MediaViewer {
    fn drop(&mut self) {
        self.lifetime.borrow_mut().close();
    }
}

struct ViewerLifetime {
    hold: Option<crate::media_retention::Hold>,
    cleanup: Vec<Box<dyn FnOnce()>>,
}

impl ViewerLifetime {
    fn new(paths: impl IntoIterator<Item = std::path::PathBuf>) -> Self {
        Self {
            hold: Some(crate::media_retention::hold(paths)),
            cleanup: Vec::new(),
        }
    }

    fn on_close(&mut self, cleanup: impl FnOnce() + 'static) {
        self.cleanup.push(Box::new(cleanup));
    }

    fn close(&mut self) {
        for cleanup in self.cleanup.drain(..) {
            cleanup();
        }
        self.hold.take();
    }
}

impl Drop for ViewerLifetime {
    fn drop(&mut self) {
        self.close();
    }
}

fn keep_while_open(
    page: &MediaViewer,
    paths: impl IntoIterator<Item = std::path::PathBuf>,
) -> Rc<RefCell<ViewerLifetime>> {
    page.lifetime.borrow_mut().hold = Some(crate::media_retention::hold(paths));
    page.lifetime.clone()
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

fn media_viewer(
    details: &ViewerDetails,
    content: &impl IsA<gtk::Widget>,
    zoom: Option<&ZoomPill>,
    actions: &[gtk::Widget],
    on_close: Rc<dyn Fn()>,
) -> Viewer {
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
        .show_back_button(false)
        .show_start_title_buttons(true)
        .show_end_title_buttons(true)
        .title_widget(&centre)
        .build();
    let close = header_button("go-previous-symbolic", &gettext("Back"));
    close.connect_clicked({
        let on_close = on_close.clone();
        move |_| on_close()
    });
    header.pack_start(&close);
    header.pack_start(&identity);
    for action in actions.iter().rev() {
        header.pack_end(action);
    }

    let metadata = label_with(&["caption", "dim-label", "numeric", "zaptide-viewer-metadata"]);
    metadata.set_hexpand(true);
    metadata.set_xalign(1.0);
    metadata.set_ellipsize(gtk::pango::EllipsizeMode::End);
    metadata.set_width_chars(1);
    metadata.set_halign(gtk::Align::End);
    let caption = caption::Caption::new(&metadata);

    let view = adw::ToolbarView::builder()
        .content(content)
        .top_bar_style(adw::ToolbarStyle::Flat)
        .bottom_bar_style(adw::ToolbarStyle::Flat)
        .css_classes(["zaptide-viewer"])
        .build();
    view.add_top_bar(&header);
    view.add_bottom_bar(&caption.widget);
    let responsive_view = adw::BreakpointBin::builder()
        .width_request(MIN_VIEWER.0)
        .height_request(MIN_VIEWER.1)
        .child(&view)
        .build();
    if let Ok(condition) = adw::BreakpointCondition::parse(NARROW_VIEWER) {
        let narrow = adw::Breakpoint::new(condition);
        let hidden = false.to_value();
        narrow.add_setter(&avatar, "visible", Some(&hidden));
        narrow.add_setter(&time, "visible", Some(&hidden));
        narrow.add_setter(&counter_slot, "visible", Some(&hidden));
        if let Some(zoom) = zoom {
            narrow.add_setter(&zoom.shrink, "visible", Some(&hidden));
            narrow.add_setter(&zoom.grow, "visible", Some(&hidden));
        }
        responsive_view.add_breakpoint(narrow);
    }
    if let Ok(condition) = adw::BreakpointCondition::parse("max-width: 380px") {
        let compact = adw::Breakpoint::new(condition);
        compact.add_setter(&identity, "visible", Some(&false.to_value()));
        compact.add_setter(&counter_slot, "visible", Some(&false.to_value()));
        if let Some(zoom) = zoom {
            compact.add_setter(&zoom.shrink, "visible", Some(&false.to_value()));
            compact.add_setter(&zoom.grow, "visible", Some(&false.to_value()));
        }
        responsive_view.add_breakpoint(compact);
    }
    responsive_view.set_widget_name("zaptide-media-viewer");
    responsive_view.set_hexpand(true);
    responsive_view.set_vexpand(true);
    responsive_view.set_focusable(true);
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed({
        let on_close = on_close.clone();
        move |_, key, _, _| {
            if key == gdk::Key::Escape {
                on_close();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }
    });
    responsive_view.add_controller(keys);

    let parts = ViewerParts {
        avatar,
        name,
        time,
        counter,
        caption,
        metadata,
    };
    parts.show(details, None);
    Viewer {
        page: MediaViewer {
            widget: responsive_view,
            lifetime: Rc::new(RefCell::new(ViewerLifetime::new([]))),
            on_close,
        },
        view,
        parts,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
thread_local! {
    static TEST_PAGE: RefCell<Option<MediaViewer>> = const { RefCell::new(None) };
}

#[cfg(test)]
fn test_host(parent: &impl IsA<gtk::Widget>) -> (gtk::Box, Rc<dyn Fn()>) {
    let window = parent
        .as_ref()
        .root()
        .and_downcast::<adw::Window>()
        .unwrap();
    let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    window.set_content(Some(&host));
    let weak = host.downgrade();
    let on_close: Rc<dyn Fn()> = Rc::new(move || {
        TEST_PAGE.with(|page| {
            page.borrow_mut().take();
        });
        if let Some(host) = weak.upgrade() {
            while let Some(child) = host.first_child() {
                host.remove(&child);
            }
        }
    });
    (host, on_close)
}

#[cfg(test)]
fn present_test_page(host: &gtk::Box, page: MediaViewer) {
    host.append(&page.widget);
    page.widget.grab_focus();
    TEST_PAGE.with(|open| open.replace(Some(page)));
}

#[cfg(test)]
mod lifetime_tests {
    use super::*;

    #[test]
    fn viewer_lifetime_releases_files_and_stops_playback_once() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("video.mp4");
        let stops = Rc::new(Cell::new(0));
        let count = stops.clone();
        let mut lifetime = ViewerLifetime::new([path.clone()]);
        lifetime.on_close(move || count.set(count.get() + 1));
        assert!(crate::media_retention::is_held(&path));
        lifetime.close();
        assert!(!crate::media_retention::is_held(&path));
        assert_eq!(stops.get(), 1);
        drop(lifetime);
        assert_eq!(stops.get(), 1);
    }

    #[test]
    fn viewer_lifetime_retains_files_until_cleanup_finishes_on_drop() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("video.mp4");
        let held_during_cleanup = Rc::new(Cell::new(false));
        {
            let mut lifetime = ViewerLifetime::new([path.clone()]);
            let held = held_during_cleanup.clone();
            let cleanup_path = path.clone();
            lifetime.on_close(move || {
                held.set(crate::media_retention::is_held(&cleanup_path));
            });
            assert!(crate::media_retention::is_held(&path));
        }
        assert!(held_during_cleanup.get());
        assert!(!crate::media_retention::is_held(&path));
    }
}
