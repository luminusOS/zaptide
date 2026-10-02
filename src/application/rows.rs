use super::*;
use relm4::typed_view::list::RelmListItem;

pub(super) struct ChatRowWidgets {
    name: gtk::Label,
    preview: gtk::Label,
    status: gtk::DrawingArea,
    status_icon: gtk::Image,
    unread: gtk::Label,
    pinned: gtk::Image,
    muted: gtk::Image,
    avatar: adw::Avatar,
}

impl RelmListItem for ChatRow {
    type Root = gtk::Box;
    type Widgets = ChatRowWidgets;

    fn setup(_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        // The row's padding lives on this box (see the zaptide-chat-item
        // style), so its open-chat background covers the whole row.
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .css_classes(["zaptide-chat-item"])
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

    fn bind(&mut self, widgets: &mut Self::Widgets, root: &mut Self::Root) {
        // Named by chat, so `mark_open_chat` can find the bound row.
        root.set_widget_name(&self.id);
        // Not on the list's row: restyling it while binding rebinds rows.
        if self.open {
            root.add_css_class("zaptide-chat-open");
        } else {
            root.remove_css_class("zaptide-chat-open");
        }
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

pub(super) struct MessageRowWidgets {
    separator: gtk::Label,
    avatar: adw::Avatar,
    leading_space: gtk::Box,
    trailing_space: gtk::Box,
    check: gtk::CheckButton,
    bubble: gtk::Box,
    header: gtk::Box,
    name: gtk::Label,
    quote: gtk::Label,
    body: gtk::Label,
    footer: gtk::Label,
    reactions: gtk::Box,
    status: gtk::DrawingArea,
    status_icon: gtk::Image,
    media: gtk::Box,
    audio: gtk::Box,
    audio_controls: Option<crate::native_media_widgets::AudioControls>,
    rendered_message: Option<crate::model::Message>,
    rendered_album: Vec<crate::model::Message>,
    action_generation: std::rc::Rc<std::cell::Cell<u64>>,
    decode_token: Option<crate::native_media::DecodeToken>,
    menu_target: MenuTarget,
}

/// The bound message and its sender, read by the row's context-menu gesture.
type MenuTarget =
    std::rc::Rc<std::cell::RefCell<Option<(String, ComponentSender<NativeApplication>)>>>;

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
        // Selection mode marker; the row's own click toggles it.
        let check = gtk::CheckButton::builder()
            .valign(gtk::Align::Center)
            .can_target(false)
            .can_focus(false)
            .visible(false)
            .css_classes(["selection-mode"])
            .build();
        row.append(&check);
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
        // Only cap the width: below the tightening threshold a Clamp narrows
        // its child and centres it, leaving wide bubbles off the row's edge.
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&bubble);
        let reactions = gtk::Box::builder()
            .spacing(4)
            .margin_top(3)
            .margin_start(6)
            .margin_end(6)
            .build();
        column.append(&reactions);
        // Not expanding: an expanding attachment row would otherwise make the
        // Clamp share the spacer's width and centre the bubble inside it.
        let clamp = adw::Clamp::builder()
            .maximum_size(480)
            .tightening_threshold(480)
            .hexpand(false)
            .child(&column)
            .build();
        row.append(&clamp);
        let trailing_space = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        trailing_space.set_hexpand(true);
        row.append(&trailing_space);
        root.append(&row);
        let menu_target: MenuTarget = std::rc::Rc::default();
        let link_target = menu_target.clone();
        body.connect_activate_link(move |_, uri| {
            // A mention with no chat to open only carries a tooltip.
            if uri == crate::safety::MENTION_SCHEME {
                return gtk::glib::Propagation::Stop;
            }
            let (Some(phone), Some((_, sender))) = (
                crate::safety::chat_link_number(uri),
                link_target.borrow().clone(),
            ) else {
                return gtk::glib::Propagation::Proceed;
            };
            sender.input(Input::NewContact {
                phone: phone.to_owned(),
                name: None,
            });
            gtk::glib::Propagation::Stop
        });
        // Double-clicking code selects and copies the whole span, not one word.
        let code_click = gtk::GestureClick::new();
        code_click.set_button(gtk::gdk::BUTTON_PRIMARY);
        code_click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let code_target = menu_target.clone();
        code_click.connect_pressed(move |gesture, presses, x, y| {
            let Some(label) = gesture.widget().and_downcast::<gtk::Label>() else {
                return;
            };
            if presses != 2 {
                return;
            }
            let layout = label.layout();
            let (left, top) = label.layout_offsets();
            let (inside, index, _) = layout.xy_to_index(
                (x as i32 - left) * gtk::pango::SCALE,
                (y as i32 - top) * gtk::pango::SCALE,
            );
            let Some(range) = layout
                .attributes()
                .filter(|_| inside)
                .and_then(|attrs| crate::safety::code_range_at(&attrs, index as usize))
            else {
                return;
            };
            gesture.set_state(gtk::EventSequenceState::Claimed);
            let text = layout.text();
            let Some(code) = text.get(range.clone()) else {
                return;
            };
            label.select_region(
                text[..range.start].chars().count() as i32,
                text[..range.end].chars().count() as i32,
            );
            if let Some((_, sender)) = code_target.borrow().as_ref() {
                sender.input(Input::CopyCode(code.to_owned()));
            }
        });
        body.add_controller(code_click);
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
                check,
                bubble,
                header,
                name,
                quote,
                body,
                footer,
                reactions,
                status,
                status_icon,
                media,
                audio,
                audio_controls: None,
                rendered_message: None,
                rendered_album: Vec::new(),
                action_generation,
                decode_token: None,
                menu_target,
            },
        )
    }

    fn bind(&mut self, widgets: &mut Self::Widgets, root: &mut Self::Root) {
        root.set_widget_name(&self.id);
        fade_in_if_arriving(root, &self.id);
        root.set_visible(!self.collapsed);
        root.update_property(&[
            gtk::accessible::Property::Label(&self.accessible_label),
            gtk::accessible::Property::Description(match self.selected {
                None => "Press Menu or Shift+F10 for actions",
                Some(true) => "Selected. Press Enter to deselect",
                Some(false) => "Not selected. Press Enter to select",
            }),
        ]);
        let outgoing = self.message.from_me;
        widgets.check.set_visible(self.selected.is_some());
        widgets.check.set_active(self.selected == Some(true));
        // While selecting, clicks reach the list row instead of the bubble's
        // links, media and text selection.
        widgets.bubble.set_can_target(self.selected.is_none());
        if self.selected == Some(true) {
            root.add_css_class("zaptide-message-selected");
        } else {
            root.remove_css_class("zaptide-message-selected");
        }
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
        widgets
            .body
            .set_markup(&crate::safety::linkify_markup_with_mentions(
                &self.body,
                &self.mentions,
            ));
        widgets.body.set_visible(!self.body.is_empty());
        widgets
            .body
            .update_property(&[gtk::accessible::Property::Label(&self.accessible_label)]);
        *widgets.menu_target.borrow_mut() = Some((self.id.clone(), self.pointer_sender.clone()));
        widgets.footer.set_label(&self.footer);
        widgets.footer.set_visible(!self.footer.is_empty());
        while let Some(child) = widgets.reactions.first_child() {
            widgets.reactions.remove(&child);
        }
        widgets.reactions.set_halign(if outgoing {
            gtk::Align::End
        } else {
            gtk::Align::Start
        });
        let counts = reaction_counts(&self.message.reactions);
        widgets.reactions.set_visible(!counts.is_empty());
        for (emoji, count, from_me) in counts {
            widgets.reactions.append(&reaction_chip(
                &self.id,
                &emoji,
                count,
                from_me,
                &self.pointer_sender,
            ));
        }
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
        let same_album = widgets.rendered_album.len() == self.album.len()
            && widgets
                .rendered_album
                .iter()
                .zip(&self.album)
                .all(|(previous, next)| same_media(previous, next));
        if !(same_album
            && widgets
                .rendered_message
                .as_ref()
                .is_some_and(|previous| same_media(previous, &self.message)))
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
            let forward = move |action| {
                if active_generation.get() == generation && media.upgrade().is_some() {
                    sender.input(Input::MediaAction(action));
                }
            };
            let rendered = if self.album.is_empty() {
                crate::native_media_widgets::build_media_widget_with_action(&self.message, forward)
            } else {
                crate::native_media_widgets::build_album_widget(&self.album, forward)
            };
            widgets
                .media
                .set_visible(rendered.widget.first_child().is_some());
            widgets.media.append(&rendered.widget);
            widgets.decode_token = Some(rendered.decode_token);
            widgets.rendered_message = Some(self.message.clone());
            widgets.rendered_album = self.album.clone();
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
