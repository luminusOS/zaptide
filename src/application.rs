//! Relm4 root shell: link page, chat list, and conversation.

use adw::prelude::*;
use relm4::{
    RelmApp,
    prelude::*,
    typed_view::list::{RelmListItem, TypedListView},
};
use relm4::{adw, gtk};

use crate::{
    backend::{Backend, Event, LinkStatus},
    event_drain::drain_backend_events,
    glib_notifier::GlibEventDrain,
    message_window::{ACTIVE_MESSAGE_LIMIT, trim_message_window},
    notifier::EventNotifier,
    paths::AppDirs,
};

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ChatRow {
    id: String,
    last_activity: i64,
    name: String,
    preview: String,
    unread: Option<String>,
    avatar: Option<std::path::PathBuf>,
    pinned: bool,
    muted: bool,
    /// Muted and archived chats count unread messages in grey.
    quiet: bool,
    /// Delivery of our own last message; `None` for incoming.
    delivery: crate::model::Delivery,
}

thread_local! {
    /// Decoded avatars by file. Rows rebind constantly while scrolling.
    static AVATAR_TEXTURES: std::cell::RefCell<
        std::collections::HashMap<std::path::PathBuf, gtk::gdk::Texture>,
    > = std::cell::RefCell::default();
}

struct ChatRowWidgets {
    name: gtk::Label,
    preview: gtk::Label,
    status: gtk::DrawingArea,
    status_icon: gtk::Image,
    unread: gtk::Label,
    pinned: gtk::Image,
    muted: gtk::Image,
    avatar: adw::Avatar,
}

enum ChatChange {
    Snapshot(Vec<crate::model::Chat>),
    Update(crate::model::Chat),
}

enum NativeEvent {
    Link(LinkStatus),
    Chats(Vec<crate::model::Chat>),
    ChatUpdated(Box<crate::model::Chat>),
    Contacts(Vec<crate::model::Contact>),
    Messages {
        chat: String,
        messages: Vec<crate::model::Message>,
        older: bool,
        complete: bool,
    },
    MessageUpdated(Box<crate::model::Message>),
    Edited {
        chat: String,
        id: String,
        success: bool,
    },
    Sent {
        chat: String,
        success: bool,
    },
    AttachmentCompleted {
        chat: String,
        path: std::path::PathBuf,
        success: bool,
    },
    Media {
        chat: String,
        message: String,
        result: Result<std::path::PathBuf, String>,
    },
    ReceiptsPrivacy {
        disabled: bool,
    },
    ContactReady {
        id: String,
        name: Option<String>,
    },
    Info(String),
    Error(String),
    Typing {
        chat: String,
        sender: String,
        composing: bool,
    },
    Presence {
        id: String,
        online: bool,
        last_seen: Option<i64>,
    },
    Avatar {
        id: String,
        path: Option<std::path::PathBuf>,
    },
    MessageDeleted {
        chat: String,
        id: String,
    },
    Incoming {
        chat: String,
        message: Box<crate::model::Message>,
    },
    OlderFetched {
        chat: String,
        more: bool,
    },
    Stickers {
        saved: Vec<std::path::PathBuf>,
        packs: Vec<crate::model::StickerPack>,
        recent: Vec<std::path::PathBuf>,
    },
}

struct PendingSend {
    chat: String,
    text: String,
    reply: Option<String>,
    attachments: Vec<std::path::PathBuf>,
    failed_attachments: Vec<std::path::PathBuf>,
    clipboard_image: bool,
    remaining: usize,
    failed: bool,
}

#[derive(Clone)]
pub struct ClipboardPixels {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    preview: gtk::gdk::Texture,
}

impl std::fmt::Debug for ClipboardPixels {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClipboardPixels")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pixels", &"[REDACTED]")
            .field("preview", &"[REDACTED]")
            .finish()
    }
}

impl PendingSend {
    fn complete(&mut self, success: bool) -> bool {
        self.remaining = self.remaining.saturating_sub(1);
        self.failed |= !success;
        self.remaining == 0
    }

    fn complete_attachment(&mut self, path: std::path::PathBuf, success: bool) -> bool {
        if !success {
            self.failed_attachments.push(path);
        }
        self.complete(success)
    }
}

impl RelmListItem for ChatRow {
    type Root = gtk::Box;
    type Widgets = ChatRowWidgets;

    fn setup(_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(6)
            .margin_end(6)
            .build();
        let avatar = adw::Avatar::new(40, None, true);
        let name = gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        name.add_css_class("heading");
        let preview = gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .hexpand(true)
            .build();
        preview.add_css_class("dim-label");
        preview.add_css_class("caption");
        let unread = gtk::Label::builder()
            .visible(false)
            .valign(gtk::Align::Center)
            .build();
        unread.add_css_class("zaptide-unread-pill");
        let status_icon = |icon: &str, label: &str| {
            let image = gtk::Image::builder()
                .icon_name(icon)
                .tooltip_text(label)
                .visible(false)
                .build();
            image.add_css_class("dim-label");
            image.update_property(&[gtk::accessible::Property::Label(label)]);
            image
        };
        let muted = status_icon("notifications-disabled-symbolic", "Muted");
        let pinned = status_icon("view-pin-symbolic", "Pinned");
        let details = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(3)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        let title = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .build();
        title.append(&name);
        title.append(&muted);
        title.append(&pinned);
        title.append(&unread);
        details.append(&title);
        let status = delivery_ticks();
        status.set_visible(false);
        let status_icon = gtk::Image::builder().pixel_size(12).visible(false).build();
        let preview_row = gtk::Box::builder().spacing(4).build();
        preview_row.append(&status);
        preview_row.append(&status_icon);
        preview_row.append(&preview);
        details.append(&preview_row);
        root.append(&avatar);
        root.append(&details);
        (
            root,
            ChatRowWidgets {
                name,
                preview,
                status,
                status_icon,
                unread,
                pinned,
                muted,
                avatar,
            },
        )
    }

    fn bind(&mut self, widgets: &mut Self::Widgets, _root: &mut Self::Root) {
        widgets.name.set_label(&self.name);
        widgets.preview.set_label(&self.preview);
        let (glyph, icon, read) = delivery_mark(self.delivery);
        set_delivery_ticks(&widgets.status, glyph);
        if read {
            widgets.status.add_css_class("read");
        } else {
            widgets.status.remove_css_class("read");
        }
        widgets.status_icon.set_icon_name(icon);
        widgets.status_icon.set_visible(icon.is_some());
        if self.delivery == crate::model::Delivery::Failed {
            widgets.status_icon.add_css_class("zaptide-delivery-failed");
        } else {
            widgets
                .status_icon
                .remove_css_class("zaptide-delivery-failed");
        }
        widgets.pinned.set_visible(self.pinned);
        widgets.muted.set_visible(self.muted);
        widgets.unread.set_visible(self.unread.is_some());
        if self.quiet {
            widgets.unread.add_css_class("muted");
        } else {
            widgets.unread.remove_css_class("muted");
        }
        if let Some(unread) = &self.unread {
            widgets.unread.set_label(unread);
        }
        widgets.avatar.set_text(Some(&self.name));
        let image = self.avatar.as_ref().and_then(|path| {
            AVATAR_TEXTURES.with_borrow_mut(|cache| {
                if !cache.contains_key(path) {
                    cache.insert(path.clone(), gtk::gdk::Texture::from_filename(path).ok()?);
                }
                cache.get(path).cloned()
            })
        });
        widgets.avatar.set_custom_image(image.as_ref());
    }
}

struct MessageRow {
    id: String,
    /// Day and unread separators shown above the row.
    separator: String,
    sender: String,
    sender_class: &'static str,
    avatar: Option<std::path::PathBuf>,
    quote: String,
    /// Selectable text: the message body, or a caption.
    body: String,
    footer: String,
    accessible_label: String,
    show_sender: bool,
    show_timestamp: bool,
    pointer_sender: ComponentSender<NativeApplication>,
    message: crate::model::Message,
    audio: Option<crate::native_voice::VoiceMessage>,
    audio_registry: AudioRegistry,
}

impl MessageRow {
    /// Whether rebinding `other` would draw exactly this row.
    fn renders_like(&self, other: &Self) -> bool {
        self.message == other.message
            && self.audio == other.audio
            && self.separator == other.separator
            && self.avatar == other.avatar
            && self.show_sender == other.show_sender
            && self.show_timestamp == other.show_timestamp
    }
}

impl PartialEq for MessageRow {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for MessageRow {}

impl PartialOrd for MessageRow {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MessageRow {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.id.cmp(&other.id)
    }
}

struct MessageRowWidgets {
    separator: gtk::Label,
    avatar: adw::Avatar,
    leading_space: gtk::Box,
    trailing_space: gtk::Box,
    bubble: gtk::Box,
    header: gtk::Box,
    name: gtk::Label,
    quote: gtk::Label,
    body: gtk::Label,
    footer: gtk::Label,
    status: gtk::DrawingArea,
    status_icon: gtk::Image,
    media: gtk::Box,
    audio: gtk::Box,
    audio_controls: Option<crate::native_media_widgets::AudioControls>,
    rendered_message: Option<crate::model::Message>,
    action_generation: std::rc::Rc<std::cell::Cell<u64>>,
    decode_token: Option<crate::native_media::DecodeToken>,
    menu_target: MenuTarget,
}

/// The bound message and its sender, read by the row's context-menu gesture.
type MenuTarget =
    std::rc::Rc<std::cell::RefCell<Option<(String, ComponentSender<NativeApplication>)>>>;
type AudioRegistry = std::rc::Rc<
    std::cell::RefCell<
        std::collections::HashMap<String, crate::native_media_widgets::AudioControls>,
    >,
>;

impl RelmListItem for MessageRow {
    type Root = gtk::Box;
    type Widgets = MessageRowWidgets;

