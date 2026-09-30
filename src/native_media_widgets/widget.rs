use super::*;

/// Build message media widget and forward attachment actions to its owner.
pub fn build_media_widget_with_action(
    message: &Message,
    on_action: impl Fn(NativeMediaAction) + 'static,
) -> NativeMediaWidget {
    let projected = project_content(message);
    let decode_token = DecodeToken::default();
    // The message row draws sender, quote, body, reactions, and forwarding;
    // this widget only adds what the content itself needs.
    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    if let Content::Text { preview, .. } = &message.content {
        if let Some(preview) = preview {
            root.append(&link_card(
                preview,
                message.thumbnail.clone(),
                &decode_token,
            ));
        }
        return NativeMediaWidget {
            widget: root,
            decode_token,
        };
    }
    if let Content::Location {
        latitude,
        longitude,
        name,
        address,
    } = &message.content
    {
        root.append(&location_card(
            *latitude,
            *longitude,
            name.as_deref(),
            address.as_deref(),
            message.thumbnail.clone(),
            &decode_token,
        ));
        return NativeMediaWidget {
            widget: root,
            decode_token,
        };
    }
    if let Content::Sticker { media, .. } = &message.content {
        append_sticker(&root, media.path.clone(), &decode_token);
        return NativeMediaWidget {
            widget: root,
            decode_token,
        };
    }
    if let Content::Video {
        media,
        seconds,
        gif,
        ..
    } = &message.content
    {
        append_video(
            &root,
            message,
            media,
            *seconds,
            *gif,
            std::rc::Rc::new(on_action),
        );
        return NativeMediaWidget {
            widget: root,
            decode_token,
        };
    }
    if let Content::Document { media, .. } = &message.content {
        root.append(&document_card(message, media, std::rc::Rc::new(on_action)));
        return NativeMediaWidget {
            widget: root,
            decode_token,
        };
    }
    if let Content::Image { media, .. } = &message.content {
        append_photo(
            &root,
            message,
            media,
            &decode_token,
            std::rc::Rc::new(on_action),
        );
        return NativeMediaWidget {
            widget: root,
            decode_token,
        };
    }
    if matches!(&message.content, Content::Audio { .. })
        && (!message.from_me
            || matches!(
                &message.content,
                Content::Audio {
                    voice_note: true,
                    ..
                }
            ))
    {
        return NativeMediaWidget {
            widget: root,
            decode_token,
        };
    }
    let on_action = std::rc::Rc::new(on_action);
    root.add_css_class("card");
    root.add_css_class("zaptide-media-card");

    let (title, detail) = match &projected.content {
        NativeMediaContent::Document { file_name, detail } => {
            add_label(&root, "Document");
            (file_name.as_str(), detail.as_str())
        }
        NativeMediaContent::Contact { display_name } => ("Contact", display_name.as_str()),
        NativeMediaContent::Location { label } => ("Location", label.as_str()),
        NativeMediaContent::Poll { question, options } => {
            add_label(&root, question);
            for option in options {
                let selected = if option.selected { " · selected" } else { "" };
                add_label(
                    &root,
                    &format!("{} · {} votes{selected}", option.text, option.votes),
                );
            }
            ("Poll", "")
        }
        NativeMediaContent::Buttons {
            text,
            footer,
            labels,
            answered,
        } => {
            if !text.is_empty() {
                add_label(&root, text);
            }
            if let Some(footer) = footer {
                let label = add_label(&root, footer);
                label.add_css_class("dim-label");
                label.add_css_class("caption");
            }
            let ids: Vec<&str> = match &message.content {
                Content::Buttons { buttons, .. } => {
                    buttons.iter().map(|button| button.id.as_str()).collect()
                }
                _ => Vec::new(),
            };
            // Only messages from others can be answered, and only once.
            let open = message.content.answer().is_none() && !message.from_me;
            for (index, label) in labels.iter().enumerate() {
                let chosen = *answered == Some(index);
                // A text marker as well as the style: an insensitive
                // accent button is faint, and styles are not announced.
                let button = reply_button(&if chosen {
                    format!("✓ {label}")
                } else {
                    label.clone()
                });
                button.set_sensitive(open && ids.get(index).is_some());
                if chosen {
                    button.add_css_class("suggested-action");
                    button.update_property(&[gtk::accessible::Property::Description(
                        "Selected reply",
                    )]);
                } else if !open {
                    let reason = if message.from_me {
                        "Only the recipient can choose from these buttons"
                    } else {
                        "Already answered"
                    };
                    button.set_tooltip_text(Some(reason));
                    button.update_property(&[gtk::accessible::Property::Description(reason)]);
                }
                if let (true, Some(id)) = (open, ids.get(index)) {
                    let (chat, message_id, button_id) =
                        (message.chat.clone(), message.id.clone(), (*id).to_owned());
                    let on_action = on_action.clone();
                    // The button stays enabled until the stored state changes:
                    // the worker ignores a second click, and a failed one
                    // leaves the row exactly as it was.
                    button.connect_clicked(move |_| {
                        on_action(NativeMediaAction::AnswerButton {
                            chat: chat.clone(),
                            message: message_id.clone(),
                            button: button_id.clone(),
                        });
                    });
                }
                root.append(&button);
            }
            ("", "")
        }
        NativeMediaContent::List {
            title,
            description,
            button,
            footer,
            sections,
            answered,
        } => {
            if !title.is_empty() {
                add_label(&root, title).add_css_class("heading");
            }
            if let Some(description) = description {
                add_label(&root, description);
            }
            // Row ids come from the message itself, in the projection's order.
            let ids: Vec<&str> = match &message.content {
                Content::List { sections, .. } => sections
                    .iter()
                    .flat_map(|section| &section.rows)
                    .map(|row| row.id.as_str())
                    .collect(),
                _ => Vec::new(),
            };
            let chosen_id = match &message.content {
                Content::List { answered, .. } => answered.as_deref(),
                _ => None,
            };
            let mut index = 0;
            for section in sections {
                if let Some(heading) = &section.title {
                    let label = add_label(&root, heading);
                    label.add_css_class("caption-heading");
                }
                for (row, detail) in &section.rows {
                    let mark = if chosen_id.is_some() && ids.get(index).copied() == chosen_id {
                        "✓"
                    } else {
                        "•"
                    };
                    index += 1;
                    add_label(&root, &format!("{mark} {row}"));
                    if let Some(detail) = detail {
                        let label = add_label(&root, detail);
                        label.set_margin_start(12);
                        label.add_css_class("dim-label");
                        label.add_css_class("caption");
                    }
                }
            }
            if let Some(footer) = footer {
                let label = add_label(&root, footer);
                label.add_css_class("dim-label");
                label.add_css_class("caption");
            }
            let label = if button.is_empty() { "Choose" } else { button };
            // From the message itself: the projection's `answered` is `None`
            // for an answer naming a row that no longer exists.
            let open = message.content.answer().is_none() && !message.from_me;
            let picker = reply_button(label);
            picker.set_sensitive(open);
            if open {
                let choices: Vec<ListChoiceSection> = match &message.content {
                    Content::List { sections, .. } => sections
                        .iter()
                        .map(|section| {
                            (
                                section.title.clone(),
                                section
                                    .rows
                                    .iter()
                                    .map(|row| {
                                        (row.id.clone(), row.title.clone(), row.description.clone())
                                    })
                                    .collect(),
                            )
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                let (chat, message_id) = (message.chat.clone(), message.id.clone());
                let (heading, on_action) = (title.clone(), on_action.clone());
                picker.connect_clicked(move |button| {
                    let (chat, message_id, on_action) =
                        (chat.clone(), message_id.clone(), on_action.clone());
                    show_list_choices(button, &heading, &choices, move |row| {
                        on_action(NativeMediaAction::AnswerListRow {
                            chat: chat.clone(),
                            message: message_id.clone(),
                            row,
                        });
                    });
                });
            } else {
                let reason = match answered {
                    Some(chosen) => format!("Already answered: {chosen}"),
                    None if message.from_me => {
                        "Only the recipient can choose from this list".into()
                    }
                    None => "This list can no longer be answered".into(),
                };
                picker.set_tooltip_text(Some(&reason));
                picker.update_property(&[gtk::accessible::Property::Description(&reason)]);
            }
            root.append(&picker);
            ("", "")
        }
        NativeMediaContent::VideoPlaceholder => ("Video", "Video preview unavailable"),
        NativeMediaContent::UnsupportedPlaceholder => (
            "Unsupported message",
            "This message type cannot be displayed",
        ),
        NativeMediaContent::Other => match &message.content {
            Content::Image { .. } if message.thumbnail.is_some() => ("", ""),
            Content::Image { .. } => ("Photo", "Image attachment"),
            Content::Audio { .. } => ("Audio", "Audio attachment"),
            Content::Revoked => ("Deleted message", "This message was deleted"),
            Content::Text { text, .. } => ("Message", text.as_str()),
            _ => ("Message", ""),
        },
    };
    if !title.is_empty() {
        add_label(&root, title);
    }
    if !detail.is_empty() {
        add_label(&root, detail);
    }

    if matches!(&message.content, Content::Image { .. }) && message.thumbnail.is_some() {
        let preview = gtk::Image::new();
        preview.set_pixel_size(256);
        preview.set_halign(gtk::Align::Start);
        preview.set_tooltip_text(Some("Image preview"));
        // An empty gtk::Image still reserves its pixel size, stretching the
        // row with a blank card until the thumbnail decodes, if it ever does.
        preview.set_visible(false);
        root.append(&preview);
        if let Some(bytes) = message.thumbnail.clone() {
            decode_preview_async(&preview, move || Some(bytes), &decode_token);
        }
    }

    if let Some(action) = attachment_action(message) {
        let button = gtk::Button::with_label(attachment_button_label(message));
        button.set_tooltip_text(Some("Open or download attachment"));
        button.connect_clicked(move |_| {
            on_action(action.clone());
        });
        root.append(&button);
    }

    NativeMediaWidget {
        widget: root,
        decode_token,
    }
}

/// Choose only known local formats for GTK/GStreamer playback. The media
/// stream reports missing plugins and unsupported codecs after opening, which
/// switches presentation to the explicit open-attachment fallback.
pub(super) fn playback_projection(message: &Message) -> PlaybackProjection {
    let Some(media) = message.content.media() else {
        return PlaybackProjection::None;
    };
    let Some(path) = media.path.as_ref() else {
        return match &message.content {
            Content::Video { .. } => PlaybackProjection::Fallback,
            _ => PlaybackProjection::None,
        };
    };

    let looping = match &message.content {
        Content::Video { gif, .. } => *gif,
        _ => return PlaybackProjection::None,
    };
    if !path.is_file() {
        return PlaybackProjection::Fallback;
    }

    let mime = media.mime.to_ascii_lowercase();
    let supported = matches!(mime.as_str(), "video/mp4" | "video/webm" | "video/ogg")
        || (looping && mime == "image/gif");
    if supported {
        PlaybackProjection::Native { looping }
    } else {
        PlaybackProjection::Fallback
    }
}

pub(super) fn attachment_button_label(message: &Message) -> &'static str {
    match &message.content {
        Content::Image { media, .. }
        | Content::Video { media, .. }
        | Content::Audio { media, .. }
        | Content::Document { media, .. } => {
            if media.path.is_some() {
                "Open attachment"
            } else {
                "Download attachment"
            }
        }
        _ => "Open attachment",
    }
}
