use gtk::prelude::*;
use relm4::prelude::*;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ComposerState {
    pub(super) active: bool,
    pub(super) recording: bool,
    pub(super) recording_blink: f64,
    pub(super) recording_time: String,
    pub(super) recording_send_enabled: bool,
    pub(super) context: Option<(String, String)>,
    pub(super) attachment_count: usize,
    pub(super) attachment_paths: Vec<std::path::PathBuf>,
    pub(super) clipboard_preview: Option<gtk::gdk::Texture>,
    pub(super) can_attach: bool,
    pub(super) can_mention: bool,
    pub(super) editable: bool,
    pub(super) editing: bool,
    pub(super) draft_empty: bool,
    pub(super) can_send_voice: bool,
    pub(super) can_send: bool,
}

impl Default for ComposerState {
    fn default() -> Self {
        Self {
            active: false,
            recording: false,
            recording_blink: 1.0,
            recording_time: String::new(),
            recording_send_enabled: false,
            context: None,
            attachment_count: 0,
            attachment_paths: Vec::new(),
            clipboard_preview: None,
            can_attach: false,
            can_mention: false,
            editable: false,
            editing: false,
            draft_empty: true,
            can_send_voice: false,
            can_send: false,
        }
    }
}

pub(super) struct ComposerViewInit {
    pub(super) state: ComposerState,
    pub(super) buffer: gtk::TextBuffer,
    pub(super) enter_sends: std::rc::Rc<std::cell::Cell<bool>>,
    pub(super) recording_meter_area: gtk::DrawingArea,
}

pub(super) struct ComposerView {
    state: ComposerState,
    buffer: gtk::TextBuffer,
    text_view: Option<gtk::TextView>,
    sticker_button: Option<gtk::Button>,
    attachment_list: Option<gtk::Box>,
}

#[derive(Debug)]
pub(super) enum ComposerViewInput {
    Sync(ComposerState),
}

#[derive(Debug)]
pub(super) enum ComposerViewOutput {
    DraftChanged(String),
    SendText(String),
    PickAttachments { gallery: bool },
    ClearAttachments,
    RemoveAttachment(std::path::PathBuf),
    CancelReply,
    CancelEdit,
    Recording(crate::native_voice::RecordingIntent),
    InsertMention,
    ShowStickerPicker,
    ShowPollCreator,
    InsertEmoji(String),
}

impl ComposerView {
    pub(super) fn is_synced(&self, state: &ComposerState) -> bool {
        self.state == *state
    }

    pub(super) fn text_view(&self) -> gtk::TextView {
        self.text_view
            .as_ref()
            .expect("composer text view initialized")
            .clone()
    }

    pub(super) fn focus_text_view(&self) {
        let _ = self.text_view().grab_focus();
    }

    pub(super) fn sticker_button(&self) -> gtk::Button {
        self.sticker_button
            .as_ref()
            .expect("composer sticker button initialized")
            .clone()
    }
}

#[relm4::component(pub(super))]
impl SimpleComponent for ComposerView {
    type Init = ComposerViewInit;
    type Input = ComposerViewInput;
    type Output = ComposerViewOutput;

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            #[watch]
            set_visible: model.state.active,

            append = &gtk::Box {
                set_margin_start: 12,
                set_margin_end: 12,
                set_margin_top: 6,
                set_spacing: 6,
                set_valign: gtk::Align::Center,
                #[watch]
                set_visible: model.state.recording,
                append = &gtk::Button {
                    set_icon_name: "user-trash-symbolic",
                    set_tooltip_text: Some("Discard recording"),
                    add_css_class: "flat",
                    add_css_class: "circular",
                    connect_clicked[sender] => move |_| sender.output(ComposerViewOutput::Recording(crate::native_voice::RecordingIntent::Cancel)).unwrap(),
                },
                append = &gtk::Image {
                    set_icon_name: Some("media-record-symbolic"),
                    update_property: &[gtk::accessible::Property::Label("Recording")],
                    add_css_class: "error",
                    #[watch]
                    set_opacity: model.state.recording_blink,
                },
                append = &gtk::Label {
                    add_css_class: "numeric",
                    #[watch]
                    set_label: &model.state.recording_time,
                },
                #[local_ref]
                recording_meter_area -> gtk::DrawingArea {},
                append = &gtk::Button {
                    #[wrap(Some)]
                    set_child = &super::paper_plane_icon() -> gtk::DrawingArea {},
                    set_tooltip_text: Some("Send voice message"),
                    update_property: &[gtk::accessible::Property::Label("Send voice message")],
                    add_css_class: "suggested-action",
                    add_css_class: "circular",
                    #[watch]
                    set_sensitive: model.state.recording_send_enabled,
                    connect_clicked[sender] => move |_| sender.output(ComposerViewOutput::Recording(crate::native_voice::RecordingIntent::Send)).unwrap(),
                },
            },