    fn setup(_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(12)
            .margin_end(12)
            .focusable(true)
            .build();
        root.add_css_class("zaptide-message-item");
        let separator = gtk::Label::builder()
            .halign(gtk::Align::Center)
            .justify(gtk::Justification::Center)
            .margin_top(12)
            .margin_bottom(6)
            .css_classes(["dim-label", "caption-heading"])
            .build();
        root.append(&separator);
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        row.add_css_class("zaptide-message-row");
        let leading_space = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        leading_space.set_hexpand(true);
        row.append(&leading_space);
        let avatar = adw::Avatar::new(36, None, true);
        avatar.set_valign(gtk::Align::Start);
        row.append(&avatar);
        let bubble = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .build();
        bubble.add_css_class("zaptide-bubble");
        let header = gtk::Box::builder().spacing(6).build();
        let name = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build();
        header.append(&name);
        bubble.append(&header);
        let quote = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .max_width_chars(52)
            .css_classes(["zaptide-quote"])
            .build();
        bubble.append(&quote);
        // A wrapped TextView inside a ListView measures its height at the wrong
        // width and leaves tall blank rows; a Label measures height-for-width.
        let body = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .max_width_chars(52)
            .selectable(true)
            .build();
        bubble.append(&body);
        let media = gtk::Box::new(gtk::Orientation::Vertical, 0);
        bubble.append(&media);
        let audio = gtk::Box::new(gtk::Orientation::Vertical, 0);
        bubble.append(&audio);
        let footer = gtk::Label::builder()
            .xalign(1.0)
            .wrap(true)
            .max_width_chars(52)
            .css_classes(["dim-label", "caption"])
            .build();
        let status = delivery_ticks();
        let status_icon = gtk::Image::builder().pixel_size(12).build();
        let footer_row = gtk::Box::builder()
            .spacing(4)
            .halign(gtk::Align::End)
            .build();
        footer_row.append(&footer);
        footer_row.append(&status);
        footer_row.append(&status_icon);
        bubble.append(&footer_row);
        let clamp = adw::Clamp::builder()
            .maximum_size(480)
            .tightening_threshold(360)
            .child(&bubble)
            .build();
        row.append(&clamp);
        let trailing_space = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        trailing_space.set_hexpand(true);
        row.append(&trailing_space);
        root.append(&row);
        let menu_target: MenuTarget = std::rc::Rc::default();
        let context_click = gtk::GestureClick::new();
        context_click.set_button(gtk::gdk::BUTTON_SECONDARY);
        context_click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let gesture_target = menu_target.clone();
        let focus_row = root.downgrade();
        context_click.connect_pressed(move |gesture, _, x, y| {
            // Claim the click so a selectable label cannot also open its own
            // context menu over ours; two grabbing popovers freeze the app.
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let (Some((id, sender)), Some(row)) =
                (gesture_target.borrow().clone(), gesture.widget())
            {
                if let Some(root) = focus_row.upgrade() {
                    root.grab_focus();
                }
                if let Some(point) = message_menu_position(&row, x, y) {
                    sender.input(Input::ShowMessageMenu {
                        id,
                        x: point.x(),
                        y: point.y(),
                    });
                }
            }
        });
        bubble.add_controller(context_click);
        let menu_keys = gtk::EventControllerKey::new();
        menu_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let key_target = menu_target.clone();
        menu_keys.connect_key_pressed(move |controller, key, _, modifiers| {
            if key != gtk::gdk::Key::Menu
                && !(key == gtk::gdk::Key::F10
                    && modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK))
            {
                return gtk::glib::Propagation::Proceed;
            }
            if let (Some((id, sender)), Some(row)) =
                (key_target.borrow().clone(), controller.widget())
                && let Some(point) = message_menu_position(&row, 24.0, row.height() as f64 / 2.0)
            {
                sender.input(Input::ShowMessageMenu {
                    id,
                    x: point.x(),
                    y: point.y(),
                });
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        root.add_controller(menu_keys);
        let action_generation = std::rc::Rc::new(std::cell::Cell::new(0));
        (
            root,
            MessageRowWidgets {
                separator,
                avatar,
                leading_space,
                trailing_space,
                bubble,
                header,
                name,
                quote,
                body,
                footer,
                status,
                status_icon,
                media,
                audio,
                audio_controls: None,
                rendered_message: None,
                action_generation,
                decode_token: None,
                menu_target,
            },
        )
    }

    fn bind(&mut self, widgets: &mut Self::Widgets, root: &mut Self::Root) {
        root.update_property(&[
            gtk::accessible::Property::Label(&self.accessible_label),
            gtk::accessible::Property::Description("Press Menu or Shift+F10 for actions"),
        ]);
        let outgoing = self.message.from_me;
        widgets.leading_space.set_visible(outgoing);
        widgets.trailing_space.set_visible(!outgoing);
        widgets.avatar.set_visible(!outgoing);
        widgets
            .bubble
            .remove_css_class(if outgoing { "incoming" } else { "outgoing" });
        widgets
            .bubble
            .add_css_class(if outgoing { "outgoing" } else { "incoming" });
        root.set_margin_top(if self.show_sender { 6 } else { 0 });
        root.set_margin_bottom(if self.show_timestamp { 6 } else { 0 });
        widgets.separator.set_label(&self.separator);
        widgets.separator.set_visible(!self.separator.is_empty());
        // Continuation rows keep the avatar's space so incoming bubbles align.
        widgets
            .avatar
            .set_opacity(if self.show_sender { 1.0 } else { 0.0 });
        widgets.avatar.set_text(Some(&self.sender));
        let image = self.avatar.as_ref().and_then(|path| {
            AVATAR_TEXTURES.with_borrow_mut(|cache| {
                if !cache.contains_key(path) {
                    cache.insert(path.clone(), gtk::gdk::Texture::from_filename(path).ok()?);
                }
                cache.get(path).cloned()
            })
        });
        widgets.avatar.set_custom_image(image.as_ref());
        widgets.header.set_visible(self.show_sender && !outgoing);
        widgets.name.set_label(&self.sender);
        widgets
            .name
            .set_css_classes(&["heading", self.sender_class]);
        widgets.quote.set_label(&self.quote);
        widgets.quote.set_visible(!self.quote.is_empty());
        widgets.body.set_label(&self.body);
        widgets.body.set_visible(!self.body.is_empty());
        widgets
            .body
            .update_property(&[gtk::accessible::Property::Label(&self.accessible_label)]);
        *widgets.menu_target.borrow_mut() = Some((self.id.clone(), self.pointer_sender.clone()));
        widgets.footer.set_label(&self.footer);
        widgets.footer.set_visible(!self.footer.is_empty());
        let (glyph, icon, read) = delivery_mark(self.message.status);
        set_delivery_ticks(&widgets.status, glyph);
        if read {
            widgets.status.add_css_class("read");
        } else {
            widgets.status.remove_css_class("read");
        }
        widgets.status_icon.set_icon_name(icon);
        if self.message.status == crate::model::Delivery::Failed {
            widgets.status_icon.add_css_class("zaptide-delivery-failed");
        } else {
            widgets
                .status_icon
                .remove_css_class("zaptide-delivery-failed");
        }
        widgets.status_icon.set_visible(icon.is_some());
        let words = delivery_label(self.message.status).trim_start_matches(" · ");
        for widget in [
            widgets.status.upcast_ref::<gtk::Widget>(),
            widgets.status_icon.upcast_ref(),
        ] {
            widget.set_tooltip_text((!words.is_empty()).then_some(words));
        }
        // Delivery and reaction updates must not rebuild media: a rebuilt
        // sticker or photo blanks while it decodes again and the list jumps.
        if !widgets
            .rendered_message
            .as_ref()
            .is_some_and(|previous| same_media(previous, &self.message))
        {
            if let Some(previous) = widgets.rendered_message.as_ref()
                && previous.id != self.id
            {
                self.audio_registry.borrow_mut().remove(&previous.id);
            }
            if let Some(token) = widgets.decode_token.take() {
                token.cancel();
            }
            while let Some(child) = widgets.media.first_child() {
                widgets.media.remove(&child);
            }
            while let Some(child) = widgets.audio.first_child() {
                widgets.audio.remove(&child);
            }
            widgets.audio_controls = None;
            let generation = widgets.action_generation.get().wrapping_add(1);
            widgets.action_generation.set(generation);
            let active_generation = widgets.action_generation.clone();
            let media = widgets.media.downgrade();
            let sender = self.pointer_sender.clone();
            let rendered = crate::native_media_widgets::build_media_widget_with_action(
                &self.message,
                move |action| {
                    if active_generation.get() == generation && media.upgrade().is_some() {
                        sender.input(Input::MediaAction(action));
                    }
                },
            );
            widgets
                .media
                .set_visible(rendered.widget.first_child().is_some());
            widgets.media.append(&rendered.widget);
            widgets.decode_token = Some(rendered.decode_token);
            widgets.rendered_message = Some(self.message.clone());
        }
        if let Some(voice) = &self.audio {
            if widgets.audio_controls.is_none() {
                let sender = self.pointer_sender.clone();
                let id = self.id.clone();
                let generation = widgets.action_generation.get();
                let active_generation = widgets.action_generation.clone();
                let voice_note = matches!(
                    &self.message.content,
                    crate::model::Content::Audio {
                        voice_note: true,
                        ..
                    }
                );
                let controls = crate::native_media_widgets::AudioControls::new(
                    voice,
                    voice_note,
                    move |intent| {
                        if active_generation.get() == generation {
                            sender.input(Input::AudioControl {
                                id: id.clone(),
                                intent,
                            });
                        }
                    },
                );
                widgets.audio.append(&controls.widget);
                self.audio_registry
                    .borrow_mut()
                    .insert(self.id.clone(), controls.clone());
                widgets.audio_controls = Some(controls);
            } else if let Some(controls) = &widgets.audio_controls {
                controls.update(voice);
            }
        }
        widgets.audio.set_visible(self.audio.is_some());
    }
}

/// True when `b` would render the same media as `a`. A sent sticker moves
/// from the picker file to the uploaded cache copy; the picture is the same,
/// so keep the decoded one while the old file still exists.
fn same_media(a: &crate::model::Message, b: &crate::model::Message) -> bool {
    use crate::model::Content;
    let same_content = match (&a.content, &b.content) {
        (Content::Sticker { media: old, .. }, Content::Sticker { media: new, .. }) => {
            old == new
                || (new.path.is_some() && old.path.as_ref().is_some_and(|path| path.is_file()))
        }
        (old, new) => old == new,
    };
    a.id == b.id
        && a.chat == b.chat
        && a.from_me == b.from_me
        && same_content
        && a.thumbnail == b.thumbnail
}

/// Express a row-local click in the list's coordinates without sending GTK objects across threads.
fn message_menu_position(row: &gtk::Widget, x: f64, y: f64) -> Option<gtk::graphene::Point> {
    let list = row.ancestor(gtk::ListView::static_type())?;
    row.compute_point(&list, &gtk::graphene::Point::new(x as f32, y as f32))
}

pub struct Init {
    pub dirs: AppDirs,
}

pub struct NativeApplication {
    window: adw::ApplicationWindow,
    backend: Option<Backend>,
    #[cfg(feature = "demo")]
    synthetic_events: Option<std::sync::mpsc::Sender<crate::backend::Event>>,
    #[cfg(feature = "demo")]
    synthetic_flow_started: bool,
    notifications: crate::native_notifications::NativeNotifications,
    notifier: EventNotifier,
    _event_drain: GlibEventDrain,
    shutdown_started: bool,
    chats: TypedListView<ChatRow, gtk::SingleSelection>,
    chat_search: Option<gtk::SearchEntry>,
    unread_filter: Option<gtk::ToggleButton>,
    pinned_filter: Option<gtk::ToggleButton>,
    chat_section: Option<adw::ToggleGroup>,
    muted_filter: Option<gtk::ToggleButton>,
    /// The "All" pill; activating it clears the private/group filter.
    chat_kind_filter: Option<gtk::ToggleButton>,
    chat_projection: crate::native_chat_list::ChatListProjection,
    chat_filters: crate::native_chat_list::ChatListFilters,
    chat_ids: Vec<String>,
    chat_snapshots: Vec<crate::model::Chat>,
    contacts: std::collections::HashMap<String, crate::model::Contact>,
    avatars: std::collections::HashMap<String, std::path::PathBuf>,
    avatar_requests: std::collections::HashSet<String>,
    typing: std::collections::HashMap<String, String>,
    typing_until: std::collections::HashMap<String, std::time::Instant>,
    composing_until: std::collections::HashMap<String, std::time::Instant>,
    presence: std::collections::HashMap<String, (bool, Option<i64>)>,
    messages: TypedListView<MessageRow, gtk::NoSelection>,
    qr_texture: Option<gtk::gdk::Texture>,
    history_complete: bool,
    loading_older: bool,
    message_ids: Vec<String>,
    message_snapshots: std::collections::HashMap<String, crate::model::Message>,
    pending_quote_navigation: Option<(String, String)>,
    pointer_sender: ComponentSender<NativeApplication>,
    editable_messages: std::collections::HashMap<String, String>,
    transcript: Vec<crate::native_transcript::TranscriptRow>,
    selected_voice: Option<crate::native_voice::VoiceMessage>,
    selected_voice_message: Option<String>,
    media: crate::services::media::MediaService,
    audio_waveforms: std::collections::HashMap<(String, String), Vec<u8>>,
    waveform_queue: std::collections::VecDeque<(String, String, std::path::PathBuf)>,
    waveform_busy: bool,
    waveform_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    waveform_attempted: std::collections::HashSet<(String, String)>,
    playing_audio: Option<String>,
    audio_errors: std::collections::HashMap<(String, String), String>,
    audio_registry: AudioRegistry,
    voice_send_pending: bool,
    message_target: Option<String>,
    message_menu: gtk::PopoverMenu,
    poll_choice: usize,
    sticker_packs: Vec<crate::model::StickerPack>,
    recent_stickers: Vec<std::path::PathBuf>,
    favorite_stickers: Vec<std::path::PathBuf>,
    sticker_emojis: std::collections::HashMap<std::path::PathBuf, Vec<String>>,
    active_chat: Option<String>,
    draft: String,
    drafts: std::collections::HashMap<String, String>,
    composer: crate::native_composer::NativeComposerState,
    composer_buffer: gtk::TextBuffer,
    composer_view: Option<gtk::TextView>,
    sticker_button: Option<gtk::Button>,
    /// The open sticker picker and its page stack, refreshed as lists arrive.
    sticker_picker: Option<(gtk::Popover, gtk::Stack)>,
    pending_composer_request: Option<crate::native_composer::ComposerRequest>,
    pending_attachments: std::collections::HashMap<String, Vec<std::path::PathBuf>>,
    pending_clipboard_images: std::collections::HashMap<String, ClipboardPixels>,
    pending_send: Option<PendingSend>,
    pending_edit: Option<(String, String, String)>,
    reply_to: Option<(String, String)>,
    editing: Option<(String, String)>,
    account_receipts_off: bool,
    played_voice: std::collections::HashSet<(String, String)>,
    link: LinkStatus,
    theme_catalog: crate::theme::custom::Catalog,
    page_title: String,
    status: String,
    settings: crate::settings::Settings,
    settings_path: std::path::PathBuf,
    portals: crate::native_portals::NativePortals,
    portal_requests: std::rc::Rc<
        std::cell::RefCell<std::collections::HashSet<crate::native_portals::RequestId>>,
    >,
    preferences: Option<crate::native_preferences::NativePreferencesDialog>,
    sidebar: Option<gtk::Widget>,
    sidebar_visible: bool,
    split_view: Option<adw::OverlaySplitView>,
    phone_linking: bool,
    /// Chat snapshots changed since the list widget was last rebuilt.
    chats_dirty: bool,
    chats_flush_scheduled: bool,
    zoom_provider: gtk::CssProvider,
    custom_theme_provider: gtk::CssProvider,
    enter_sends: std::rc::Rc<std::cell::Cell<bool>>,
}

#[derive(Debug)]
pub enum Input {
    WindowMapped,
    ToggleSidebar,
    StartBackend,
    BackendReady,
    SelectChat(u32),
    HighlightChat(u32),
    OpenChatId(String),
    LoadOlder,
    SearchChats(String),
    SetUnreadFilter(bool),
    SetPinnedFilter(bool),
    SetChatKindFilter(usize),
    SetArchivedFilter(bool),
    SetMutedFilter(bool),
    Reconnect,
    SelectMessage(u32),
    ShowMessageMenu {
        id: String,
        x: f32,
        y: f32,
    },
    OpenQuoted,
    ReplySelected,
    EditSelected,
    CancelReply,
    CancelEdit,
    DraftChanged(String),
    PickAttachments,
    AttachmentsPicked {
        chat: String,
        paths: Vec<std::path::PathBuf>,
    },
    ClearAttachments,
    SendText(String),
    CopyTranscript,
    ActivateVoice,
    CycleVoiceSpeed,
    SeekVoice(f64),
    AudioControl {
        id: String,
        intent: crate::native_voice::VoiceIntent,
    },
    AudioWaveformReady {
        chat: String,
        id: String,
        bars: Option<Vec<u8>>,
    },
    ActivateSelectedAttachment,
    ReactSelected(String),
    CopySelectedText,
    ShowForward,
    ForwardSelected(String),
    DeleteSelected(bool),
    VoteOption(usize),
    CreatePoll {
        question: String,
        first: String,
        second: String,
    },
    Recording(crate::native_voice::RecordingIntent),
    PollVoice,
    ShowStickerPicker,
    SendSticker(std::path::PathBuf),
    ClearTyping(String),
    StopComposing(String),
    InsertEmoji(String),
    InsertMention,
    InsertMentionId(String),
    PasteClipboardImage,
    AttachDropped(Vec<std::path::PathBuf>),
    ClipboardImageReady {
        chat: String,
        pixels: ClipboardPixels,
    },
    PortalActionFinished(String),
    OpenSelectedUri,
    PortalUriFinished(bool),
    SaveSelectedAttachment,
    SaveAttachmentFinished(bool),
    ToggleSelectedPin,
    ToggleSelectedArchive,
    ToggleSelectedMute,
    Close,
    Quit,
    ShutdownComplete,
    MediaAction(crate::native_media::NativeMediaAction),
    ShowPreferences,
    OpenThemesFolder,
    ThemesFolderPrepared(bool),
    ThemesFolderFinished(bool),
    SplitCollapsed(bool),
    ApplyPreferences,
    ShowAbout,
    ShowShortcuts,
    ConfirmDelete(bool),
    UnlinkConfirmed,
    ShowChatInfo,
    PairWithPhone(String),
    TogglePhoneLinking,
    FlushChats,
    NewContact {
        phone: String,
        name: Option<String>,
    },
}

#[relm4::component(pub)]
impl SimpleComponent for NativeApplication {
    type Init = Init;
    type Input = Input;
    type Output = ();

    view! {
        adw::ApplicationWindow {
            set_title: Some("ZapTide"),
            set_default_size: (960, 640),
            set_size_request: (360, 480),

            connect_map[sender] => move |_| sender.input(Input::WindowMapped),

            connect_close_request[sender] => move |_| {
                sender.input(Input::Close);
                gtk::glib::Propagation::Stop
            },

            #[wrap(Some)]
            set_content = &adw::ToastOverlay {
                #[wrap(Some)]
                set_child = &gtk::Stack {
                    set_transition_type: gtk::StackTransitionType::Crossfade,

                    add_named[Some("link")] = &adw::ToolbarView {
                        add_top_bar = &adw::HeaderBar {
                            set_show_title: false,
                            pack_end = &gtk::MenuButton {
                                set_icon_name: "open-menu-symbolic",
                                set_tooltip_text: Some("Main menu"),
                                set_menu_model: Some(&link_menu),
                            },
                        },

                        #[wrap(Some)]
                        set_content = &gtk::ScrolledWindow {
                            set_hscrollbar_policy: gtk::PolicyType::Never,
                            #[wrap(Some)]
                            set_child = &adw::Clamp {
                                set_maximum_size: 420,
                                set_valign: gtk::Align::Center,
                                #[wrap(Some)]
                                set_child = &gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_spacing: 18,
                                    set_margin_top: 24,
                                    set_margin_bottom: 24,
                                    set_margin_start: 24,
                                    set_margin_end: 24,

                                    append = &gtk::Label {
                                        add_css_class: "title-1",
                                        set_wrap: true,
                                        set_justify: gtk::Justification::Center,
                                        #[watch]
                                        set_label: if model.phone_linking && model.pair_code().is_none() { "Link with phone number" } else { model.page_title.as_str() },
                                    },
                                    #[name = "status_label"]
                                    append = &gtk::Label {
                                        add_css_class: "dim-label",
                                        set_wrap: true,
                                        set_justify: gtk::Justification::Center,
                                        #[watch]
                                        set_label: if model.phone_linking && model.pair_code().is_none() && !model.pairing_requested() { "Enter your phone number with country code. WhatsApp will send a code to type on your phone." } else { model.status.as_str() },
                                    },
                                    append = &gtk::Picture {
                                        add_css_class: "zaptide-qr",
                                        set_halign: gtk::Align::Center,
                                        set_size_request: (264, 264),
                                        set_can_shrink: false,
                                        set_alternative_text: Some("WhatsApp device-linking QR code"),
                                        #[watch]
                                        set_visible: model.qr_texture.is_some() && !model.phone_linking,
                                        #[watch]
                                        set_paintable: model.qr_texture.as_ref(),
                                    },
                                    append = &gtk::Label {
                                        add_css_class: "title-1",
                                        add_css_class: "monospace",
                                        set_selectable: true,
                                        #[watch]
                                        set_visible: model.pair_code().is_some(),
                                        #[watch]
                                        set_label: model.pair_code().unwrap_or_default(),
                                    },
                                    append = &adw::Spinner {
                                        set_halign: gtk::Align::Center,
                                        set_size_request: (32, 32),
                                        #[watch]
                                        set_visible: model.link_busy(),
                                    },
                                    append = &gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 12,
                                        #[watch]
                                        set_visible: model.phone_linking && model.pair_code().is_none() && !model.pairing_requested(),
                                        #[name = "phone_entry"]
                                        append = &gtk::Entry {
                                            set_placeholder_text: Some("+55 11 91234 5678"),
                                            set_input_purpose: gtk::InputPurpose::Phone,
                                            connect_activate[sender] => move |entry| sender.input(Input::PairWithPhone(entry.text().to_string())),
                                        },
                                        append = &gtk::Button {
                                            set_label: "Get Code",
                                            set_halign: gtk::Align::Center,
                                            add_css_class: "pill",
                                            add_css_class: "suggested-action",
                                            connect_clicked[sender, phone_entry] => move |_| sender.input(Input::PairWithPhone(phone_entry.text().to_string())),
                                        },
                                    },
                                    append = &gtk::Button {
                                        set_halign: gtk::Align::Center,
                                        add_css_class: "pill",
                                        #[watch]
                                        set_visible: matches!(model.link, LinkStatus::Unlinked { pairing_phone: None, pair_code: None, .. }),
                                        #[watch]
                                        set_label: if model.phone_linking { "Use QR Code Instead" } else { "Link With Phone Number" },
                                        connect_clicked => Input::TogglePhoneLinking,
                                    },
                                    append = &gtk::Button {
                                        set_label: "Try Again",
                                        set_halign: gtk::Align::Center,
                                        add_css_class: "pill",
                                        add_css_class: "suggested-action",
                                        #[watch]
                                        set_visible: matches!(model.link, LinkStatus::Failed(_) | LinkStatus::LoggedOut | LinkStatus::Disconnected { .. }),
                                        connect_clicked => Input::Reconnect,
                                    },
                                },
                            },
                        },
                    },

                    #[name = "main_split_view"]
                    add_named[Some("chats")] = &adw::OverlaySplitView {
                        set_min_sidebar_width: 260.0,
                        set_max_sidebar_width: 420.0,
                        #[watch]
                        set_show_sidebar: model.sidebar_visible,

                        #[wrap(Some)]
                        #[name = "sidebar"]
                        set_sidebar = &adw::ToolbarView {
                            add_top_bar = &adw::HeaderBar {
                                #[wrap(Some)]
                                set_title_widget = &adw::WindowTitle {
                                    set_title: "ZapTide",
                                },
                                pack_start = &gtk::Button {
                                    set_icon_name: "chat-message-new-symbolic",
                                    set_tooltip_text: Some("New chat"),
                                    set_action_name: Some("win.new-contact"),
                                },
                                pack_end = &gtk::MenuButton {
                                    set_icon_name: "open-menu-symbolic",
                                    set_tooltip_text: Some("Main menu"),
                                    set_primary: true,
                                    set_menu_model: Some(&primary_menu),
                                },
                            },

                            #[wrap(Some)]
                            set_content = &gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                append = &gtk::Box {
                                    set_spacing: 6,
                                    set_margin_start: 12,
                                    set_margin_end: 12,
                                    set_margin_bottom: 6,
                                    #[name = "chat_search"]
                                    append = &gtk::SearchEntry {
                                        set_hexpand: true,
                                        set_placeholder_text: Some("Search chats"),
                                        connect_search_changed[sender] => move |entry| sender.input(Input::SearchChats(entry.text().to_string())),
                                    },
                                },
                                append = &gtk::ScrolledWindow {
                                    set_vscrollbar_policy: gtk::PolicyType::Never,
                                    set_hscrollbar_policy: gtk::PolicyType::Automatic,
                                    set_margin_start: 12,
                                    set_margin_end: 12,
                                    #[wrap(Some)]
                                    set_child = &gtk::Box {
                                        set_spacing: 6,
                                        // Room for the overlay scrollbar below the pills.
                                        set_margin_bottom: 10,
                                        update_property: &[gtk::accessible::Property::Label("Filter chats")],
                                        #[name = "chat_kind_filter"]
                                        append = &gtk::ToggleButton {
                                            set_label: "All",
                                            set_active: true,
                                            add_css_class: "zaptide-filter-pill",
                                            connect_toggled[sender] => move |button| if button.is_active() { sender.input(Input::SetChatKindFilter(0)) },
                                        },
                                        append = &gtk::ToggleButton {
                                            set_label: "Private",
                                            set_group: Some(&chat_kind_filter),
                                            add_css_class: "zaptide-filter-pill",
                                            connect_toggled[sender] => move |button| if button.is_active() { sender.input(Input::SetChatKindFilter(1)) },
                                        },
                                        append = &gtk::ToggleButton {
                                            set_label: "Groups",
                                            set_group: Some(&chat_kind_filter),
                                            add_css_class: "zaptide-filter-pill",
                                            connect_toggled[sender] => move |button| if button.is_active() { sender.input(Input::SetChatKindFilter(2)) },
                                        },
                                        #[name = "unread_filter"]
                                        append = &gtk::ToggleButton {
                                            set_label: "Unread",
                                            add_css_class: "zaptide-filter-pill",
                                            connect_toggled[sender] => move |button| sender.input(Input::SetUnreadFilter(button.is_active())),
                                        },
                                        #[name = "pinned_filter"]
                                        append = &gtk::ToggleButton {
                                            set_label: "Pinned",
                                            add_css_class: "zaptide-filter-pill",
                                            connect_toggled[sender] => move |button| sender.input(Input::SetPinnedFilter(button.is_active())),
                                        },
                                        #[name = "muted_filter"]
                                        append = &gtk::ToggleButton {
                                            set_label: "Muted",
                                            add_css_class: "zaptide-filter-pill",
                                            connect_toggled[sender] => move |button| sender.input(Input::SetMutedFilter(button.is_active())),
                                        },
                                    },
                                },
                                append = &gtk::Box {
                                    set_spacing: 4,
                                    set_margin_start: 12,
                                    set_margin_end: 12,
                                    set_margin_bottom: 6,
                                    #[name = "chat_section"]
                                    append = &adw::ToggleGroup {
                                        set_hexpand: true,
                                        set_homogeneous: true,
                                        add_css_class: "flat",
                                        add = adw::Toggle {
                                            set_name: Some("chats"),
                                            set_icon_name: Some("user-available-symbolic"),
                                            set_tooltip: "Chats",
                                        },
                                        add = adw::Toggle {
                                            set_name: Some("archived"),
                                            // With a child, only the label names the button for
                                            // screen readers; the child is what is shown.
                                            set_label: Some("Archived"),
                                            set_tooltip: "Archived",
                                            #[wrap(Some)]
                                            set_child = &gtk::Box {
                                                set_spacing: 6,
                                                set_halign: gtk::Align::Center,
                                                append = &gtk::Image {
                                                    set_icon_name: Some("package-x-generic-symbolic"),
                                                },
                                                append = &gtk::Label {
                                                    add_css_class: "zaptide-unread-pill",
                                                    add_css_class: "muted",
                                                    #[watch]
                                                    set_visible: model.archived_unread_count() > 0,
                                                    #[watch]
                                                    set_label: &model.archived_unread_count().to_string(),
                                                },
                                            },
                                        },
                                        set_active_name: Some("chats"),
                                        connect_active_name_notify[sender] => move |group| {
                                            sender.input(Input::SetArchivedFilter(group.active_name().as_deref() == Some("archived")));
                                        },
                                    },
                                },
                                append = &gtk::ScrolledWindow {
                                    set_vexpand: true,
                                    set_hscrollbar_policy: gtk::PolicyType::Never,
                                    #[watch]
                                    set_visible: !model.chat_ids.is_empty(),
                                    #[local_ref]
                                    chat_view -> gtk::ListView {
                                        add_css_class: "navigation-sidebar",
                                        set_single_click_activate: true,
                                        connect_activate[sender] => move |_, position| sender.input(Input::SelectChat(position)),
                                    },
                                },
                                append = &adw::StatusPage {
                                    add_css_class: "compact",
                                    set_vexpand: true,
                                    set_icon_name: Some("system-search-symbolic"),
                                    #[watch]
                                    set_visible: model.chat_ids.is_empty(),
                                    #[watch]
                                    set_title: model.chat_list_empty_title(),
                                    #[watch]
                                    set_description: Some(model.chat_list_empty_description()),
                                },
                            },
                        },

                        #[wrap(Some)]
                        set_content = &adw::ToolbarView {
                            add_top_bar = &adw::HeaderBar {
                                pack_start = &gtk::Button {
                                    set_icon_name: "sidebar-show-symbolic",
                                    set_tooltip_text: Some("Show chats"),
                                    #[watch]
                                    set_visible: !model.sidebar_visible || model.split_view.as_ref().is_some_and(adw::OverlaySplitView::is_collapsed),
                                    connect_clicked => Input::ToggleSidebar,
                                },
                                #[wrap(Some)]
                                #[name = "conversation_title"]
                                set_title_widget = &adw::WindowTitle {
                                    #[watch]
                                    set_title: if model.active_chat.is_some() { model.page_title.as_str() } else { "" },
                                    #[watch]
                                    set_subtitle: &model.header_subtitle(),
                                },
                                pack_end = &gtk::MenuButton {
                                    set_icon_name: "view-more-symbolic",
                                    set_tooltip_text: Some("Chat menu"),
                                    #[watch]
                                    set_visible: model.active_chat.is_some(),
                                    #[wrap(Some)]
                                    #[name = "chat_popover"]
                                    set_popover = &gtk::Popover {
                                        add_css_class: "menu",
                                        #[wrap(Some)]
                                        set_child = &gtk::Box {
                                            set_orientation: gtk::Orientation::Vertical,
                                            append = &gtk::Button {
                                                set_label: "Chat Info",
                                                add_css_class: "flat",
                                                connect_clicked[sender, chat_popover] => move |_| { chat_popover.popdown(); sender.input(Input::ShowChatInfo) },
                                            },
                                            append = &gtk::Button {
                                                add_css_class: "flat",
                                                #[watch]
                                                set_label: if model.selected_chat().is_some_and(|chat| chat.pinned) { "Unpin" } else { "Pin" },
                                                connect_clicked[sender, chat_popover] => move |_| { chat_popover.popdown(); sender.input(Input::ToggleSelectedPin) },
                                            },
                                            append = &gtk::Button {
                                                add_css_class: "flat",
                                                #[watch]
                                                set_label: if model.selected_chat().is_some_and(|chat| chat.archived) { "Unarchive" } else { "Archive" },
                                                connect_clicked[sender, chat_popover] => move |_| { chat_popover.popdown(); sender.input(Input::ToggleSelectedArchive) },
                                            },
                                            append = &gtk::Button {
                                                add_css_class: "flat",
                                                #[watch]
                                                set_label: if model.selected_chat().is_some_and(|chat| chat.muted(crate::util::now())) { "Unmute" } else { "Mute" },
                                                connect_clicked[sender, chat_popover] => move |_| { chat_popover.popdown(); sender.input(Input::ToggleSelectedMute) },
                                            },
                                            append = &gtk::Separator {},
                                            append = &gtk::Button {
                                                set_label: "Copy Transcript",
                                                add_css_class: "flat",
                                                #[watch]
                                                set_sensitive: !model.message_ids.is_empty(),
                                                connect_clicked[sender, chat_popover] => move |_| { chat_popover.popdown(); sender.input(Input::CopyTranscript) },
                                            },
                                        },
                                    },
                                },
                            },
                            add_top_bar = &adw::Banner {
                                set_title: "Connection lost",
                                set_button_label: Some("Reconnect"),
                                #[watch]
                                set_revealed: matches!(model.link, LinkStatus::Failed(_) | LinkStatus::Disconnected { .. }),
                                connect_button_clicked => Input::Reconnect,
                            },

                            #[wrap(Some)]
                            set_content = &gtk::Stack {

                                add_named[Some("empty")] = &adw::StatusPage {
                                    set_icon_name: Some("chat-message-new-symbolic"),
                                    set_title: "No Conversation Selected",
                                    set_description: Some("Choose a chat from the list to start messaging."),
                                },

                                #[name = "conversation_body"]
                                add_named[Some("conversation")] = &gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,

                                    append = &gtk::Button {
                                        set_label: "Load Older Messages",
                                        set_halign: gtk::Align::Center,
                                        set_margin_top: 6,
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: !model.message_ids.is_empty() && !model.history_complete,
                                        #[watch]
                                        set_sensitive: !model.loading_older,
                                        connect_clicked => Input::LoadOlder,
                                    },

                                    append = &gtk::ScrolledWindow {
                                        set_vexpand: true,
                                        set_hscrollbar_policy: gtk::PolicyType::Never,
                                        #[local_ref]
                                        message_view -> gtk::ListView {
                                            add_css_class: "zaptide-transcript",
                                            set_single_click_activate: true,
                                            connect_activate[sender] => move |_, position| sender.input(Input::SelectMessage(position)),
                                        },
                                    },

                                    append = &gtk::Box {
                                        set_margin_start: 12,
                                        set_margin_end: 12,
                                        set_margin_top: 6,
                                        set_spacing: 6,
                                        #[watch]
                                        set_visible: model.recording_active(),
                                        append = &gtk::Label {
                                            set_hexpand: true,
                                            set_xalign: 0.0,
                                            #[watch]
                                            set_label: &model.recording_status(),
                                        },
                                        append = &gtk::Button {
                                            set_label: "Cancel",
                                            connect_clicked => Input::Recording(crate::native_voice::RecordingIntent::Cancel),
                                        },
                                        append = &gtk::Button {
                                            set_label: "Send Voice",
                                            add_css_class: "suggested-action",
                                            #[watch]
                                            set_sensitive: model.pending_send.is_none() && !model.voice_send_pending,
                                            connect_clicked => Input::Recording(crate::native_voice::RecordingIntent::Send),
                                        },
                                    },

                                    append = &gtk::Box {
                                        set_margin_start: 12,
                                        set_margin_end: 12,
                                        set_margin_top: 6,
                                        set_spacing: 6,
                                        #[watch]
                                        set_visible: model.reply_to.is_some() || model.editing.is_some(),
                                        append = &gtk::Label {
                                            add_css_class: "zaptide-quote",
                                            set_hexpand: true,
                                            set_xalign: 0.0,
                                            set_ellipsize: gtk::pango::EllipsizeMode::End,
                                            #[watch]
                                            set_label: if model.editing.is_some() { "Editing message" } else { "Replying to message" },
                                        },
                                        append = &gtk::Button {
                                            set_icon_name: "window-close-symbolic",
                                            set_tooltip_text: Some("Cancel"),
                                            add_css_class: "flat",
                                            add_css_class: "circular",
                                            connect_clicked[sender] => move |_| {
                                                sender.input(Input::CancelReply);
                                                sender.input(Input::CancelEdit);
                                            },
                                        },
                                    },

                                    append = &gtk::Box {
                                        set_margin_start: 12,
                                        set_margin_end: 12,
                                        set_margin_top: 6,
                                        set_spacing: 6,
                                        #[watch]
                                        set_visible: model.pending_attachment_count() > 0,
                                        append = &gtk::Picture {
                                            set_size_request: (96, 72),
                                            set_can_shrink: true,
                                            set_content_fit: gtk::ContentFit::Contain,
                                            set_tooltip_text: Some("Clipboard image staged for sending"),
                                            set_alternative_text: Some("Clipboard image preview"),
                                            #[watch]
                                            set_visible: model.active_chat.as_ref().is_some_and(|chat| model.pending_clipboard_images.contains_key(chat)),
                                            #[watch]
                                            set_paintable: model.active_chat.as_ref().and_then(|chat| model.pending_clipboard_images.get(chat)).map(|image| &image.preview),
                                        },
                                        append = &gtk::Label {
                                            set_hexpand: true,
                                            set_xalign: 0.0,
                                            #[watch]
                                            set_label: &attachment_summary(model.pending_attachment_count()),
                                        },
                                        append = &gtk::Button {
                                            set_icon_name: "window-close-symbolic",
                                            set_tooltip_text: Some("Clear attachments"),
                                            add_css_class: "flat",
                                            add_css_class: "circular",
                                            connect_clicked => Input::ClearAttachments,
                                        },
                                    },

                                    append = &gtk::Box {
                                        set_spacing: 6,
                                        set_margin_top: 6,
                                        set_margin_bottom: 6,
                                        set_margin_start: 6,
                                        set_margin_end: 6,

                                        append = &gtk::MenuButton {
                                            set_icon_name: "mail-attachment-symbolic",
                                            set_tooltip_text: Some("Attach"),
                                            set_valign: gtk::Align::End,
                                            add_css_class: "flat",
                                            add_css_class: "circular",
                                            set_direction: gtk::ArrowType::Up,
                                            #[wrap(Some)]
                                            #[name = "attach_popover"]
                                            set_popover = &gtk::Popover {
                                                add_css_class: "menu",
                                                #[wrap(Some)]
                                                set_child = &gtk::Box {
                                                    set_orientation: gtk::Orientation::Vertical,
                                                    append = &gtk::Button {
                                                        set_label: "Files…",
                                                        add_css_class: "flat",
                                                        #[watch]
                                                        set_sensitive: model.can_attach(),
                                                        connect_clicked[sender, attach_popover] => move |_| { attach_popover.popdown(); sender.input(Input::PickAttachments) },
                                                    },
                                                    append = &gtk::Button {
                                                        set_label: "Paste Image",
                                                        add_css_class: "flat",
                                                        #[watch]
                                                        set_sensitive: model.can_attach(),
                                                        connect_clicked[sender, attach_popover] => move |_| { attach_popover.popdown(); sender.input(Input::PasteClipboardImage) },
                                                    },
                                                    append = &gtk::Button {
                                                        set_label: "Poll…",
                                                        add_css_class: "flat",
                                                        #[watch]
                                                        set_sensitive: model.can_attach(),
                                                        connect_clicked[sender, attach_popover, dialog_parent] => move |_| { attach_popover.popdown(); show_poll_dialog(&dialog_parent, &sender) },
                                                    },
                                                    append = &gtk::Button {
                                                        set_label: "Mention…",
                                                        add_css_class: "flat",
                                                        #[watch]
                                                        set_sensitive: model.active_chat.as_deref().and_then(|id| model.chat_snapshots.iter().find(|chat| chat.id == id)).is_some_and(|chat| !chat.participants.is_empty()),
                                                        connect_clicked[sender, attach_popover] => move |_| { attach_popover.popdown(); sender.input(Input::InsertMention) },
                                                    },
                                                },
                                            },
                                        },

                                         append = &gtk::MenuButton {
                                            set_icon_name: "face-smile-symbolic",
                                            set_tooltip_text: Some("Emoji"),
                                            set_valign: gtk::Align::End,
                                            add_css_class: "flat",
                                            add_css_class: "circular",
                                            #[wrap(Some)]
                                            set_popover = &gtk::EmojiChooser {
                                                connect_emoji_picked[sender] => move |_, emoji| sender.input(Input::InsertEmoji(emoji.to_owned())),
                                            },
                                        },

                                        #[name = "sticker_button"]
                                        append = &gtk::Button {
                                            set_icon_name: "emoji-nature-symbolic",
                                            set_tooltip_text: Some("Sticker"),
                                            set_valign: gtk::Align::End,
                                            add_css_class: "flat",
                                            add_css_class: "circular",
                                            connect_clicked => Input::ShowStickerPicker,
                                        },

                                        append = &gtk::ScrolledWindow {
                                            add_css_class: "zaptide-composer",
                                            set_hexpand: true,
                                            set_hscrollbar_policy: gtk::PolicyType::Never,
                                            set_propagate_natural_height: true,
                                            set_max_content_height: 160,
                                            #[name = "composer"]
                                            #[wrap(Some)]
                                            set_child = &gtk::TextView {
                                                set_wrap_mode: gtk::WrapMode::WordChar,
                                                set_top_margin: 8,
                                                set_bottom_margin: 8,
                                                set_left_margin: 12,
                                                set_right_margin: 12,
                                                set_buffer: Some(&model.composer_buffer),
                                                #[watch]
                                                set_editable: !model.voice_send_pending && model.active_chat.as_deref().and_then(|id| model.chat_snapshots.iter().find(|chat| chat.id == id)).is_some_and(crate::model::Chat::can_send),
                                                #[watch]
                                                set_tooltip_text: Some(if model.editing.is_some() { "Edit message" } else { "Write a message" }),
                                            },
                                        },

                                        append = &gtk::Button {
                                            set_icon_name: "audio-input-microphone-symbolic",
                                            set_tooltip_text: Some("Record voice message"),
                                            set_valign: gtk::Align::End,
                                            add_css_class: "flat",
                                            add_css_class: "circular",
                                            #[watch]
                                            set_visible: model.draft.trim().is_empty() && model.editing.is_none(),
                                            #[watch]
                                            set_sensitive: model.can_send_voice() && !model.recording_active(),
                                            connect_clicked => Input::Recording(crate::native_voice::RecordingIntent::Start),
                                        },

                                        append = &gtk::Button {
                                            #[wrap(Some)]
                                            set_child = &paper_plane_icon() -> gtk::DrawingArea {},
                                            set_valign: gtk::Align::End,
                                            add_css_class: "circular",
                                            add_css_class: "suggested-action",
                                            #[watch]
                                            set_tooltip_text: Some(if model.editing.is_some() { "Save edit" } else { "Send" }),
                                            #[watch]
                                            update_property: &[gtk::accessible::Property::Label(if model.editing.is_some() { "Save edit" } else { "Send" })],
                                            #[watch]
                                            set_sensitive: model.active_chat.as_deref().and_then(|id| model.chat_snapshots.iter().find(|chat| chat.id == id)).is_some_and(crate::model::Chat::can_send),
                                            connect_clicked[sender, composer] => move |_| {
                                                let buffer = composer.buffer();
                                                sender.input(Input::SendText(buffer.text(&buffer.start_iter(), &buffer.end_iter(), true).to_string()));
                                            },
                                        },
                                    },
                                },

                                #[watch]
                                set_visible_child_name: if model.active_chat.is_some() { "conversation" } else { "empty" },
                            },
                        },
                    },

                    #[watch]
                    set_visible_child_name: if model.is_linked() { "chats" } else { "link" },
                },
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        #[cfg(feature = "demo")]
        if synthetic_e2e_enabled()
            && let Some((width, height)) = std::env::var("ZAPTIDE_NATIVE_SYNTHETIC_SIZE")
                .ok()
                .and_then(|size| {
                    size.split_once('x')
                        .map(|(width, height)| (width.to_owned(), height.to_owned()))
                })
                .and_then(|(width, height)| {
                    Some((width.parse::<i32>().ok()?, height.parse::<i32>().ok()?))
                })
            && (720..=3_000).contains(&width)
            && (480..=2_000).contains(&height)
        {
            root.set_default_size(width, height);
        }
        let (notifier, drain) = EventNotifier::new();
        let input = sender.clone();
        let event_drain = GlibEventDrain::install(
            &notifier,
            drain,
            gtk::glib::MainContext::default(),
            move || input.input(Input::BackendReady),
        );
        let notification_sender = sender.clone();
        let application: gtk::Application = relm4::main_application().upcast();
        let notifications =
            crate::native_notifications::NativeNotifications::new(&application, move |chat| {
                notification_sender.input(Input::OpenChatId(chat))
            });
        let chats: TypedListView<ChatRow, gtk::SingleSelection> = TypedListView::new();
        let chat_view = &chats.view.clone();
        let selection_sender = sender.clone();
        let observed_selection = chats.selection_model.clone();
        chats
            .selection_model
            .connect_selection_changed(move |_, _, _| {
                let position = observed_selection.selected();
                if position != gtk::INVALID_LIST_POSITION {
                    selection_sender.input(Input::HighlightChat(position));
                }
            });
        let chat_keys = gtk::EventControllerKey::new();
        chat_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let chat_selection = chats.selection_model.clone();
        let chat_key_sender = sender.clone();
        chat_keys.connect_key_pressed(move |_, key, _, _| {
            let selected = chat_selection.selected();
            let count = chat_selection.n_items();
            match key {
                gtk::gdk::Key::Up if count > 0 => {
                    let current = if selected == gtk::INVALID_LIST_POSITION {
                        count - 1
                    } else {
                        selected.saturating_sub(1)
                    };
                    chat_selection.set_selected(current);
                    chat_key_sender.input(Input::HighlightChat(current));
                    gtk::glib::Propagation::Stop
                }
                gtk::gdk::Key::Down if count > 0 => {
                    let current = if selected == gtk::INVALID_LIST_POSITION {
                        0
                    } else {
                        (selected + 1).min(count - 1)
                    };
                    chat_selection.set_selected(current);
                    chat_key_sender.input(Input::HighlightChat(current));
                    gtk::glib::Propagation::Stop
                }
                gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter if count > 0 => {
                    let current = if selected == gtk::INVALID_LIST_POSITION {
                        0
                    } else {
                        selected.min(count - 1)
                    };
                    chat_key_sender.input(Input::SelectChat(current));
                    gtk::glib::Propagation::Stop
                }
                _ => gtk::glib::Propagation::Proceed,
            }
        });
        chat_view.add_controller(chat_keys);
        let messages: TypedListView<MessageRow, gtk::NoSelection> = TypedListView::new();
        let message_view = &messages.view.clone();
        let composer_buffer = gtk::TextBuffer::new(None);
        let enter_sends = std::rc::Rc::new(std::cell::Cell::new(true));
        let composer_keys = gtk::EventControllerKey::new();
        composer_keys.set_propagation_phase(gtk::PropagationPhase::Bubble);
        let enter_setting = enter_sends.clone();
        let enter_buffer = composer_buffer.clone();
        let enter_sender = sender.clone();
        composer_keys.connect_key_pressed(move |_, key, _, modifiers| {
            if matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter)
                && should_send_on_enter(
                    enter_setting.get(),
                    modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK),
                    modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK),
                )
            {
                let text = enter_buffer
                    .text(&enter_buffer.start_iter(), &enter_buffer.end_iter(), true)
                    .to_string();
                enter_sender.input(Input::SendText(text));
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        let settings_path = init.dirs.settings_file();
        let settings = crate::settings::Settings::load(&settings_path);
        // Mirror allowlisted preferences into GSettings. The JSON file stays
        // authoritative and untouched, so theming below reads it as before.
        crate::native_settings_migration::migrate_json_to_gsettings(&init.dirs);
        enter_sends.set(settings.enter_sends);
        let mut theme_catalog = crate::theme::custom::Catalog::default();
        theme_catalog.enable_desktop_themes();
        theme_catalog.start(
            settings_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("themes"),
            settings.custom_theme.clone(),
            &notifier,
        );
        let mut media_service = crate::services::media::MediaService::default();
        media_service.set_speed(settings.voice_speed);
        #[cfg(feature = "demo")]
        let synthetic = synthetic_e2e_enabled();
        #[cfg(feature = "demo")]
        let (backend, synthetic_events, page_title, status) = if synthetic {
            let (mut backend, events) = Backend::detached();
            backend.record_demo_commands();
            let mut chat = crate::model::Chat::new(
                "synthetic-contact@s.whatsapp.net".into(),
                "Synthetic Contact".into(),
            );
            chat.unread = 1;
            chat.last_activity = 1_700_000_002;
            chat.last = Some(crate::model::LastMessage {
                from_me: false,
                sender: chat.id.clone(),
                sender_name: None,
                summary: "Offline link preview sample".into(),
                status: crate::model::Delivery::None,
            });
            let _ = events.send(crate::backend::Event::Link(LinkStatus::Connected));
            let _ = events.send(crate::backend::Event::Chats(vec![chat]));
            let incoming = synthetic_message();
            let _ = events.send(crate::backend::Event::Incoming {
                chat: incoming.chat.clone(),
                message: Box::new(incoming),
            });
            crate::backend::Wake::wake(&notifier);
            if let Some(count) = std::env::var("ZAPTIDE_NATIVE_SYNTHETIC_FLOOD")
                .ok()
                .and_then(|count| count.parse().ok())
            {
                synthetic_flood(events.clone(), notifier.clone(), count);
            }
            (
                Some(backend),
                Some(events),
                "Synthetic offline conversation".into(),
                "Development-only synthetic data · offline".into(),
            )
        } else {
            match Backend::try_spawn(init.dirs, notifier.clone()) {
                Ok(backend) => (
                    Some(backend),
                    None,
                    "Starting ZapTide".into(),
                    "Waiting for first native frame".into(),
                ),
                Err(_) => {
                    log::error!("native backend could not start");
                    (
                        None,
                        None,
                        "ZapTide needs attention".into(),
                        "Backend unavailable".into(),
                    )
                }
            }
        };
        #[cfg(not(feature = "demo"))]
        let (backend, page_title, status) = match Backend::try_spawn(init.dirs, notifier.clone()) {
            Ok(backend) => (
                Some(backend),
                "Starting ZapTide".into(),
                "Waiting for first native frame".into(),
            ),
            Err(_) => {
                log::error!("native backend could not start");
                (
                    None,
                    "ZapTide needs attention".into(),
                    "Backend unavailable".into(),
                )
            }
        };
        let mut model = Self {
            window: root.clone(),
            backend,
            #[cfg(feature = "demo")]
            synthetic_events,
            #[cfg(feature = "demo")]
            synthetic_flow_started: false,
            notifications,
            notifier,
            _event_drain: event_drain,
            shutdown_started: false,
            chats,
            chat_search: None,
            unread_filter: None,
            pinned_filter: None,
            chat_section: None,
            muted_filter: None,
            chat_kind_filter: None,
            chat_projection: crate::native_chat_list::ChatListProjection::default(),
            chat_filters: crate::native_chat_list::ChatListFilters::default(),
            chat_ids: Vec::new(),
            chat_snapshots: Vec::new(),
            contacts: std::collections::HashMap::new(),
            avatars: std::collections::HashMap::new(),
            avatar_requests: std::collections::HashSet::new(),
            typing: std::collections::HashMap::new(),
            typing_until: std::collections::HashMap::new(),
            composing_until: std::collections::HashMap::new(),
            presence: std::collections::HashMap::new(),
            messages,
            qr_texture: None,
            history_complete: false,
            loading_older: false,
            message_ids: Vec::new(),
            message_snapshots: std::collections::HashMap::new(),
            pending_quote_navigation: None,
            pointer_sender: sender.clone(),
            editable_messages: std::collections::HashMap::new(),
            transcript: Vec::new(),
            selected_voice: None,
            selected_voice_message: None,
            media: media_service,
            audio_waveforms: Default::default(),
            waveform_queue: Default::default(),
            waveform_busy: false,
            waveform_cancel: None,
            waveform_attempted: Default::default(),
            playing_audio: None,
            audio_errors: Default::default(),
            audio_registry: Default::default(),
            voice_send_pending: false,
            message_target: None,
            message_menu: gtk::PopoverMenu::from_model(None::<&gtk::gio::MenuModel>),
            poll_choice: 0,
            sticker_packs: Vec::new(),
            recent_stickers: Vec::new(),
            favorite_stickers: Vec::new(),
            sticker_emojis: std::collections::HashMap::new(),
            active_chat: None,
            draft: String::new(),
            drafts: std::collections::HashMap::new(),
            composer: crate::native_composer::NativeComposerState::default(),
            composer_buffer,
            composer_view: None,
            sticker_button: None,
            sticker_picker: None,
            pending_composer_request: None,
            pending_attachments: std::collections::HashMap::new(),
            pending_clipboard_images: std::collections::HashMap::new(),
            pending_send: None,
            pending_edit: None,
            reply_to: None,
            editing: None,
            account_receipts_off: false,
            played_voice: std::collections::HashSet::new(),
            link: LinkStatus::Starting,
            theme_catalog,
            page_title,
            status,
            settings,
            settings_path,
            portals: crate::native_portals::NativePortals::default(),
            portal_requests: std::rc::Rc::default(),
            preferences: None,
            sidebar: None,
            sidebar_visible: true,
            split_view: None,
            phone_linking: false,
            chats_dirty: false,
            chats_flush_scheduled: false,
            zoom_provider: gtk::CssProvider::new(),
            custom_theme_provider: gtk::CssProvider::new(),
            enter_sends,
        };
        let buffer_sender = sender.clone();
        model.composer_buffer.connect_changed(move |buffer| {
            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .to_string();
            buffer_sender.input(Input::DraftChanged(text));
        });
        let dialog_parent = model.window.clone();
        let link_menu = gtk::gio::Menu::new();
        link_menu.append(Some("_Preferences"), Some("win.preferences"));
        link_menu.append(Some("_About ZapTide"), Some("win.about"));
        link_menu.append(Some("_Quit"), Some("win.quit"));
        let primary_menu = gtk::gio::Menu::new();
        let section = gtk::gio::Menu::new();
        section.append(Some("_New Chat"), Some("win.new-contact"));
        section.append(Some("_Unlink This Computer"), Some("win.unlink"));
        primary_menu.append_section(None, &section);
        let section = gtk::gio::Menu::new();
        section.append(Some("_Preferences"), Some("win.preferences"));
        section.append(Some("_Keyboard Shortcuts"), Some("win.shortcuts"));
        section.append(Some("_About ZapTide"), Some("win.about"));
        section.append(Some("_Quit"), Some("win.quit"));
        primary_menu.append_section(None, &section);
        install_window_actions(&root, &sender);
        let widgets = view_output!();
        model.chat_search = Some(widgets.chat_search.clone());
        model.unread_filter = Some(widgets.unread_filter.clone());
        model.pinned_filter = Some(widgets.pinned_filter.clone());
        model.chat_section = Some(widgets.chat_section.clone());
        model.muted_filter = Some(widgets.muted_filter.clone());
        model.chat_kind_filter = Some(widgets.chat_kind_filter.clone());
        model.composer_view = Some(widgets.composer.clone());
        model.sticker_button = Some(widgets.sticker_button.clone());
        model.message_menu.set_parent(&widgets.conversation_body);
        model.message_menu.set_has_arrow(false);
        model.message_menu.set_halign(gtk::Align::Start);
        install_message_actions(&root, &sender);
        widgets
            .status_label
            .set_accessible_role(gtk::AccessibleRole::Status);
        widgets
            .status_label
            .connect_notify_local(Some("label"), |widget, _| {
                if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                    let text = label.label();
                    if !text.is_empty() {
                        label.announce(&text, gtk::AccessibleAnnouncementPriority::Medium);
                    }
                }
            });
        widgets
            .conversation_title
            .connect_notify_local(Some("subtitle"), |title, _| {
                let text = title.subtitle();
                if !text.is_empty() {
                    title
                        .upcast_ref::<gtk::Widget>()
                        .announce(&text, gtk::AccessibleAnnouncementPriority::Medium);
                }
            });
        let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            640.0,
            adw::LengthUnit::Sp,
        ));
        breakpoint.add_setter(
            &widgets.main_split_view,
            "collapsed",
            Some(&true.to_value()),
        );
        root.add_breakpoint(breakpoint);
        widgets.composer.add_controller(composer_keys);
        widgets
            .composer
            .update_property(&[gtk::accessible::Property::Label("Message composer")]);
        widgets
            .composer
            .update_property(&[gtk::accessible::Property::Description(
                "Write a message. Return inserts a line; use Send to submit when using IME.",
            )]);
        model.sidebar = Some(widgets.sidebar.clone().upcast());
        model.split_view = Some(widgets.main_split_view.clone());
        model.sidebar_visible = !widgets.main_split_view.is_collapsed();
        let split_sender = sender.clone();
        widgets
            .main_split_view
            .connect_notify_local(Some("collapsed"), move |split, _| {
                split_sender.input(Input::SplitCollapsed(split.is_collapsed()));
            });
        let drop_target = gtk::DropTarget::new(
            gtk::gdk::FileList::static_type(),
            gtk::gdk::DragAction::COPY,
        );
        let drop_sender = sender.clone();
        drop_target.connect_drop(move |_, value, _, _| {
            let Ok(files) = value.get::<gtk::gdk::FileList>() else {
                return false;
            };
            let paths: Vec<_> = files
                .files()
                .into_iter()
                .filter_map(|file| file.path())
                .collect();
            if paths.is_empty() {
                return false;
            }
            drop_sender.input(Input::AttachDropped(paths));
            true
        });
        widgets.composer.add_controller(drop_target);
        model.install_zoom_provider();
        model.apply_runtime_settings();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, input: Self::Input, sender: ComponentSender<Self>) {
        match input {
            Input::WindowMapped => {
                gtk::glib::idle_add_local_once(move || sender.input(Input::StartBackend));
            }
            Input::ToggleSidebar => self.sidebar_visible = !self.sidebar_visible,
            Input::SplitCollapsed(collapsed) => self.sidebar_visible = !collapsed,
            Input::ShowPreferences => {
                let preferences =
                    crate::native_preferences::NativePreferences::from(&self.settings);
                let theme_choices: Vec<_> = self
                    .theme_catalog
                    .picker_themes()
                    .map(|theme| theme.filename.clone())
                    .collect();
                let dialog = crate::native_preferences::NativePreferencesDialog::new(
                    &preferences,
                    &theme_choices,
                );
                dialog.present(&self.window);
                let input_sender = sender.clone();
                connect_widget_changes(dialog.dialog().upcast_ref(), &input_sender);
                let input_sender = sender.clone();
                dialog
                    .open_themes_folder_button()
                    .connect_clicked(move |_| input_sender.input(Input::OpenThemesFolder));
                self.preferences = Some(dialog);
            }
            Input::OpenThemesFolder => self.prepare_themes_folder(&sender),
            Input::ThemesFolderPrepared(success) => {
                if success {
                    self.launch_themes_folder(&sender);
                } else {
                    self.status = "Could not open themes folder".into();
                    self.toast("Could not open themes folder");
                }
            }
            Input::ThemesFolderFinished(success) => {
                self.status = if success {
                    "Opened themes folder".into()
                } else {
                    "Could not open themes folder".into()
                };
                if !success {
                    self.toast("Could not open themes folder");
                }
            }
            Input::ShowAbout => {
                let about = adw::AboutDialog::builder()
                    .application_name("ZapTide")
                    .application_icon("dev.luminusos.ZapTide")
                    .version(env!("CARGO_PKG_VERSION"))
                    .developer_name("LuminusOS")
                    .build();
                about.present(Some(&self.window));
            }
            Input::ShowShortcuts => {
                let (send, newline) = if self.enter_sends.get() {
                    ("Return", "<Shift>Return")
                } else {
                    ("<Control>Return", "Return")
                };
                let dialog = adw::ShortcutsDialog::new();
                for (title, items) in [
                    (
                        "Chat List",
                        &[
                            ("Previous chat", "Up"),
                            ("Next chat", "Down"),
                            ("Open chat", "Return"),
                        ][..],
                    ),
                    (
                        "Composer",
                        &[("Send message", send), ("New line", newline)][..],
                    ),
                    (
                        "Messages",
                        &[
                            ("Open message menu", "Menu <Shift>F10"),
                            ("Copy selected text", "<Control>c"),
                        ][..],
                    ),
                ] {
                    let section = adw::ShortcutsSection::new(Some(title));
                    for (item, accelerator) in items {
                        section.add(adw::ShortcutsItem::new(item, accelerator));
                    }
                    dialog.add(section);
                }
                dialog.present(Some(&self.window));
            }
            Input::ApplyPreferences => {
                if let Some(dialog) = &self.preferences {
                    let changes = dialog.take_changes();
                    let errors = dialog.take_errors();
                    let custom_theme_changed = changes.iter().any(|change| {
                        change.settings_field()
                            == crate::native_preferences::SettingsField::CustomTheme
                    });
                    for change in changes {
                        if change.apply(&mut self.settings).is_ok() {
                            if let Err(_error) = self.settings.save(&self.settings_path) {
                                self.status = "Could not save preferences".into();
                                self.toast("Could not save preferences");
                            } else {
                                self.status = "Preferences saved".into();
                            }
                        } else {
                            self.status = "Preference value is invalid".into();
                            self.toast("Preference value is invalid");
                        }
                    }
                    if custom_theme_changed {
                        self.sync_custom_theme_selection();
                        if self.settings.save(&self.settings_path).is_err() {
                            self.status = "Could not save preferences".into();
                            self.toast("Could not save preferences");
                        }
                    }
                    if !errors.is_empty() {
                        self.status = "Preference value is invalid".into();
                        self.toast("Preference value is invalid");
                    }
                    self.apply_runtime_settings();
                }
            }
            Input::StartBackend => {
                if let Some(startup) = self.backend.as_mut().and_then(Backend::take_startup) {
                    let _ = startup.send(());
                    self.status = "Starting backend".into();
                }
            }
            Input::BackendReady => {
                let mut events = Vec::new();
                if let Some(backend) = &self.backend {
                    drain_backend_events(backend, &self.notifier, |event| match event {
                        Event::Link(status) => events.push(NativeEvent::Link(status)),
                        Event::Chats(rows) => events.push(NativeEvent::Chats(rows)),
                        Event::ChatUpdated(chat) => events.push(NativeEvent::ChatUpdated(chat)),
                        Event::Contacts(contacts) => events.push(NativeEvent::Contacts(contacts)),
                        Event::Messages {
                            chat,
                            messages,
                            older,
                            complete,
                        } => events.push(NativeEvent::Messages {
                            chat,
                            messages,
                            older,
                            complete,
                        }),
                        Event::OlderFetched { chat, more } => {
                            events.push(NativeEvent::OlderFetched { chat, more })
                        }
                        Event::Stickers {
                            saved,
                            packs,
                            recent,
                        } => events.push(NativeEvent::Stickers {
                            saved,
                            packs,
                            recent,
                        }),
                        Event::MessageUpdated(message) => {
                            events.push(NativeEvent::MessageUpdated(message))
                        }
                        Event::Edited { chat, id, success } => {
                            events.push(NativeEvent::Edited { chat, id, success })
                        }
                        Event::Sent { chat, success } => {
                            events.push(NativeEvent::Sent { chat, success })
                        }
                        Event::AttachmentCompleted {
                            chat,
                            path,
                            success,
                            ..
                        } => events.push(NativeEvent::AttachmentCompleted {
                            chat,
                            path,
                            success,
                        }),
                        Event::Media {
                            chat,
                            message,
                            result,
                        } => events.push(NativeEvent::Media {
                            chat,
                            message,
                            result,
                        }),
                        Event::ReceiptsPrivacy { disabled } => {
                            events.push(NativeEvent::ReceiptsPrivacy { disabled })
                        }
                        Event::ContactReady { id, name } => {
                            events.push(NativeEvent::ContactReady { id, name })
                        }
                        Event::Info(message) => events.push(NativeEvent::Info(message)),
                        Event::MessageDeleted { chat, id } => {
                            events.push(NativeEvent::MessageDeleted { chat, id })
                        }
                        Event::Incoming { chat, message } => {
                            events.push(NativeEvent::Incoming { chat, message })
                        }
                        Event::Typing {
                            chat,
                            sender,
                            composing,
                        } => events.push(NativeEvent::Typing {
                            chat,
                            sender,
                            composing,
                        }),
                        Event::Presence {
                            id,
                            online,
                            last_seen,
                        } => events.push(NativeEvent::Presence {
                            id,
                            online,
                            last_seen,
                        }),
                        Event::Avatar {
                            id,
                            full: false,
                            path,
                        } => events.push(NativeEvent::Avatar { id, path }),
                        Event::Error(error) => events.push(NativeEvent::Error(error)),
                        _ => {}
                    });
                }
                for event in events {
                    match event {
                        NativeEvent::Link(link) => {
                            if matches!(link, LinkStatus::LoggedOut) {
                                self.media.stop_playback();
                                self.playing_audio = None;
                                self.waveform_queue.clear();
                                if let Some(cancel) = self.waveform_cancel.take() {
                                    cancel.store(true, std::sync::atomic::Ordering::Release);
                                }
                                self.audio_waveforms.clear();
                                self.waveform_attempted.clear();
                                self.audio_errors.clear();
                                self.audio_registry.borrow_mut().clear();
                                for chat in &self.chat_snapshots {
                                    self.notifications.clear_chat(&chat.id);
                                }
                                self.active_chat = None;
                                self.chat_snapshots.clear();
                                self.chat_ids.clear();
                                self.reset_chat_filters(false);
                                self.message_ids.clear();
                                self.message_snapshots.clear();
                                self.editable_messages.clear();
                                self.messages.clear();
                                self.contacts.clear();
                                self.avatars.clear();
                                self.presence.clear();
                                self.typing.clear();
                                self.typing_until.clear();
                                self.composing_until.clear();
                                self.drafts.clear();
                                self.composer =
                                    crate::native_composer::NativeComposerState::default();
                                self.composer_buffer.set_text("");
                                self.draft.clear();
                                self.editing = None;
                                self.reply_to = None;
                                self.pending_edit = None;
                                self.pending_composer_request = None;
                                self.pending_send = None;
                                self.voice_send_pending = false;
                                self.account_receipts_off = false;
                                self.played_voice.clear();
                                self.avatar_requests.clear();
                                self.pending_attachments.clear();
                                self.pending_clipboard_images.clear();
                                self.selected_voice = None;
                                self.selected_voice_message = None;
                                self.pending_quote_navigation = None;
                                self.history_complete = false;
                                self.loading_older = false;
                                self.transcript.clear();
                                self.chats_dirty = true;
                                self.sync_chat_projection();
                            }
                            self.qr_texture = match &link {
                                LinkStatus::Unlinked { qr: Some(qr), .. } => qr_texture(qr),
                                _ => None,
                            };
                            self.link = link;
                            (self.page_title, self.status) = link_page(&self.link);
                        }
                        NativeEvent::Chats(chats) => {
                            self.apply_chat_changes(vec![ChatChange::Snapshot(chats)]);
                            #[cfg(feature = "demo")]
                            if synthetic_e2e_enabled() {
                                let sender = sender.clone();
                                gtk::glib::idle_add_local_once(move || {
                                    sender.input(Input::SearchChats("Synthetic".into()));
                                    sender.input(Input::OpenChatId(
                                        "synthetic-contact@s.whatsapp.net".into(),
                                    ));
                                });
                            }
                        }
                        NativeEvent::ChatUpdated(chat) => {
                            self.apply_chat_changes(vec![ChatChange::Update(*chat)])
                        }
                        NativeEvent::Contacts(contacts) => {
                            self.contacts = contacts
                                .into_iter()
                                .map(|contact| (contact.id.clone(), contact))
                                .collect();
                        }
                        NativeEvent::Messages {
                            chat,
                            messages,
                            older,
                            complete,
                        } => {
                            if self.active_chat.as_deref() == Some(&chat) {
                                self.history_complete = complete;
                                self.loading_older = false;
                            }
                            self.apply_messages(chat.clone(), messages, older);
                            #[cfg(feature = "demo")]
                            if synthetic_e2e_enabled()
                                && !self.synthetic_flow_started
                                && self.active_chat.as_deref() == Some(&chat)
                            {
                                self.synthetic_flow_started = true;
                                let sender = sender.clone();
                                gtk::glib::idle_add_local_once(move || {
                                    sender.input(Input::LoadOlder);
                                    sender.input(Input::DraftChanged(
                                        "Synthetic end-to-end message".into(),
                                    ));
                                    sender.input(Input::SendText(
                                        "Synthetic end-to-end message".into(),
                                    ));
                                });
                            }
                            if let Some((target_chat, target_id)) =
                                self.pending_quote_navigation.clone()
                                && target_chat == chat
                                && self.message_ids.contains(&target_id)
                            {
                                self.pending_quote_navigation = None;
                                self.scroll_message_into_view(&target_id);
                            }
                        }
                        NativeEvent::OlderFetched { chat, more } => {
                            if self.active_chat.as_deref() == Some(&chat) {
                                self.loading_older = false;
                                self.history_complete = !more;
                                if !more {
                                    self.status = "No older messages available".into();
                                }
                            }
                        }
                        NativeEvent::Stickers {
                            saved,
                            packs,
                            recent,
                        } => {
                            let changed = saved != self.favorite_stickers
                                || packs != self.sticker_packs
                                || recent != self.recent_stickers;
                            let packs_changed = packs != self.sticker_packs;
                            self.favorite_stickers = saved;
                            self.sticker_packs = packs;
                            self.recent_stickers = recent;
                            if changed {
                                self.refresh_sticker_picker(&sender);
                            }
                            if !packs_changed {
                                continue;
                            }
                            self.sticker_emojis.clear();
                            for pack in &self.sticker_packs {
                                for sticker in &pack.stickers {
                                    if let Ok(bytes) = std::fs::read(sticker) {
                                        let emojis = crate::sticker_meta::emojis(&bytes);
                                        if !emojis.is_empty() {
                                            self.sticker_emojis.insert(sticker.clone(), emojis);
                                        }
                                    }
                                }
                            }
                        }
                        NativeEvent::MessageUpdated(message) => self.message_updated(*message),
                        NativeEvent::Edited { chat, id, success } => self.edited(chat, id, success),
                        NativeEvent::Sent { chat, success } => {
                            if self.voice_send_pending {
                                self.voice_send_pending = false;
                                self.status = if success {
                                    "Voice message sent".into()
                                } else {
                                    "Voice message could not be sent".into()
                                };
                            } else {
                                self.sent(chat, success);
                            }
                        }
                        NativeEvent::AttachmentCompleted {
                            chat,
                            path,
                            success,
                        } => self.attachment_completed(chat, path, success),
                        NativeEvent::Media {
                            chat,
                            message,
                            result,
                        } => self.media_completed(&chat, &message, result),
                        NativeEvent::MessageDeleted { chat, id } => {
                            self.message_deleted(&chat, &id)
                        }
                        NativeEvent::Incoming { chat, message } => {
                            if self.settings.auto_download
                                && should_auto_download(message.content.media())
                                && let Some(backend) = &self.backend
                            {
                                backend.send(crate::backend::Command::Download {
                                    chat: chat.clone(),
                                    message: message.id.clone(),
                                });
                                if let Some(media) = self
                                    .message_snapshots
                                    .get_mut(&message.id)
                                    .and_then(|known| known.content.media_mut())
                                {
                                    media.state = crate::model::MediaState::Downloading;
                                }
                            }
                            let known = self.chat_snapshots.iter().find(|known| known.id == chat);
                            if notification_should_show(
                                self.settings.notifications,
                                self.window.is_active(),
                                self.active_chat.as_deref(),
                                &chat,
                                known,
                            ) {
                                let title = known.map_or_else(
                                    || {
                                        sender_label(
                                            message.sender_name.as_deref(),
                                            &message.sender,
                                        )
                                    },
                                    |known| known.name.clone(),
                                );
                                let body = if !self.settings.notification_previews {
                                    "New message".to_owned()
                                } else if known.is_some_and(crate::model::Chat::is_group) {
                                    format!(
                                        "{}: {}",
                                        sender_label(
                                            message.sender_name.as_deref(),
                                            &message.sender
                                        ),
                                        message.summary()
                                    )
                                } else {
                                    message.summary()
                                };
                                if let Err(error) = self.notifications.show(
                                    &chat,
                                    &title,
                                    &body,
                                    self.avatars.get(&chat).map(std::path::PathBuf::as_path),
                                ) {
                                    log::warn!("could not show a notification: {error}");
                                }
                            }
                        }
                        NativeEvent::Typing {
                            chat,
                            sender: typing_sender,
                            composing,
                        } => {
                            if composing {
                                let name = self
                                    .chat_snapshots
                                    .iter()
                                    .find(|known| known.id == typing_sender)
                                    .map(|known| known.name.clone())
                                    .unwrap_or_else(|| "Someone".into());
                                self.typing.insert(chat.clone(), name);
                                self.typing_until.insert(
                                    chat.clone(),
                                    std::time::Instant::now() + std::time::Duration::from_secs(5),
                                );
                                let input = sender.clone();
                                gtk::glib::timeout_add_local_once(
                                    std::time::Duration::from_secs(5),
                                    move || {
                                        input.input(Input::ClearTyping(chat));
                                    },
                                );
                            } else {
                                self.typing.remove(&chat);
                                self.typing_until.remove(&chat);
                            }
                        }
                        NativeEvent::Presence {
                            id,
                            online,
                            last_seen,
                        } => {
                            self.presence.insert(id, (online, last_seen));
                        }
                        NativeEvent::Avatar { id, path } => {
                            if let Some(path) = path {
                                self.avatars.insert(id, path);
                            } else {
                                self.avatars.remove(&id);
                            }
                            self.chats_dirty = true;
                        }
                        NativeEvent::ReceiptsPrivacy { disabled } => {
                            self.account_receipts_off = disabled
                        }
                        NativeEvent::ContactReady { id, name } => {
                            let display_name = name
                                .filter(|name| !name.trim().is_empty())
                                .unwrap_or_else(|| crate::util::phone(&id));
                            self.apply_chat_changes(vec![ChatChange::Update(
                                crate::model::Chat::new(id.clone(), display_name),
                            )]);
                            self.status = "Contact is on WhatsApp".into();
                            sender.input(Input::OpenChatId(id));
                        }
                        NativeEvent::Info(message) => {
                            self.status = message.clone();
                            self.toast(&message);
                        }
                        NativeEvent::Error(error) => {
                            self.loading_older = false;
                            log::warn!("native backend operation failed");
                            let feedback = sanitized_error_feedback(&error);
                            self.status = feedback.into();
                            self.toast(feedback);
                        }
                    }
                }
                if self.chats_dirty && self.chat_ids.is_empty() {
                    self.flush_chats();
                } else if self.chats_dirty && !self.chats_flush_scheduled {
                    // ponytail: fixed 200 ms coalescing window; make it adaptive if
                    // very large archives still stutter during sync.
                    self.chats_flush_scheduled = true;
                    let flush = sender.clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(200),
                        move || flush.input(Input::FlushChats),
                    );
                }
                self.poll_theme_catalog();
                #[cfg(feature = "demo")]
                self.audit_synthetic_commands();
            }
            Input::SelectChat(position) => {
                let Some(chat) = self.chat_ids.get(position as usize).cloned() else {
                    return;
                };
                if self
                    .split_view
                    .as_ref()
                    .is_some_and(adw::OverlaySplitView::is_collapsed)
                {
                    self.sidebar_visible = false;
                }
                if self.active_chat.as_deref() != Some(&chat) {
                    self.cancel_portal_requests();
                    self.media.stop_playback();
                    self.playing_audio = None;
                    self.waveform_queue.clear();
                    if let Some(cancel) = self.waveform_cancel.take() {
                        cancel.store(true, std::sync::atomic::Ordering::Release);
                    }
                    self.audio_waveforms.clear();
                    self.waveform_attempted.clear();
                    self.audio_errors.clear();
                    self.audio_registry.borrow_mut().clear();
                }
                self.notifications.clear_chat(&chat);
                if let Some(previous) = self.active_chat.as_deref() {
                    if self
                        .editing
                        .as_ref()
                        .is_some_and(|(editing_chat, _)| editing_chat == previous)
                    {
                        self.composer.set_draft(
                            previous,
                            self.drafts.get(previous).cloned().unwrap_or_default(),
                        );
                    }
                    self.composer.cancel_context(previous);
                }
                self.chat_projection.select(chat.clone());
                self.active_chat = Some(chat.clone());
                self.reply_to = None;
                self.editing = None;
                self.pending_edit = None;
                self.draft = self.composer.draft(&chat).to_owned();
                self.composer_buffer.set_text(&self.draft);
                self.messages.clear();
                self.message_target = None;
                self.history_complete = false;
                self.loading_older = false;
                self.message_ids.clear();
                self.message_snapshots.clear();
                self.editable_messages.clear();
                self.transcript.clear();
                self.selected_voice = None;
                self.selected_voice_message = None;
                self.page_title = self
                    .chat_snapshots
                    .iter()
                    .find(|known| known.id == chat)
                    .map(|known| known.name.clone())
                    .unwrap_or_else(|| "Conversation".into());
                self.status = "Loading messages".into();
                if let Some(backend) = &self.backend {
                    backend.send(crate::backend::Command::MarkRead {
                        chat: chat.clone(),
                        receipts: self.settings.send_read_receipts && !self.account_receipts_off,
                    });
                    backend.send(crate::backend::Command::LoadChat { chat, before: None });
                }
                self.focus_composer();
                #[cfg(feature = "demo")]
                if synthetic_e2e_enabled()
                    && let Some(events) = &self.synthetic_events
                {
                    let chat = "synthetic-contact@s.whatsapp.net".to_owned();
                    let mut message = synthetic_message();
                    message.chat.clone_from(&chat);
                    let mut attachment = synthetic_attachment_message();
                    attachment.chat.clone_from(&chat);
                    let filler = std::env::var("ZAPTIDE_NATIVE_SYNTHETIC_MESSAGES")
                        .ok()
                        .and_then(|count| count.parse::<usize>().ok())
                        .unwrap_or(0);
                    let mut messages = (0..filler)
                        .map(|index| {
                            let mut filler = synthetic_message();
                            filler.id = format!("synthetic-filler-{index}");
                            filler.chat.clone_from(&chat);
                            filler.from_me = index % 3 == 0;
                            filler.timestamp = 1_699_990_000 + index as i64 * 90;
                            filler.content = crate::model::Content::text(format!(
                                "Synthetic message {index} with enough words to wrap on narrow windows"
                            ));
                            filler
                        })
                        .collect::<Vec<_>>();
                    let mut voice = synthetic_audio_message(true);
                    voice.chat.clone_from(&chat);
                    let mut audio = synthetic_audio_message(false);
                    audio.chat.clone_from(&chat);
                    messages.extend([message, attachment, voice, audio]);
                    let _ = events.send(crate::backend::Event::Messages {
                        chat,
                        messages,
                        older: false,
                        complete: false,
                    });
                    crate::backend::Wake::wake(&self.notifier);
                    self.audit_synthetic_commands();
                }
            }
            Input::HighlightChat(position) => {
                if let Some(chat) = self.chat_ids.get(position as usize) {
                    self.chat_projection.select(chat.clone());
                }
            }
            Input::LoadOlder => {
                let Some(chat) = self.active_chat.clone() else {
                    return;
                };
                if self.loading_older {
                    return;
                }
                let Some(backend) = &self.backend else {
                    self.status = "Backend unavailable".into();
                    return;
                };
                self.loading_older = true;
                if self.history_complete {
                    backend.send(crate::backend::Command::FetchOlder(chat.clone()));
                } else if let Some(oldest) = self
                    .message_snapshots
                    .values()
                    .filter(|message| message.chat == chat)
                    .min_by_key(|message| (message.timestamp, message.id.as_str()))
                {
                    backend.send(crate::backend::Command::LoadChat {
                        chat: chat.clone(),
                        before: Some((oldest.timestamp, oldest.id.clone())),
                    });
                } else {
                    self.loading_older = false;
                }
                #[cfg(feature = "demo")]
                if synthetic_e2e_enabled()
                    && let Some(events) = &self.synthetic_events
                    && !self.history_complete
                {
                    let older = synthetic_older_message(&chat);
                    let _ = events.send(crate::backend::Event::Messages {
                        chat,
                        messages: vec![older],
                        older: true,
                        complete: false,
                    });
                    crate::backend::Wake::wake(&self.notifier);
                }
            }
            Input::OpenChatId(chat) => {
                self.window.present();
                let archived = self
                    .chat_snapshots
                    .iter()
                    .any(|known| known.id == chat && known.archived);
                self.reset_chat_filters(archived);
                self.sync_chat_projection();
                if let Some(position) = self.chat_ids.iter().position(|id| id == &chat) {
                    sender.input(Input::SelectChat(position as u32));
                }
            }
            Input::SearchChats(query) => {
                self.chat_projection.set_query(query);
                self.sync_chat_projection();
            }
            Input::SetUnreadFilter(enabled) => {
                self.chat_filters.unread_only = enabled;
                self.chat_projection.set_filters(self.chat_filters);
                self.sync_chat_projection();
            }
            Input::SetPinnedFilter(enabled) => {
                self.chat_filters.pinned_only = enabled;
                self.chat_projection.set_filters(self.chat_filters);
                self.sync_chat_projection();
            }
            Input::SetArchivedFilter(enabled) => {
                self.chat_filters.archive = if enabled {
                    crate::native_chat_list::ArchiveFilter::Only
                } else {
                    crate::native_chat_list::ArchiveFilter::Exclude
                };
                self.chat_projection.set_filters(self.chat_filters);
                self.sync_chat_projection();
            }
            Input::SetMutedFilter(enabled) => {
                self.chat_filters.muted = if enabled {
                    crate::native_chat_list::MutedFilter::Only
                } else {
                    crate::native_chat_list::MutedFilter::All
                };
                self.chat_projection.set_filters(self.chat_filters);
                self.sync_chat_projection();
            }
            Input::SetChatKindFilter(kind) => {
                self.chat_filters.private_only = kind == 1;
                self.chat_filters.groups_only = kind == 2;
                self.chat_projection.set_filters(self.chat_filters);
                self.sync_chat_projection();
            }
            Input::ToggleSelectedPin => {
                if let Some(chat) = self.selected_chat().cloned()
                    && let Some(backend) = &self.backend
                {
                    backend.send(crate::backend::Command::SetPinned(chat.id, !chat.pinned));
                }
            }
            Input::ToggleSelectedArchive => {
                if let Some(chat) = self.selected_chat().cloned()
                    && let Some(backend) = &self.backend
                {
                    backend.send(crate::backend::Command::SetArchived(
                        chat.id,
                        !chat.archived,
                    ));
                }
            }
            Input::ToggleSelectedMute => {
                if let Some(chat) = self.selected_chat().cloned()
                    && let Some(backend) = &self.backend
                {
                    let muted = chat.muted(crate::util::now());
                    backend.send(crate::backend::Command::SetMuted(
                        chat.id,
                        (!muted).then_some(i64::MAX),
                    ));
                }
            }
            Input::Reconnect => {
                if let Some(backend) = &self.backend {
                    backend.send(crate::backend::Command::Reconnect);
                    self.status = "Reconnecting to WhatsApp".into();
                }
            }
            Input::UnlinkConfirmed => {
                if let Some(backend) = &self.backend {
                    backend.send(crate::backend::Command::Unlink);
                    self.status = "Unlinking this computer".into();
                } else {
                    self.status = "Backend unavailable".into();
                }
            }
            Input::ShowChatInfo => {
                if let Some(chat) = self
                    .active_chat
                    .as_ref()
                    .and_then(|id| self.chat_snapshots.iter().find(|chat| &chat.id == id))
                {
                    show_chat_info_dialog(&self.window, chat, self.contacts.get(&chat.id));
                }
            }
            Input::TogglePhoneLinking => self.phone_linking = !self.phone_linking,
            Input::FlushChats => {
                self.chats_flush_scheduled = false;
                self.flush_chats();
            }
            Input::PairWithPhone(phone) => {
                let Some(digits) = normalized_phone(&phone) else {
                    self.status = "Enter valid phone number with country code".into();
                    self.toast("Enter valid phone number with country code");
                    return;
                };
                if let Some(backend) = &self.backend {
                    backend.send(crate::backend::Command::PairWithPhone(digits));
                    self.status = "Requesting a pairing code from WhatsApp".into();
                } else {
                    self.status = "Backend unavailable".into();
                }
            }
            Input::NewContact { phone, name } => {
                let Some(digits) = normalized_phone(&phone) else {
                    self.status = "Enter valid phone number with country code".into();
                    self.toast("Enter valid phone number with country code");
                    return;
                };
                if let Some(backend) = &self.backend {
                    let name = name.filter(|name| !name.trim().is_empty());
                    backend.send(crate::backend::Command::NewContact {
                        phone: digits,
                        first_name: name.clone(),
                        full_name: name,
                        to_phone: self.settings.save_contacts_to_phone,
                    });
                    self.status = "Checking contact on WhatsApp".into();
                } else {
                    self.status = "Backend unavailable".into();
                }
            }
            Input::ShowMessageMenu { id, x, y } => self.show_message_menu(id, x, y, &sender),
            Input::SelectMessage(position) => {
                if self.active_chat.is_none() {
                    return;
                }
                let Some(message) = self.message_ids.get(position as usize) else {
                    return;
                };
                self.selected_voice_message = self
                    .message_snapshots
                    .get(message)
                    .and_then(|message| self.project_voice(message).map(|_| message.id.clone()));
                self.selected_voice = self
                    .message_snapshots
                    .get(message)
                    .and_then(|message| self.project_voice(message));
            }
            Input::OpenQuoted => {
                let Some(message) = self.selected_message().cloned() else {
                    return;
                };
                let Some(quoted_id) = message.quoted.map(|quoted| quoted.id) else {
                    return;
                };
                if self.message_ids.contains(&quoted_id) {
                    self.scroll_message_into_view(&quoted_id);
                } else if let Some(backend) = &self.backend {
                    self.pending_quote_navigation = Some((message.chat.clone(), quoted_id.clone()));
                    backend.send(crate::backend::Command::LoadUntil {
                        chat: message.chat,
                        id: quoted_id,
                        before: (message.timestamp, message.id),
                    });
                    self.status = "Loading quoted message".into();
                }
            }
            Input::ReplySelected => {
                let Some((chat, message)) =
                    self.active_chat.clone().zip(self.selected_message_id())
                else {
                    return;
                };
                self.composer.begin_reply(&chat, message.clone());
                self.reply_to = Some((chat, message));
                self.editing = None;
                self.status = "Replying to selected message".into();
                self.focus_composer();
            }
            Input::EditSelected => {
                if self.pending_edit.is_some() {
                    self.status = "Saving edit".into();
                    return;
                }
                let Some((chat, message)) =
                    self.active_chat.clone().zip(self.selected_message_id())
                else {
                    return;
                };
                if let Some(text) = self.editable_messages.get(&message).cloned() {
                    self.editing = Some((chat.clone(), message.clone()));
                    self.reply_to = None;
                    self.draft = text;
                    self.composer.begin_edit(&chat, message, self.draft.clone());
                    self.composer_buffer.set_text(&self.draft);
                    self.status = "Editing selected message".into();
                    self.focus_composer();
                } else {
                    self.reply_to = Some((chat.clone(), message.clone()));
                    self.composer.begin_reply(&chat, message);
                    self.composer.begin_reply(
                        &self.reply_to.as_ref().unwrap().0,
                        self.reply_to.as_ref().unwrap().1.clone(),
                    );
                    self.status = "Replying to selected message".into();
                }
            }
            Input::CancelReply => {
                if let Some((chat, _)) = self.reply_to.take() {
                    self.composer.cancel_context(&chat);
                }
            }
            Input::CancelEdit => {
                if self.editing.take().is_some()
                    && let Some(chat) = &self.active_chat
                {
                    self.draft = self.drafts.get(chat).cloned().unwrap_or_default();
                    self.composer.cancel_context(chat);
                    self.composer.set_draft(chat, self.draft.clone());
                    self.composer_buffer.set_text(&self.draft);
                }
                self.pending_edit = None;
            }
            Input::DraftChanged(text) => {
                self.draft = text;
                if let Some(chat) = &self.active_chat {
                    self.composer.set_draft(chat, self.draft.clone());
                    if self.editing.is_none() {
                        self.drafts.insert(chat.clone(), self.draft.clone());
                    }
                    if self.settings.send_typing
                        && let Some(backend) = &self.backend
                    {
                        backend.send(crate::backend::Command::Composing {
                            chat: chat.clone(),
                            composing: true,
                        });
                    }
                    self.composing_until.insert(
                        chat.clone(),
                        std::time::Instant::now() + std::time::Duration::from_secs(3),
                    );
                    let input = sender.clone();
                    let chat = chat.clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_secs(3),
                        move || {
                            input.input(Input::StopComposing(chat));
                        },
                    );
                }
            }
            Input::ClearTyping(chat) => {
                if self
                    .typing_until
                    .get(&chat)
                    .is_some_and(|until| *until <= std::time::Instant::now())
                {
                    self.typing.remove(&chat);
                    self.typing_until.remove(&chat);
                }
            }
            Input::StopComposing(chat) => {
                if self
                    .composing_until
                    .get(&chat)
                    .is_some_and(|until| *until <= std::time::Instant::now())
                {
                    self.composing_until.remove(&chat);
                    if self.settings.send_typing
                        && let Some(backend) = &self.backend
                    {
                        backend.send(crate::backend::Command::Composing {
                            chat,
                            composing: false,
                        });
                    }
                }
            }
            Input::InsertEmoji(emoji) => {
                let mut end = self.composer_buffer.end_iter();
                self.composer_buffer.insert(&mut end, &emoji);
            }
            Input::ShowStickerPicker => {
                if let Some(backend) = &self.backend {
                    backend.send(crate::backend::Command::RecentStickers);
                }
                let Some(anchor) = &self.sticker_button else {
                    return;
                };
                // Anchored to the button, the popover flips above it near screen edges.
                let popover = gtk::Popover::builder()
                    .position(gtk::PositionType::Top)
                    .css_classes(["zaptide-sticker-picker"])
                    .build();
                popover.connect_closed(|popover| {
                    let popover = popover.clone();
                    gtk::glib::idle_add_local_once(move || popover.unparent());
                });
                let (content, stack) = sticker_picker_content(
                    &self.sticker_packs,
                    &self.favorite_stickers,
                    &self.recent_stickers,
                    None,
                    &sender,
                );
                popover.set_child(Some(&content));
                popover.set_parent(anchor);
                popover.popup();
                self.sticker_picker = Some((popover, stack));
            }
            Input::SendSticker(path) => {
                if let Some((popover, _)) = self.sticker_picker.take() {
                    popover.popdown();
                }
                if let (Some(chat), Some(backend)) = (&self.active_chat, &self.backend) {
                    backend.send(crate::backend::Command::SendSticker {
                        chat: chat.clone(),
                        path,
                    });
                }
            }
            Input::InsertMention => {
                let participants = self
                    .active_chat
                    .as_deref()
                    .and_then(|chat| self.chat_snapshots.iter().find(|known| known.id == chat))
                    .map(|chat| chat.participants.clone())
                    .unwrap_or_default();
                if participants.is_empty() {
                    return;
                }
                let dialog = gtk::Window::builder()
                    .title("Mention participant")
                    .transient_for(&self.window)
                    .modal(true)
                    .default_width(320)
                    .build();
                let list = gtk::Box::new(gtk::Orientation::Vertical, 6);
                for (index, participant) in participants.into_iter().enumerate() {
                    let button =
                        gtk::Button::with_label(&self.participant_label(&participant, index));
                    let input_sender = sender.clone();
                    let close = dialog.clone();
                    button.connect_clicked(move |_| {
                        input_sender.input(Input::InsertMentionId(participant.clone()));
                        close.close();
                    });
                    list.append(&button);
                }
                let scroller = gtk::ScrolledWindow::builder()
                    .min_content_height(240)
                    .max_content_height(420)
                    .child(&list)
                    .build();
                dialog.set_child(Some(&scroller));
                dialog.present();
            }
            Input::InsertMentionId(participant) => {
                let known = self
                    .active_chat
                    .as_deref()
                    .and_then(|chat| self.chat_snapshots.iter().find(|known| known.id == chat))
                    .is_some_and(|chat| chat.participants.contains(&participant));
                if known {
                    let token = participant.split('@').next().unwrap_or_default();
                    let mut end = self.composer_buffer.end_iter();
                    self.composer_buffer.insert(&mut end, &format!("@{token} "));
                }
            }
            Input::PasteClipboardImage => self.paste_clipboard_image(&sender),
            Input::AttachDropped(paths) => {
                let Some(chat) = self.active_chat.clone().filter(|_| self.can_attach()) else {
                    self.status = "Choose a writable chat before attaching files".into();
                    return;
                };
                if paths.is_empty() {
                    return;
                }
                if self.pending_clipboard_images.contains_key(&chat) {
                    self.status = "Clear the staged clipboard image before adding files".into();
                    return;
                }
                self.composer
                    .stage_attachment_caption(&chat, self.draft.clone());
                self.pending_attachments
                    .entry(chat)
                    .or_default()
                    .extend(paths);
                self.status = attachment_summary(self.pending_attachment_count());
            }
            Input::ClipboardImageReady { chat, pixels } => {
                self.stage_clipboard_image(chat, pixels);
            }
            Input::OpenSelectedUri => self.open_selected_uri(&sender),
            Input::PortalUriFinished(success) => {
                self.status = if success {
                    "Link opened".into()
                } else {
                    "Could not open link".into()
                };
            }
            Input::SaveSelectedAttachment => self.save_selected_attachment(&sender),
            Input::SaveAttachmentFinished(success) => {
                self.status = if success {
                    "Attachment saved".into()
                } else {
                    "Could not save attachment".into()
                };
                if !success {
                    self.toast("Could not save attachment");
                }
            }
            Input::PortalActionFinished(error) => {
                self.status = error;
                self.toast("Clipboard or portal action failed");
            }
            Input::PickAttachments => {
                let Some(chat) = self.active_chat.clone().filter(|_| self.can_attach()) else {
                    return;
                };
                if !self.portal_requests.borrow().is_empty() {
                    self.status = "Finish the active portal action first".into();
                    return;
                }
                if self.pending_clipboard_images.contains_key(&chat) {
                    self.status = "Clear the staged clipboard image before adding files".into();
                    return;
                }
                let input = sender.clone();
                let requests = self.portal_requests.clone();
                let request_id = std::rc::Rc::new(std::cell::Cell::new(None));
                let callback_request_id = request_id.clone();
                let request =
                    self.portals
                        .open_files(Some(&self.window), "Attach files", move |result| {
                            if let Some(id) = callback_request_id.get() {
                                requests.borrow_mut().remove(&id);
                            }
                            let paths = result
                                .unwrap_or_default()
                                .into_iter()
                                .filter_map(|file| file.path())
                                .collect();
                            input.input(Input::AttachmentsPicked { chat, paths });
                        });
                if let Some(id) = request {
                    request_id.set(Some(id));
                    self.portal_requests.borrow_mut().insert(id);
                }
            }
            Input::AttachmentsPicked { chat, paths } => {
                if paths.is_empty() {
                    return;
                }
                if self.pending_clipboard_images.contains_key(&chat) {
                    self.status = "Clear the staged clipboard image before adding files".into();
                    return;
                }
                self.composer
                    .stage_attachment_caption(&chat, self.composer.draft(&chat).to_owned());
                self.pending_attachments
                    .entry(chat)
                    .or_default()
                    .extend(paths);
            }
            Input::ClearAttachments => {
                if let Some(chat) = &self.active_chat {
                    self.pending_attachments.remove(chat);
                    self.pending_clipboard_images.remove(chat);
                }
            }
            Input::CopyTranscript => self.copy_transcript(),
            Input::CopySelectedText => {
                if let Some(text) = self.selected_message().and_then(message_text) {
                    crate::native_portals::NativePortals::write_clipboard_text(
                        &self.window.clipboard(),
                        &text,
                    );
                    self.toast("Copied");
                }
            }
            Input::ActivateVoice => self.activate_voice(&sender),
            Input::CycleVoiceSpeed => self.cycle_voice_speed(),
            Input::SeekVoice(fraction) => self.seek_voice(fraction, &sender),
            Input::AudioControl { id, intent } => self.audio_control(&id, intent, &sender),
            Input::AudioWaveformReady { chat, id, bars } => {
                self.waveform_busy = false;
                self.waveform_cancel = None;
                if self.active_chat.as_deref() == Some(&chat)
                    && let Some(bars) = bars
                {
                    self.audio_waveforms.insert((chat, id.clone()), bars);
                    self.refresh_message_row(&id);
                }
                self.pump_waveforms(&sender);
            }
            Input::MediaAction(action) => match action {
                crate::native_media::NativeMediaAction::Download { message, .. } => {
                    self.activate_attachment(&message)
                }
                crate::native_media::NativeMediaAction::Open(path) => {
                    if let Some(id) = self
                        .message_snapshots
                        .values()
                        .find(|message| {
                            message
                                .content
                                .media()
                                .and_then(|media| media.path.as_ref())
                                == Some(&path)
                        })
                        .map(|message| message.id.clone())
                    {
                        self.activate_attachment(&id);
                    }
                }
            },
            Input::ActivateSelectedAttachment => {
                if let Some(id) = self.selected_message_id() {
                    self.activate_attachment(&id);
                }
            }
            Input::ReactSelected(emoji) => self.react_selected(emoji),
            Input::ShowForward => {
                let mut chats: Vec<_> = self
                    .chat_snapshots
                    .iter()
                    .filter(|chat| forwardable_chat(chat))
                    .cloned()
                    .collect();
                chats.sort_by_key(|chat| std::cmp::Reverse(chat.last_activity));
                show_forward_dialog(&self.window, &sender, chats);
            }
            Input::ForwardSelected(destination) => self.forward_selected(destination),
            Input::DeleteSelected(everyone) => {
                let dialog = adw::AlertDialog::builder()
                    .heading("Delete message?")
                    .body(if everyone {
                        "Delete this message for everyone?"
                    } else {
                        "Delete this message for you?"
                    })
                    .build();
                dialog.add_response("cancel", "Cancel");
                dialog.add_response("delete", "Delete");
                dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                let input = sender.clone();
                dialog.connect_response(None, move |_, response| {
                    if response == "delete" {
                        input.input(Input::ConfirmDelete(everyone));
                    }
                });
                dialog.present(Some(&self.window));
            }
            Input::ConfirmDelete(everyone) => self.delete_selected(everyone),
            Input::VoteOption(choice) => {
                self.poll_choice = choice;
                self.vote_selected();
            }
            Input::CreatePoll {
                question,
                first,
                second,
            } => self.create_poll(question, first, second),
            Input::Recording(intent) => {
                self.recording_action(intent);
                self.schedule_voice_poll(&sender);
            }
            Input::PollVoice => {
                if let Err(error) = self.media.poll() {
                    self.status = "Audio playback could not continue.".into();
                    if let (Some(chat), Some(id)) = (&self.active_chat, &self.playing_audio) {
                        self.audio_errors.insert((chat.clone(), id.clone()), error);
                    }
                }
                if let Some(id) = self.playing_audio.clone() {
                    if self.media.actually_playing(&id) {
                        self.tell_played(&id);
                    }
                    self.update_audio_row(&id);
                }
                self.refresh_selected_voice();
                self.schedule_voice_poll(&sender);
            }
            Input::SendText(text) => {
                self.draft = text;
                if let Some(chat) = &self.active_chat {
                    self.composer.set_draft(chat, self.draft.clone());
                }
                if self.pending_send.is_some() || self.voice_send_pending {
                    self.status = "Waiting for previous message.".into();
                    return;
                }
                let Some(chat) = self.active_chat.clone() else {
                    return;
                };
                let mentions = self
                    .chat_snapshots
                    .iter()
                    .find(|known| known.id == chat)
                    .map(|known| {
                        crate::native_composer::mention_ids(&self.draft, &known.participants)
                    })
                    .unwrap_or_default();
                let attachment_count = self.pending_attachment_count();
                if self.draft.trim().is_empty() && attachment_count == 0 {
                    return;
                }
                let writable = self
                    .chat_snapshots
                    .iter()
                    .find(|known| known.id == chat)
                    .is_some_and(crate::model::Chat::can_send);
                if !writable {
                    self.status = "This conversation is read-only.".into();
                    return;
                }
                if let Some(backend) = &self.backend {
                    if attachment_count > 0 && self.editing.is_some() {
                        self.status = "Finish editing before sending attachments.".into();
                        return;
                    }
                    if let Some((edit_chat, id)) = self.editing.as_ref()
                        && edit_chat == &chat
                    {
                        if self.pending_edit.is_some() {
                            self.status = "Saving edit".into();
                            return;
                        }
                        self.pending_composer_request = self.composer.submit(&chat);
                        self.pending_edit = Some((chat.clone(), id.clone(), self.draft.clone()));
                        backend.send(crate::backend::Command::EditText {
                            chat,
                            id: id.clone(),
                            text: self.draft.clone(),
                            mentions: mentions.clone(),
                        });
                        self.status = "Saving edit".into();
                        return;
                    }
                    let quote = self
                        .reply_to
                        .as_ref()
                        .filter(|(reply_chat, _)| reply_chat == &chat)
                        .map(|(_, message)| message.clone());
                    self.pending_composer_request = self.composer.submit(&chat);
                    let attachments = self.pending_attachments.remove(&chat).unwrap_or_default();
                    let has_clipboard_image = self.pending_clipboard_images.contains_key(&chat);
                    self.pending_send = Some(PendingSend {
                        chat: chat.clone(),
                        text: self.draft.clone(),
                        reply: quote.clone(),
                        attachments,
                        failed_attachments: Vec::new(),
                        clipboard_image: has_clipboard_image,
                        remaining: attachment_count.max(1),
                        failed: false,
                    });
                    if let Some(pending) = &self.pending_send
                        && !pending.attachments.is_empty()
                    {
                        backend.send(crate::backend::Command::SendFiles {
                            chat,
                            paths: pending.attachments.clone(),
                            caption: caption(&self.draft),
                            quoting: quote,
                            mentions: mentions.clone(),
                        });
                        self.status = "Sending attachments".into();
                    } else if let Some(image) = self.pending_clipboard_images.get(&chat) {
                        backend.send(crate::backend::Command::SendImage {
                            chat,
                            width: image.width,
                            height: image.height,
                            rgba: image.rgba.clone(),
                            caption: caption(&self.draft),
                            quoting: quote,
                            mentions: mentions.clone(),
                        });
                        self.status = "Sending clipboard image".into();
                    } else {
                        backend.send(crate::backend::Command::SendText {
                            chat,
                            text: self.draft.clone(),
                            quoting: quote,
                            mentions,
                        });
                        self.status = "Sending message".into();
                    }
                } else {
                    self.status = "Backend unavailable. Draft kept.".into();
                }
                #[cfg(feature = "demo")]
                self.audit_synthetic_commands();
            }
            Input::Close => {
                self.cancel_portal_requests();
                if self.settings.keep_running_in_background {
                    self.present_quit_confirmation_dialog(sender);
                } else {
                    self.request_shutdown(sender);
                }
            }
            Input::Quit => {
                self.cancel_portal_requests();
                self.request_shutdown(sender);
            }
            Input::ShutdownComplete => {
                self.status = "Shutdown complete".into();
                relm4::main_application().quit();
            }
        }
    }
}

