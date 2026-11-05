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
const SEEK_STEP: f64 = 5_000_000.0;
const SEEK_PAGE: f64 = 10_000_000.0;

/// Who sent a photo or video, when, its caption, and a one-line summary of
/// the file, for the viewer.
#[derive(Clone)]
pub(super) struct ViewerDetails {
    title: String,
    subtitle: String,
    caption: Option<String>,
    thumbnail: Option<Vec<u8>>,
    metadata: String,
    dimensions: Option<(u32, u32)>,
}

impl ViewerDetails {
    pub(super) fn of(message: &Message) -> Self {
        let title = if message.from_me {
            gettext("You")
        } else {
            message
                .sender_name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| {
                    crate::model::phone_of(&message.sender)
                        .map(crate::util::phone)
                        .unwrap_or_default()
                })
        };
        let (caption, metadata, dimensions) = match &message.content {
            Content::Image { caption, media } => (
                caption.clone(),
                metadata_line(&media.mime, media.width, media.height, None, media.size),
                media.width.zip(media.height),
            ),
            Content::Video {
                caption,
                media,
                seconds,
                ..
            } => (
                caption.clone(),
                metadata_line(&media.mime, media.width, media.height, *seconds, media.size),
                None,
            ),
            _ => (None, String::new(), None),
        };
        Self {
            title,
            subtitle: crate::util::clock(message.timestamp),
            caption: caption.filter(|caption| !caption.is_empty()),
            thumbnail: message.thumbnail.clone(),
            metadata,
            dimensions,
        }
    }
}

#[derive(Clone)]
pub(super) struct PhotoItem {
    pub(super) path: std::path::PathBuf,
    pub(super) details: ViewerDetails,
}

fn format_name(mime: &str) -> Option<String> {
    let subtype = mime.split_once('/')?.1;
    let subtype = subtype.split([';', '+']).next()?.trim();
    let subtype = subtype.strip_prefix("x-").unwrap_or(subtype);
    let name = match subtype {
        "" => return None,
        "quicktime" => "MOV",
        "matroska" => "MKV",
        other => return Some(other.to_ascii_uppercase()),
    };
    Some(name.to_owned())
}

fn dimensions_text(width: Option<u32>, height: Option<u32>) -> Option<String> {
    match (width, height) {
        (Some(width), Some(height)) if width > 0 && height > 0 => {
            Some(format!("{width} × {height}"))
        }
        _ => None,
    }
}

fn file_size_text(size: u64) -> Option<String> {
    const UNITS: [&str; 3] = ["KB", "MB", "GB"];
    if size == 0 {
        return None;
    }
    if size < 1024 {
        return Some(format!("{size} B"));
    }
    let mut value = size as f64 / 1024.0;
    let mut unit = 0;
    loop {
        let precision = usize::from(unit > 0);
        let rounded = format!("{value:.precision$}");
        let overflows = rounded
            .parse::<f64>()
            .is_ok_and(|rounded| rounded >= 1024.0);
        if !overflows || unit + 1 == UNITS.len() {
            return Some(format!("{rounded} {}", UNITS[unit]));
        }
        value /= 1024.0;
        unit += 1;
    }
}