            append = &gtk::Box {
                set_margin_start: 12,
                set_margin_end: 12,
                set_margin_top: 6,
                set_spacing: 6,
                #[watch]
                set_visible: model.state.context.is_some(),
                append = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    add_css_class: "zaptide-quote",
                    set_hexpand: true,
                    append = &gtk::Label {
                        add_css_class: "heading",
                        set_xalign: 0.0,
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                        #[watch]
                        set_label: model.state.context.as_ref().map_or("", |context| &context.0),
                    },
                    append = &gtk::Label {
                        add_css_class: "dim-label",
                        set_xalign: 0.0,
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                        set_single_line_mode: true,
                        #[watch]
                        set_label: model.state.context.as_ref().map_or("", |context| &context.1),
                        #[watch]
                        set_visible: model.state.context.as_ref().is_some_and(|context| !context.1.is_empty()),
                    },
                },
                append = &gtk::Button {
                    set_icon_name: "window-close-symbolic",
                    set_tooltip_text: Some("Cancel"),
                    add_css_class: "flat",
                    add_css_class: "circular",
                    connect_clicked[sender] => move |_| {
                        sender.output(ComposerViewOutput::CancelReply).unwrap();
                        sender.output(ComposerViewOutput::CancelEdit).unwrap();
                    },
                },
            },

            append = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_margin_start: 12,
                set_margin_end: 12,
                set_margin_top: 6,
                set_spacing: 6,
                add_css_class: "zaptide-attachments",
                #[watch]
                set_visible: model.state.attachment_count > 0,
                append = &gtk::Box {
                    set_spacing: 6,
                    append = &gtk::Label {
                        set_hexpand: true,
                        set_xalign: 0.0,
                        set_margin_start: 2,
                        add_css_class: "caption-heading",
                        add_css_class: "dim-label",
                        #[watch]
                        set_label: &super::attachment_summary(&[], model.state.attachment_count),
                    },
                    append = &gtk::Button {
                        #[wrap(Some)]
                        set_child = &libadwaita::ButtonContent {
                            set_icon_name: "edit-clear-all-symbolic",
                            set_label: "Remove All",
                        },
                        set_valign: gtk::Align::Center,
                        add_css_class: "flat",
                        add_css_class: "zaptide-tray-action",
                        connect_clicked[sender] => move |_| sender.output(ComposerViewOutput::ClearAttachments).unwrap(),
                    },
                },
                append = &gtk::ScrolledWindow {
                    set_vscrollbar_policy: gtk::PolicyType::Never,
                    set_propagate_natural_height: true,
                    #[name = "attachment_list"]
                    #[wrap(Some)]
                    set_child = &gtk::Box {
                        set_spacing: 8,
                        set_margin_top: 2,
                        set_margin_bottom: 4,
                        set_margin_start: 2,
                        set_margin_end: 2,
                    },
                },
            },

            append = &gtk::Box {
                set_spacing: 6,
                set_margin_top: 6,
                set_margin_bottom: 6,
                set_margin_start: 6,
                set_margin_end: 6,

                #[name = "emoji_button"]
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
                        set_child = &gtk::Grid {
                            set_column_spacing: 4,
                            set_row_spacing: 4,
                            set_column_homogeneous: true,
                            attach[0, 0, 1, 1] = &gtk::Button {
                                set_child: Some(&super::attach_tile("image-x-generic-symbolic", "Gallery", "gallery")),
                                set_tooltip_text: Some("Send photos and videos"),
                                add_css_class: "flat",
                                add_css_class: "zaptide-attach-tile",
                                #[watch]
                                set_sensitive: model.state.can_attach,
                                connect_clicked[sender, attach_popover] => move |_| { attach_popover.popdown(); sender.output(ComposerViewOutput::PickAttachments { gallery: true }).unwrap() },
                            },
                            attach[1, 0, 1, 1] = &gtk::Button {
                                set_child: Some(&super::attach_tile("text-x-generic-symbolic", "Files", "files")),
                                set_tooltip_text: Some("Send any file as a document"),
                                add_css_class: "flat",
                                add_css_class: "zaptide-attach-tile",
                                #[watch]
                                set_sensitive: model.state.can_attach,
                                connect_clicked[sender, attach_popover] => move |_| { attach_popover.popdown(); sender.output(ComposerViewOutput::PickAttachments { gallery: false }).unwrap() },
                            },
                            attach[0, 1, 1, 1] = &gtk::Button {
                                set_child: Some(&super::attach_tile("view-list-bullet-symbolic", "Poll", "poll")),
                                set_tooltip_text: Some("Create a poll"),
                                add_css_class: "flat",
                                add_css_class: "zaptide-attach-tile",
                                #[watch]
                                set_sensitive: model.state.can_attach,
                                connect_clicked[sender, attach_popover] => move |_| { attach_popover.popdown(); sender.output(ComposerViewOutput::ShowPollCreator).unwrap() },
                            },
                            attach[1, 1, 1, 1] = &gtk::Button {
                                set_child: Some(&super::attach_tile("avatar-default-symbolic", "Mention", "mention")),
                                set_tooltip_text: Some("Mention a participant"),
                                add_css_class: "flat",
                                add_css_class: "zaptide-attach-tile",
                                #[watch]
                                set_sensitive: model.state.can_mention,
                                connect_clicked[sender, attach_popover] => move |_| { attach_popover.popdown(); sender.output(ComposerViewOutput::InsertMention).unwrap() },
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
                    set_popover: Some(&{
                        let emoji_sender = sender.clone();
                        crate::native_emoji::picker(move |emoji| {
                            emoji_sender.output(ComposerViewOutput::InsertEmoji(emoji.to_owned())).unwrap();
                        })
                    }),
                },

                #[name = "sticker_button"]
                append = &gtk::Button {
                    set_icon_name: "emoji-nature-symbolic",
                    set_tooltip_text: Some("Sticker"),
                    set_valign: gtk::Align::End,
                    add_css_class: "flat",
                    add_css_class: "circular",
                    connect_clicked[sender] => move |_| sender.output(ComposerViewOutput::ShowStickerPicker).unwrap(),
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
                        set_buffer: Some(&model.buffer),
                        update_property: &[gtk::accessible::Property::Label("Message composer")],
                        update_property: &[gtk::accessible::Property::Description("Write a message. Return inserts a line; use Send to submit when using IME.")],
                        #[watch]
                        set_editable: model.state.editable,
                        #[watch]
                        set_tooltip_text: Some(if model.state.editing { "Edit message" } else { "Write a message" }),
                    },
                },

                append = &gtk::Button {
                    set_icon_name: "audio-input-microphone-symbolic",
                    set_tooltip_text: Some("Record voice message"),
                    set_valign: gtk::Align::End,
                    add_css_class: "flat",
                    add_css_class: "circular",
                    #[watch]
                    set_visible: model.state.draft_empty && !model.state.editing,
                    #[watch]
                    set_sensitive: model.state.can_send_voice,
                    connect_clicked[sender] => move |_| sender.output(ComposerViewOutput::Recording(crate::native_voice::RecordingIntent::Start)).unwrap(),
                },

                append = &gtk::Button {
                    #[wrap(Some)]
                    set_child = &super::paper_plane_icon() -> gtk::DrawingArea {},
                    set_valign: gtk::Align::End,
                    add_css_class: "circular",
                    add_css_class: "suggested-action",
                    #[watch]
                    set_tooltip_text: Some(if model.state.editing { "Save edit" } else { "Send" }),
                    #[watch]
                    update_property: &[gtk::accessible::Property::Label(if model.state.editing { "Save edit" } else { "Send" })],
                    #[watch]
                    set_sensitive: model.state.can_send,
                    connect_clicked[sender, composer] => move |_| {
                        let buffer = composer.buffer();
                        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true).to_string();
                        sender.output(ComposerViewOutput::SendText(text)).unwrap();
                    },
                },
            },
        }
    }

    fn init(
        init: Self::Init,
        _root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let mut model = Self {
            state: init.state,
            buffer: init.buffer,
            text_view: None,
            sticker_button: None,
            attachment_list: None,
        };
        let enter_setting = init.enter_sends;
        let input = sender.clone();
        model.buffer.connect_changed(move |buffer| {
            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .to_string();
            input
                .output(ComposerViewOutput::DraftChanged(text))
                .unwrap();
        });
        let enter_buffer = model.buffer.clone();
        let enter_sender = sender.clone();
        let composer_keys = gtk::EventControllerKey::new();
        composer_keys.set_propagation_phase(gtk::PropagationPhase::Bubble);
        composer_keys.connect_key_pressed(move |_, key, _, modifiers| {
            if matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter)
                && super::should_send_on_enter(
                    enter_setting.get(),
                    modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK),
                    modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK),
                )
            {
                let text = enter_buffer
                    .text(&enter_buffer.start_iter(), &enter_buffer.end_iter(), true)
                    .to_string();
                enter_sender
                    .output(ComposerViewOutput::SendText(text))
                    .unwrap();
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        let recording_meter_area = &init.recording_meter_area;
        let widgets = view_output!();
        widgets.composer.add_controller(composer_keys);
        model.text_view = Some(widgets.composer.clone());
        model.sticker_button = Some(widgets.sticker_button.clone());
        model.attachment_list = Some(widgets.attachment_list.clone());
        model.rebuild_attachments(&sender);
        ComponentParts { model, widgets }
    }

    fn update(&mut self, input: Self::Input, sender: ComponentSender<Self>) {
        match input {
            ComposerViewInput::Sync(state) => {
                let staged_changed = state.attachment_paths != self.state.attachment_paths
                    || state.clipboard_preview != self.state.clipboard_preview;
                self.state = state;
                if staged_changed {
                    self.rebuild_attachments(&sender);
                }
            }
        }
    }
}