/// Moves a list to its end once rows are laid out. `ListView::scroll_to`
/// leaves blank space with rows of varying height, and the height estimate
/// only settles after the newly shown rows are measured, hence the second pass.
fn scroll_to_end(view: &gtk::ListView) {
    let Some(adjustment) = view.vadjustment() else {
        return;
    };
    let end = move || adjustment.set_value(adjustment.upper() - adjustment.page_size());
    let settle = end.clone();
    gtk::glib::idle_add_local_once(end);
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(150), settle);
}

/// Start, removed count, and inserted count of the span where `new` differs
/// from an old list of `old_len` items, keeping their common prefix and suffix.
fn changed_span<T>(
    old_len: usize,
    new: &[T],
    same: impl Fn(usize, &T) -> bool,
) -> (usize, usize, usize) {
    let prefix = new
        .iter()
        .enumerate()
        .take_while(|(position, row)| *position < old_len && same(*position, row))
        .count();
    let suffix = new[prefix..]
        .iter()
        .rev()
        .enumerate()
        .take_while(|(offset, row)| *offset < old_len - prefix && same(old_len - 1 - offset, row))
        .count();
    (
        prefix,
        old_len - prefix - suffix,
        new.len() - prefix - suffix,
    )
}

/// A paper-plane send icon filled with the button's text color, so it follows
/// the theme like a symbolic icon.
fn paper_plane_icon() -> gtk::DrawingArea {
    let icon = gtk::DrawingArea::builder()
        .content_width(16)
        .content_height(16)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    icon.set_draw_func(|area, cairo, _, _| {
        let color = area.color();
        cairo.set_source_rgba(
            color.red().into(),
            color.green().into(),
            color.blue().into(),
            color.alpha().into(),
        );
        for (index, (x, y)) in [
            (1.2, 1.6),
            (15.2, 8.0),
            (1.2, 14.4),
            (3.1, 8.8),
            (9.5, 8.0),
            (3.1, 7.2),
        ]
        .into_iter()
        .enumerate()
        {
            if index == 0 {
                cairo.move_to(x, y);
            } else {
                cairo.line_to(x, y);
            }
        }
        cairo.close_path();
        let _ = cairo.fill();
    });
    icon
}