fn metadata_line(
    mime: &str,
    width: Option<u32>,
    height: Option<u32>,
    seconds: Option<u32>,
    size: u64,
) -> String {
    [
        format_name(mime),
        dimensions_text(width, height),
        seconds.map(crate::util::duration),
        file_size_text(size),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
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

fn seek_target(value: f64, duration: i64) -> i64 {
    (value.max(0.0) as i64).min(duration.max(0))
}

fn photo_label(name: &str) -> String {
    gettext("Photo from {name}").replace("{name}", name)
}

fn video_label(name: &str) -> String {
    gettext("Video from {name}").replace("{name}", name)
}

fn clock_text(micros: i64) -> String {
    crate::util::duration(u32::try_from(micros.max(0) / 1_000_000).unwrap_or(u32::MAX))
}

fn volume_icon(volume: f64, muted: bool) -> &'static str {
    if muted || volume <= 0.0 {
        "audio-volume-muted-symbolic"
    } else if volume < 0.34 {
        "audio-volume-low-symbolic"
    } else if volume < 0.67 {
        "audio-volume-medium-symbolic"
    } else {
        "audio-volume-high-symbolic"
    }
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

fn step_button(icon: &str, tooltip: &str, side: gtk::Align) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .halign(side)
        .valign(gtk::Align::Center)
        .margin_start(12)
        .margin_end(12)
        .css_classes(["circular", "zaptide-viewer-nav"])
        .build();
    button.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    button
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ZoomKey {
    In,
    Out,
    Fit,
    Actual,
}

fn zoom_key(key: gdk::Key, state: gdk::ModifierType) -> Option<ZoomKey> {
    let held =
        gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK | gdk::ModifierType::META_MASK;
    if state.intersects(held) {
        return None;
    }
    match key {
        gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add => Some(ZoomKey::In),
        gdk::Key::minus | gdk::Key::KP_Subtract => Some(ZoomKey::Out),
        gdk::Key::_0 | gdk::Key::KP_0 => Some(ZoomKey::Fit),
        gdk::Key::_1 | gdk::Key::KP_1 => Some(ZoomKey::Actual),
        _ => None,
    }
}

fn pan_key(key: gdk::Key, state: gdk::ModifierType) -> Option<(f64, f64)> {
    let held = gdk::ModifierType::SHIFT_MASK
        | gdk::ModifierType::CONTROL_MASK
        | gdk::ModifierType::ALT_MASK
        | gdk::ModifierType::SUPER_MASK
        | gdk::ModifierType::META_MASK;
    if state.intersects(held) {
        return None;
    }
    match key {
        gdk::Key::Left => Some((-1.0, 0.0)),
        gdk::Key::Right => Some((1.0, 0.0)),
        gdk::Key::Up => Some((0.0, -1.0)),
        gdk::Key::Down => Some((0.0, 1.0)),
        _ => None,
    }
}

fn can_scroll(adjustment: &gtk::Adjustment) -> bool {
    adjustment.upper() - adjustment.page_size() > 0.5
}

type WaitingCopy = Box<dyn FnOnce(&gdk::Texture)>;

/// What the photo viewer is showing. Widgets that own a handler for it are
/// held weakly, so it never keeps the dialog alive.
struct PhotoState {
    items: Rc<Vec<PhotoItem>>,
    current: Cell<usize>,
    canvas: ZoomCanvas,
    scroller: glib::WeakRef<gtk::ScrolledWindow>,
    pill: ZoomPillWeak,
    copy: glib::WeakRef<gtk::Button>,
    previous: glib::WeakRef<gtk::Button>,
    next: glib::WeakRef<gtk::Button>,
    parts: ViewerParts,
    /// Size of the first decode: what the viewer needs to show the photo
    /// fitted, not the photo itself.
    first_decode: (u32, u32),
    /// A photo has loaded: zoom and copy wait for it, so neither acts on the
    /// sender's tiny thumbnail.
    ready: Cell<bool>,
    texture: RefCell<Option<gdk::Texture>>,
    /// Smallest density a decode in flight will reach, if one is.
    loading: Cell<Option<f64>>,
    /// Copies waiting for the photo at its real size.
    waiting: RefCell<Vec<WaitingCopy>>,
    token: DecodeToken,
}

impl PhotoState {
    fn item(&self) -> &PhotoItem {
        &self.items[self.current.get()]
    }

    fn view_size(&self) -> (f64, f64) {
        self.scroller.upgrade().map_or((0.0, 0.0), |scroller| {
            (
                scroller.hadjustment().page_size(),
                scroller.vadjustment().page_size(),
            )
        })
    }

    fn bounds(&self) -> Bounds {
        Bounds::new(self.canvas.picture(), self.view_size())
    }

    fn pannable(&self) -> (bool, bool) {
        if self.canvas.level() == Level::Fit {
            return (false, false);
        }
        self.scroller.upgrade().map_or((false, false), |scroller| {
            (
                can_scroll(&scroller.hadjustment()),
                can_scroll(&scroller.vadjustment()),
            )
        })
    }

    fn sync_controls(&self) {
        let ready = self.ready.get();
        let bounds = self.bounds();
        let scale = self.canvas.scale();
        self.pill.show(&ZoomView {
            percent: zoom::percent(scale),
            at_fit: self.canvas.level() == Level::Fit,
            can_shrink: bounds.can_shrink(scale),
            can_grow: bounds.can_grow(scale),
            ready,
        });
        if let Some(copy) = self.copy.upgrade() {
            copy.set_sensitive(ready);
        }
        if let Some(scroller) = self.scroller.upgrade() {
            let (across, down) = self.pannable();
            scroller.set_cursor_from_name((across || down).then_some("grab"));
        }
    }

    fn texture_size(&self) -> (u32, u32) {
        self.texture.borrow().as_ref().map_or((0, 0), |texture| {
            (
                u32::try_from(texture.width()).unwrap_or(0),
                u32::try_from(texture.height()).unwrap_or(0),
            )
        })
    }

    fn real_size(&self) -> (u32, u32) {
        zoom::decode_size(self.canvas.picture(), 1.0, NATIVE_PIXELS)
    }

    fn has_real_size(&self) -> bool {
        let (have, real) = (self.texture_size(), self.real_size());
        have.0 + 1 >= real.0 && have.1 + 1 >= real.1
    }

    fn show(self: &Rc<Self>, index: usize) {
        self.current.set(index);
        self.ready.set(false);
        self.loading.set(None);
        self.waiting.take();
        self.texture.take();
        let item = &self.items[index];
        let thumbnail = item
            .details
            .thumbnail
            .as_deref()
            .and_then(super::inline_thumbnail_texture);
        let picture = thumbnail.as_ref().map_or((1.0, 1.0), |texture| {
            zoom::logical_size(
                (
                    u32::try_from(texture.width()).unwrap_or(1),
                    u32::try_from(texture.height()).unwrap_or(1),
                ),
                item.details.dimensions,
            )
        });
        self.canvas.set_level(Level::Fit);
        self.canvas.set_picture(thumbnail, picture);
        if let Some(scroller) = self.scroller.upgrade() {
            scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Never);
            scroller.hadjustment().set_value(0.0);
            scroller.vadjustment().set_value(0.0);
        }
        self.sync_controls();
        self.canvas
            .update_property(&[gtk::accessible::Property::Label(&photo_label(
                &item.details.title,
            ))]);
        self.parts
            .show(&item.details, Some((index, self.items.len())));
        if let Some(previous) = self.previous.upgrade() {
            previous.set_sensitive(index > 0);
        }
        if let Some(next) = self.next.upgrade() {
            next.set_sensitive(index + 1 < self.items.len());
        }
        let declared = item.details.dimensions;
        let state = self.clone();
        decode_photo(
            item.path.clone(),
            self.first_decode.0,
            self.first_decode.1,
            &self.token,
            move |texture| {
                let size = (
                    u32::try_from(texture.width()).unwrap_or(1),
                    u32::try_from(texture.height()).unwrap_or(1),
                );
                let picture = zoom::logical_size(size, declared);
                state.canvas.set_picture(Some(texture.clone()), picture);
                state.texture.replace(Some(texture));
                state.ready.set(true);
                state.sync_controls();
                state.refine();
            },
        );
    }

    fn step(self: &Rc<Self>, delta: isize) {
        if let Some(target) = step_target(self.current.get(), delta, self.items.len()) {
            self.show(target);
        }
    }

    /// Zooms to `level`, keeping the point of the photo under `anchor` (a
    /// position in the view, by default its middle) where it is.
    fn set_level(self: &Rc<Self>, level: Level, anchor: Option<(f64, f64)>) {
        let Some(scroller) = self.scroller.upgrade().filter(|_| self.ready.get()) else {
            return;
        };
        let view = self.view_size();
        let picture = self.canvas.picture();
        let bounds = Bounds::new(picture, view);
        let level = match level {
            Level::Fit => Level::Fit,
            Level::Scale(scale) => bounds.level(scale),
        };
        if level == self.canvas.level() {
            return;
        }
        let from = self.canvas.scale();
        let to = match level {
            Level::Fit => bounds.fit,
            Level::Scale(scale) => scale,
        };
        let (across, down) = (scroller.hadjustment(), scroller.vadjustment());
        let anchor = anchor.unwrap_or((view.0 / 2.0, view.1 / 2.0));
        let offsets = (
            zoom::anchored_offset(picture.0, view.0, from, to, across.value(), anchor.0),
            zoom::anchored_offset(picture.1, view.1, from, to, down.value(), anchor.1),
        );
        self.canvas.set_level(level);
        let policy = if level == Level::Fit {
            gtk::PolicyType::Never
        } else {
            gtk::PolicyType::Automatic
        };
        scroller.set_policy(policy, policy);
        if level == Level::Fit {
            across.set_value(0.0);
            down.set_value(0.0);
        } else {
            across.set_upper(zoom::content_len(picture.0, view.0, to));
            down.set_upper(zoom::content_len(picture.1, view.1, to));
            across.set_value(offsets.0);
            down.set_value(offsets.1);
        }
        self.sync_controls();
        self.refine();
    }

    fn zoom_by(self: &Rc<Self>, zoom_in: bool) {
        let level = self.bounds().stepped(self.canvas.scale(), zoom_in);
        self.set_level(level, None);
    }

    fn toggle_fit(self: &Rc<Self>, anchor: Option<(f64, f64)>) {
        self.set_level(self.canvas.level().toggled(), anchor);
    }

    /// Holding Ctrl, the wheel zooms about the pointer; otherwise it is left
    /// to the scrolled window.
    fn on_scroll(
        self: &Rc<Self>,
        state: gdk::ModifierType,
        delta: f64,
        anchor: Option<(f64, f64)>,
    ) -> glib::Propagation {
        if !state.contains(gdk::ModifierType::CONTROL_MASK) || !self.ready.get() {
            return glib::Propagation::Proceed;
        }
        let scale = self.bounds().wheeled(self.canvas.scale(), delta);
        self.set_level(Level::Scale(scale), anchor);
        glib::Propagation::Stop
    }

    /// Moves the view by one key press's worth, if the photo is larger than
    /// the view that way; true when it did.
    fn pan(&self, (across, down): (f64, f64)) -> bool {
        let Some(scroller) = self.scroller.upgrade() else {
            return false;
        };
        let (wide, tall) = self.pannable();
        let mut moved = false;
        for (adjustment, direction, scrolls) in [
            (scroller.hadjustment(), across, wide),
            (scroller.vadjustment(), down, tall),
        ] {
            if direction != 0.0 && scrolls {
                adjustment.set_value(adjustment.value() + direction * adjustment.step_increment());
                moved = true;
            }
        }
        moved
    }

    fn layout_changed(self: &Rc<Self>) {
        self.sync_controls();
        self.refine();
    }

    /// Loads more of the photo when what is on screen has become soft.
    fn refine(self: &Rc<Self>) {
        if !self.ready.get() {
            return;
        }
        let factor = self.canvas.scale_factor();
        let scale = self.canvas.scale();
        let density = zoom::density(self.texture_size(), self.canvas.picture());
        if zoom::needs_sharper(density, scale, factor) {
            self.load(zoom::next_density(scale, factor));
        }
    }

    fn load(self: &Rc<Self>, density: f64) {
        if self.loading.get().is_some_and(|loading| loading >= density) {
            return;
        }
        let size = zoom::decode_size(self.canvas.picture(), density, NATIVE_PIXELS);
        let have = self.texture_size();
        if size.0 <= have.0 && size.1 <= have.1 {
            return;
        }
        self.loading.set(Some(density));
        let state = self.clone();
        decode_photo(
            self.item().path.clone(),
            size.0,
            size.1,
            &self.token,
            move |texture| {
                state.loading.set(None);
                state
                    .canvas
                    .set_picture(Some(texture.clone()), state.canvas.picture());
                state.texture.replace(Some(texture.clone()));
                if state.has_real_size() {
                    for then in state.waiting.take() {
                        then(&texture);
                    }
                }
                state.refine();
            },
        );
    }

    fn with_real_size(self: &Rc<Self>, then: impl FnOnce(&gdk::Texture) + 'static) {
        let texture = self.texture.borrow().clone();
        match texture {
            Some(texture) if self.has_real_size() => then(&texture),
            Some(_) => {
                self.waiting.borrow_mut().push(Box::new(then));
                self.load(1.0);
            }
            None => {}
        }
    }
}

/// Opens the photos in the viewer, starting at `start`: fitted to the window,
/// then zoomed freely with the pill, the keyboard, Ctrl and the scroll wheel
/// or a pinch, and dragged around when larger than the window.
pub(super) fn show_photo(
    parent: &impl IsA<gtk::Widget>,
    items: Rc<Vec<PhotoItem>>,
    start: usize,
    on_action: Rc<dyn Fn(NativeMediaAction)>,
) {
    if items.is_empty() {
        return;
    }
    let start = start.min(items.len() - 1);
    let canvas = ZoomCanvas::new();
    let scroller = gtk::ScrolledWindow::builder()
        .child(&canvas)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .build();

    let pill = ZoomPill::new();
    let copy = header_button("edit-copy-symbolic", &gettext("Copy Image"));
    copy.set_sensitive(false);
    let folder = header_button("folder-open-symbolic", &gettext("Show in Folder"));
    let open = header_button(
        "adw-external-link-symbolic",
        &gettext("Open With Another App"),
    );
    let previous = step_button(
        "go-previous-symbolic",
        &gettext("Previous Photo"),
        gtk::Align::Start,
    );
    let next = step_button("go-next-symbolic", &gettext("Next Photo"), gtk::Align::End);
    let overlay = gtk::Overlay::builder().child(&scroller).build();
    if items.len() > 1 {
        overlay.add_overlay(&previous);
        overlay.add_overlay(&next);
    }
    let viewer = media_viewer(
        parent,
        &items[start].details,
        &overlay,
        Some(&pill),
        &[
            copy.clone().upcast(),
            folder.clone().upcast(),
            open.clone().upcast(),
        ],
    );
    let dialog = viewer.dialog.clone();
    keep_while_open(&dialog, items.iter().map(|item| item.path.clone()));
    let photo = Rc::new(PhotoState {
        items,
        current: Cell::new(start),
        canvas: canvas.clone(),
        scroller: scroller.downgrade(),
        pill: pill.downgrade(),
        copy: copy.downgrade(),
        previous: previous.downgrade(),
        next: next.downgrade(),
        parts: viewer.parts,
        first_decode: zoom::decode_box(viewer_size(parent), parent.scale_factor(), FIT_EDGE),
        ready: Cell::new(false),
        texture: RefCell::default(),
        loading: Cell::new(None),
        waiting: RefCell::default(),
        token: DecodeToken::default(),
    });
    pill.connect(&photo);
    canvas.connect_layout_changed({
        let photo = Rc::downgrade(&photo);
        move |_| {
            if let Some(photo) = photo.upgrade() {
                photo.layout_changed();
            }
        }
    });

    copy.connect_clicked({
        let photo = photo.clone();
        move |button| {
            let clipboard = button.clipboard();
            photo.with_real_size(move |texture| clipboard.set_texture(texture));
        }
    });
    folder.connect_clicked({
        let photo = photo.clone();
        move |button| show_in_folder(button, &photo.item().path)
    });
    open.connect_clicked({
        let (photo, dialog) = (photo.clone(), dialog.downgrade());
        move |_| {
            on_action(NativeMediaAction::Open(photo.item().path.clone()));
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
        }
    });
    for (button, delta) in [(&previous, -1), (&next, 1)] {
        let photo = photo.clone();
        button.connect_clicked(move |_| photo.step(delta));
    }

    let pointer: Rc<Cell<Option<(f64, f64)>>> = Rc::default();
    let motion = gtk::EventControllerMotion::new();
    motion.connect_motion({
        let pointer = pointer.clone();
        move |_, x, y| pointer.set(Some((x, y)))
    });
    motion.connect_leave({
        let pointer = pointer.clone();
        move |_| pointer.set(None)
    });
    scroller.add_controller(motion);

    let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
    wheel.connect_scroll({
        let (photo, pointer) = (photo.clone(), pointer.clone());
        move |controller, _, delta| {
            photo.on_scroll(controller.current_event_state(), delta, pointer.get())
        }
    });
    scroller.add_controller(wheel);

    let pinch = gtk::GestureZoom::new();
    let pinched_from = Rc::new(Cell::new(1.0));
    pinch.connect_begin({
        let (photo, pinched_from) = (photo.clone(), pinched_from.clone());
        move |_, _| pinched_from.set(photo.canvas.scale())
    });
    pinch.connect_scale_changed({
        let photo = photo.clone();
        move |gesture, factor| {
            photo.set_level(
                Level::Scale(pinched_from.get() * factor),
                gesture.bounding_box_center(),
            );
        }
    });
    scroller.add_controller(pinch);

    let double_click = gtk::GestureClick::new();
    double_click.connect_pressed({
        let photo = photo.clone();
        move |_, presses, x, y| {
            if presses == 2 {
                photo.toggle_fit(Some((x, y)));
            }
        }
    });
    scroller.add_controller(double_click);
    let pan = gtk::GestureDrag::new();
    let origin = Rc::new(Cell::new((0.0, 0.0)));
    {
        let (scroller, origin) = (scroller.clone(), origin.clone());
        pan.connect_drag_begin(move |_, _, _| {
            origin.set((
                scroller.hadjustment().value(),
                scroller.vadjustment().value(),
            ));
            if scroller.cursor().is_some() {
                scroller.set_cursor_from_name(Some("grabbing"));
            }
        });
    }
    {
        let scroller = scroller.clone();
        pan.connect_drag_update(move |_, x, y| {
            let (h, v) = origin.get();
            scroller.hadjustment().set_value(h - x);
            scroller.vadjustment().set_value(v - y);
        });
    }
    {
        let photo = photo.clone();
        pan.connect_drag_end(move |_, _, _| photo.sync_controls());
    }
    scroller.add_controller(pan);

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed({
        let photo = photo.clone();
        move |controller, key, _, modifiers| {
            if let Some(command) = zoom_key(key, modifiers) {
                if !photo.ready.get() {
                    return glib::Propagation::Proceed;
                }
                match command {
                    ZoomKey::In => photo.zoom_by(true),
                    ZoomKey::Out => photo.zoom_by(false),
                    ZoomKey::Fit => photo.set_level(Level::Fit, None),
                    ZoomKey::Actual => photo.set_level(Level::Scale(zoom::ACTUAL), None),
                }
                return glib::Propagation::Stop;
            }
            // A photo larger than the view is scrolled by the arrows instead.
            if let Some(direction) = pan_key(key, modifiers)
                && photo.pan(direction)
            {
                return glib::Propagation::Stop;
            }
            let rtl = controller
                .widget()
                .is_some_and(|widget| widget.direction() == gtk::TextDirection::Rtl);
            match key_step(key, modifiers, rtl) {
                Some(delta) => {
                    photo.step(delta);
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            }
        }
    });
    dialog.add_controller(keys);
    #[cfg(test)]
    OPEN_PHOTO.with(|open| open.replace(Some(photo.clone())));
    photo.show(start);
    dialog.present(Some(parent));
}

#[cfg(test)]
thread_local! {
    static OPEN_PHOTO: RefCell<Option<Rc<PhotoState>>> = const { RefCell::new(None) };
}

struct VideoControls {
    bar: adw::Clamp,
    big_play: gtk::Button,
}

/// The floating play, progress, volume and fullscreen controls of `clip`,
/// and the large play button shown while it is paused. The widgets hold the
/// stream weakly, so the picture showing it stays its only owner.
///
/// Fullscreen belongs to the window that hosts the viewer: it is remembered
/// when entered and left again when the dialog closes, whatever the order of
/// the dialog's teardown.
fn video_controls(
    clip: &VideoStream,
    dialog: &adw::Dialog,
    parent: &impl IsA<gtk::Widget>,
) -> VideoControls {
    let window = parent.as_ref().root().and_downcast::<gtk::Window>();
    let size = viewer_size(parent);
    let play = header_button("media-playback-start-symbolic", &gettext("Play"));
    let position = label_with(&["caption", "numeric"]);
    let length = label_with(&["caption", "dim-label", "numeric"]);
    let seek = gtk::Scale::builder()
        .orientation(gtk::Orientation::Horizontal)
        .adjustment(&gtk::Adjustment::new(
            0.0, 0.0, 1.0, SEEK_STEP, SEEK_PAGE, 0.0,
        ))
        .draw_value(false)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .css_classes(["zaptide-video-seek"])
        .build();
    seek.update_property(&[gtk::accessible::Property::Label(&gettext("Seek"))]);

    let volume_scale = gtk::Scale::builder()
        .orientation(gtk::Orientation::Vertical)
        .adjustment(&gtk::Adjustment::new(
            clip.volume(),
            0.0,
            1.0,
            0.05,
            0.1,
            0.0,
        ))
        .draw_value(false)
        .inverted(true)
        .height_request(120)
        .build();
    volume_scale.update_property(&[gtk::accessible::Property::Label(&gettext("Volume"))]);
    let volume = gtk::MenuButton::builder()
        .icon_name(volume_icon(clip.volume(), clip.is_muted()))
        .tooltip_text(gettext("Volume"))
        .direction(gtk::ArrowType::Up)
        .valign(gtk::Align::Center)
        .popover(&gtk::Popover::builder().child(&volume_scale).build())
        .css_classes(["flat"])
        .build();
    volume.update_property(&[gtk::accessible::Property::Label(&gettext("Volume"))]);
    let fullscreen = header_button("view-fullscreen-symbolic", &gettext("Fullscreen"));
    let set_fullscreen_look = |button: &gtk::Button, on: bool| {
        let (icon, label) = if on {
            ("view-restore-symbolic", gettext("Exit Fullscreen"))
        } else {
            ("view-fullscreen-symbolic", gettext("Fullscreen"))
        };
        button.set_icon_name(icon);
        button.set_tooltip_text(Some(&label));
        button.update_property(&[gtk::accessible::Property::Label(&label)]);
    };
    if window.as_ref().is_some_and(|window| window.is_fullscreen()) {
        set_fullscreen_look(&fullscreen, true);
    }

    let row = gtk::Box::builder()
        .spacing(10)
        .css_classes(["zaptide-video-controls"])
        .build();
    for widget in [
        play.upcast_ref::<gtk::Widget>(),
        position.upcast_ref(),
        seek.upcast_ref(),
        length.upcast_ref(),
        volume.upcast_ref(),
        fullscreen.upcast_ref(),
    ] {
        row.append(widget);
    }
    let bar = adw::Clamp::builder()
        .maximum_size(640)
        .tightening_threshold(480)
        .child(&row)
        .valign(gtk::Align::End)
        .margin_start(16)
        .margin_end(16)
        .margin_bottom(16)
        .visible(false)
        .build();

    let big_play = gtk::Button::builder()
        .icon_name("media-playback-start-symbolic")
        .tooltip_text(gettext("Play"))
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes(["circular", "zaptide-play", "zaptide-video-play"])
        .visible(false)
        .build();
    big_play.update_property(&[gtk::accessible::Property::Label(&gettext("Play"))]);

    play.connect_clicked({
        let clip = clip.downgrade();
        move |_| {
            if let Some(clip) = clip.upgrade() {
                if clip.is_playing() {
                    clip.pause();
                } else {
                    clip.play();
                }
            }
        }
    });
    big_play.connect_clicked({
        let clip = clip.downgrade();
        move |_| {
            if let Some(clip) = clip.upgrade() {
                clip.play();
            }
        }
    });

    // A drag moves the handle and the time only; the video seeks once, where
    // the handle is let go. Keys and the wheel seek at once.
    let dragging = Rc::new(Cell::new(false));
    let finish_drag: Rc<dyn Fn()> = Rc::new({
        let (clip, seek, dragging) = (clip.downgrade(), seek.downgrade(), dragging.clone());
        move || {
            if !dragging.replace(false) {
                return;
            }
            if let (Some(clip), Some(seek)) = (clip.upgrade(), seek.upgrade())
                && clip.is_seekable()
            {
                clip.seek(seek_target(seek.value(), clip.duration()));
            }
        }
    });
    let drag = gtk::GestureDrag::new();
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);
    drag.connect_drag_begin({
        let dragging = dragging.clone();
        move |_, _, _| dragging.set(true)
    });
    drag.connect_drag_end({
        let finish_drag = finish_drag.clone();
        move |_, _, _| finish_drag()
    });
    drag.connect_cancel(move |_, _| finish_drag());
    seek.add_controller(drag);
    seek.connect_change_value({
        let (clip, position, dragging) = (clip.downgrade(), position.downgrade(), dragging.clone());
        move |_, _, value| {
            if let Some(clip) = clip.upgrade() {
                let target = seek_target(value, clip.duration());
                if dragging.get() {
                    if let Some(position) = position.upgrade() {
                        position.set_label(&clock_text(target));
                    }
                } else if clip.is_seekable() {
                    clip.seek(target);
                }
            }
            glib::Propagation::Proceed
        }
    });
    volume_scale.connect_value_changed({
        let clip = clip.downgrade();
        move |scale| {
            if let Some(clip) = clip.upgrade() {
                clip.set_muted(false);
                clip.set_volume(scale.value());
            }
        }
    });

    let entered = Rc::new(Cell::new(false));
    fullscreen.connect_clicked({
        let (entered, dialog, window) = (
            entered.clone(),
            dialog.downgrade(),
            window.as_ref().map(ObjectExt::downgrade),
        );
        move |button| {
            let (Some(window), Some(dialog)) = (
                window.as_ref().and_then(glib::WeakRef::upgrade),
                dialog.upgrade(),
            ) else {
                return;
            };
            let enter = !window.is_fullscreen();
            window.set_fullscreened(enter);
            entered.set(enter);
            let (width, height) = if enter {
                (UNLIMITED_DIALOG_SIZE, UNLIMITED_DIALOG_SIZE)
            } else {
                size
            };
            dialog.set_content_width(width);
            dialog.set_content_height(height);
            set_fullscreen_look(button, enter);
        }
    });
    dialog.connect_closed({
        let window = window.as_ref().map(ObjectExt::downgrade);
        move |_| {
            if entered.replace(false)
                && let Some(window) = window.as_ref().and_then(glib::WeakRef::upgrade)
            {
                window.set_fullscreened(false);
            }
        }
    });

    let refresh = Rc::new(glib::clone!(
        #[weak]
        play,
        #[weak]
        position,
        #[weak]
        length,
        #[weak]
        seek,
        #[weak]
        volume,
        #[weak]
        bar,
        #[weak]
        big_play,
        move |clip: &VideoStream| {
            let ready = clip.is_prepared();
            let playing = clip.is_playing();
            if bar.is_visible() != ready {
                bar.set_visible(ready);
            }
            let show_big = ready && !playing;
            if big_play.is_visible() != show_big {
                big_play.set_visible(show_big);
            }
            let (icon, label) = if playing {
                ("media-playback-pause-symbolic", gettext("Pause"))
            } else {
                ("media-playback-start-symbolic", gettext("Play"))
            };
            if play.icon_name().as_deref() != Some(icon) {
                play.set_icon_name(icon);
                play.set_tooltip_text(Some(&label));
                play.update_property(&[gtk::accessible::Property::Label(&label)]);
            }
            let upper = clip.duration().max(1) as f64;
            let adjustment = seek.adjustment();
            if adjustment.upper() != upper {
                adjustment.set_upper(upper);
            }
            let seekable = clip.is_seekable();
            if seek.is_sensitive() != seekable {
                seek.set_sensitive(seekable);
            }
            if !dragging.get() {
                seek.set_value(clip.timestamp() as f64);
                let elapsed = clock_text(clip.timestamp());
                let total = clock_text(clip.duration());
                if position.label().as_str() != elapsed || length.label().as_str() != total {
                    position.set_label(&elapsed);
                    length.set_label(&total);
                    seek.update_property(&[gtk::accessible::Property::ValueText(&of_text(
                        &elapsed, &total,
                    ))]);
                }
            }
            volume.set_icon_name(volume_icon(clip.volume(), clip.is_muted()));
        }
    ));
    clip.connect_notify_local(None, {
        let refresh = refresh.clone();
        move |clip, _| refresh(clip)
    });
    refresh(clip);
    VideoControls { bar, big_play }
}

/// Plays `path` in the viewer with sound; stops when the viewer closes and
/// offers another app when GStreamer cannot play it.
pub(super) fn show_video(
    parent: &impl IsA<gtk::Widget>,
    path: &std::path::Path,
    details: &ViewerDetails,
    on_action: Rc<dyn Fn(NativeMediaAction)>,
) {
    let dialog_slot: Rc<std::cell::OnceCell<glib::WeakRef<adw::Dialog>>> = Rc::default();
    let clip = VideoStream::open_in_background(path, 1920, 1080, {
        let (path, on_action, dialog_slot) =
            (path.to_path_buf(), on_action.clone(), dialog_slot.clone());
        move || {
            on_action(NativeMediaAction::Open(path));
            if let Some(dialog) = dialog_slot.get().and_then(glib::WeakRef::upgrade) {
                dialog.close();
            }
        }
    });
    let picture = gtk::Picture::builder()
        .paintable(&clip)
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .hexpand(true)
        .vexpand(true)
        .build();
    picture.update_property(&[gtk::accessible::Property::Label(&video_label(
        &details.title,
    ))]);
    let failed = adw::StatusPage::builder()
        .icon_name("video-x-generic-symbolic")
        .title(gettext("Can't Play This Video"))
        .description(gettext("Open it with another app instead"))
        .build();
    let another = gtk::Button::builder()
        .label(gettext("Open in Another App"))
        .halign(gtk::Align::Center)
        .css_classes(["pill", "suggested-action"])
        .build();
    failed.set_child(Some(&another));

    let folder = header_button("folder-open-symbolic", &gettext("Show in Folder"));
    folder.connect_clicked({
        let target = path.to_path_buf();
        move |button| show_in_folder(button, &target)
    });
    let open = header_button(
        "adw-external-link-symbolic",
        &gettext("Open With Another App"),
    );
    let viewer = media_viewer(
        parent,
        details,
        &gtk::Box::new(gtk::Orientation::Vertical, 0),
        None,
        &[folder.upcast(), open.clone().upcast()],
    );
    let dialog = viewer.dialog.clone();
    let _ = dialog_slot.set(dialog.downgrade());
    keep_while_open(&dialog, [path.to_path_buf()]);
    open.connect_clicked(open_and_close(path, &on_action, &dialog));
    another.connect_clicked(open_and_close(path, &on_action, &dialog));

    let controls = video_controls(&clip, &dialog, parent);
    let overlay = gtk::Overlay::builder().child(&picture).build();
    let waiting = video_placeholder(details, &clip);
    overlay.add_overlay(&waiting);
    overlay.add_overlay(&controls.big_play);
    overlay.add_overlay(&controls.bar);
    let stack = gtk::Stack::new();
    stack.add_named(&overlay, Some("video"));
    stack.add_named(&failed, Some("failed"));
    viewer.view.set_content(Some(&stack));

    clip.connect_error_notify(glib::clone!(
        #[weak]
        stack,
        move |stream| {
            if let Some(error) = stream.error() {
                log::warn!("video could not be played: {error}");
                stack.set_visible_child_name("failed");
            }
        }
    ));
    if clip.is_prepared() {
        clip.play();
    } else {
        clip.connect_prepared_notify(|clip| {
            if clip.is_prepared() {
                clip.play();
            }
        });
    }
    // Held by the dialog, not looked up through the picture, which the
    // dialog may already have let go of by the time it reports closing.
    dialog.connect_closed(move |_| clip.close());
    dialog.present(Some(parent));
}

/// The sender's thumbnail with a spinner over it, shown until the clip has a
/// frame of its own or fails.
fn video_placeholder(details: &ViewerDetails, clip: &VideoStream) -> gtk::Overlay {
    let poster = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .build();
    poster.set_paintable(
        details
            .thumbnail
            .as_deref()
            .and_then(super::inline_thumbnail_texture)
            .as_ref(),
    );
    let placeholder = gtk::Overlay::builder()
        .child(&poster)
        .can_target(false)
        .build();
    placeholder.add_overlay(
        &adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build(),
    );
    let update = glib::clone!(
        #[weak]
        placeholder,
        move |clip: &VideoStream| {
            let shown =
                clip.error().is_none() && !(clip.is_prepared() && clip.intrinsic_width() > 0);
            placeholder.set_visible(shown);
        }
    );
    let update = Rc::new(update);
    clip.connect_invalidate_size({
        let update = update.clone();
        move |clip| update(clip)
    });
    clip.connect_notify_local(None, move |clip, _| update(clip));
    placeholder
}

fn open_and_close(
    path: &std::path::Path,
    on_action: &Rc<dyn Fn(NativeMediaAction)>,
    dialog: &adw::Dialog,
) -> impl Fn(&gtk::Button) + 'static {
    let (path, on_action, dialog) = (path.to_path_buf(), on_action.clone(), dialog.downgrade());
    move |_| {
        on_action(NativeMediaAction::Open(path.clone()));
        if let Some(dialog) = dialog.upgrade() {
            dialog.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_matches_the_prototype() {
        assert_eq!(
            metadata_line("image/png", Some(1600), Some(1000), None, 184 * 1024),
            "PNG · 1600 × 1000 · 184 KB"
        );
        assert_eq!(
            metadata_line("video/mp4", Some(1280), Some(720), Some(3), 1_258_291),
            "MP4 · 1280 × 720 · 0:03 · 1.2 MB"
        );
    }

    #[test]
    fn metadata_skips_what_is_unknown() {
        assert_eq!(metadata_line("image/jpeg", None, Some(10), None, 0), "JPEG");
        assert_eq!(metadata_line("", Some(0), Some(0), None, 12), "12 B");
        assert_eq!(metadata_line("garbage", None, None, None, 0), "");
    }

    #[test]
    fn format_names_follow_the_extension_people_know() {
        for (mime, name) in [
            ("image/jpeg", "JPEG"),
            ("image/svg+xml", "SVG"),
            ("video/quicktime", "MOV"),
            ("video/x-matroska", "MKV"),
            ("video/webm; codecs=vp8", "WEBM"),
        ] {
            assert_eq!(format_name(mime).as_deref(), Some(name), "{mime}");
        }
        assert_eq!(format_name("image/"), None);
    }

    #[test]
    fn file_sizes_round_to_the_nearest_unit() {
        assert_eq!(file_size_text(0), None);
        assert_eq!(file_size_text(1023).as_deref(), Some("1023 B"));
        assert_eq!(file_size_text(1024).as_deref(), Some("1 KB"));
        assert_eq!(file_size_text(1024 * 1024 - 1).as_deref(), Some("1.0 MB"));
        assert_eq!(file_size_text(5 * 1024 * 1024).as_deref(), Some("5.0 MB"));
        assert_eq!(file_size_text(3 << 30).as_deref(), Some("3.0 GB"));
    }

    #[test]
    fn the_counter_shows_only_for_several_items() {
        assert_eq!(counter_text(0, 1), None);
        assert_eq!(counter_text(0, 0), None);
        assert_eq!(counter_text(0, 2).as_deref(), Some("1 of 2"));
        assert_eq!(counter_text(11, 12).as_deref(), Some("12 of 12"));
    }

    #[test]
    fn zoom_keys_ignore_other_chords() {
        let none = gdk::ModifierType::empty();
        let ctrl = gdk::ModifierType::CONTROL_MASK;
        for (key, state, command) in [
            (gdk::Key::plus, ctrl, ZoomKey::In),
            (gdk::Key::equal, ctrl, ZoomKey::In),
            (gdk::Key::KP_Add, none, ZoomKey::In),
            (
                gdk::Key::plus,
                ctrl | gdk::ModifierType::SHIFT_MASK,
                ZoomKey::In,
            ),
            (gdk::Key::minus, ctrl, ZoomKey::Out),
            (gdk::Key::KP_Subtract, ctrl, ZoomKey::Out),
            (gdk::Key::_0, ctrl, ZoomKey::Fit),
            (gdk::Key::KP_0, none, ZoomKey::Fit),
            (gdk::Key::_1, none, ZoomKey::Actual),
            (gdk::Key::_1, ctrl, ZoomKey::Actual),
        ] {
            assert_eq!(zoom_key(key, state), Some(command), "{key:?}");
        }
        assert_eq!(zoom_key(gdk::Key::_1, gdk::ModifierType::ALT_MASK), None);
        assert_eq!(zoom_key(gdk::Key::Left, ctrl), None);
        assert_eq!(zoom_key(gdk::Key::c, ctrl), None);
    }

    #[test]
    fn arrows_pan_only_when_bare() {
        let none = gdk::ModifierType::empty();
        assert_eq!(pan_key(gdk::Key::Left, none), Some((-1.0, 0.0)));
        assert_eq!(pan_key(gdk::Key::Down, none), Some((0.0, 1.0)));
        assert_eq!(pan_key(gdk::Key::Right, gdk::ModifierType::ALT_MASK), None);
        assert_eq!(pan_key(gdk::Key::Up, gdk::ModifierType::CONTROL_MASK), None);
        assert_eq!(pan_key(gdk::Key::a, none), None);
    }

    #[test]
    fn the_zoom_level_has_an_accessible_name() {
        assert_eq!(zoom_level_text(120), "Zoom level 120 percent");
    }

    #[test]
    fn video_clock_and_volume_icons() {
        assert_eq!(clock_text(-5), "0:00");
        assert_eq!(clock_text(3_400_000), "0:03");
        assert_eq!(clock_text(125_000_000), "2:05");
        assert_eq!(volume_icon(0.0, false), "audio-volume-muted-symbolic");
        assert_eq!(volume_icon(1.0, true), "audio-volume-muted-symbolic");
        assert_eq!(volume_icon(0.2, false), "audio-volume-low-symbolic");
        assert_eq!(volume_icon(0.5, false), "audio-volume-medium-symbolic");
        assert_eq!(volume_icon(1.0, false), "audio-volume-high-symbolic");
    }

    fn descendants(root: &gtk::Widget, found: &mut Vec<gtk::Widget>) {
        let mut child = root.first_child();
        while let Some(widget) = child {
            found.push(widget.clone());
            descendants(&widget, found);
            child = widget.next_sibling();
        }
    }

    fn tree(window: &gtk::Window) -> Vec<gtk::Widget> {
        let mut found = Vec::new();
        descendants(window.upcast_ref(), &mut found);
        found
    }

    fn button(window: &gtk::Window, tooltip: &str) -> gtk::Button {
        tree(window)
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
            .find(|button| button.tooltip_text().as_deref() == Some(tooltip))
            .unwrap_or_else(|| panic!("no button {tooltip}"))
    }

    fn has_button(window: &gtk::Window, tooltip: &str) -> bool {
        tree(window).into_iter().any(|widget| {
            widget
                .downcast::<gtk::Button>()
                .is_ok_and(|button| button.tooltip_text().as_deref() == Some(tooltip))
        })
    }

    fn zoom_percent(window: &gtk::Window) -> String {
        tree(window)
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
            .filter(|button| {
                matches!(
                    button.tooltip_text().as_deref(),
                    Some("Actual Size" | "Fit to Window")
                )
            })
            .find_map(|button| button.child().and_downcast::<gtk::Label>())
            .expect("zoom level")
            .label()
            .to_string()
    }

    fn label(window: &gtk::Window, class: &str) -> gtk::Label {
        tree(window)
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
            .find(|label| label.has_css_class(class))
            .unwrap_or_else(|| panic!("no label {class}"))
    }

    fn screenshot(window: &impl IsA<gtk::Widget>, name: &str) {
        let Some(directory) = std::env::var_os("ZAPTIDE_VIEWER_SHOTS") else {
            return;
        };
        super::super::playback_tests::drain();
        let paintable = gtk::WidgetPaintable::new(Some(window));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(
            snapshot.upcast_ref::<gdk::Snapshot>(),
            f64::from(window.width()),
            f64::from(window.height()),
        );
        let node = snapshot.to_node().expect("render node");
        let texture = window
            .native()
            .and_then(|native| native.renderer())
            .expect("renderer")
            .render_texture(&node, None);
        let path = std::path::Path::new(&directory).join(format!("{name}.png"));
        texture.save_to_png(path).expect("screenshot");
    }

    fn fixture_message(
        directory: &std::path::Path,
        name: &str,
        width: u32,
        height: u32,
    ) -> crate::model::Message {
        let path = directory.join(name);
        image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([(x * 255 / width) as u8, (y * 255 / height) as u8, 160, 255])
        })
        .save(&path)
        .unwrap();
        let mut thumbnail = Vec::new();
        image::RgbaImage::from_fn(64, 40, |x, y| {
            image::Rgba([(x * 4) as u8, (y * 6) as u8, 160, 255])
        })
        .write_to(
            &mut std::io::Cursor::new(&mut thumbnail),
            image::ImageFormat::Png,
        )
        .unwrap();
        let mut message = crate::native_media_widgets::tests::message(Content::Image {
            caption: Some("Quarterly roadmap".into()),
            media: crate::model::Media {
                mime: "image/png".into(),
                size: 184 * 1024,
                width: Some(width),
                height: Some(height),
                path: Some(path),
                state: crate::model::MediaState::Idle,
            },
        });
        message.sender_name = Some("Marina Costa".into());
        message.thumbnail = Some(thumbnail);
        message
    }

    fn init_display() -> tokio::runtime::Runtime {
        gtk::init().expect("display");
        adw::init().expect("libadwaita");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let provider = gtk::CssProvider::new();
        provider.load_from_string(include_str!("../../data/style.css"));
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().unwrap(),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        runtime
    }

    fn photo_item(message: &Message) -> PhotoItem {
        PhotoItem {
            path: match attachment_action(message) {
                Some(NativeMediaAction::Open(path)) => path,
                _ => panic!("fixture is on disk"),
            },
            details: ViewerDetails::of(message),
        }
    }

    fn open_photo_state() -> Rc<PhotoState> {
        OPEN_PHOTO.with(|open| open.borrow().clone().expect("a photo viewer was opened"))
    }

    fn controller<T: IsA<gtk::EventController> + IsA<glib::Object>>(
        widget: &impl IsA<gtk::Widget>,
    ) -> T {
        let controllers = widget.observe_controllers();
        (0..controllers.n_items())
            .filter_map(|index| controllers.item(index))
            .find_map(|controller| controller.downcast::<T>().ok())
            .expect("controller")
    }

    struct Opening {
        returned: std::time::Duration,
        first_frame: Option<std::time::Duration>,
        settled: std::time::Duration,
        worst_stall: std::time::Duration,
    }

    /// Runs `open`, then pumps the main loop until `settled` holds plus a
    /// moment more, recording how long the call blocked, when the viewer
    /// first painted, when it settled, and the longest gap in which the
    /// main loop did not turn.
    fn measure_opening(
        window: &adw::Window,
        open: impl FnOnce(),
        settled: impl Fn() -> bool,
    ) -> Opening {
        use std::time::{Duration, Instant};
        let window_ref = window.upcast_ref::<gtk::Window>();
        let started = Instant::now();
        let last = Rc::new(Cell::new(started));
        let worst = Rc::new(Cell::new(Duration::ZERO));
        let first_frame = Rc::new(Cell::new(None));
        let source = glib::timeout_add_local(Duration::from_millis(2), {
            let (last, worst) = (last.clone(), worst.clone());
            move || {
                let now = Instant::now();
                let gap = now - last.replace(now);
                worst.set(worst.get().max(gap));
                glib::ControlFlow::Continue
            }
        });
        let clock = window.frame_clock().expect("frame clock");
        let painted = clock.connect_after_paint({
            let (first_frame, window) = (first_frame.clone(), window_ref.clone());
            move |_| {
                if first_frame.get().is_none() && has_button(&window, "Close") {
                    first_frame.set(Some(started.elapsed()));
                }
            }
        });
        open();
        let returned = started.elapsed();
        super::super::playback_tests::wait_until("viewer did not settle", &settled);
        let settled_at = started.elapsed();
        let tail = Instant::now() + Duration::from_millis(300);
        super::super::playback_tests::wait_until("tail", || Instant::now() >= tail);
        source.remove();
        clock.disconnect(painted);
        Opening {
            returned,
            first_frame: first_frame.get(),
            settled: settled_at,
            worst_stall: worst.get(),
        }
    }

    fn report(label: &str, opening: &Opening) {
        let ms = |duration: std::time::Duration| duration.as_secs_f64() * 1000.0;
        eprintln!(
            "OPEN {label}: call returns {:.1} ms, first viewer frame {} ms, ready {:.1} ms, worst main-loop stall {:.1} ms",
            ms(opening.returned),
            opening
                .first_frame
                .map_or("never".to_owned(), |d| format!("{:.1}", ms(d))),
            ms(opening.settled),
            ms(opening.worst_stall),
        );
    }

    fn generate(directory: &std::path::Path, name: &str, pipeline: &str) -> std::path::PathBuf {
        let path = directory.join(name);
        let status = std::process::Command::new("gst-launch-1.0")
            .arg("-q")
            .args(pipeline.split_whitespace())
            .arg(format!("location={}", path.display()))
            .status()
            .expect("GStreamer fixture generator");
        assert!(status.success(), "{pipeline}");
        path
    }

    fn media_message(
        content: impl FnOnce(crate::model::Media) -> Content,
        path: &std::path::Path,
        mime: &str,
        (width, height): (u32, u32),
    ) -> Message {
        let mut message = fixture_message(path.parent().unwrap(), "thumb.png", 64, 40);
        message.content = content(crate::model::Media {
            mime: mime.into(),
            size: std::fs::metadata(path).unwrap().len(),
            width: Some(width),
            height: Some(height),
            path: Some(path.to_path_buf()),
            state: crate::model::MediaState::Idle,
        });
        message
    }

    /// Needs a display, glycin loaders, bubblewrap and GStreamer's
    /// openh264 plugins; prints the timings it measures:
    /// `cargo test --lib opening_media -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs a display, glycin loaders and GStreamer H.264 plugins"]
    fn opening_media_keeps_the_main_loop_turning() {
        use super::super::playback_tests::{drain, wait_until};
        let runtime = init_display();
        let _runtime = runtime.enter();
        let directory = tempfile::tempdir().unwrap();
        let big_jpeg = generate(
            directory.path(),
            "big.jpg",
            "videotestsrc num-buffers=1 pattern=circular ! video/x-raw,width=6000,height=4000 ! jpegenc quality=90 ! filesink",
        );
        let big_png = generate(
            directory.path(),
            "big.png",
            "videotestsrc num-buffers=1 pattern=circular ! video/x-raw,width=6000,height=4000 ! pngenc compression-level=1 ! filesink",
        );
        let phone_jpeg = generate(
            directory.path(),
            "phone.jpg",
            "videotestsrc num-buffers=1 pattern=circular ! video/x-raw,width=1197,height=1229 ! jpegenc quality=90 ! filesink",
        );
        let clip = generate(
            directory.path(),
            "clip.mp4",
            "videotestsrc num-buffers=90 pattern=ball ! video/x-raw,width=1920,height=1080,framerate=30/1 ! videoconvert ! openh264enc ! h264parse ! mp4mux ! filesink",
        );
        let window = adw::Window::builder()
            .default_width(1100)
            .default_height(760)
            .content(&gtk::Box::new(gtk::Orientation::Vertical, 0))
            .build();
        window.present();
        drain();
        let window_ref = window.upcast_ref::<gtk::Window>();
        let on_action: Rc<dyn Fn(NativeMediaAction)> = Rc::new(|_| {});

        for (label, path, mime, size) in [
            (
                "phone JPEG 1197x1229",
                &phone_jpeg,
                "image/jpeg",
                (1197, 1229),
            ),
            ("JPEG 6000x4000", &big_jpeg, "image/jpeg", (6000, 4000)),
            ("PNG 6000x4000", &big_png, "image/png", (6000, 4000)),
        ] {
            let message = media_message(
                |media| Content::Image {
                    caption: None,
                    media,
                },
                path,
                mime,
                size,
            );
            let item = PhotoItem {
                path: path.clone(),
                details: ViewerDetails::of(&message),
            };
            for attempt in ["cold", "warm"] {
                let items = Rc::new(vec![item.clone()]);
                let opening = measure_opening(
                    &window,
                    || show_photo(&window, items, 0, on_action.clone()),
                    || button(window_ref, "Copy Image").is_sensitive(),
                );
                report(&format!("{label} {attempt}"), &opening);
                assert!(
                    opening.returned < std::time::Duration::from_millis(100),
                    "{label} {attempt} blocked the caller"
                );
                assert!(
                    opening
                        .first_frame
                        .is_some_and(|frame| frame < std::time::Duration::from_millis(250)),
                    "{label} {attempt} was late to appear"
                );
                button(window_ref, "Close").emit_clicked();
                wait_until("viewer did not close", || !has_button(window_ref, "Close"));
                drain();
            }
        }

        let message = media_message(
            |media| Content::Video {
                caption: None,
                media,
                seconds: Some(3),
                gif: false,
            },
            &clip,
            "video/mp4",
            (1920, 1080),
        );
        let details = ViewerDetails::of(&message);
        for attempt in ["cold", "warm"] {
            let opening = measure_opening(
                &window,
                || show_video(&window, &clip, &details, on_action.clone()),
                || has_button(window_ref, "Pause"),
            );
            report(&format!("video H.264 1920x1080 {attempt}"), &opening);
            assert!(
                opening.returned < std::time::Duration::from_millis(100),
                "video {attempt} blocked the caller"
            );
            button(window_ref, "Close").emit_clicked();
            wait_until("viewer did not close", || !has_button(window_ref, "Close"));
            drain();
        }
        window.destroy();
        drain();
        wait_until("quiet", || true);
    }

    /// Needs a display and glycin's loaders:
    /// `cargo test --lib photos_zoom_freely -- --ignored`. Setting
    /// `ZAPTIDE_VIEWER_SHOTS` also saves screenshots of each state.
    #[test]
    #[ignore = "needs a display and glycin loaders"]
    fn photos_zoom_freely_about_the_pointer() {
        use super::super::playback_tests::{drain, wait_until};
        let none = gdk::ModifierType::empty();
        let ctrl = gdk::ModifierType::CONTROL_MASK;
        let runtime = init_display();
        let _runtime = runtime.enter();
        let directory = tempfile::tempdir().unwrap();
        let first = fixture_message(directory.path(), "one.png", 3000, 2000);
        let second = fixture_message(directory.path(), "two.png", 800, 600);
        let window = adw::Window::builder()
            .default_width(1100)
            .default_height(760)
            .content(&gtk::Box::new(gtk::Orientation::Vertical, 0))
            .build();
        window.present();
        drain();
        let window_ref = window.upcast_ref::<gtk::Window>();
        let on_action: Rc<dyn Fn(NativeMediaAction)> = Rc::new(|_| {});
        let album = Rc::new(vec![photo_item(&first), photo_item(&second)]);

        show_photo(&window, album, 0, on_action);
        let photo = open_photo_state();
        wait_until("photo did not load", || photo.ready.get());
        drain();
        let scroller = photo.scroller.upgrade().unwrap();
        let view = photo.view_size();
        let fit = (view.0 / 3000.0).min(view.1 / 2000.0);
        assert!((photo.canvas.scale() - fit).abs() < 1e-6);
        assert_eq!(zoom_percent(window_ref), format!("{}%", zoom::percent(fit)));
        assert_eq!(photo.canvas.level(), Level::Fit);
        assert!(
            photo.texture_size().0 < 3000,
            "the first decode fits the view"
        );
        assert!(button(window_ref, "Zoom Out").is_sensitive());
        assert!(button(window_ref, "Zoom In").is_sensitive());
        screenshot(window_ref, "zoom-fit");

        let pointer = (view.0 * 0.55, view.1 * 0.45);
        let on_screen = |photo: &PhotoState, point: (f64, f64)| {
            let scale = photo.canvas.scale();
            let view = photo.view_size();
            let picture = photo.canvas.picture();
            let (across, down) = (
                photo.scroller.upgrade().unwrap().hadjustment().value(),
                photo.scroller.upgrade().unwrap().vadjustment().value(),
            );
            (
                point.0 * scale + zoom::origin(picture.0, view.0, scale) - across,
                point.1 * scale + zoom::origin(picture.1, view.1, scale) - down,
            )
        };
        let under_pointer = |photo: &PhotoState| {
            let scale = photo.canvas.scale();
            let view = photo.view_size();
            let picture = photo.canvas.picture();
            let scroller = photo.scroller.upgrade().unwrap();
            (
                (scroller.hadjustment().value() + pointer.0
                    - zoom::origin(picture.0, view.0, scale))
                    / scale,
                (scroller.vadjustment().value() + pointer.1
                    - zoom::origin(picture.1, view.1, scale))
                    / scale,
            )
        };
        let anchored = |photo: &PhotoState, point: (f64, f64)| {
            let shown = on_screen(photo, point);
            (shown.0 - pointer.0).abs() < 1.5 && (shown.1 - pointer.1).abs() < 1.5
        };

        assert_eq!(
            photo.on_scroll(none, -1.0, Some(pointer)),
            glib::Propagation::Proceed,
            "the wheel alone is left to the scrolled window"
        );
        assert_eq!(photo.canvas.level(), Level::Fit);
        assert_eq!(
            photo.on_scroll(ctrl, -1.0, Some(pointer)),
            glib::Propagation::Stop
        );
        drain();
        assert!((photo.canvas.scale() - fit * 1.1).abs() < 1e-6);
        assert_eq!(
            zoom_percent(window_ref),
            format!("{}%", zoom::percent(fit * 1.1))
        );
        for step in 0..6 {
            let point = under_pointer(&photo);
            photo.on_scroll(ctrl, -1.0, Some(pointer));
            drain();
            assert!(
                anchored(&photo, point),
                "the point under the pointer moved on step {step}"
            );
        }

        for _ in 0..80 {
            photo.on_scroll(ctrl, -1.0, Some(pointer));
        }
        drain();
        assert_eq!(photo.canvas.scale(), 8.0);
        assert_eq!(zoom_percent(window_ref), "800%");
        assert!(!button(window_ref, "Zoom In").is_sensitive());
        assert!(button(window_ref, "Zoom Out").is_sensitive());
        wait_until("the full photo did not load for the zoom", || {
            photo.texture_size().0 == 3000
        });
        screenshot(window_ref, "zoom-800");
        let before = (
            scroller.hadjustment().value(),
            scroller.vadjustment().value(),
        );
        let drag: gtk::GestureDrag = controller(&scroller);
        drag.emit_by_name::<()>("drag-begin", &[&10.0f64, &10.0f64]);
        drag.emit_by_name::<()>("drag-update", &[&-120.0f64, &-60.0f64]);
        drag.emit_by_name::<()>("drag-end", &[&-120.0f64, &-60.0f64]);
        assert!((scroller.hadjustment().value() - before.0 - 120.0).abs() < 1.0);
        assert!((scroller.vadjustment().value() - before.1 - 60.0).abs() < 1.0);

        for _ in 0..400 {
            photo.on_scroll(ctrl, 1.0, Some(pointer));
        }
        drain();
        let smallest = fit / 4.0;
        assert!((photo.canvas.scale() - smallest).abs() < 1e-6);
        assert!(!button(window_ref, "Zoom Out").is_sensitive());
        assert!(button(window_ref, "Zoom In").is_sensitive());
        screenshot(window_ref, "zoom-out-past-fit");

        press(window_ref, gdk::Key::_1, ctrl);
        assert_eq!(zoom_percent(window_ref), "100%");
        press(window_ref, gdk::Key::minus, ctrl);
        assert_eq!(zoom_percent(window_ref), "80%");
        press(
            window_ref,
            gdk::Key::plus,
            ctrl | gdk::ModifierType::SHIFT_MASK,
        );
        press(window_ref, gdk::Key::equal, ctrl);
        assert_eq!(zoom_percent(window_ref), "125%");
        button(window_ref, "Zoom Out").emit_clicked();
        assert_eq!(zoom_percent(window_ref), "100%");
        press(window_ref, gdk::Key::_0, ctrl);
        assert_eq!(photo.canvas.level(), Level::Fit);
        assert!(has_button(window_ref, "Actual Size"));
        button(window_ref, "Actual Size").emit_clicked();
        assert_eq!(zoom_percent(window_ref), "100%");
        button(window_ref, "Fit to Window").emit_clicked();
        assert_eq!(photo.canvas.level(), Level::Fit);

        let click: gtk::GestureClick = controller(&scroller);
        let point = under_pointer(&photo);
        click.emit_by_name::<()>("pressed", &[&2i32, &pointer.0, &pointer.1]);
        drain();
        assert_eq!(photo.canvas.level(), Level::Scale(1.0));
        assert!(
            anchored(&photo, point),
            "double click did not keep the anchor"
        );
        click.emit_by_name::<()>("pressed", &[&2i32, &pointer.0, &pointer.1]);
        assert_eq!(photo.canvas.level(), Level::Fit);

        press(window_ref, gdk::Key::plus, ctrl);
        assert_ne!(photo.canvas.level(), Level::Fit);
        button(window_ref, "Next Photo").emit_clicked();
        assert_eq!(
            photo.canvas.level(),
            Level::Fit,
            "zoom resets for the next photo"
        );
        assert!(!button(window_ref, "Zoom In").is_sensitive());
        wait_until("second photo did not load", || photo.ready.get());
        drain();
        assert!(button(window_ref, "Zoom In").is_sensitive());

        let narrow = adw::Window::builder()
            .default_width(360)
            .default_height(700)
            .content(&gtk::Box::new(gtk::Orientation::Vertical, 0))
            .build();
        narrow.present();
        drain();
        let narrow_ref = narrow.upcast_ref::<gtk::Window>();
        show_photo(
            &narrow,
            Rc::new(vec![photo_item(&first), photo_item(&second)]),
            0,
            Rc::new(|_| {}),
        );
        let narrow_photo = open_photo_state();
        wait_until("narrow photo did not load", || narrow_photo.ready.get());
        drain();
        screenshot(narrow_ref, "zoom-narrow");
        let header = tree(narrow_ref)
            .into_iter()
            .find_map(|widget| widget.downcast::<adw::HeaderBar>().ok())
            .expect("header");
        assert!(
            header.measure(gtk::Orientation::Horizontal, -1).0 <= narrow_ref.width(),
            "the header fits a narrow window"
        );
        let level = button(narrow_ref, "Actual Size");
        let close = button(narrow_ref, "Close");
        for widget in [level.upcast_ref::<gtk::Widget>(), close.upcast_ref()] {
            let bounds = widget
                .compute_bounds(narrow_ref)
                .expect("bounds in the window");
            assert!(
                bounds.x() >= 0.0 && bounds.x() + bounds.width() <= narrow_ref.width() as f32,
                "{} is inside the narrow window",
                widget.type_().name()
            );
        }
        button(narrow_ref, "Close").emit_clicked();
        narrow.destroy();
        OPEN_PHOTO.with(|open| open.take());
        window.destroy();
        drain();
    }

    /// Needs a display and GStreamer's VP8 plugins:
    /// `cargo test --lib videos_open_in -- --ignored`.
    #[test]
    #[ignore = "needs a display and GStreamer VP8 plugins"]
    fn videos_open_in_the_background_with_a_poster() {
        use super::super::playback_tests::{drain, wait_until};
        let runtime = init_display();
        let _runtime = runtime.enter();
        let directory = tempfile::tempdir().unwrap();
        let clip = generate(
            directory.path(),
            "clip.webm",
            "videotestsrc num-buffers=90 ! video/x-raw,width=640,height=360,framerate=30/1 ! vp8enc deadline=1 ! webmmux ! filesink",
        );
        let message = media_message(
            |media| Content::Video {
                caption: None,
                media,
                seconds: Some(3),
                gif: false,
            },
            &clip,
            "video/webm",
            (640, 360),
        );
        let details = ViewerDetails::of(&message);
        let window = adw::Window::builder()
            .default_width(1100)
            .default_height(760)
            .content(&gtk::Box::new(gtk::Orientation::Vertical, 0))
            .build();
        window.present();
        drain();
        let window_ref = window.upcast_ref::<gtk::Window>();
        let opened = Rc::new(RefCell::new(Vec::new()));
        let on_action: Rc<dyn Fn(NativeMediaAction)> = {
            let opened = opened.clone();
            Rc::new(move |action| {
                if let NativeMediaAction::Open(path) = action {
                    opened.borrow_mut().push(path);
                }
            })
        };
        let spinner_shown = |window: &gtk::Window| {
            tree(window)
                .into_iter()
                .any(|widget| widget.is::<adw::Spinner>() && widget.is_mapped())
        };

        show_video(&window, &clip, &details, on_action.clone());
        assert!(has_button(window_ref, "Close"), "the viewer is up at once");
        assert!(
            !has_button(window_ref, "Pause"),
            "playing waits for the pipeline"
        );
        drain();
        wait_until("video controls did not appear", || {
            has_button(window_ref, "Pause")
        });
        wait_until("the poster stayed over the video", || {
            !spinner_shown(window_ref)
        });
        assert!(opened.borrow().is_empty());
        button(window_ref, "Close").emit_clicked();
        wait_until("viewer did not close", || !has_button(window_ref, "Close"));

        let broken = directory.path().join("broken.webm");
        std::fs::write(&broken, b"synthetic invalid video").unwrap();
        show_video(&window, &broken, &details, on_action.clone());
        wait_until("failure page did not show", || {
            failure_button(window_ref).is_some()
        });
        assert!(!spinner_shown(window_ref));
        button(window_ref, "Close").emit_clicked();
        wait_until("viewer did not close", || !has_button(window_ref, "Close"));

        show_video(&window, &clip, &details, on_action);
        button(window_ref, "Close").emit_clicked();
        wait_until("viewer did not close", || !has_button(window_ref, "Close"));
        drain();
        window.destroy();
        drain();
    }

    #[test]
    fn stepping_stays_within_the_items() {
        assert_eq!(step_target(0, 1, 3), Some(1));
        assert_eq!(step_target(2, 1, 3), None);
        assert_eq!(step_target(0, -1, 3), None);
        assert_eq!(step_target(1, -1, 3), Some(0));
        assert_eq!(step_target(1, 0, 3), None);
        assert_eq!(step_target(0, 1, 0), None);
    }

    #[test]
    fn arrow_keys_step_unless_a_modifier_is_held() {
        let none = gdk::ModifierType::empty();
        assert_eq!(key_step(gdk::Key::Left, none, false), Some(-1));
        assert_eq!(key_step(gdk::Key::Right, none, false), Some(1));
        assert_eq!(key_step(gdk::Key::Left, none, true), Some(1));
        assert_eq!(key_step(gdk::Key::Right, none, true), Some(-1));
        assert_eq!(key_step(gdk::Key::Up, none, false), None);
        for held in [
            gdk::ModifierType::ALT_MASK,
            gdk::ModifierType::CONTROL_MASK,
            gdk::ModifierType::SHIFT_MASK,
        ] {
            assert_eq!(key_step(gdk::Key::Right, held, false), None);
        }
        let locks = gdk::ModifierType::LOCK_MASK;
        assert_eq!(key_step(gdk::Key::Right, locks, false), Some(1));
    }

    #[test]
    fn seeks_stay_within_the_video() {
        assert_eq!(seek_target(-4.0, 30_000_000), 0);
        assert_eq!(seek_target(12_000_000.0, 30_000_000), 12_000_000);
        assert_eq!(seek_target(99_000_000.0, 30_000_000), 30_000_000);
        assert_eq!(seek_target(5.0, 0), 0);
    }

    #[test]
    fn accessible_names_say_whose_media_it_is() {
        assert_eq!(photo_label("Marina"), "Photo from Marina");
        assert_eq!(video_label("Marina"), "Video from Marina");
        assert_eq!(of_text("0:12", "0:30"), "0:12 of 0:30");
    }

    fn dialog_keys(window: &gtk::Window) -> gtk::EventControllerKey {
        let dialog = tree(window)
            .into_iter()
            .find_map(|widget| widget.downcast::<adw::Dialog>().ok())
            .expect("viewer dialog");
        let controllers = dialog.observe_controllers();
        (0..controllers.n_items())
            .filter_map(|index| controllers.item(index))
            .find_map(|controller| controller.downcast::<gtk::EventControllerKey>().ok())
            .expect("viewer key controller")
    }

    fn press(window: &gtk::Window, key: gdk::Key, modifiers: gdk::ModifierType) {
        use gtk::glib::translate::IntoGlib;
        dialog_keys(window)
            .emit_by_name::<bool>("key-pressed", &[&key.into_glib(), &0u32, &modifiers]);
    }

    fn playing_clip(window: &gtk::Window) -> Option<VideoStream> {
        tree(window).into_iter().find_map(|widget| {
            widget
                .downcast::<gtk::Picture>()
                .ok()?
                .paintable()
                .and_downcast::<VideoStream>()
        })
    }

    fn failure_button(window: &gtk::Window) -> Option<gtk::Button> {
        tree(window)
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
            .find(|button| button.label().as_deref() == Some("Open in Another App"))
    }

    /// Needs a display, and runs alone because GTK initialises on one thread:
    /// `cargo test --lib viewer_steps -- --ignored`. Setting
    /// `ZAPTIDE_VIEWER_SHOTS=/tmp/opencode/shots` also saves screenshots of
    /// each state.
    #[test]
    #[ignore = "needs a display and GStreamer VP8 plugins"]
    fn viewer_steps_zooms_and_controls_video() {
        use super::super::playback_tests::{drain, wait_until};
        gtk::init().expect("display");
        adw::init().expect("libadwaita");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let _runtime = runtime.enter();
        let provider = gtk::CssProvider::new();
        provider.load_from_string(include_str!("../../data/style.css"));
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().unwrap(),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        let directory = tempfile::tempdir().unwrap();
        let first = fixture_message(directory.path(), "one.png", 1600, 1000);
        let second = fixture_message(directory.path(), "two.png", 800, 600);
        let window = adw::Window::builder()
            .default_width(1100)
            .default_height(760)
            .content(&gtk::Box::new(gtk::Orientation::Vertical, 0))
            .build();
        window.present();
        drain();
        let opened = Rc::new(std::cell::RefCell::new(Vec::new()));
        let on_action: Rc<dyn Fn(NativeMediaAction)> = {
            let opened = opened.clone();
            Rc::new(move |action| {
                if let NativeMediaAction::Open(path) = action {
                    opened.borrow_mut().push(path);
                }
            })
        };
        let item = |message: &Message| PhotoItem {
            path: match attachment_action(message) {
                Some(NativeMediaAction::Open(path)) => path,
                _ => panic!("fixture is on disk"),
            },
            details: ViewerDetails::of(message),
        };
        let album = Rc::new(vec![item(&first), item(&second)]);
        let window_ref = window.upcast_ref::<gtk::Window>();

        show_photo(&window, album.clone(), 0, on_action.clone());
        assert!(
            album
                .iter()
                .all(|item| crate::media_retention::is_held(&item.path)),
            "photos on screen are kept from the cleanup"
        );
        assert!(
            !button(window_ref, "Copy Image").is_sensitive(),
            "copy waits for the full photo"
        );
        assert!(
            !button(window_ref, "Zoom In").is_sensitive(),
            "zoom waits for the full photo"
        );
        wait_until("photo did not load", || {
            button(window_ref, "Copy Image").is_sensitive()
        });
        drain();
        assert_eq!(label(window_ref, "zaptide-viewer-pill").label(), "1 of 2");
        assert!(label(window_ref, "zaptide-viewer-time").is_visible());
        assert_eq!(
            label(window_ref, "zaptide-viewer-name").label(),
            "Marina Costa"
        );
        assert_eq!(
            label(window_ref, "zaptide-viewer-metadata").label(),
            "PNG · 1600 × 1000 · 184 KB"
        );
        assert!(button(window_ref, "Zoom Out").is_sensitive());
        assert!(button(window_ref, "Zoom In").is_sensitive());
        assert!(has_button(window_ref, "Actual Size"));
        assert!(!button(window_ref, "Previous Photo").is_sensitive());
        screenshot(window_ref, "13-view-image");
        press(window_ref, gdk::Key::_1, gdk::ModifierType::empty());
        assert_eq!(zoom_percent(window_ref), "100%");
        assert!(has_button(window_ref, "Fit to Window"));
        drain();
        screenshot(window_ref, "14-image-original-size");
        press(window_ref, gdk::Key::Right, gdk::ModifierType::empty());
        assert_eq!(
            label(window_ref, "zaptide-viewer-pill").label(),
            "1 of 2",
            "arrows scroll a photo at 100%"
        );
        press(window_ref, gdk::Key::_0, gdk::ModifierType::CONTROL_MASK);
        assert!(has_button(window_ref, "Actual Size"));
        press(window_ref, gdk::Key::Right, gdk::ModifierType::ALT_MASK);
        assert_eq!(label(window_ref, "zaptide-viewer-pill").label(), "1 of 2");
        press(window_ref, gdk::Key::Right, gdk::ModifierType::empty());
        assert_eq!(label(window_ref, "zaptide-viewer-pill").label(), "2 of 2");
        assert!(
            has_button(window_ref, "Actual Size"),
            "zoom resets for the next photo"
        );
        assert!(!button(window_ref, "Next Photo").is_sensitive());
        assert!(
            !button(window_ref, "Copy Image").is_sensitive(),
            "copy waits for the next photo"
        );
        wait_until("second photo did not load", || {
            button(window_ref, "Copy Image").is_sensitive()
        });
        press(window_ref, gdk::Key::Left, gdk::ModifierType::empty());
        assert_eq!(label(window_ref, "zaptide-viewer-pill").label(), "1 of 2");
        button(window_ref, "Next Photo").emit_clicked();
        button(window_ref, "Open With Another App").emit_clicked();
        assert_eq!(opened.borrow().len(), 1);
        drain();
        assert!(!has_button(window_ref, "Next Photo"), "viewer closed");
        assert!(
            album
                .iter()
                .all(|item| !crate::media_retention::is_held(&item.path)),
            "a closed viewer releases its photos"
        );

        let video = directory.path().join("clip.webm");
        let status = std::process::Command::new("gst-launch-1.0")
            .args("-q videotestsrc num-buffers=90 ! video/x-raw,width=640,height=360,framerate=30/1 ! vp8enc deadline=1 ! queue ! mux. audiotestsrc wave=silence num-buffers=140 ! audioconvert ! vorbisenc ! queue ! mux. webmmux name=mux ! filesink".split_whitespace())
            .arg(format!("location={}", video.display()))
            .status()
            .expect("GStreamer fixture generator");
        assert!(status.success());
        let mut message = first.clone();
        message.content = Content::Video {
            caption: None,
            media: crate::model::Media {
                mime: "video/mp4".into(),
                size: 1_258_291,
                width: Some(1280),
                height: Some(720),
                path: Some(video.clone()),
                state: crate::model::MediaState::Idle,
            },
            seconds: Some(3),
            gif: false,
        };
        let details = ViewerDetails::of(&message);
        show_video(&window, &video, &details, on_action.clone());
        wait_until("video controls did not appear", || {
            has_button(window_ref, "Pause")
        });
        assert!(!has_button(window_ref, "Copy Image"));
        assert!(!has_button(window_ref, "Zoom In"));
        assert_eq!(
            label(window_ref, "zaptide-viewer-metadata").label(),
            "MP4 · 1280 × 720 · 0:03 · 1.2 MB"
        );
        let clip = playing_clip(window_ref).expect("video stream");
        let released = clip.downgrade();
        let seek = tree(window_ref)
            .into_iter()
            .find_map(|widget| widget.downcast::<gtk::Scale>().ok())
            .filter(|scale| scale.has_css_class("zaptide-video-seek"))
            .expect("seek bar");
        assert!(seek.adjustment().step_increment() > 0.0);
        assert!(seek.adjustment().page_increment() > seek.adjustment().step_increment());
        screenshot(window_ref, "15-play-video");
        button(window_ref, "Pause").emit_clicked();
        wait_until("video did not pause", || has_button(window_ref, "Play"));
        drain();
        screenshot(window_ref, "16-video-paused");
        button(window_ref, "Fullscreen").emit_clicked();
        wait_until("window did not enter fullscreen", || window.is_fullscreen());
        assert!(has_button(window_ref, "Exit Fullscreen"));
        button(window_ref, "Close").emit_clicked();
        wait_until("closing the viewer left the window fullscreen", || {
            !window.is_fullscreen()
        });
        drain();
        assert!(!has_button(window_ref, "Close"), "viewer closed");
        assert!(!clip.is_prepared(), "closing releases the playback");
        drop(clip);
        wait_until("closed viewer kept its video stream", || {
            released.upgrade().is_none()
        });

        let broken = directory.path().join("broken.webm");
        std::fs::write(&broken, b"synthetic invalid video").unwrap();
        show_video(&window, &broken, &details, on_action.clone());
        wait_until("failure page did not show", || {
            failure_button(window_ref).is_some()
        });
        drain();
        screenshot(window_ref, "48-video-unavailable");
        let failed = playing_clip(window_ref).expect("failed stream").downgrade();
        let before = opened.borrow().len();
        failure_button(window_ref).unwrap().emit_clicked();
        assert_eq!(opened.borrow().len(), before + 1);
        wait_until("failed viewer kept its video stream", || {
            failed.upgrade().is_none()
        });

        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceLight);
        show_video(&window, &broken, &details, on_action.clone());
        wait_until("light failure page did not show", || {
            failure_button(window_ref).is_some()
        });
        drain();
        screenshot(window_ref, "48-video-unavailable-light");
        button(window_ref, "Close").emit_clicked();
        drain();
        show_video(&window, &video, &details, on_action.clone());
        wait_until("light video controls did not appear", || {
            has_button(window_ref, "Pause")
        });
        let volume = tree(window_ref)
            .into_iter()
            .find_map(|widget| widget.downcast::<gtk::MenuButton>().ok())
            .expect("volume button");
        volume.popup();
        drain();
        screenshot(
            &volume.popover().expect("volume popover"),
            "15-volume-popover-light",
        );
        button(window_ref, "Close").emit_clicked();
        drain();
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);

        let narrow = adw::Window::builder()
            .default_width(400)
            .default_height(700)
            .content(&gtk::Box::new(gtk::Orientation::Vertical, 0))
            .build();
        narrow.present();
        drain();
        let narrow_ref = narrow.upcast_ref::<gtk::Window>();
        show_photo(&narrow, album, 0, on_action.clone());
        wait_until("narrow photo did not load", || {
            button(narrow_ref, "Copy Image").is_sensitive()
        });
        drain();
        assert!(!label(narrow_ref, "zaptide-viewer-time").is_visible());
        screenshot(narrow_ref, "narrow-album");
        button(narrow_ref, "Close").emit_clicked();
        drain();
        show_video(&narrow, &video, &details, on_action.clone());
        wait_until("narrow video controls did not appear", || {
            has_button(narrow_ref, "Pause")
        });
        for _ in 0..4 {
            drain();
        }
        screenshot(narrow_ref, "narrow-video");
        button(narrow_ref, "Close").emit_clicked();
        drain();
        narrow.destroy();
        window.destroy();
        drain();
    }
}