/// Side of an attachment card in the tray above the composer.
const CARD_EDGE: i32 = 72;

impl ComposerView {
    /// One card per staged item; rebuilt only when the staged set changes.
    fn rebuild_attachments(&self, sender: &ComponentSender<Self>) {
        let Some(list) = &self.attachment_list else {
            return;
        };
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        if let Some(texture) = &self.state.clipboard_preview {
            let picture = gtk::Picture::for_paintable(texture);
            picture.set_alternative_text(Some("Clipboard image"));
            list.append(&attachment_card(
                &thumbnail(picture),
                "Clipboard image",
                None,
                sender,
            ));
        }
        for path in &self.state.attachment_paths {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            let content_type = gtk::gio::content_type_guess(Some(path), None).0;
            let content = if content_type.starts_with("image/") {
                // ponytail: decodes on the main thread; fine for a few photos,
                // move to glycin like the timeline if large batches stall.
                let picture = gtk::Picture::for_filename(path);
                picture.set_alternative_text(Some(&name));
                thumbnail(picture)
            } else {
                file_tile(&name, &content_type)
            };
            list.append(&attachment_card(
                &content,
                &name,
                Some(path.clone()),
                sender,
            ));
        }
    }
}

/// A square cover-cropped preview; the clamps stop the picture's natural
/// size from growing the card.
fn thumbnail(picture: gtk::Picture) -> gtk::Widget {
    picture.set_content_fit(gtk::ContentFit::Cover);
    picture.set_can_shrink(true);
    picture.set_size_request(CARD_EDGE, CARD_EDGE);
    let clamp = |child: &gtk::Widget, orientation| {
        libadwaita::Clamp::builder()
            .orientation(orientation)
            .maximum_size(CARD_EDGE)
            .tightening_threshold(CARD_EDGE)
            .child(child)
            .build()
    };
    let wide = clamp(picture.upcast_ref(), gtk::Orientation::Horizontal);
    clamp(wide.upcast_ref(), gtk::Orientation::Vertical).upcast()
}