fn chat_row(chat: crate::model::Chat, avatar: Option<std::path::PathBuf>) -> ChatRow {
    let unread = (chat.unread != 0).then(|| chat.unread.to_string());
    let muted = chat.muted(crate::util::now());
    let delivery = chat
        .last
        .as_ref()
        .filter(|last| last.from_me)
        .map_or(crate::model::Delivery::None, |last| last.status);
    let preview = chat
        .last
        .as_ref()
        .map(|last| last.summary.clone())
        .unwrap_or_else(|| "No messages yet".into());
    ChatRow {
        id: chat.id,
        last_activity: chat.last_activity,
        name: chat.name.clone(),
        preview,
        unread,
        avatar,
        pinned: chat.pinned,
        muted,
        quiet: muted || chat.archived,
        delivery,
    }
}

fn apply_theme(settings: &crate::settings::Settings, theme_provider: &gtk::CssProvider) {
    let palette = settings.cached_palette();
    let scheme = palette.map_or_else(
        || match settings.theme {
            crate::settings::ThemeChoice::Dark => adw::ColorScheme::ForceDark,
            crate::settings::ThemeChoice::Light => adw::ColorScheme::ForceLight,
            crate::settings::ThemeChoice::System => adw::ColorScheme::Default,
        },
        |palette| {
            if palette.dark {
                adw::ColorScheme::ForceDark
            } else {
                adw::ColorScheme::ForceLight
            }
        },
    );
    adw::StyleManager::default().set_color_scheme(scheme);
    let mut css = "@define-color zaptide_bubble_in @card_bg_color;\n\
                   @define-color zaptide_bubble_out color-mix(in srgb, @success_bg_color 60%, @card_bg_color);\n\
                   @define-color zaptide_bubble_in_text @window_fg_color;\n\
                   @define-color zaptide_bubble_out_text @window_fg_color;\n".to_owned();
    if let Some(palette) = palette {
        css.push_str(&crate::native_theme::css_for_palette(&palette));
    }
    css.push_str(
        ".zaptide-transcript, .zaptide-transcript textview, .zaptide-transcript text { background: none; }\n\
         .zaptide-message-row { padding: 2px 6px; }\n\
         .zaptide-bubble { padding: 8px 11px; border-radius: 13px; }\n\
         .zaptide-bubble.incoming { background-color: @zaptide_bubble_in; color: @zaptide_bubble_in_text; }\n\
         .zaptide-bubble.outgoing { background-color: @zaptide_bubble_out; color: @zaptide_bubble_out_text; }\n\
         .zaptide-message-item:focus-visible .zaptide-bubble { outline: 2px solid @accent_color; outline-offset: 2px; }\n\
         .zaptide-media-card { padding: 8px 12px; margin-top: 4px; background-color: color-mix(in srgb, currentColor 8%, transparent); }\n\
         .zaptide-sender-blue { color: @blue_3; }\n\
         .zaptide-sender-green { color: @green_4; }\n\
         .zaptide-sender-yellow { color: @yellow_5; }\n\
         .zaptide-sender-orange { color: @orange_4; }\n\
         .zaptide-sender-red { color: @red_3; }\n\
         .zaptide-sender-purple { color: @purple_3; }\n\
         .zaptide-bubble:hover { box-shadow: inset 0 0 0 1px color-mix(in srgb, currentColor 12%, transparent); }\n\
         .zaptide-message-timestamp { min-width: 36px; font-weight: normal; }\n\
         .zaptide-filter-pill { border-radius: 9999px; padding: 4px 12px; min-height: 22px; background-color: alpha(currentColor, 0.08); box-shadow: none; }\n\
         .zaptide-filter-pill:hover { background-color: alpha(currentColor, 0.11); }\n\
         .zaptide-filter-pill:checked { background-color: alpha(@accent_color, 0.18); color: @accent_color; font-weight: bold; }\n\
         .zaptide-filter-pill:checked:hover { background-color: alpha(@accent_color, 0.24); }\n\
         .zaptide-unread-pill { font-weight: bold; font-size: 0.8em; border-radius: 9999px; min-width: 1.4em; padding: 2px 6px; color: @accent_fg_color; background-color: @accent_bg_color; }\n\
         .zaptide-delivery { opacity: 0.6; }\n\
         .zaptide-delivery.read { opacity: 1; color: #53bdeb; }\n\
         .zaptide-delivery-failed { color: @error_color; }\n\
         .zaptide-sticker { border-radius: 12px; padding: 4px; }\n\
         .zaptide-audio-seek trough, .zaptide-audio-seek highlight, .zaptide-audio-seek slider { background: none; border-color: transparent; box-shadow: none; outline-color: transparent; }\n\
          .zaptide-audio-seek:focus-visible trough { outline: 2px solid alpha(@accent_color, 0.5); outline-offset: 2px; }\n\
          .zaptide-audio-speed.compact { font-size: 0.85em; }\n\
          .zaptide-reaction { font-size: 1.4em; min-width: 40px; min-height: 40px; padding: 0; }\n\
         .zaptide-reaction.chosen { background-color: alpha(@accent_bg_color, 0.25); }\n\
         .zaptide-sticker-tab { border-radius: 8px; min-width: 36px; min-height: 36px; padding: 2px; }\n\
         .zaptide-sticker-tab:checked { background-color: alpha(currentColor, 0.12); }\n\
         .zaptide-sticker-picker > contents { padding: 0; }\n\
         .zaptide-unread-pill.muted { color: @window_fg_color; background-color: alpha(currentColor, 0.18); }\n\
         .zaptide-composer { border-radius: 18px; background-color: color-mix(in srgb, currentColor 8%, transparent); }\n\
         .zaptide-composer textview, .zaptide-composer text { background: none; }\n\
         .zaptide-qr { border-radius: 12px; }\n\
         .zaptide-quote { border-left: 2px solid @accent_bg_color; padding-left: 6px; opacity: 0.7; }\n",
    );
    theme_provider.load_from_string(&css);
}

fn should_auto_download(media: Option<&crate::model::Media>) -> bool {
    const MAX_AUTO_DOWNLOAD: u64 = 64 * 1024 * 1024;
    media.is_some_and(|media| {
        media.path.is_none()
            && media.size <= MAX_AUTO_DOWNLOAD
            && matches!(media.state, crate::model::MediaState::Idle)
    })
}

fn should_send_on_enter(enter_sends: bool, control: bool, shift: bool) -> bool {
    !shift && (enter_sends || control)
}

fn normalized_phone(input: &str) -> Option<String> {
    let digits: String = input.chars().filter(char::is_ascii_digit).collect();
    (7..=15).contains(&digits.len()).then_some(digits)
}

/// Notify unless the chat is on screen, muted, archived, or locked.
fn notification_should_show(
    enabled: bool,
    window_active: bool,
    active_chat: Option<&str>,
    chat: &str,
    known: Option<&crate::model::Chat>,
) -> bool {
    enabled
        && !(window_active && active_chat == Some(chat))
        && known.is_none_or(|known| {
            !known.muted(crate::util::now()) && !known.archived && !known.locked
        })
}

fn connect_widget_changes(widget: &gtk::Widget, sender: &ComponentSender<NativeApplication>) {
    let input = sender.clone();
    widget.connect_notify_local(None, move |_, _| input.input(Input::ApplyPreferences));
    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        connect_widget_changes(&current, sender);
    }
}

fn conversation_prefixes(timestamps: &[i64], unread: usize) -> Vec<String> {
    let unread_at = timestamps.len().saturating_sub(unread);
    let mut previous_day = None;
    timestamps
        .iter()
        .enumerate()
        .map(|(index, timestamp)| {
            let day = crate::util::day_label(*timestamp);
            let mut prefix = String::new();
            if previous_day.as_deref() != Some(day.as_str()) {
                prefix.push_str(&format!("── {day} ──\n"));
                previous_day = Some(day);
            }
            if unread > 0 && index == unread_at {
                prefix.push_str("── Unread messages ──\n");
            }
            prefix
        })
        .collect()
}

/// Best label for a sender: the saved or pushed name, else the formatted
/// phone number, so unnamed contacts never read as a bare "Contact".
fn sender_label(name: Option<&str>, id: &str) -> String {
    if let Some(name) = name.filter(|name| !name.is_empty()) {
        return name.to_owned();
    }
    crate::model::phone_of(id)
        .map(crate::util::phone)
        .unwrap_or_else(|| id.to_owned())
}

fn delivery_label(delivery: crate::model::Delivery) -> &'static str {
    match delivery {
        crate::model::Delivery::Pending => " · Queued",
        crate::model::Delivery::Sent => " · Sent",
        crate::model::Delivery::Delivered => " · Delivered",
        crate::model::Delivery::Read => " · Read",
        crate::model::Delivery::Played => " · Played",
        crate::model::Delivery::Failed => " · Failed",
        crate::model::Delivery::None => "",
    }
}

/// Compact delivery indicator: check glyphs, highlighted once read, or an
/// icon for queued and failed sends.
fn delivery_mark(delivery: crate::model::Delivery) -> (&'static str, Option<&'static str>, bool) {
    match delivery {
        crate::model::Delivery::Pending => ("", Some("document-open-recent-symbolic"), false),
        crate::model::Delivery::Failed => ("", Some("dialog-error-symbolic"), false),
        crate::model::Delivery::Sent => ("✓", None, false),
        crate::model::Delivery::Delivered => ("✓✓", None, false),
        crate::model::Delivery::Read | crate::model::Delivery::Played => ("✓✓", None, true),
        crate::model::Delivery::None => ("", None, false),
    }
}

/// WhatsApp-style check marks, drawn so the double tick overlaps like the
/// original instead of depending on the font's check glyph.
fn delivery_ticks() -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(16)
        .content_height(11)
        .valign(gtk::Align::Center)
        .css_classes(["zaptide-delivery"])
        .build();
    area.set_draw_func(|area, cr, _, height| {
        let color = area.color();
        let bottom = f64::from(height) - 2.0;
        cr.set_source_rgba(
            color.red().into(),
            color.green().into(),
            color.blue().into(),
            color.alpha().into(),
        );
        cr.set_line_width(1.5);
        cr.set_line_cap(gtk::cairo::LineCap::Round);
        cr.set_line_join(gtk::cairo::LineJoin::Round);
        let double = area.has_css_class("double");
        let first = if double { 1.0 } else { 3.0 };
        cr.move_to(first, bottom - 3.5);
        cr.line_to(first + 3.5, bottom);
        cr.line_to(first + 10.0, 1.5);
        if double {
            // The second tick's short stroke hides behind the first one.
            cr.move_to(first + 6.0, bottom - 1.5);
            cr.line_to(first + 7.5, bottom);
            cr.line_to(first + 14.0, 1.5);
        }
        let _ = cr.stroke();
    });
    area
}

fn set_delivery_ticks(area: &gtk::DrawingArea, glyph: &str) {
    area.set_visible(!glyph.is_empty());
    if glyph.chars().count() == 2 {
        area.add_css_class("double");
    } else {
        area.remove_css_class("double");
    }
    area.queue_draw();
}

fn message_row(
    message: crate::model::Message,
    pointer_sender: ComponentSender<NativeApplication>,
    prefix: &str,
    avatar: Option<std::path::PathBuf>,
    show_sender: bool,
    show_timestamp: bool,
    audio_registry: AudioRegistry,
) -> MessageRow {
    const SENDER_CLASSES: [&str; 6] = [
        "zaptide-sender-blue",
        "zaptide-sender-green",
        "zaptide-sender-yellow",
        "zaptide-sender-orange",
        "zaptide-sender-red",
        "zaptide-sender-purple",
    ];
    let sender: std::borrow::Cow<'_, str> = if message.from_me {
        "You".into()
    } else {
        sender_label(message.sender_name.as_deref(), &message.sender).into()
    };
    let separator = prefix
        .lines()
        .map(|line| line.trim_matches(|c| c == '─' || c == ' '))
        .collect::<Vec<_>>()
        .join("\n");
    let body = match &message.content {
        crate::model::Content::Text { text, .. } => text.clone(),
        crate::model::Content::Image { caption, .. }
        | crate::model::Content::Video { caption, .. }
        | crate::model::Content::Document { caption, .. } => caption.clone().unwrap_or_default(),
        _ => String::new(),
    };
    let quote = message
        .quoted
        .as_ref()
        .map(|quoted| {
            format!(
                "{}: {}",
                sender_label(quoted.sender_name.as_deref(), &quoted.sender),
                quoted.summary
            )
        })
        .unwrap_or_default();
    let clock = crate::util::clock(message.timestamp);
    let delivery = delivery_label(message.status);
    let mut footer = Vec::new();
    if message.forwarded {
        footer.push("Forwarded".to_owned());
    }
    if message.edited {
        footer.push("Edited".to_owned());
    }
    let reactions = reaction_summary(&message.reactions);
    if !reactions.is_empty() {
        footer.push(reactions.trim().to_owned());
    }
    footer.push(clock.clone());
    let accessible_label = format!(
        "{prefix}{sender}: {}{}{} · {clock}{delivery}",
        transcript_text(&message),
        if quote.is_empty() {
            String::new()
        } else {
            format!("\n↪ {quote}")
        },
        reactions,
    );
    MessageRow {
        id: message.id.clone(),
        separator,
        sender: sender.into_owned(),
        sender_class: SENDER_CLASSES
            [crate::util::hue(&message.sender) as usize * SENDER_CLASSES.len() / 360],
        avatar,
        quote,
        body,
        footer: footer.join(" · "),
        accessible_label,
        show_sender,
        show_timestamp,
        pointer_sender,
        message,
        audio: None,
        audio_registry,
    }
}

fn message_group_boundaries(messages: &[crate::model::Message]) -> Vec<(bool, bool)> {
    const GROUP_WINDOW_SECONDS: i64 = 5 * 60;
    let same_group = |first: &crate::model::Message, next: &crate::model::Message| {
        first.from_me == next.from_me
            && first.sender == next.sender
            && next.timestamp >= first.timestamp
            && next.timestamp - first.timestamp <= GROUP_WINDOW_SECONDS
            && crate::util::day_label(first.timestamp) == crate::util::day_label(next.timestamp)
    };
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let starts_group = index == 0 || !same_group(&messages[index - 1], message);
            let ends_group =
                index + 1 == messages.len() || !same_group(message, &messages[index + 1]);
            (starts_group, ends_group)
        })
        .collect()
}

fn reaction_summary(reactions: &[crate::model::Reaction]) -> String {
    let mut counts = std::collections::BTreeMap::<String, (usize, bool)>::new();
    for reaction in reactions {
        let entry = counts.entry(reaction.emoji.clone()).or_default();
        entry.0 += 1;
        entry.1 |= reaction.from_me;
    }
    if counts.is_empty() {
        return String::new();
    }
    let summary = counts
        .into_iter()
        .map(|(emoji, (count, from_me))| {
            format!("{emoji} × {count}{}", if from_me { " · You" } else { "" })
        })
        .collect::<Vec<_>>()
        .join("   ");
    format!("\n{summary}")
}

fn transcript_row(message: &crate::model::Message) -> crate::native_transcript::TranscriptRow {
    let sender = if message.from_me {
        "You".to_owned()
    } else {
        sender_label(message.sender_name.as_deref(), &message.sender)
    };
    crate::native_transcript::TranscriptRow {
        header: format!(
            "[{}] {sender}: ",
            crate::util::copy_stamp(message.timestamp)
        ),
        text: transcript_text(message),
    }
}

fn transcript_text(message: &crate::model::Message) -> String {
    match &message.content {
        crate::model::Content::Text { text, .. } => text.clone(),
        _ => message.summary(),
    }
}

fn editable_text(message: &crate::model::Message) -> Option<String> {
    match (&message.from_me, &message.content) {
        (true, crate::model::Content::Text { text, .. }) => Some(text.clone()),
        _ => None,
    }
}

fn link_page(link: &LinkStatus) -> (String, String) {
    match link {
        LinkStatus::Starting => (
            "Starting ZapTide".into(),
            "Preparing WhatsApp connection.".into(),
        ),
        LinkStatus::Unlinked {
            pair_code: Some(pair_code),
            ..
        } => (
            "Enter code on your phone".into(),
            format!("Enter {pair_code} in WhatsApp under Linked devices."),
        ),
        LinkStatus::Unlinked { pairing_phone, .. } if pairing_phone.is_some() => (
            "Requesting pairing code".into(),
            "Waiting for WhatsApp to provide a pairing code.".into(),
        ),
        LinkStatus::Unlinked { qr: Some(_), .. } => (
            "Link this computer".into(),
            "Open WhatsApp, choose Linked devices, then scan the QR code.".into(),
        ),
        LinkStatus::Unlinked { .. } => (
            "Link this computer".into(),
            "Waiting for WhatsApp to provide a QR code.".into(),
        ),
        LinkStatus::Connecting => ("Connecting".into(), "Completing WhatsApp linking.".into()),
        LinkStatus::Connected => ("Connected".into(), "Loading your chats.".into()),
        LinkStatus::Disconnected { .. } => (
            "Reconnecting".into(),
            "Connection lost. ZapTide is reconnecting automatically.".into(),
        ),
        LinkStatus::LoggedOut => (
            "Phone unlinked this computer".into(),
            "Requesting a new WhatsApp link code.".into(),
        ),
        LinkStatus::Failed(_) => (
            "ZapTide needs attention".into(),
            "WhatsApp connection could not start. Check the desktop log for details.".into(),
        ),
    }
}

fn qr_texture(qr: &str) -> Option<gtk::gdk::Texture> {
    const QUIET: usize = 4;
    // GtkPicture shows the texture at its pixel size; keep it near 264 px.
    const SIDE: usize = 264;
    let code = qrcode::QrCode::new(qr.as_bytes()).ok()?;
    let width = code.width();
    let module = (SIDE / (width + 2 * QUIET)).max(2);
    let side = (width + 2 * QUIET) * module;
    let mut image = image::RgbaImage::from_pixel(side as u32, side as u32, image::Rgba([255; 4]));
    for (index, color) in code.to_colors().into_iter().enumerate() {
        if color != qrcode::Color::Dark {
            continue;
        }
        let x0 = (index % width + QUIET) * module;
        let y0 = (index / width + QUIET) * module;
        for y in y0..y0 + module {
            for x in x0..x0 + module {
                image.put_pixel(x as u32, y as u32, image::Rgba([0, 0, 0, 255]));
            }
        }
    }
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .ok()?;
    gtk::gdk::Texture::from_bytes(&gtk::glib::Bytes::from_owned(encoded.into_inner())).ok()
}

impl NativeApplication {
    /// Clears search and filters, keeping widgets in step with the model.
    fn reset_chat_filters(&mut self, archived: bool) {
        self.chat_filters = crate::native_chat_list::ChatListFilters {
            archive: if archived {
                crate::native_chat_list::ArchiveFilter::Only
            } else {
                crate::native_chat_list::ArchiveFilter::Exclude
            },
            ..Default::default()
        };
        self.chat_projection.set_query("");
        self.chat_projection.set_filters(self.chat_filters);
        if let Some(search) = &self.chat_search {
            search.set_text("");
        }
        for filter in [
            self.unread_filter.as_ref(),
            self.pinned_filter.as_ref(),
            self.muted_filter.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            filter.set_active(false);
        }
        if let Some(filter) = &self.chat_kind_filter {
            filter.set_active(true);
        }
        if let Some(section) = &self.chat_section {
            section.set_active_name(Some(if archived { "archived" } else { "chats" }));
        }
    }

    /// Archived chats with unread messages, as counted on the phone.
    fn archived_unread_count(&self) -> usize {
        self.chat_snapshots
            .iter()
            .filter(|chat| chat.archived && !chat.locked && chat.unread > 0)
            .count()
    }

    fn showing_archived(&self) -> bool {
        self.chat_filters.archive == crate::native_chat_list::ArchiveFilter::Only
    }

    fn leaves_section(&self, chat: &crate::model::Chat) -> bool {
        chat.locked || chat.archived != self.showing_archived()
    }

    /// Whether a search or filter pill, rather than an empty section, hides chats.
    fn chat_list_narrowed(&self) -> bool {
        let filters = self.chat_filters;
        !self.chat_projection.query().is_empty()
            || filters.unread_only
            || filters.pinned_only
            || filters.private_only
            || filters.groups_only
            || filters.muted == crate::native_chat_list::MutedFilter::Only
    }