/// File type icon beside the file name and type, for non-image files.
fn file_tile(name: &str, content_type: &str) -> gtk::Widget {
    let tile = gtk::Box::builder()
        .spacing(8)
        .height_request(CARD_EDGE)
        .css_classes(["zaptide-attachment-file"])
        .build();
    tile.append(
        &gtk::Image::builder()
            .gicon(&gtk::gio::content_type_get_symbolic_icon(content_type))
            .pixel_size(24)
            .build(),
    );
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .build();
    text.append(
        &gtk::Label::builder()
            .label(name)
            .xalign(0.0)
            .max_width_chars(18)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build(),
    );
    text.append(
        &gtk::Label::builder()
            .label(gtk::gio::content_type_get_description(content_type))
            .xalign(0.0)
            .max_width_chars(18)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["caption", "dim-label"])
            .build(),
    );
    tile.append(&text);
    tile.upcast()
}

/// Rounded card with a remove button in its corner. `None` removes a
/// clipboard image, which is the only staged item when present.
fn attachment_card(
    content: &gtk::Widget,
    name: &str,
    path: Option<std::path::PathBuf>,
    sender: &ComponentSender<ComposerView>,
) -> gtk::Overlay {
    let card = gtk::Overlay::builder()
        .child(content)
        .tooltip_text(name)
        .overflow(gtk::Overflow::Hidden)
        .valign(gtk::Align::Center)
        .css_classes(["card", "zaptide-attachment"])
        .build();
    let label = format!("Remove {name}");
    let remove = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text(&label)
        .halign(gtk::Align::End)
        .valign(gtk::Align::Start)
        .margin_top(4)
        .margin_end(4)
        .css_classes(["circular", "osd", "zaptide-attachment-remove"])
        .build();
    remove.update_property(&[gtk::accessible::Property::Label(&label)]);
    let sender = sender.clone();
    remove.connect_clicked(move |_| {
        let output = match &path {
            Some(path) => ComposerViewOutput::RemoveAttachment(path.clone()),
            None => ComposerViewOutput::ClearAttachments,
        };
        sender.output(output).unwrap();
    });
    card.add_overlay(&remove);
    card
}