    fn chat_list_empty_title(&self) -> &'static str {
        match (!self.chat_list_narrowed(), self.showing_archived()) {
            (false, _) => "No Results",
            (true, true) => "No Archived Chats",
            (true, false) => "No Chats Yet",
        }
    }

    fn chat_list_empty_description(&self) -> &'static str {
        match (!self.chat_list_narrowed(), self.showing_archived()) {
            (false, _) => "No chats match this search or filter.",
            (true, true) => "Archived conversations appear here.",
            (true, false) => "Conversations appear here as WhatsApp syncs.",
        }
    }

    fn is_linked(&self) -> bool {
        matches!(
            self.link,
            LinkStatus::Connected | LinkStatus::Connecting | LinkStatus::Disconnected { .. }
        ) || (!self.chat_snapshots.is_empty() && !matches!(self.link, LinkStatus::LoggedOut))
    }

    fn pair_code(&self) -> Option<&str> {
        match &self.link {
            LinkStatus::Unlinked {
                pair_code: Some(code),
                ..
            } => Some(code),
            _ => None,
        }
    }

    fn pairing_requested(&self) -> bool {
        matches!(
            self.link,
            LinkStatus::Unlinked {
                pairing_phone: Some(_),
                ..
            }
        )
    }

    /// The link page is waiting on WhatsApp rather than on the user.
    fn link_busy(&self) -> bool {
        match &self.link {
            LinkStatus::Starting | LinkStatus::Connecting | LinkStatus::Connected => true,
            LinkStatus::Unlinked { qr, pair_code, .. } => {
                pair_code.is_none()
                    && (self.pairing_requested() || (qr.is_none() && !self.phone_linking))
            }
            _ => false,
        }
    }

    fn header_subtitle(&self) -> String {
        let Some(chat) = &self.active_chat else {
            return String::new();
        };
        if let Some(name) = self.typing.get(chat) {
            return format!("{name} is typing…");
        }
        match self.presence.get(chat) {
            Some((true, _)) => "Online".into(),
            Some((false, Some(at))) => format!("Last seen {}", crate::util::moment_stamp(*at)),
            _ => self.status.clone(),
        }
    }

    fn focus_composer(&self) {
        if let Some(composer) = &self.composer_view {
            composer.grab_focus();
        }
    }

    fn themes_directory(&self) -> std::path::PathBuf {
        self.settings_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("themes")
    }

    fn theme_choice_filenames(&self) -> Vec<String> {
        self.theme_catalog
            .picker_themes()
            .map(|theme| theme.filename.clone())
            .collect()
    }

    fn sync_custom_theme_selection(&mut self) {
        if let Some(filename) = self.settings.custom_theme.clone() {
            if let Some(theme) = self.theme_catalog.find(&filename).cloned() {
                self.settings.custom_theme_cache = Some(theme);
            } else {
                self.theme_catalog
                    .start(self.themes_directory(), Some(filename), &self.notifier);
            }
        } else {
            self.settings.custom_theme_cache = None;
        }
        self.apply_runtime_settings();
    }

    fn poll_theme_catalog(&mut self) {
        if self.theme_catalog.needs_reload() {
            self.theme_catalog.start(
                self.themes_directory(),
                self.settings.custom_theme.clone(),
                &self.notifier,
            );
        }
        if !self.theme_catalog.poll() {
            return;
        }

        let previous_palette = self.settings.cached_palette();
        if let Some(filename) = self.settings.custom_theme.as_deref() {
            if let Some(theme) = self.theme_catalog.find(filename).cloned() {
                self.settings.custom_theme_cache = Some(theme);
            }
        } else if self.settings.theme == crate::settings::ThemeChoice::System {
            self.settings.system_theme_cache = self.theme_catalog.system_theme().cloned();
        } else {
            self.settings.system_theme_cache = None;
        }

        if self.settings.cached_palette() != previous_palette {
            self.apply_runtime_settings();
            if self.settings.save(&self.settings_path).is_err() {
                self.status = "Could not save theme preference".into();
            }
        }
        let choices = self.theme_choice_filenames();
        let selected = self.settings.custom_theme.clone();
        if let Some(dialog) = self.preferences.as_mut() {
            dialog.set_custom_theme_choices(&choices, selected.as_deref());
        }
    }

    fn install_zoom_provider(&self) {
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &self.zoom_provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            gtk::style_context_add_provider_for_display(
                &display,
                &self.custom_theme_provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
            );
        }
    }

    fn apply_runtime_settings(&mut self) {
        apply_theme(&self.settings, &self.custom_theme_provider);
        self.zoom_provider.load_from_string(&format!(
            "window {{ font-size: {:.0}%; }}",
            100.0 * self.settings.zoom
        ));
        if let Some(sidebar) = &self.sidebar {
            sidebar.set_width_request(self.settings.sidebar_width.round() as i32);
        }
        self.enter_sends.set(self.settings.enter_sends);
        self.media.set_speed(self.settings.voice_speed);
    }

    fn cancel_portal_requests(&mut self) {
        let ids: Vec<_> = self.portal_requests.borrow_mut().drain().collect();
        for id in ids {
            self.portals.cancel(id);
        }
    }

    #[cfg(feature = "demo")]
    fn audit_synthetic_commands(&self) {
        if !synthetic_e2e_enabled() {
            return;
        }
        if let Some(backend) = &self.backend {
            for (variant, count) in backend.take_demo_command_counts() {
                log::info!("synthetic command audit {variant}={count}");
            }
        }
    }

    fn scroll_message_into_view(&self, id: &str) {
        let Some(position) = self.message_ids.iter().position(|message| message == id) else {
            return;
        };
        let view = self.messages.view.clone();
        gtk::glib::idle_add_local_once(move || {
            view.scroll_to(position as u32, gtk::ListScrollFlags::NONE, None);
        });
    }

    fn selected_chat(&self) -> Option<&crate::model::Chat> {
        self.chat_projection.selected_chat()
    }

    fn show_message_menu(
        &mut self,
        id: String,
        x: f32,
        y: f32,
        sender: &ComponentSender<NativeApplication>,
    ) {
        if !self.message_ids.contains(&id) {
            return;
        }
        let Some(parent) = self.message_menu.parent() else {
            return;
        };
        let Some(point) = self
            .messages
            .view
            .compute_point(&parent, &gtk::graphene::Point::new(x, y))
        else {
            return;
        };
        self.message_target = Some(id);
        let rect = gtk::gdk::Rectangle::new(point.x() as i32, point.y() as i32, 1, 1);
        self.message_menu
            .set_menu_model(Some(&self.message_menu_model()));
        let current = self.selected_message().and_then(|message| {
            message
                .reactions
                .iter()
                .find(|reaction| reaction.from_me)
                .map(|reaction| reaction.emoji.clone())
        });
        self.message_menu.add_child(
            &reaction_bar(&self.message_menu, &parent, rect, current, sender),
            "reactions",
        );
        self.message_menu.set_pointing_to(Some(&rect));
        self.message_menu.popup();
    }

    /// Actions for the right-clicked message, grouped as GNOME menus are.
    fn message_menu_model(&self) -> gtk::gio::Menu {
        let menu = gtk::gio::Menu::new();
        let Some(message) = self.selected_message() else {
            return menu;
        };
        let open = gtk::gio::Menu::new();
        if self.attachment_action_available() {
            let downloaded = message
                .content
                .media()
                .is_some_and(|media| media.path.is_some());
            open.append(
                Some(if downloaded {
                    "Open Attachment"
                } else {
                    "Download Attachment"
                }),
                Some("message.attachment"),
            );
            if downloaded {
                open.append(Some("Save Attachment…"), Some("message.save"));
            }
        }
        if matches!(
            &message.content,
            crate::model::Content::Text {
                preview: Some(_),
                ..
            }
        ) {
            open.append(Some("Open Link"), Some("message.open-link"));
        }
        if message.quoted.is_some() {
            open.append(Some("Go to Quoted Message"), Some("message.quoted"));
        }
        let reactions = gtk::gio::Menu::new();
        let item = gtk::gio::MenuItem::new(None, None);
        item.set_attribute_value("custom", Some(&"reactions".to_variant()));
        reactions.append_item(&item);
        let respond = gtk::gio::Menu::new();
        if message_text(message).is_some() {
            respond.append(Some("Copy Text"), Some("message.copy"));
        }
        respond.append(Some("Reply"), Some("message.reply"));
        if self.editable_messages.contains_key(&message.id) {
            respond.append(Some("Edit"), Some("message.edit"));
        }
        respond.append(Some("Forward…"), Some("message.forward"));
        let vote = gtk::gio::Menu::new();
        if self.selected_poll_can_vote() {
            for (index, option) in self
                .selected_poll_options()
                .unwrap_or_default()
                .iter()
                .enumerate()
            {
                let item = gtk::gio::MenuItem::new(Some(&format!("Vote: {option}")), None);
                item.set_action_and_target_value(
                    Some("message.vote"),
                    Some(&(index as u32).to_variant()),
                );
                vote.append_item(&item);
            }
        }
        let delete = gtk::gio::Menu::new();
        delete.append(Some("Delete for Me"), Some("message.delete"));
        if message.from_me {
            delete.append(Some("Delete for Everyone"), Some("message.delete-everyone"));
        }
        for section in [reactions, open, respond, vote, delete] {
            if section.n_items() > 0 {
                menu.append_section(None, &section);
            }
        }
        menu
    }

    fn selected_message_id(&self) -> Option<String> {
        self.message_target.clone()
    }

    fn participant_label(&self, id: &str, index: usize) -> String {
        let name = self
            .contacts
            .get(id)
            .and_then(crate::model::Contact::display_name)
            .map(str::to_owned)
            .unwrap_or_else(|| id.split('@').next().unwrap_or_default().to_owned());
        if name.is_empty() {
            format!("Participant {}", index + 1)
        } else {
            name
        }
    }

    fn selected_message(&self) -> Option<&crate::model::Message> {
        self.selected_message_id()
            .and_then(|id| self.message_snapshots.get(&id))
    }

    fn selected_attachment(&self) -> Option<crate::native_attachments::AttachmentPresentation> {
        self.selected_message()
            .and_then(crate::native_attachments::project)
    }

    fn attachment_action_available(&self) -> bool {
        self.selected_attachment().is_some_and(|attachment| {
            attachment.control != crate::native_attachments::AttachmentControl::Downloading
        })
    }

    fn selected_poll_options(&self) -> Option<&[String]> {
        match &self.selected_message()?.content {
            crate::model::Content::Poll { options, .. } => Some(options),
            _ => None,
        }
    }

    fn selected_poll_can_vote(&self) -> bool {
        matches!(&self.selected_message().map(|message| &message.content),
            Some(crate::model::Content::Poll { state, .. }) if state.can_vote)
    }

    fn recording_active(&self) -> bool {
        self.media.is_recording()
    }

    fn recording_status(&self) -> String {
        let Some(elapsed) = self.media.recording_elapsed() else {
            return String::new();
        };
        let levels = self.media.recording_levels();
        let projection =
            crate::native_voice::project_recording(crate::native_voice::VoiceRecordingInput {
                recording: true,
                elapsed,
                levels: &levels,
            });
        format!(
            "Recording {} · {} waveform bars",
            projection.time,
            projection.waveform.len()
        )
    }

    fn can_send_voice(&self) -> bool {
        !self.recording_active()
            && self.pending_send.is_none()
            && !self.voice_send_pending
            && self
                .active_chat
                .as_deref()
                .and_then(|id| self.chat_snapshots.iter().find(|chat| chat.id == id))
                .is_some_and(crate::model::Chat::can_send)
    }

    fn activate_attachment(&mut self, id: &str) {
        let Some(action) = self
            .message_snapshots
            .get(id)
            .and_then(crate::native_attachments::project)
            .and_then(|attachment| {
                attachment.action(crate::native_attachments::AttachmentIntent::Activate)
            })
        else {
            return;
        };
        match action {
            crate::model::Action::Download { chat, message } => {
                if let Some(media) = self
                    .message_snapshots
                    .get_mut(&message)
                    .and_then(|message| message.content.media_mut())
                {
                    media.state = crate::model::MediaState::Downloading;
                }
                if let Some(backend) = &self.backend {
                    backend.send(crate::backend::Command::Download {
                        chat,
                        message: message.clone(),
                    });
                    self.status = "Downloading attachment".into();
                }
                self.refresh_message_row(&message);
            }
            crate::model::Action::OpenFile(path) => {
                let file = gtk::gio::File::for_path(path);
                if gtk::gio::AppInfo::launch_default_for_uri(
                    &file.uri(),
                    None::<&gtk::gio::AppLaunchContext>,
                )
                .is_err()
                {
                    self.status = "Could not open attachment".into();
                }
            }
            _ => {}
        }
    }

    fn paste_clipboard_image(&mut self, sender: &ComponentSender<Self>) {
        let Some(chat) = self.active_chat.clone().filter(|_| self.can_attach()) else {
            return;
        };
        if !self.portal_requests.borrow().is_empty() {
            self.status = "Finish the active portal action first".into();
            return;
        }
        let Some(display) = gtk::gdk::Display::default() else {
            self.status = "Clipboard is unavailable".into();
            return;
        };
        let input = sender.clone();
        let requests = self.portal_requests.clone();
        let request_id = std::rc::Rc::new(std::cell::Cell::new(None));
        let callback_id = request_id.clone();
        let request = self
            .portals
            .read_clipboard_image(&display.clipboard(), move |result| {
                if let Some(id) = callback_id.get() {
                    requests.borrow_mut().remove(&id);
                }
                let pixels = result
                    .map_err(|_| "Clipboard image is unavailable".to_owned())
                    .and_then(|texture| {
                        let Some(texture) = texture else {
                            return Err("Clipboard does not contain an image".into());
                        };
                        let width = texture.width() as u32;
                        let height = texture.height() as u32;
                        let Some(length) = (width as usize)
                            .checked_mul(height as usize)
                            .and_then(|pixels| pixels.checked_mul(4))
                        else {
                            return Err("Clipboard image is too large".into());
                        };
                        if width == 0
                            || height == 0
                            || u64::from(width) * u64::from(height) > 16_777_216
                        {
                            return Err("Clipboard image is too large".into());
                        }
                        let mut rgba = vec![0; length];
                        texture.download(&mut rgba, width as usize * 4);
                        convert_premultiplied_bgra_to_rgba(&mut rgba);
                        Ok(ClipboardPixels {
                            width,
                            height,
                            preview: gtk::gdk::MemoryTexture::new(
                                width as i32,
                                height as i32,
                                gtk::gdk::MemoryFormat::R8g8b8a8,
                                &gtk::glib::Bytes::from_owned(rgba.clone()),
                                width as usize * 4,
                            )
                            .into(),
                            rgba,
                        })
                    });
                match pixels {
                    Ok(pixels) => input.input(Input::ClipboardImageReady { chat, pixels }),
                    Err(error) => input.input(Input::PortalActionFinished(error)),
                }
            });
        if let Some(id) = request {
            request_id.set(Some(id));
            self.portal_requests.borrow_mut().insert(id);
            self.status = "Reading clipboard image".into();
        } else {
            self.status = "Could not read clipboard image".into();
        }
    }

    fn stage_clipboard_image(&mut self, chat: String, pixels: ClipboardPixels) {
        if self.active_chat.as_deref() != Some(&chat) || !self.can_attach() {
            self.status = "Choose an available conversation before pasting an image".into();
            return;
        }
        if self.pending_send.is_some()
            || self.voice_send_pending
            || self
                .pending_attachments
                .get(&chat)
                .is_some_and(|paths| !paths.is_empty())
            || self
                .pending_clipboard_images
                .keys()
                .any(|staged_chat| staged_chat != &chat)
        {
            self.status = "Send or clear the staged attachment before pasting an image".into();
            return;
        }
        self.pending_clipboard_images.insert(chat, pixels);
        self.status = "Clipboard image ready. Review preview, then press Send.".into();
    }

    fn open_selected_uri(&mut self, sender: &ComponentSender<Self>) {
        let Some(uri) = self
            .selected_message()
            .and_then(|message| match &message.content {
                crate::model::Content::Text {
                    preview: Some(preview),
                    ..
                } => Some(preview.url.clone()),
                _ => None,
            })
        else {
            return;
        };
        let input = sender.clone();
        let requests = self.portal_requests.clone();
        let request_id = std::rc::Rc::new(std::cell::Cell::new(None));
        let callback_id = request_id.clone();
        match self.portals.open_uri(
            crate::native_portals::UserAction::for_explicit_user_action(),
            &uri,
            move |result| {
                if let Some(id) = callback_id.get() {
                    requests.borrow_mut().remove(&id);
                }
                input.input(Input::PortalUriFinished(result.is_ok()));
            },
        ) {
            Ok(id) => {
                request_id.set(Some(id));
                self.portal_requests.borrow_mut().insert(id);
                self.status = "Opening link".into();
            }
            Err(_) => self.status = "Link is not supported".into(),
        }
    }

    fn prepare_themes_folder(&mut self, sender: &ComponentSender<Self>) {
        let Some(config_dir) = self.settings_path.parent() else {
            self.status = "Could not open themes folder".into();
            self.toast("Could not open themes folder");
            return;
        };
        let folder = config_dir.join("themes");
        let sender = sender.clone();
        std::thread::spawn(move || {
            let success = std::fs::create_dir_all(folder).is_ok();
            sender.input(Input::ThemesFolderPrepared(success));
        });
        self.status = "Preparing themes folder".into();
    }

    fn launch_themes_folder(&mut self, sender: &ComponentSender<Self>) {
        let Some(config_dir) = self.settings_path.parent() else {
            self.status = "Could not open themes folder".into();
            self.toast("Could not open themes folder");
            return;
        };
        let folder = config_dir.join("themes");
        let input = sender.clone();
        let requests = self.portal_requests.clone();
        let request_id = std::rc::Rc::new(std::cell::Cell::new(None));
        let callback_id = request_id.clone();
        match self.portals.open_folder(
            crate::native_portals::UserAction::for_explicit_user_action(),
            &folder,
            move |result| {
                if let Some(id) = callback_id.get() {
                    requests.borrow_mut().remove(&id);
                }
                input.input(Input::ThemesFolderFinished(result.is_ok()));
            },
        ) {
            Ok(id) => {
                request_id.set(Some(id));
                self.portal_requests.borrow_mut().insert(id);
                self.status = "Opening themes folder".into();
            }
            Err(_) => {
                self.status = "Could not open themes folder".into();
                self.toast("Could not open themes folder");
            }
        }
    }

    fn save_selected_attachment(&mut self, sender: &ComponentSender<Self>) {
        let Some(path) = self
            .selected_message()
            .and_then(|message| message.content.media())
            .and_then(|media| media.path.clone())
        else {
            return;
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "attachment".into());
        let input = sender.clone();
        let requests = self.portal_requests.clone();
        let callback_id = std::rc::Rc::new(std::cell::Cell::new(None));
        let id_slot = callback_id.clone();
        let source = gtk::gio::File::for_path(path);
        let request = self.portals.save_copy(
            Some(&self.window),
            &source,
            "Save attachment",
            &name,
            move |result| {
                if let Some(id) = id_slot.get() {
                    requests.borrow_mut().remove(&id);
                }
                input.input(Input::SaveAttachmentFinished(result.is_ok()));
            },
        );
        if let Some(id) = request {
            callback_id.set(Some(id));
            self.portal_requests.borrow_mut().insert(id);
            self.status = "Choose save location".into();
        } else {
            self.status = "Could not open save dialog".into();
        }
    }

    fn refresh_message_row(&mut self, id: &str) {
        if self.message_ids.iter().any(|known| known == id) {
            self.rebuild_message_rows();
        }
    }

    fn update_audio_row(&mut self, id: &str) {
        let Some(voice) = self
            .message_snapshots
            .get(id)
            .and_then(|message| self.project_voice(message))
        else {
            return;
        };
        if let Some(position) = self.message_ids.iter().position(|known| known == id)
            && let Some(row) = self.messages.get(position as u32)
        {
            row.borrow_mut().audio = Some(voice.clone());
        }
        if let Some(controls) = self.audio_registry.borrow().get(id) {
            controls.update(&voice);
        }
    }

    fn react_selected(&mut self, emoji: String) {
        let Some((chat, message)) = self.active_chat.clone().zip(self.selected_message_id()) else {
            return;
        };
        let action = crate::native_actions::react(chat, message, emoji);
        if let crate::model::Action::React {
            chat,
            message,
            emoji,
        } = action
            && let Some(backend) = &self.backend
        {
            backend.send(crate::backend::Command::React {
                chat,
                message,
                emoji,
            });
        }
    }

    fn forward_selected(&mut self, destination: String) {
        let Some((source, message)) = self.active_chat.clone().zip(self.selected_message_id())
        else {
            return;
        };
        let visible_destination = self
            .chat_snapshots
            .iter()
            .any(|chat| chat.id == destination && !chat.archived && !chat.locked);
        if !visible_destination || destination == source {
            self.status = "Choose another available conversation".into();
            return;
        }
        let action = crate::native_actions::forward(source, message, destination);
        if let crate::model::Action::Forward {
            from_chat,
            message,
            to_chat,
        } = action
            && let Some(backend) = &self.backend
        {
            backend.send(crate::backend::Command::Forward {
                from_chat,
                message,
                to_chat,
            });
            self.status = "Forwarding message".into();
        }
    }

    fn delete_selected(&mut self, everyone: bool) {
        let Some(message) = self.selected_message().cloned() else {
            return;
        };
        let action = if everyone {
            if !message.from_me {
                return;
            }
            crate::native_actions::delete_for_everyone(message.chat.clone(), message.id.clone())
        } else {
            crate::native_actions::delete_for_me(message.chat.clone(), message.id.clone())
        };
        if let Some(backend) = &self.backend {
            match action {
                crate::model::Action::DeleteForEveryone { chat, message } => {
                    backend.send(crate::backend::Command::Revoke { chat, id: message });
                    self.status = "Deleting message for everyone".into();
                }
                crate::model::Action::DeleteForMe { chat, message } => {
                    backend.send(crate::backend::Command::DeleteLocal { chat, id: message });
                    self.status = "Deleting message".into();
                }
                _ => {}
            }
        }
    }

    fn vote_selected(&mut self) {
        let Some(message) = self.selected_message() else {
            return;
        };
        let crate::model::Content::Poll { options, state, .. } = &message.content else {
            return;
        };
        let choice = self.poll_choice;
        if choice >= options.len() || !state.can_vote {
            return;
        }
        let action = crate::native_actions::vote_poll(
            message.chat.clone(),
            message.id.clone(),
            vec![choice],
        );
        if let crate::model::Action::VotePoll {
            chat,
            message,
            choices,
        } = action
            && let Some(backend) = &self.backend
        {
            backend.send(crate::backend::Command::VotePoll {
                chat,
                message,
                choices,
            });
        }
    }

    fn create_poll(&mut self, question: String, first: String, second: String) {
        let Some(chat) = self.active_chat.clone() else {
            return;
        };
        let draft = crate::model::PollDraft {
            question,
            options: vec![first, second],
            multiple: false,
        };
        let Ok(draft) = draft.validated() else {
            self.status = "Enter a question and two different poll options".into();
            return;
        };
        let action = crate::native_actions::create_poll(chat, draft);
        if let crate::model::Action::CreatePoll { chat, draft } = action
            && let Some(backend) = &self.backend
        {
            backend.send(crate::backend::Command::CreatePoll { chat, draft });
            self.status = "Creating poll".into();
        }
    }

    fn seek_voice(&mut self, fraction: f64, sender: &ComponentSender<Self>) {
        let Some(action) = self.selected_voice.as_ref().and_then(|voice| {
            voice.action(crate::native_voice::VoiceIntent::Seek(fraction as f32))
        }) else {
            return;
        };
        if let crate::model::Action::SeekVoice {
            message,
            path,
            fraction,
        } = action
        {
            if self.media.seek(&message, &path, fraction).is_err() {
                self.status = "Voice playback could not seek".into();
            } else {
                self.playing_audio = Some(message.clone());
            }
            self.refresh_selected_voice();
            self.schedule_voice_poll(sender);
        }
    }

    fn recording_action(&mut self, intent: crate::native_voice::RecordingIntent) {
        let active = self.media.is_recording();
        let projection =
            crate::native_voice::project_recording(crate::native_voice::VoiceRecordingInput {
                recording: active,
                elapsed: self.media.recording_elapsed().unwrap_or_default(),
                levels: &self.media.recording_levels(),
            });
        let Some(action) = projection.action(intent) else {
            return;
        };
        match action {
            crate::model::Action::StartRecording => {
                if self.can_send_voice() {
                    self.media.start_recording();
                }
            }
            crate::model::Action::CancelRecording => {
                self.media.cancel_recording();
                self.status = "Recording canceled".into();
            }
            crate::model::Action::SendRecording => {
                let Some(samples) = self.media.finish_recording() else {
                    return;
                };
                match samples {
                    Ok(samples) => {
                        if let (Some(chat), Some(backend)) =
                            (self.active_chat.clone(), self.backend.as_ref())
                        {
                            let quoting = self
                                .reply_to
                                .as_ref()
                                .filter(|(reply_chat, _)| reply_chat == &chat)
                                .map(|(_, id)| id.clone());
                            backend.send(crate::backend::Command::SendVoice {
                                chat,
                                samples,
                                quoting,
                            });
                            self.voice_send_pending = true;
                            self.status = "Sending voice message".into();
                        }
                    }
                    Err(_) => self.status = "Could not record voice message".into(),
                }
            }
            _ => {}
        }
    }

    fn apply_chat_changes(&mut self, changes: Vec<ChatChange>) {
        for change in changes {
            match change {
                ChatChange::Snapshot(chats) => {
                    for chat in &chats {
                        self.request_avatar(&chat.id);
                    }
                    self.reset_chats(chats)
                }
                ChatChange::Update(chat) => {
                    self.request_avatar(&chat.id);
                    self.update_chat(chat)
                }
            }
        }
    }

    fn request_avatar(&mut self, id: &str) {
        if self.avatar_requests.insert(id.to_owned())
            && let Some(backend) = &self.backend
        {
            backend.send(crate::backend::Command::FetchAvatar {
                id: id.to_owned(),
                full: false,
            });
        }
    }

    fn reset_chats(&mut self, chats: Vec<crate::model::Chat>) {
        self.chat_snapshots = chats;
        self.chats_dirty = true;
        if self.active_chat.as_deref().is_some_and(|id| {
            self.chat_snapshots
                .iter()
                .find(|chat| chat.id == id)
                .is_none_or(|chat| self.leaves_section(chat))
        }) {
            self.clear_active_chat();
        }
    }

    fn update_chat(&mut self, chat: crate::model::Chat) {
        if let Some(index) = self
            .chat_snapshots
            .iter()
            .position(|known| known.id == chat.id)
        {
            self.chat_snapshots.remove(index);
        }
        let index = self
            .chat_snapshots
            .partition_point(|known| known.last_activity >= chat.last_activity);
        if self.leaves_section(&chat) && self.active_chat.as_deref() == Some(&chat.id) {
            self.clear_active_chat();
        }
        self.chat_snapshots.insert(index, chat);
        self.chats_dirty = true;
    }

    /// Rebuilds the chat list widget from the snapshots, once per burst of
    /// backend changes. History sync sends thousands of chat and avatar
    /// updates; rebuilding per event froze the interface.
    fn flush_chats(&mut self) {
        if self.chats_dirty {
            self.sync_chat_projection();
        }
    }

    fn sync_chat_projection(&mut self) {
        // New activity inserts rows above the visible ones, and the list keeps
        // its anchor on the old first row; stay pinned to the newest chat.
        let at_top = self
            .chats
            .view
            .vadjustment()
            .is_none_or(|adjustment| adjustment.value() <= 0.0);
        if std::mem::take(&mut self.chats_dirty) {
            self.chat_projection
                .replace_snapshot(self.chat_snapshots.clone());
            self.chat_projection.set_filters(self.chat_filters);
        }
        let selected = self.chat_projection.selected_id().map(str::to_owned);
        self.chat_ids.clear();
        let mut rows = Vec::new();
        for chat in self.chat_projection.visible() {
            self.chat_ids.push(chat.id.clone());
            rows.push(chat_row(chat.clone(), self.avatars.get(&chat.id).cloned()));
        }
        // Replace only the changed middle so scrolling and a click in progress
        // survive the frequent small reorders of history sync.
        let old_len = self.chats.len() as usize;
        let (prefix, removed, inserted) = changed_span(old_len, &rows, |position, row| {
            self.chats
                .get(position as u32)
                .is_some_and(|item| *item.borrow() == *row)
        });
        if removed + inserted > old_len.max(rows.len()) / 2 {
            self.chats.clear();
            self.chats.extend_from_iter(rows);
        } else {
            for _ in 0..removed {
                self.chats.remove(prefix as u32);
            }
            for (offset, row) in rows.into_iter().skip(prefix).take(inserted).enumerate() {
                self.chats.insert((prefix + offset) as u32, row);
            }
        }
        if let Some(position) = selected
            .as_deref()
            .and_then(|id| self.chat_ids.iter().position(|known| known == id))
        {
            self.chats.selection_model.set_selected(position as u32);
        }
        if at_top && !self.chat_ids.is_empty() {
            self.chats
                .view
                .scroll_to(0, gtk::ListScrollFlags::NONE, None);
        }
    }

    /// Rebuilds the open picker from the latest lists, staying on its page.
    fn refresh_sticker_picker(&mut self, sender: &ComponentSender<NativeApplication>) {
        let Some((popover, stack)) = &self.sticker_picker else {
            return;
        };
        if !popover.is_visible() {
            self.sticker_picker = None;
            return;
        }
        let page = stack.visible_child_name();
        let scroll = |stack: &gtk::Stack| {
            stack
                .visible_child()
                .and_downcast::<gtk::ScrolledWindow>()
                .map(|scroller| scroller.vadjustment())
        };
        let offset = scroll(stack).map(|adjustment| adjustment.value());
        let had_focus = popover.focus_child().is_some();
        let (content, new_stack) = sticker_picker_content(
            &self.sticker_packs,
            &self.favorite_stickers,
            &self.recent_stickers,
            page.as_deref(),
            sender,
        );
        popover.set_child(Some(&content));
        if had_focus {
            content.child_focus(gtk::DirectionType::TabForward);
        }
        // The rebuilt grid has no height yet; restore the scroll once it is
        // tall enough to hold the old position.
        if let (Some(offset), Some(adjustment)) = (offset, scroll(&new_stack))
            && offset > 0.0
        {
            let handler = std::rc::Rc::new(std::cell::Cell::new(None));
            let slot = handler.clone();
            let id = adjustment.connect_upper_notify(move |adjustment| {
                if adjustment.upper() - adjustment.page_size() >= offset {
                    adjustment.set_value(offset);
                    if let Some(id) = slot.take() {
                        adjustment.disconnect(id);
                    }
                }
            });
            handler.set(Some(id));
        }
        self.sticker_picker = Some((popover.clone(), new_stack));
    }

    /// Stickers are small and meaningless as placeholders, so loaded history
    /// fetches them the way live messages are fetched.
    fn download_missing_stickers(&mut self) {
        let Some(backend) = self
            .backend
            .as_ref()
            .filter(|_| self.settings.auto_download)
        else {
            return;
        };
        for message in self.message_snapshots.values_mut() {
            if let crate::model::Content::Sticker { media, .. } = &mut message.content
                && should_auto_download(Some(media))
            {
                backend.send(crate::backend::Command::Download {
                    chat: message.chat.clone(),
                    message: message.id.clone(),
                });
                media.state = crate::model::MediaState::Downloading;
            }
        }
    }

    fn apply_messages(&mut self, chat: String, messages: Vec<crate::model::Message>, older: bool) {
        if self.active_chat.as_deref() != Some(&chat) {
            return;
        }
        let anchor = older.then(|| self.message_ids.first().cloned()).flatten();
        let mut page_ids = std::collections::HashSet::new();
        let messages = messages
            .into_iter()
            .filter(|message| {
                page_ids.insert(message.id.clone())
                    && !self.message_ids.iter().any(|id| id == &message.id)
            })
            .collect::<Vec<_>>();
        if older {
            for message in messages.into_iter().rev() {
                if let Some(text) = editable_text(&message) {
                    self.editable_messages.insert(message.id.clone(), text);
                }
                self.message_ids.insert(0, message.id.clone());
                self.message_snapshots
                    .insert(message.id.clone(), message.clone());
            }
        } else {
            for message in messages {
                if let Some(text) = editable_text(&message) {
                    self.editable_messages.insert(message.id.clone(), text);
                }
                self.message_ids.push(message.id.clone());
                self.message_snapshots
                    .insert(message.id.clone(), message.clone());
            }
        }
        for id in trim_message_window(&mut self.message_ids, older, ACTIVE_MESSAGE_LIMIT) {
            self.editable_messages.remove(&id);
            self.message_snapshots.remove(&id);
        }
        self.download_missing_stickers();
        self.queue_waveforms();
        self.rebuild_message_rows();
        self.sync_transcript();
        if older
            && let Some(anchor) = anchor
            && let Some(position) = self.message_ids.iter().position(|id| id == &anchor)
        {
            let view = self.messages.view.clone();
            gtk::glib::idle_add_local_once(move || {
                view.scroll_to(position as u32, gtk::ListScrollFlags::NONE, None);
            });
        }
        self.status = "Conversation loaded".into();
    }

    fn message_updated(&mut self, message: crate::model::Message) {
        let text = editable_text(&message);
        if self.active_chat.as_deref() == Some(&message.chat)
            && self.message_ids.iter().any(|id| id == &message.id)
        {
            if let Some(text) = &text {
                self.editable_messages
                    .insert(message.id.clone(), text.clone());
            } else {
                self.editable_messages.remove(&message.id);
            }
            self.message_snapshots
                .insert(message.id.clone(), message.clone());
            self.rebuild_message_rows();
            self.sync_transcript();
            self.refresh_selected_voice();
        }
    }

    fn edited(&mut self, chat: String, id: String, success: bool) {
        if !is_edit_completion(self.pending_edit.as_ref(), &chat, &id) {
            return;
        }
        self.pending_edit = None;
        if success {
            if let Some(request) = self.pending_composer_request.take() {
                self.composer.complete_edit(&request);
            }
            if self
                .editing
                .as_ref()
                .is_some_and(|editing| editing == &(chat.clone(), id.clone()))
            {
                self.editing = None;
                self.draft = self.drafts.get(&chat).cloned().unwrap_or_default();
                self.composer.cancel_context(&chat);
                self.composer.set_draft(&chat, self.draft.clone());
                self.composer_buffer.set_text(&self.draft);
                self.status = "Message updated".into();
            }
        } else {
            if let Some(request) = self.pending_composer_request.take() {
                let text = request.text().to_owned();
                self.composer.complete_edit(&request);
                self.composer.set_draft(&chat, text);
            }
            self.status = "Edit could not be saved. Draft kept.".into();
        }
    }

    fn sent(&mut self, chat: String, success: bool) {
        if self
            .pending_send
            .as_ref()
            .is_some_and(|pending| !pending.attachments.is_empty())
        {
            return;
        }
        self.send_completed(chat, success);
    }

    fn attachment_completed(&mut self, chat: String, path: std::path::PathBuf, success: bool) {
        let Some(pending) = self.pending_send.as_mut() else {
            return;
        };
        if pending.chat != chat || !pending.attachments.contains(&path) {
            return;
        }
        if pending.complete_attachment(path, success) {
            self.finish_pending_send(chat);
        }
    }

    fn send_completed(&mut self, chat: String, success: bool) {
        let Some(pending) = self.pending_send.as_mut() else {
            return;
        };
        if pending.chat != chat {
            return;
        }
        if !pending.complete(success) {
            return;
        }
        self.finish_pending_send(chat);
    }

    fn finish_pending_send(&mut self, chat: String) {
        let Some(pending) = self.pending_send.take() else {
            return;
        };
        let request = self.pending_composer_request.take();
        if let Some(request) = request {
            let text = request.text().to_owned();
            self.composer.complete_send(&request);
            if pending.failed && self.composer.draft(&chat).is_empty() {
                self.composer.set_draft(&chat, text);
            }
        }
        if !pending.failed {
            if pending.clipboard_image {
                self.pending_clipboard_images.remove(&chat);
            }
            let unchanged = self.drafts.get(&chat) == Some(&pending.text);
            if unchanged && self.active_chat.as_deref() == Some(&chat) && self.draft == pending.text
            {
                self.draft.clear();
                self.composer_buffer.set_text("");
            }
            if unchanged {
                self.drafts.remove(&chat);
            }
            if self
                .reply_to
                .as_ref()
                .is_some_and(|reply| Some(&reply.1) == pending.reply.as_ref())
            {
                self.reply_to = None;
            }
            self.status = "Message sent".into();
        } else {
            if !pending.failed_attachments.is_empty() {
                self.pending_attachments
                    .entry(chat.clone())
                    .or_default()
                    .splice(0..0, pending.failed_attachments);
            }
            self.status = "Message could not be sent. Draft kept.".into();
        }
    }

    fn media_completed(
        &mut self,
        chat: &str,
        id: &str,
        result: Result<std::path::PathBuf, String>,
    ) {
        if self.active_chat.as_deref() != Some(chat) {
            return;
        }
        let Some(media) = self
            .message_snapshots
            .get_mut(id)
            .and_then(|message| message.content.media_mut())
        else {
            return;
        };
        match result {
            Ok(path) => {
                media.path = Some(path);
                media.state = crate::model::MediaState::Idle;
            }
            Err(error) => {
                let notice = if error.contains("403") || error.contains("404") {
                    "No longer available on WhatsApp's servers".to_owned()
                } else {
                    error
                };
                media.state = crate::model::MediaState::Failed(notice);
            }
        }
        self.refresh_message_row(id);
        self.refresh_selected_voice();
        self.queue_waveforms();
    }

    fn message_deleted(&mut self, chat: &str, id: &str) {
        if self.active_chat.as_deref() != Some(chat) {
            return;
        }
        if self.playing_audio.as_deref() == Some(id) {
            self.media.stop_playback();
            self.playing_audio = None;
        }
        self.audio_waveforms
            .remove(&(chat.to_owned(), id.to_owned()));
        self.audio_errors.remove(&(chat.to_owned(), id.to_owned()));
        self.audio_registry.borrow_mut().remove(id);
        let Some(position) = self.message_ids.iter().position(|known| known == id) else {
            return;
        };
        self.message_ids.remove(position);
        self.message_snapshots.remove(id);
        self.editable_messages.remove(id);
        self.rebuild_message_rows();
        if self
            .editing
            .as_ref()
            .is_some_and(|(_, editing_id)| editing_id == id)
        {
            self.editing = None;
            self.pending_edit = None;
        }
        if self
            .reply_to
            .as_ref()
            .is_some_and(|(_, reply_id)| reply_id == id)
        {
            self.reply_to = None;
        }
        self.sync_transcript();
        if self.selected_message_id().is_none() {
            self.selected_voice = None;
            self.selected_voice_message = None;
        } else {
            self.refresh_selected_voice();
        }
        self.status = "Message deleted".into();
    }

    fn clear_active_chat(&mut self) {
        self.media.stop_playback();
        self.playing_audio = None;
        self.waveform_queue.clear();
        if let Some(cancel) = self.waveform_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Release);
        }
        self.audio_waveforms.clear();
        self.waveform_attempted.clear();
        self.audio_errors.clear();
        self.audio_registry.borrow_mut().clear();
        self.active_chat = None;
        self.pending_send = None;
        self.pending_edit = None;
        self.reply_to = None;
        self.editing = None;
        self.message_ids.clear();
        self.message_snapshots.clear();
        self.editable_messages.clear();
        self.transcript.clear();
        self.selected_voice = None;
        self.selected_voice_message = None;
        self.messages.clear();
        self.page_title = "Conversation unavailable".into();
        self.status = "This chat is no longer available.".into();
    }

    fn pending_attachment_count(&self) -> usize {
        let files = self
            .active_chat
            .as_ref()
            .and_then(|chat| self.pending_attachments.get(chat))
            .map_or(0, Vec::len);
        let image = self
            .active_chat
            .as_ref()
            .is_some_and(|chat| self.pending_clipboard_images.contains_key(chat));
        files + usize::from(image)
    }

    fn can_attach(&self) -> bool {
        self.editing.is_none()
            && self.pending_send.is_none()
            && self
                .active_chat
                .as_deref()
                .and_then(|id| self.chat_snapshots.iter().find(|chat| chat.id == id))
                .is_some_and(crate::model::Chat::can_send)
    }

    fn sync_transcript(&mut self) {
        let rows = self
            .message_ids
            .iter()
            .filter_map(|id| self.message_snapshots.get(id))
            .map(transcript_row)
            .collect();
        self.transcript = rows;
        if self
            .selected_voice_message
            .as_ref()
            .is_some_and(|id| !self.message_snapshots.contains_key(id))
        {
            self.selected_voice = None;
            self.selected_voice_message = None;
        }
    }

    fn rebuild_message_rows(&mut self) {
        let unread = self
            .active_chat
            .as_deref()
            .and_then(|chat_id| self.chat_snapshots.iter().find(|chat| chat.id == chat_id))
            .map_or(0, |chat| chat.unread as usize);
        let messages = self
            .message_ids
            .iter()
            .filter_map(|id| self.message_snapshots.get(id).cloned())
            .collect::<Vec<_>>();
        let timestamps = messages
            .iter()
            .map(|message| message.timestamp)
            .collect::<Vec<_>>();
        let prefixes = conversation_prefixes(&timestamps, unread);
        let boundaries = message_group_boundaries(&messages);
        let rows = messages
            .into_iter()
            .enumerate()
            .map(|(index, message)| {
                let audio = self.project_voice(&message);
                let (show_sender, show_timestamp) = boundaries[index];
                let avatar = (!message.from_me)
                    .then(|| self.avatars.get(&message.sender).cloned())
                    .flatten();
                let mut row = message_row(
                    message,
                    self.pointer_sender.clone(),
                    &prefixes[index],
                    avatar,
                    show_sender,
                    show_timestamp,
                    self.audio_registry.clone(),
                );
                row.audio = audio;
                row
            })
            .collect::<Vec<_>>();
        let at_bottom = self.messages.view.vadjustment().is_none_or(|adjustment| {
            adjustment.value() + adjustment.page_size() >= adjustment.upper() - 48.0
        });
        // Rebinding only the changed span keeps the reader's scroll position
        // through receipts, reactions, and incoming messages.
        let old_len = self.messages.len() as usize;
        let (prefix, removed, inserted) = changed_span(old_len, &rows, |position, row| {
            self.messages
                .get(position as u32)
                .is_some_and(|item| item.borrow().renders_like(row))
        });
        let count = rows.len();
        if removed + inserted > old_len.max(count) / 2 {
            self.messages.clear();
            self.messages.extend_from_iter(rows);
        } else {
            for _ in 0..removed {
                self.messages.remove(prefix as u32);
            }
            for (offset, row) in rows.into_iter().skip(prefix).take(inserted).enumerate() {
                self.messages.insert((prefix + offset) as u32, row);
            }
        }
        // Follow the conversation only when the reader is already at its end.
        if at_bottom && count > 0 && (removed > 0 || inserted > 0) {
            scroll_to_end(&self.messages.view);
        }
    }

    fn copy_transcript(&mut self) {
        if let Some(text) = crate::native_transcript::copied_text(&self.transcript) {
            crate::native_portals::NativePortals::write_clipboard_text(
                &self.window.clipboard(),
                &text,
            );
            self.status = "Transcript copied".into();
        }
    }

    fn queue_waveforms(&mut self) {
        let Some(chat) = &self.active_chat else {
            return;
        };
        for id in &self.message_ids {
            let Some(message) = self.message_snapshots.get(id) else {
                continue;
            };
            let crate::model::Content::Audio {
                media, waveform, ..
            } = &message.content
            else {
                continue;
            };
            let Some(path) = &media.path else { continue };
            if !waveform.is_empty()
                || self
                    .audio_waveforms
                    .contains_key(&(chat.clone(), id.clone()))
                || self
                    .waveform_attempted
                    .contains(&(chat.clone(), id.clone()))
                || self
                    .waveform_queue
                    .iter()
                    .any(|(queued_chat, queued_id, _)| queued_chat == chat && queued_id == id)
                || !path.is_file()
            {
                continue;
            }
            self.waveform_queue
                .push_back((chat.clone(), id.clone(), path.clone()));
        }
        self.pump_waveforms(&self.pointer_sender.clone());
    }

    fn pump_waveforms(&mut self, sender: &ComponentSender<Self>) {
        if self.waveform_busy {
            return;
        }
        while let Some((chat, id, path)) = self.waveform_queue.pop_front() {
            if self.active_chat.as_deref() != Some(&chat)
                || self
                    .audio_waveforms
                    .contains_key(&(chat.clone(), id.clone()))
            {
                continue;
            }
            let sender = sender.clone();
            self.waveform_busy = true;
            self.waveform_attempted.insert((chat.clone(), id.clone()));
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            self.waveform_cancel = Some(cancel.clone());
            if std::thread::Builder::new()
                .name("audio-waveform".into())
                .spawn(move || {
                    let bars = crate::audio::waveform_file_cancellable(&path, &cancel).ok();
                    sender.input(Input::AudioWaveformReady { chat, id, bars });
                })
                .is_err()
            {
                self.waveform_busy = false;
                self.waveform_cancel = None;
            }
            break;
        }
    }

    fn audio_control(
        &mut self,
        id: &str,
        intent: crate::native_voice::VoiceIntent,
        sender: &ComponentSender<Self>,
    ) {
        let Some(audio_message) = self
            .message_snapshots
            .get(id)
            .cloned()
            .filter(|message| self.active_chat.as_deref() == Some(&message.chat))
        else {
            return;
        };
        let Some(voice) = self.project_voice(&audio_message) else {
            return;
        };
        let Some(action) = voice.action(intent) else {
            return;
        };
        match action {
            crate::model::Action::Download { chat, message } => {
                if let Some(backend) = &self.backend {
                    if let Some(media) = self
                        .message_snapshots
                        .get_mut(&message)
                        .and_then(|message| message.content.media_mut())
                    {
                        media.state = crate::model::MediaState::Downloading;
                    }
                    backend.send(crate::backend::Command::Download {
                        chat,
                        message: message.clone(),
                    });
                    self.refresh_message_row(&message);
                }
            }
            crate::model::Action::PlayVoice { message, path } => {
                let voice_note = matches!(
                    self.message_snapshots
                        .get(&message)
                        .map(|message| &message.content),
                    Some(crate::model::Content::Audio {
                        voice_note: true,
                        ..
                    })
                );
                self.media.set_speed(if voice_note {
                    self.settings.voice_speed
                } else {
                    1.0
                });
                self.audio_errors
                    .remove(&(audio_message.chat.clone(), message.clone()));
                let previous = self.playing_audio.replace(message.clone());
                if let Err(error) = self.media.toggle_playback(&message, &path) {
                    self.audio_errors.insert(
                        (
                            self.active_chat.clone().unwrap_or_default(),
                            message.clone(),
                        ),
                        error,
                    );
                }
                if let Some(previous) = previous {
                    self.refresh_message_row(&previous);
                }
                self.refresh_message_row(&message);
                self.schedule_voice_poll(sender);
            }
            crate::model::Action::SeekVoice {
                message,
                path,
                fraction,
            } => {
                if matches!(
                    audio_message.content,
                    crate::model::Content::Audio {
                        voice_note: false,
                        ..
                    }
                ) {
                    self.media.set_speed(1.0);
                }
                let previous = self.playing_audio.replace(message.clone());
                if let Err(error) = self.media.seek(&message, &path, fraction) {
                    self.audio_errors.insert(
                        (
                            self.active_chat.clone().unwrap_or_default(),
                            message.clone(),
                        ),
                        error,
                    );
                }
                if let Some(previous) = previous {
                    self.refresh_message_row(&previous);
                }
                self.refresh_message_row(&message);
                self.schedule_voice_poll(sender);
            }
            crate::model::Action::CycleVoiceSpeed => {
                self.settings.voice_speed = self.media.cycle_speed();
                if self.settings.save(&self.settings_path).is_err() {
                    self.status = "Could not save voice playback speed".into();
                }
                if let Some(id) = self.playing_audio.clone() {
                    self.refresh_message_row(&id);
                }
            }
            _ => {}
        }
    }

    fn project_voice(
        &self,
        message: &crate::model::Message,
    ) -> Option<crate::native_voice::VoiceMessage> {
        let crate::model::Content::Audio {
            media,
            seconds,
            voice_note,
            waveform,
        } = &message.content
        else {
            return None;
        };
        if message.from_me && !voice_note {
            return None;
        }
        let mut projected = crate::native_voice::project(crate::native_voice::VoiceMessageInput {
            chat: &message.chat,
            message: &message.id,
            media,
            seconds: *seconds,
            waveform,
            generated_waveform: self
                .audio_waveforms
                .get(&(message.chat.clone(), message.id.clone()))
                .map(Vec::as_slice)
                .or_else(|| self.media.waveform(&message.id)),
            playback: self.media.playback_status(&message.id),
            speed: if *voice_note { self.media.speed() } else { 1.0 },
        });
        if let Some(error) = self
            .audio_errors
            .get(&(message.chat.clone(), message.id.clone()))
        {
            projected.error = Some(error.clone());
        }
        Some(projected)
    }

    fn refresh_selected_voice(&mut self) {
        self.selected_voice = self
            .selected_voice_message
            .as_ref()
            .and_then(|id| self.message_snapshots.get(id))
            .and_then(|message| self.project_voice(message));
        if self.selected_voice.is_none() {
            self.selected_voice_message = None;
        }
    }

    fn activate_voice(&mut self, sender: &ComponentSender<Self>) {
        let Some(action) = self
            .selected_voice
            .as_ref()
            .and_then(|voice| voice.action(crate::native_voice::VoiceIntent::Activate))
        else {
            return;
        };
        match action {
            crate::model::Action::Download { chat, message } => {
                if let Some(backend) = &self.backend {
                    if let Some(media) = self
                        .message_snapshots
                        .get_mut(&message)
                        .and_then(|message| message.content.media_mut())
                    {
                        media.state = crate::model::MediaState::Downloading;
                    }
                    backend.send(crate::backend::Command::Download { chat, message });
                    self.status = "Downloading voice".into();
                    self.refresh_selected_voice();
                }
            }
            crate::model::Action::PlayVoice { message, path } => {
                if self.media.toggle_playback(&message, &path).is_err() {
                    self.status = "Voice playback could not start.".into();
                } else if self.media.is_playing() {
                    self.playing_audio = Some(message.clone());
                }
                self.refresh_selected_voice();
                self.schedule_voice_poll(sender);
            }
            _ => {}
        }
    }

    fn tell_played(&mut self, id: &str) {
        let Some((chat, message)) = self
            .active_chat
            .as_ref()
            .zip(self.message_snapshots.get(id))
            .filter(|(_, message)| {
                !message.from_me
                    && matches!(
                        message.content,
                        crate::model::Content::Audio {
                            voice_note: true,
                            ..
                        }
                    )
            })
            .map(|(chat, message)| (chat.clone(), message.clone()))
        else {
            return;
        };
        if !self.played_voice.insert((chat.clone(), message.id.clone())) {
            return;
        }
        if let Some(backend) = &self.backend {
            backend.send(crate::backend::Command::MarkPlayed {
                chat,
                message: message.id,
                sender: message.sender,
                receipts: self.settings.send_read_receipts && !self.account_receipts_off,
            });
        }
    }

    fn cycle_voice_speed(&mut self) {
        let Some(action) = self
            .selected_voice
            .as_ref()
            .and_then(|voice| voice.action(crate::native_voice::VoiceIntent::CycleSpeed))
        else {
            return;
        };
        if matches!(action, crate::model::Action::CycleVoiceSpeed) {
            self.settings.voice_speed = self.media.cycle_speed();
            if let Err(_error) = self.settings.save(&self.settings_path) {
                self.status = "Could not save voice playback speed".into();
            }
            self.refresh_selected_voice();
        }
    }

    fn schedule_voice_poll(&self, sender: &ComponentSender<Self>) {
        if self.media.is_playing() || self.media.is_recording() {
            let sender = sender.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || {
                sender.input(Input::PollVoice);
            });
        }
    }

    fn request_shutdown(&mut self, sender: ComponentSender<Self>) {
        if std::mem::replace(&mut self.shutdown_started, true) {
            return;
        }
        self._event_drain.close();
        self.portals.cancel_all();
        if let Err(_error) = self.settings.save(&self.settings_path) {
            self.status = "Could not save preferences".into();
        }
        self.status = "Shutdown requested".into();
        if let Some(mut backend) = self.backend.take() {
            std::thread::spawn(move || {
                backend.shutdown();
                sender.input(Input::ShutdownComplete);
            });
        } else {
            sender.input(Input::ShutdownComplete);
        }
    }

    fn present_quit_confirmation_dialog(&self, sender: ComponentSender<Self>) {
        use libadwaita::prelude::*;

        let alert = adw::AlertDialog::new(
            Some("Quit ZapTide?"),
            Some(
                "This build cannot keep ZapTide running in the background. \
                 Closing the window will quit the app.",
            ),
        );
        alert.add_response("cancel", "Cancel");
        alert.add_response("quit", "Quit");
        alert.set_response_appearance("quit", adw::ResponseAppearance::Destructive);
        alert.set_close_response("cancel");
        alert.set_default_response(Some("cancel"));
        alert.connect_response(None, move |_, response| {
            if response == "quit" {
                sender.input(Input::Quit);
            }
        });
        alert.present(Some(&self.window));
    }

    fn toast(&self, text: &str) {
        if let Some(child) = gtk::prelude::GtkWindowExt::child(&self.window)
            && let Ok(overlay) = child.downcast::<adw::ToastOverlay>()
        {
            overlay.add_toast(adw::Toast::new(text));
        }
    }
}

fn is_edit_completion(pending: Option<&(String, String, String)>, chat: &str, id: &str) -> bool {
    pending.is_some_and(|(pending_chat, pending_id, _)| pending_chat == chat && pending_id == id)
}

fn attachment_summary(count: usize) -> String {
    format!(
        "{count} attachment{} ready",
        if count == 1 { "" } else { "s" }
    )
}

fn caption(text: &str) -> Option<String> {
    Some(text.trim().to_owned()).filter(|text| !text.is_empty())
}

fn convert_premultiplied_bgra_to_rgba(pixels: &mut [u8]) {
    let (pixels, _) = pixels.as_chunks_mut::<4>();
    for pixel in pixels {
        let alpha = u16::from(pixel[3]);
        let channels = [pixel[2], pixel[1], pixel[0]];
        for (slot, channel) in pixel[..3].iter_mut().zip(channels) {
            *slot = (u16::from(channel) * 255 + alpha / 2)
                .checked_div(alpha)
                .unwrap_or_default()
                .min(255) as u8;
        }
    }
}

fn sanitized_error_feedback(error: &str) -> &'static str {
    let error = error.to_ascii_lowercase();
    if error.contains("timeout") || error.contains("timed out") || error.contains("connect") {
        "Connection failed. Check your network and try again."
    } else if error.contains("permission") || error.contains("denied") {
        "Permission denied. Check access and try again."
    } else if error.contains("disk") || error.contains("storage") || error.contains("space") {
        "Storage operation failed. Check available disk space."
    } else {
        "Action failed. Check connection and try again."
    }
}

#[cfg(feature = "demo")]
fn synthetic_e2e_enabled() -> bool {
    std::env::var_os("ZAPTIDE_NATIVE_SYNTHETIC").as_deref() == Some(std::ffi::OsStr::new("1"))
}

/// Replays a history-sync sized burst: `count` chats, twenty rounds of
/// updates, and an avatar per chat, paced like the worker sends them.
#[cfg(feature = "demo")]
fn synthetic_flood(
    events: std::sync::mpsc::Sender<crate::backend::Event>,
    notifier: EventNotifier,
    count: usize,
) {
    std::thread::spawn(move || {
        let avatar = std::env::temp_dir().join("zaptide-synthetic-avatar.png");
        let _ =
            image::RgbaImage::from_pixel(96, 96, image::Rgba([40, 120, 200, 255])).save(&avatar);
        let chat = |index: usize, activity: i64| {
            let mut chat = crate::model::Chat::new(
                format!("{index}@s.whatsapp.net"),
                format!("Synthetic {index}"),
            );
            chat.last_activity = activity;
            chat
        };
        let _ = events.send(crate::backend::Event::Chats(
            (0..count).map(|index| chat(index, index as i64)).collect(),
        ));
        for round in 0..20 {
            for index in 0..count {
                let _ = events.send(crate::backend::Event::ChatUpdated(Box::new(chat(
                    index,
                    (round * count + index) as i64,
                ))));
                if round == 0 {
                    let _ = events.send(crate::backend::Event::Avatar {
                        id: format!("{index}@s.whatsapp.net"),
                        full: false,
                        path: Some(avatar.clone()),
                    });
                }
                if index % 50 == 0 {
                    crate::backend::Wake::wake(&notifier);
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        }
        crate::backend::Wake::wake(&notifier);
        eprintln!("synthetic flood sent");
    });
}

#[cfg(feature = "demo")]
fn synthetic_message() -> crate::model::Message {
    crate::model::Message {
        id: "synthetic-message-001".into(),
        chat: "synthetic-contact@s.whatsapp.net".into(),
        sender: "synthetic-contact@s.whatsapp.net".into(),
        sender_name: Some("Synthetic Contact".into()),
        from_me: false,
        timestamp: 1_700_000_002,
        content: crate::model::Content::Text {
            text: "Offline link preview sample".into(),
            preview: Some(crate::model::LinkPreview {
                url: "https://example.invalid/synthetic".into(),
                title: Some("Synthetic preview".into()),
                description: Some("Generated locally for native UI testing".into()),
            }),
        },
        status: crate::model::Delivery::None,
        delivered_at: None,
        read_at: None,
        quoted: None,
        reactions: Vec::new(),
        edited: false,
        mentions: Vec::new(),
        forwarded: false,
        thumbnail: None,
    }
}

#[cfg(feature = "demo")]
fn synthetic_attachment_message() -> crate::model::Message {
    crate::model::Message {
        id: "synthetic-message-002".into(),
        chat: "synthetic-contact@s.whatsapp.net".into(),
        sender: "synthetic-contact@s.whatsapp.net".into(),
        sender_name: Some("Synthetic Contact".into()),
        from_me: false,
        timestamp: 1_700_000_003,
        content: crate::model::Content::Image {
            media: crate::model::Media {
                mime: "image/png".into(),
                size: 1_024,
                width: Some(16),
                height: Some(16),
                path: None,
                state: crate::model::MediaState::Idle,
            },
            caption: Some("Synthetic preview image".into()),
        },
        status: crate::model::Delivery::None,
        delivered_at: None,
        read_at: None,
        quoted: None,
        reactions: Vec::new(),
        edited: false,
        mentions: Vec::new(),
        forwarded: false,
        thumbnail: Some(vec![0; 64]),
    }
}

#[cfg(feature = "demo")]
fn synthetic_audio_message(voice_note: bool) -> crate::model::Message {
    let mut message = synthetic_message();
    message.id = if voice_note {
        "synthetic-voice"
    } else {
        "synthetic-audio"
    }
    .into();
    message.timestamp += if voice_note { 4 } else { 5 };
    message.content = crate::model::Content::Audio {
        media: crate::model::Media {
            mime: if voice_note {
                "audio/ogg"
            } else {
                "audio/mpeg"
            }
            .into(),
            size: 8_192,
            width: None,
            height: None,
            path: None,
            state: crate::model::MediaState::Idle,
        },
        seconds: Some(14),
        voice_note,
        waveform: (0..crate::voice::BARS)
            .map(|bar| (bar * 13 % 80 + 12) as u8)
            .collect(),
    };
    message
}

#[cfg(feature = "demo")]
fn synthetic_older_message(chat: &str) -> crate::model::Message {
    let mut message = synthetic_message();
    message.id = "synthetic-message-older".into();
    message.chat = chat.to_owned();
    message.timestamp = 1_700_000_001;
    message.content = crate::model::Content::text("Earlier synthetic history row");
    message
}

fn install_message_actions(
    window: &adw::ApplicationWindow,
    sender: &ComponentSender<NativeApplication>,
) {
    let group = gtk::gio::SimpleActionGroup::new();
    type MessageAction = (&'static str, fn() -> Input);
    let actions: [MessageAction; 10] = [
        ("copy", || Input::CopySelectedText),
        ("attachment", || Input::ActivateSelectedAttachment),
        ("save", || Input::SaveSelectedAttachment),
        ("open-link", || Input::OpenSelectedUri),
        ("quoted", || Input::OpenQuoted),
        ("reply", || Input::ReplySelected),
        ("edit", || Input::EditSelected),
        ("forward", || Input::ShowForward),
        ("delete", || Input::DeleteSelected(false)),
        ("delete-everyone", || Input::DeleteSelected(true)),
    ];
    for (name, input) in actions {
        let action = gtk::gio::SimpleAction::new(name, None);
        let sender = sender.clone();
        action.connect_activate(move |_, _| sender.input(input()));
        group.add_action(&action);
    }
    let vote = gtk::gio::SimpleAction::new("vote", Some(gtk::glib::VariantTy::UINT32));
    let vote_sender = sender.clone();
    vote.connect_activate(move |_, choice| {
        if let Some(choice) = choice.and_then(gtk::glib::Variant::get::<u32>) {
            vote_sender.input(Input::VoteOption(choice as usize));
        }
    });
    group.add_action(&vote);
    window.insert_action_group("message", Some(&group));
}

fn install_window_actions(
    window: &adw::ApplicationWindow,
    sender: &ComponentSender<NativeApplication>,
) {
    let add = |name: &str, activate: Box<dyn Fn()>| {
        let action = gtk::gio::SimpleAction::new(name, None);
        action.connect_activate(move |_, _| activate());
        window.add_action(&action);
    };
    let input = |input: fn() -> Input| -> Box<dyn Fn()> {
        let sender = sender.clone();
        Box::new(move || sender.input(input()))
    };
    add("preferences", input(|| Input::ShowPreferences));
    add("shortcuts", input(|| Input::ShowShortcuts));
    add("about", input(|| Input::ShowAbout));
    add("quit", input(|| Input::Quit));
    let (parent, dialog_sender) = (window.clone(), sender.clone());
    add(
        "new-contact",
        Box::new(move || show_new_contact_dialog(&parent, &dialog_sender)),
    );
    let (parent, dialog_sender) = (window.clone(), sender.clone());
    add(
        "unlink",
        Box::new(move || show_unlink_confirmation(&parent, &dialog_sender)),
    );
}

fn show_poll_dialog(parent: &adw::ApplicationWindow, sender: &ComponentSender<NativeApplication>) {
    let dialog = gtk::Window::builder()
        .title("Create poll")
        .transient_for(parent)
        .modal(true)
        .default_width(360)
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    let question = gtk::Entry::builder().placeholder_text("Question").build();
    let first = gtk::Entry::builder()
        .placeholder_text("First option")
        .build();
    let second = gtk::Entry::builder()
        .placeholder_text("Second option")
        .build();
    let buttons = gtk::Box::builder()
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    let create = gtk::Button::with_label("Create");
    create.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&create);
    content.append(&question);
    content.append(&first);
    content.append(&second);
    content.append(&buttons);
    dialog.set_child(Some(&content));
    let close_dialog = dialog.clone();
    cancel.connect_clicked(move |_| close_dialog.close());
    let sender = sender.clone();
    let close_dialog = dialog.clone();
    create.connect_clicked(move |_| {
        sender.input(Input::CreatePoll {
            question: question.text().to_string(),
            first: first.text().to_string(),
            second: second.text().to_string(),
        });
        close_dialog.close();
    });
    dialog.present();
}

fn show_new_contact_dialog(
    parent: &adw::ApplicationWindow,
    sender: &ComponentSender<NativeApplication>,
) {
    let dialog = gtk::Window::builder()
        .title("New contact")
        .transient_for(parent)
        .modal(true)
        .default_width(380)
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    let phone = gtk::Entry::builder()
        .placeholder_text("Phone number, including country code")
        .input_purpose(gtk::InputPurpose::Phone)
        .activates_default(true)
        .build();
    let name = gtk::Entry::builder()
        .placeholder_text("Name (optional)")
        .activates_default(true)
        .build();
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    let add = gtk::Button::with_label("Add contact");
    add.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&add);
    content.append(&phone);
    content.append(&name);
    content.append(&buttons);
    dialog.set_child(Some(&content));
    dialog.set_default_widget(Some(&add));
    let close = dialog.clone();
    cancel.connect_clicked(move |_| close.close());
    let close = dialog.clone();
    let sender = sender.clone();
    let phone_input = phone.clone();
    add.connect_clicked(move |_| {
        sender.input(Input::NewContact {
            phone: phone_input.text().to_string(),
            name: Some(name.text().to_string()),
        });
        close.close();
    });
    phone.connect_map(|entry| {
        entry.grab_focus();
    });
    dialog.present();
}

fn show_forward_dialog(
    parent: &adw::ApplicationWindow,
    sender: &ComponentSender<NativeApplication>,
    chats: Vec<crate::model::Chat>,
) {
    let dialog = gtk::Window::builder()
        .title("Forward message")
        .transient_for(parent)
        .modal(true)
        .default_width(400)
        .default_height(480)
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search chats")
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .min_content_height(180)
        .build();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    let rows = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));

    for chat in chats {
        let title = gtk::Label::builder()
            .label(&chat.name)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let summary = forward_chat_detail(&chat);
        let subtitle = gtk::Label::builder()
            .label(&summary)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        subtitle.add_css_class("dim-label");
        subtitle.add_css_class("caption");
        let labels = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(3)
            .hexpand(true)
            .build();
        labels.append(&title);
        labels.append(&subtitle);
        let button = gtk::Button::builder()
            .child(&labels)
            .hexpand(true)
            .halign(gtk::Align::Fill)
            .build();
        let accessible_name = forward_accessible_label(&chat);
        button.update_property(&[gtk::accessible::Property::Label(&accessible_name)]);
        let row = gtk::ListBoxRow::builder().child(&button).build();
        let close = dialog.clone();
        let input = sender.clone();
        let destination = chat.id.clone();
        button.connect_clicked(move |_| {
            input.input(Input::ForwardSelected(destination.clone()));
            close.close();
        });
        rows.borrow_mut()
            .push((row.clone(), forward_search_key(&chat)));
        list.append(&row);
    }

    let empty = gtk::Label::builder()
        .label(if rows.borrow().is_empty() {
            "No available chats to forward to."
        } else {
            "No chats match this search."
        })
        .wrap(true)
        .margin_top(8)
        .margin_bottom(8)
        .build();
    empty.add_css_class("dim-label");
    empty.set_visible(rows.borrow().is_empty());
    let filter_rows = rows.clone();
    let no_matches = empty.clone();
    search.connect_search_changed(move |entry| {
        let needle = entry.text().trim().to_lowercase();
        let mut any_visible = false;
        for (row, searchable) in filter_rows.borrow().iter() {
            let visible = needle.is_empty() || searchable.contains(&needle);
            row.set_visible(visible);
            any_visible |= visible;
        }
        no_matches.set_visible(!any_visible);
    });
    scroll.set_child(Some(&list));
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    buttons.append(&cancel);
    let close = dialog.clone();
    cancel.connect_clicked(move |_| close.close());
    content.append(&search);
    content.append(&scroll);
    content.append(&empty);
    content.append(&buttons);
    dialog.set_child(Some(&content));
    dialog.set_default_widget(Some(&search));
    search.connect_map(|entry| {
        entry.grab_focus();
    });
    dialog.present();
}

fn forwardable_chat(chat: &crate::model::Chat) -> bool {
    chat.kind != crate::model::ChatKind::Broadcast && chat.can_send()
}

thread_local! {
    /// Small sticker previews by file, so picker rebuilds draw instantly.
    static STICKER_TEXTURES: std::cell::RefCell<
        std::collections::HashMap<std::path::PathBuf, gtk::gdk::Texture>,
    > = std::cell::RefCell::default();
}

/// Loads a sticker preview into `button` off the main thread, once mapped.
fn load_sticker_preview(button: &gtk::Button, path: &std::path::Path, size: i32) {
    let show = move |button: &gtk::Button, texture: Option<&gtk::gdk::Texture>| {
        let image = match texture {
            Some(texture) => gtk::Image::from_paintable(Some(texture)),
            None => gtk::Image::from_icon_name("image-missing-symbolic"),
        };
        image.set_pixel_size(size);
        button.set_child(Some(&image));
    };
    if let Some(texture) = STICKER_TEXTURES.with_borrow(|cache| cache.get(path).cloned()) {
        show(button, Some(&texture));
        return;
    }
    button.set_child(Some(&adw::Spinner::new()));
    let path = path.to_path_buf();
    let started = std::cell::Cell::new(false);
    button.connect_map(move |button| {
        if started.replace(true) {
            return;
        }
        let path = path.clone();
        let button = button.downgrade();
        gtk::glib::spawn_future_local(async move {
            let source = path.clone();
            // Decoded to a small RGBA preview; full-size WebP textures made the
            // picker slow and heavy.
            let decoded = gtk::gio::spawn_blocking(move || {
                let bytes = std::fs::read(&source).ok()?;
                let image = image::load_from_memory(&bytes)
                    .ok()?
                    .thumbnail(144, 144)
                    .to_rgba8();
                Some((image.width(), image.height(), image.into_raw()))
            })
            .await
            .ok()
            .flatten();
            let texture = decoded.map(|(width, height, rgba)| {
                gtk::gdk::MemoryTexture::new(
                    width as i32,
                    height as i32,
                    gtk::gdk::MemoryFormat::R8g8b8a8,
                    &gtk::glib::Bytes::from_owned(rgba),
                    width as usize * 4,
                )
                .upcast::<gtk::gdk::Texture>()
            });
            if let Some(texture) = &texture {
                STICKER_TEXTURES.with_borrow_mut(|cache| {
                    // ponytail: wholesale reset at ~40 MB of previews; an LRU if
                    // large libraries make reopening noticeably slower.
                    if cache.len() >= 500 {
                        cache.clear();
                    }
                    cache.insert(path, texture.clone());
                });
            }
            if let Some(button) = button.upgrade() {
                show(&button, texture.as_ref());
            }
        });
    });
}

/// Plays an animated sticker while the pointer is over its picker button,
/// and puts the still preview back when it leaves.
fn animate_sticker_on_hover(button: &gtk::Button, path: &std::path::Path) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let hovering = std::sync::Arc::new(AtomicBool::new(false));
    // Known after the first decode; still stickers are not decoded again.
    let still = std::rc::Rc::new(std::cell::Cell::new(false));
    let playing: std::rc::Rc<
        std::cell::RefCell<
            Option<(
                crate::native_media_widgets::StickerAnimation,
                gtk::gdk::Paintable,
            )>,
        >,
    > = Default::default();
    let hover = gtk::EventControllerMotion::new();
    {
        let (hovering, playing, path) = (hovering.clone(), playing.clone(), path.to_path_buf());
        let button = button.downgrade();
        hover.connect_enter(move |_, _, _| {
            if still.get() || hovering.swap(true, Ordering::AcqRel) {
                return;
            }
            let (hovering, playing, still, path) = (
                hovering.clone(),
                playing.clone(),
                still.clone(),
                path.clone(),
            );
            let button = button.clone();
            gtk::glib::spawn_future_local(async move {
                let current = hovering.clone();
                // ponytail: decoded again on every hover, and dropped on leave, so
                // an open picker holds one sticker's frames at most.
                let frames = gtk::gio::spawn_blocking(move || {
                    crate::native_media_widgets::decode_sticker_file(&path, || {
                        current.load(Ordering::Acquire)
                    })
                })
                .await
                .ok()
                .flatten();
                // Undecodable while still pointed at, not cancelled: treat as still.
                let Some(frames) = frames else {
                    still.set(hovering.load(Ordering::Acquire));
                    return;
                };
                if frames.len() < 2 {
                    still.set(true);
                    return;
                }
                let Some(image) = button
                    .upgrade()
                    .and_then(|button| button.child())
                    .and_downcast::<gtk::Image>()
                else {
                    return;
                };
                // A quick leave and return starts a second decode; the first
                // to finish plays.
                if !hovering.load(Ordering::Acquire) || playing.borrow().is_some() {
                    return;
                }
                let Some(preview) = image.paintable() else {
                    return;
                };
                let animation = crate::native_media_widgets::StickerAnimation::new(&image, frames);
                animation.play(None);
                *playing.borrow_mut() = Some((animation, preview));
            });
        });
    }
    // Closing the picker under the pointer sends no leave; stop there too.
    let rest = std::rc::Rc::new(move |button: &gtk::Button| {
        hovering.store(false, Ordering::Release);
        if let Some((animation, preview)) = playing.borrow_mut().take() {
            animation.stop();
            if let Some(image) = button.child().and_downcast::<gtk::Image>() {
                image.set_paintable(Some(&preview));
            }
        }
    });
    {
        let rest = rest.clone();
        hover.connect_leave(move |controller| {
            if let Some(button) = controller.widget().and_downcast::<gtk::Button>() {
                rest(&button);
            }
        });
    }
    button.connect_unmap(move |button| rest(button));
    button.add_controller(hover);
}

/// Text a message shows, if any: its body or a media caption.
fn message_text(message: &crate::model::Message) -> Option<String> {
    let text = match &message.content {
        crate::model::Content::Text { text, .. } => Some(text),
        crate::model::Content::Image { caption, .. }
        | crate::model::Content::Video { caption, .. }
        | crate::model::Content::Document { caption, .. } => caption.as_ref(),
        _ => None,
    };
    text.filter(|text| !text.trim().is_empty()).cloned()
}

/// WhatsApp-style quick reactions above the message menu. Picking the
/// reaction already sent removes it; "+" opens the full emoji chooser.
fn reaction_bar(
    menu: &gtk::PopoverMenu,
    parent: &gtk::Widget,
    rect: gtk::gdk::Rectangle,
    current: Option<String>,
    sender: &ComponentSender<NativeApplication>,
) -> gtk::Box {
    let bar = gtk::Box::builder()
        .spacing(2)
        .css_classes(["zaptide-reactions"])
        .build();
    for emoji in ["👍", "❤️", "😂", "😮", "😢", "🙏"] {
        let chosen = current.as_deref() == Some(emoji);
        let button = gtk::Button::builder()
            .label(emoji)
            .tooltip_text(if chosen { "Remove reaction" } else { emoji })
            .css_classes(["flat", "circular", "zaptide-reaction"])
            .build();
        if chosen {
            button.add_css_class("chosen");
        }
        let (menu, sender) = (menu.clone(), sender.clone());
        let emoji = if chosen {
            String::new()
        } else {
            emoji.to_owned()
        };
        button.connect_clicked(move |_| {
            menu.popdown();
            sender.input(Input::ReactSelected(emoji.clone()));
        });
        bar.append(&button);
    }
    let more = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("More Reactions")
        .css_classes(["flat", "circular", "zaptide-reaction"])
        .build();
    let (menu, parent, sender) = (menu.clone(), parent.clone(), sender.clone());
    more.connect_clicked(move |_| {
        menu.popdown();
        let chooser = gtk::EmojiChooser::new();
        chooser.set_parent(&parent);
        chooser.set_pointing_to(Some(&rect));
        let sender = sender.clone();
        chooser.connect_emoji_picked(move |_, emoji| {
            sender.input(Input::ReactSelected(emoji.to_owned()));
        });
        chooser.connect_closed(|chooser| {
            let chooser = chooser.clone();
            gtk::glib::idle_add_local_once(move || chooser.unparent());
        });
        chooser.popup();
    });
    bar.append(&more);
    bar
}

/// Sticker pages over a bottom row of page buttons, like the phone's picker.
fn sticker_picker_content(
    packs: &[crate::model::StickerPack],
    favorites: &[std::path::PathBuf],
    recent: &[std::path::PathBuf],
    page: Option<&str>,
    sender: &ComponentSender<NativeApplication>,
) -> (gtk::Box, gtk::Stack) {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_size_request(380, 440);
    let stack = gtk::Stack::builder()
        .vexpand(true)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    let tabs = gtk::Box::builder()
        .spacing(4)
        .margin_start(6)
        .margin_end(6)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    let mut first_tab: Option<gtk::ToggleButton> = None;

    let mut add_page =
        |name: String, title: &str, icon: Option<&str>, paths: &[std::path::PathBuf]| {
            let grid = gtk::FlowBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .homogeneous(true)
                .min_children_per_line(4)
                .max_children_per_line(4)
                .column_spacing(4)
                .row_spacing(4)
                .margin_start(8)
                .margin_end(8)
                .margin_top(8)
                .margin_bottom(8)
                .valign(gtk::Align::Start)
                .build();
            for path in paths {
                let button = gtk::Button::builder()
                    .css_classes(["flat", "zaptide-sticker"])
                    .tooltip_text("Send sticker")
                    .build();
                button.update_property(&[gtk::accessible::Property::Label("Sticker")]);
                load_sticker_preview(&button, path, 72);
                animate_sticker_on_hover(&button, path);
                let path = path.clone();
                let sender = sender.clone();
                button.connect_clicked(move |_| sender.input(Input::SendSticker(path.clone())));
                grid.append(&button);
            }
            let scroller = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&grid)
                .build();
            stack.add_titled(&scroller, Some(&name), title);

            let tab = gtk::ToggleButton::builder()
                .css_classes(["flat", "zaptide-sticker-tab"])
                .tooltip_text(title)
                .build();
            tab.update_property(&[gtk::accessible::Property::Label(title)]);
            match (icon, paths.first()) {
                (Some(icon), _) => tab.set_icon_name(icon),
                (None, Some(cover)) => load_sticker_preview(tab.upcast_ref(), cover, 28),
                (None, None) => tab.set_label(title),
            }
            match &first_tab {
                Some(first) => tab.set_group(Some(first)),
                None => {
                    tab.set_active(true);
                    first_tab = Some(tab.clone());
                }
            }
            {
                let stack = stack.clone();
                let name = name.clone();
                tab.connect_toggled(move |tab| {
                    if tab.is_active() {
                        stack.set_visible_child_name(&name);
                    }
                });
            }
            if page == Some(name.as_str()) {
                tab.set_active(true);
            }
            tabs.append(&tab);
        };

    if !recent.is_empty() {
        add_page(
            "recent".into(),
            "Recent",
            Some("document-open-recent-symbolic"),
            recent,
        );
    }
    if !favorites.is_empty() {
        add_page(
            "favorites".into(),
            "Favorites",
            Some("starred-symbolic"),
            favorites,
        );
    }
    for pack in packs {
        add_page(
            format!("pack:{}", pack.dir.display()),
            &pack.name,
            None,
            &pack.stickers,
        );
    }

    if first_tab.is_none() {
        let empty = adw::StatusPage::builder()
            .icon_name("emoji-nature-symbolic")
            .title("No Stickers Yet")
            .description("Stickers you send, receive, or save appear here.")
            .vexpand(true)
            .build();
        empty.add_css_class("compact");
        content.append(&empty);
        return (content, stack);
    }
    content.append(&stack);
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let tab_scroller = gtk::ScrolledWindow::builder()
        .vscrollbar_policy(gtk::PolicyType::Never)
        .child(&tabs)
        .build();
    content.append(&tab_scroller);
    (content, stack)
}

fn forward_search_key(chat: &crate::model::Chat) -> String {
    format!("{} {}", chat.name, chat.phone().unwrap_or_default()).to_lowercase()
}

fn forward_chat_detail(chat: &crate::model::Chat) -> String {
    if chat.is_group() {
        format!("Group · {} participants", chat.participants.len())
    } else {
        chat.phone().unwrap_or("Direct chat").to_owned()
    }
}

fn forward_accessible_label(chat: &crate::model::Chat) -> String {
    format!("Forward to {}, {}", chat.name, forward_chat_detail(chat))
}

fn show_chat_info_dialog(
    parent: &adw::ApplicationWindow,
    chat: &crate::model::Chat,
    contact: Option<&crate::model::Contact>,
) {
    let dialog = adw::AlertDialog::builder()
        .heading(chat.name.as_str())
        .body(format!(
            "{}\n{}",
            contact
                .and_then(crate::model::Contact::display_name)
                .unwrap_or(&chat.name),
            if chat.is_group() {
                format!("Group · {} participants", chat.participants.len())
            } else {
                chat.phone()
                    .unwrap_or("Phone number unavailable")
                    .to_owned()
            }
        ))
        .build();
    dialog.add_response("close", "Close");
    dialog.set_default_response(Some("close"));
    dialog.present(Some(parent));
}

fn show_unlink_confirmation(
    parent: &adw::ApplicationWindow,
    sender: &ComponentSender<NativeApplication>,
) {
    let dialog = adw::AlertDialog::builder()
        .heading("Unlink this computer?")
        .body(
            "This removes this device from your linked devices and clears its local conversations.",
        )
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("unlink", "Unlink");
    dialog.set_response_appearance("unlink", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let sender = sender.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "unlink" {
            sender.input(Input::UnlinkConfirmed);
        }
    });
    dialog.present(Some(parent));
}

/// Runs the native shell and starts the backend only after the first main-context turn.
pub fn run(dirs: AppDirs) {
    use gettextrs::{LocaleCategory, bindtextdomain, setlocale, textdomain};

    let app_id = "zaptide";
    let locale_dir = option_env!("ZAPTIDE_LOCALE_DIR").unwrap_or("/usr/share/locale");

    setlocale(LocaleCategory::LcAll, "");
    let _ = textdomain(app_id);
    let _ = bindtextdomain(app_id, locale_dir);
    #[cfg(feature = "demo")]
    let application_id = if synthetic_e2e_enabled() {
        format!("dev.luminusos.ZapTide.Synthetic.p{}", std::process::id())
    } else {
        std::env::var("FLATPAK_ID").unwrap_or_else(|_| "dev.luminusos.ZapTide".into())
    };
    #[cfg(not(feature = "demo"))]
    let application_id =
        std::env::var("FLATPAK_ID").unwrap_or_else(|_| "dev.luminusos.ZapTide".into());
    RelmApp::new(&application_id).run::<NativeApplication>(Init { dirs });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grouping_message(
        id: &str,
        sender: &str,
        from_me: bool,
        timestamp: i64,
    ) -> crate::model::Message {
        crate::model::Message {
            id: id.into(),
            chat: "chat".into(),
            sender: sender.into(),
            sender_name: Some(sender.into()),
            from_me,
            timestamp,
            content: crate::model::Content::text(id),
            status: crate::model::Delivery::None,
            delivered_at: None,
            read_at: None,
            quoted: None,
            reactions: Vec::new(),
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: None,
        }
    }

    #[test]
    fn chat_list_replaces_only_the_changed_span() {
        let span = |old: &[i32], new: &[i32]| {
            super::changed_span(old.len(), new, |position, row| old[position] == *row)
        };
        // A chat moving to the top rewrites only the rows above its old place.
        assert_eq!(span(&[1, 2, 3, 4, 5], &[4, 1, 2, 3, 5]), (0, 4, 4));
        assert_eq!(span(&[1, 2, 3], &[1, 2, 3]), (3, 0, 0));
        assert_eq!(span(&[], &[1, 2]), (0, 0, 2));
        assert_eq!(span(&[1, 2], &[]), (0, 2, 0));
        assert_eq!(span(&[1, 2, 3], &[1, 9, 3]), (1, 1, 1));
        // Repeated rows must not let prefix and suffix overlap.
        assert_eq!(span(&[1, 1], &[1, 1, 1]), (2, 0, 1));
        assert_eq!(span(&[1, 1, 1], &[1]), (1, 2, 0));
    }

    #[test]
    fn consecutive_messages_group_sender_and_timestamp_metadata() {
        let messages = vec![
            grouping_message("a1", "Alice", false, 1_700_000_000),
            grouping_message("a2", "Alice", false, 1_700_000_120),
            grouping_message("b1", "Bob", false, 1_700_000_180),
            grouping_message("a3", "Alice", false, 1_700_000_500),
        ];

        assert_eq!(
            message_group_boundaries(&messages),
            vec![(true, false), (false, true), (true, true), (true, true)]
        );
    }

    #[test]
    fn forward_chat_picker_filters_locked_and_broadcast_destinations() {
        let mut chat =
            crate::model::Chat::new("15551234567@s.whatsapp.net".into(), "Ada Lovelace".into());
        assert!(forwardable_chat(&chat));
        assert!(forward_search_key(&chat).contains("ada lovelace"));
        assert!(forward_search_key(&chat).contains("15551234567"));

        let same_name =
            crate::model::Chat::new("15557654321@s.whatsapp.net".into(), "Ada Lovelace".into());
        assert_ne!(
            forward_accessible_label(&chat),
            forward_accessible_label(&same_name),
            "duplicate chat names remain distinguishable to assistive technology"
        );

        chat.locked = true;
        assert!(!forwardable_chat(&chat));
        chat.locked = false;
        chat.kind = crate::model::ChatKind::Broadcast;
        assert!(!forwardable_chat(&chat));
    }

    #[test]
    fn only_own_text_messages_are_editable() {
        let mut message = crate::model::Message {
            id: "message".into(),
            chat: "chat".into(),
            sender: "me".into(),
            sender_name: None,
            from_me: true,
            timestamp: 0,
            content: crate::model::Content::text("hello"),
            status: Default::default(),
            delivered_at: None,
            read_at: None,
            quoted: None,
            reactions: Vec::new(),
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: None,
        };

        assert_eq!(editable_text(&message).as_deref(), Some("hello"));
        message.from_me = false;
        assert_eq!(editable_text(&message), None);
    }

    #[test]
    fn transcript_keeps_full_text_message_body() {
        let message = crate::model::Message {
            id: "message".into(),
            chat: "chat".into(),
            sender: "sender".into(),
            sender_name: Some("Contact".into()),
            from_me: false,
            timestamp: 0,
            content: crate::model::Content::text("first line\nsecond line"),
            status: Default::default(),
            delivered_at: None,
            read_at: None,
            quoted: None,
            reactions: Vec::new(),
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: None,
        };

        assert_eq!(transcript_text(&message), "first line\nsecond line");
    }

    #[test]
    fn edit_completion_matches_only_its_result() {
        let pending = ("chat".into(), "message".into(), "saved".into());

        assert!(is_edit_completion(Some(&pending), "chat", "message"));
        assert!(!is_edit_completion(Some(&pending), "chat", "other"));
        assert!(!is_edit_completion(Some(&pending), "other", "message"));
    }

    #[test]
    fn attachment_caption_omits_whitespace_and_keeps_text() {
        assert_eq!(caption("  "), None);
        assert_eq!(caption("  caption  ").as_deref(), Some("caption"));
    }

    #[test]
    fn native_preference_changes_round_trip_through_existing_settings_json() {
        let directory = tempfile::tempdir().expect("temporary settings directory");
        let path = directory.path().join("settings.json");
        let mut settings = crate::settings::Settings::default();
        crate::native_preferences::PreferenceChange::SetSendTyping(false)
            .apply(&mut settings)
            .expect("valid preference change");
        crate::native_preferences::PreferenceChange::SetNotificationPreviews(false)
            .apply(&mut settings)
            .expect("valid preference change");
        settings.save(&path).expect("persist preference");

        let loaded = crate::settings::Settings::load(&path);
        assert!(!loaded.send_typing);
        assert!(!loaded.notification_previews);
        assert!(crate::settings::Settings::default().notification_previews);
    }

    #[test]
    fn auto_download_requires_unfetched_idle_media_within_size_limit() {
        let media = crate::model::Media {
            mime: "image/jpeg".into(),
            size: 64 * 1024 * 1024,
            width: None,
            height: None,
            path: None,
            state: crate::model::MediaState::Idle,
        };
        assert!(should_auto_download(Some(&media)));

        let too_large = crate::model::Media {
            size: 64 * 1024 * 1024 + 1,
            ..media.clone()
        };
        assert!(!should_auto_download(Some(&too_large)));

        let already_downloading = crate::model::Media {
            state: crate::model::MediaState::Downloading,
            ..media.clone()
        };
        assert!(!should_auto_download(Some(&already_downloading)));

        let downloaded = crate::model::Media {
            path: Some("cached.jpg".into()),
            ..media
        };
        assert!(!should_auto_download(Some(&downloaded)));
        assert!(!should_auto_download(None));
    }

    #[test]
    fn enter_send_respects_setting_control_and_shift() {
        assert!(should_send_on_enter(true, false, false));
        assert!(should_send_on_enter(false, true, false));
        assert!(!should_send_on_enter(false, false, false));
        assert!(!should_send_on_enter(true, true, true));
    }

    #[test]
    fn pairing_and_contact_phone_numbers_normalize_to_international_digits() {
        assert_eq!(
            normalized_phone("+1 (202) 555-0137").as_deref(),
            Some("12025550137")
        );
        assert_eq!(normalized_phone("123456").as_deref(), None);
        assert_eq!(normalized_phone("1234567890123456").as_deref(), None);
    }

    #[test]
    fn notifications_skip_the_visible_muted_archived_and_locked_chats() {
        assert!(notification_should_show(
            true,
            true,
            Some("other"),
            "chat",
            None
        ));
        assert!(!notification_should_show(
            false,
            true,
            Some("other"),
            "chat",
            None
        ));
        assert!(!notification_should_show(
            true,
            true,
            Some("chat"),
            "chat",
            None
        ));
        assert!(notification_should_show(
            true,
            false,
            Some("chat"),
            "chat",
            None
        ));
        assert!(notification_should_show(true, true, None, "chat", None));
        let mut known = crate::model::Chat::new("chat".into(), "Chat".into());
        assert!(notification_should_show(
            true,
            true,
            None,
            "chat",
            Some(&known)
        ));
        known.muted_until = Some(crate::util::now() + 3600);
        assert!(!notification_should_show(
            true,
            true,
            None,
            "chat",
            Some(&known)
        ));
        known.muted_until = None;
        known.archived = true;
        assert!(!notification_should_show(
            true,
            true,
            None,
            "chat",
            Some(&known)
        ));
        known.archived = false;
        known.locked = true;
        assert!(!notification_should_show(
            true,
            true,
            None,
            "chat",
            Some(&known)
        ));
    }

    #[test]
    fn attachment_send_waits_for_every_completion() {
        let failed_path = std::path::PathBuf::from("failed.png");
        let mut pending = PendingSend {
            chat: "chat".into(),
            text: String::new(),
            reply: None,
            attachments: vec!["sent.png".into(), failed_path.clone()],
            failed_attachments: Vec::new(),
            clipboard_image: false,
            remaining: 2,
            failed: false,
        };

        assert!(!pending.complete_attachment("sent.png".into(), true));
        assert!(!pending.failed);
        assert!(pending.complete_attachment(failed_path.clone(), false));
        assert!(pending.failed);
        assert_eq!(pending.failed_attachments, vec![failed_path]);
    }

    #[test]
    fn backend_errors_are_reduced_to_safe_actionable_feedback() {
        assert_eq!(
            sanitized_error_feedback("connect failed for private jid 123"),
            "Connection failed. Check your network and try again."
        );
        assert_eq!(
            sanitized_error_feedback("unclassified private backend detail"),
            "Action failed. Check connection and try again."
        );
    }

    #[test]
    fn linking_projection_shows_only_required_pairing_details() {
        let (_, pairing) = link_page(&LinkStatus::Unlinked {
            qr: Some("private-qr".into()),
            pair_code: Some("123-456".into()),
            pairing_phone: Some("15551234567".into()),
        });
        let (_, failure) = link_page(&LinkStatus::Failed("private protocol detail".into()));

        assert!(pairing.contains("123-456"));
        assert!(!pairing.contains("15551234567"));
        assert!(!failure.contains("private protocol detail"));
    }

    #[test]
    fn linking_qr_becomes_a_decodable_native_image() {
        let texture = qr_texture("synthetic-link-payload").expect("QR texture");
        assert!(texture.width() >= 200);
        assert_eq!(texture.width(), texture.height());
    }

    #[test]
    fn conversation_rows_mark_day_changes_and_unread_boundary() {
        let timestamps = [1_700_000_000, 1_700_000_060, 1_700_086_400];
        let prefixes = conversation_prefixes(&timestamps, 2);
        assert!(prefixes[0].contains("──"));
        assert!(prefixes[1].contains("Unread messages"));
        assert!(prefixes[2].contains("──"));
        assert!(!prefixes[0].contains("Unread messages"));
    }

    #[test]
    fn delivery_projection_distinguishes_each_outgoing_state() {
        assert_eq!(delivery_label(crate::model::Delivery::Pending), " · Queued");
        assert_eq!(delivery_label(crate::model::Delivery::Sent), " · Sent");
        assert_eq!(
            delivery_label(crate::model::Delivery::Delivered),
            " · Delivered"
        );
        assert_eq!(delivery_label(crate::model::Delivery::Read), " · Read");
        assert_eq!(delivery_label(crate::model::Delivery::Played), " · Played");
        assert_eq!(
            delivery_mark(crate::model::Delivery::Sent),
            ("✓", None, false)
        );
        assert!(!delivery_mark(crate::model::Delivery::Delivered).2);
        assert!(delivery_mark(crate::model::Delivery::Read).2);
        assert!(delivery_mark(crate::model::Delivery::Pending).1.is_some());
        assert!(delivery_mark(crate::model::Delivery::Failed).1.is_some());
        assert_eq!(
            delivery_mark(crate::model::Delivery::None),
            ("", None, false)
        );
        assert_eq!(delivery_label(crate::model::Delivery::Failed), " · Failed");
        assert_eq!(delivery_label(crate::model::Delivery::None), "");
    }

    #[test]
    fn reaction_summary_aggregates_counts_and_marks_our_reaction() {
        let reactions = vec![
            crate::model::Reaction {
                sender: "a".into(),
                from_me: true,
                emoji: "👍".into(),
            },
            crate::model::Reaction {
                sender: "b".into(),
                from_me: false,
                emoji: "👍".into(),
            },
        ];

        assert_eq!(reaction_summary(&reactions), "\n👍 × 2 · You");
    }

    #[test]
    fn clipboard_texture_channels_become_straight_alpha_rgba() {
        let mut pixels = [0, 0, 128, 128, 200, 20, 10, 0];
        convert_premultiplied_bgra_to_rgba(&mut pixels);

        assert_eq!(&pixels[..4], &[255, 0, 0, 128]);
        assert_eq!(&pixels[4..], &[0, 0, 0, 0]);
    }
}
