//! Convert WhatsApp message payloads into UI-safe application content.

use std::collections::HashSet;

use super::{Content, LinkPreview, first_link, media, non_empty};
use whatsapp_rust::prelude::{MessageExt, wa};

/// Most buttons, list rows and characters per string kept from a sender, so
/// one crafted message cannot build thousands of widgets.
pub(super) const MAX_INTERACTIVE_ITEMS: usize = 100;
pub(super) const MAX_INTERACTIVE_BUTTONS: usize = 10;
pub(super) const MAX_INTERACTIVE_CHARS: usize = 1_000;

/// Converts a protocol message to visible content, or `None` for internal traffic.
pub(super) fn classify(base: &wa::Message) -> Option<Content> {
    if let Some(text) = base.text_content() {
        let preview = base.extended_text_message.as_option().and_then(|extended| {
            let title = non_empty(&extended.title);
            let description = non_empty(&extended.description);
            let has_picture = extended
                .jpeg_thumbnail
                .as_ref()
                .is_some_and(|bytes| !bytes.is_empty());
            if title.is_none() && description.is_none() && !has_picture {
                return None;
            }
            let url = non_empty(&extended.matched_text)
                .and_then(|url| crate::safety::preview_url(&url))
                .or_else(|| first_link(text).and_then(|url| crate::safety::preview_url(&url)))?;
            Some(LinkPreview {
                url,
                title,
                description,
            })
        });
        return Some(Content::Text {
            text: text.to_owned(),
            preview,
        });
    }
    if let Some(image) = base.image_message.as_option() {
        return Some(Content::Image {
            caption: non_empty(&image.caption),
            media: media(
                image.mimetype.as_ref(),
                image.file_length,
                image.width,
                image.height,
            ),
        });
    }
    if let Some(video) = base
        .video_message
        .as_option()
        .or(base.ptv_message.as_option())
    {
        return Some(Content::Video {
            caption: non_empty(&video.caption),
            media: media(
                video.mimetype.as_ref(),
                video.file_length,
                video.width,
                video.height,
            ),
            seconds: video.seconds,
            gif: video.gif_playback.unwrap_or(false),
        });
    }
    if let Some(audio) = base.audio_message.as_option() {
        return Some(Content::Audio {
            media: media(audio.mimetype.as_ref(), audio.file_length, None, None),
            seconds: audio.seconds,
            voice_note: audio.ptt.unwrap_or(false),
            waveform: audio.waveform.clone().unwrap_or_default(),
        });
    }
    if let Some(document) = base.document_message.as_option() {
        let file_name = non_empty(&document.file_name)
            .or_else(|| non_empty(&document.title))
            .unwrap_or_else(|| "Document".to_owned());
        return Some(Content::Document {
            media: media(document.mimetype.as_ref(), document.file_length, None, None),
            file_name,
            caption: non_empty(&document.caption),
            pages: document.page_count,
        });
    }
    if let Some(sticker) = base.sticker_message.as_option() {
        return Some(Content::Sticker {
            media: media(
                sticker.mimetype.as_ref(),
                sticker.file_length,
                sticker.width,
                sticker.height,
            ),
            animated: sticker.is_animated.unwrap_or(false),
        });
    }
    if let Some(location) = base.location_message.as_option() {
        return Some(Content::Location {
            latitude: location.degrees_latitude.unwrap_or(0.0),
            longitude: location.degrees_longitude.unwrap_or(0.0),
            name: non_empty(&location.name),
            address: non_empty(&location.address),
        });
    }
    if let Some(live) = base.live_location_message.as_option() {
        return Some(Content::Location {
            latitude: live.degrees_latitude.unwrap_or(0.0),
            longitude: live.degrees_longitude.unwrap_or(0.0),
            name: Some("Live location".to_owned()),
            address: None,
        });
    }
    if let Some(contact) = base.contact_message.as_option() {
        return Some(Content::Contact {
            display_name: non_empty(&contact.display_name).unwrap_or_else(|| "Contact".to_owned()),
            vcard: contact.vcard.clone().unwrap_or_default(),
        });
    }
    if let Some(contacts) = base.contacts_array_message.as_option() {
        let count = contacts.contacts.len();
        return Some(Content::Contact {
            display_name: non_empty(&contacts.display_name)
                .unwrap_or_else(|| format!("{count} contacts")),
            vcard: contacts
                .contacts
                .iter()
                .filter_map(|contact| contact.vcard.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        });
    }
    if let Some(poll) = base
        .poll_creation_message
        .as_option()
        .or(base.poll_creation_message_v2.as_option())
        .or(base.poll_creation_message_v3.as_option())
    {
        return Some(Content::Poll {
            question: non_empty(&poll.name).unwrap_or_else(|| "Poll".to_owned()),
            state: crate::model::PollState {
                selectable: poll.selectable_options_count.unwrap_or(0) as usize,
                ..Default::default()
            },
            options: poll
                .options
                .iter()
                .map(|option| option.option_name.clone().unwrap_or_default())
                .collect(),
        });
    }
    let unsupported = |what: &str| {
        Some(Content::Unsupported {
            what: what.to_owned(),
        })
    };
    if let Some(buttons) = base.buttons_message.as_option() {
        return buttons_content(buttons).or_else(|| unsupported("interactive message"));
    }
    if let Some(list) = base.list_message.as_option() {
        return list_content(list).or_else(|| unsupported("list"));
    }
    if let Some(response) = base.buttons_response_message.as_option() {
        use wa::__buffa::oneof::message::buttons_response_message::Response;
        return Some(Content::text(match &response.response {
            Some(Response::SelectedDisplayText(text)) if !text.is_empty() => text.clone(),
            _ => "Button reply".to_owned(),
        }));
    }
    if let Some(response) = base.list_response_message.as_option() {
        return Some(Content::text(
            response
                .title
                .as_deref()
                .filter(|title| !title.is_empty())
                .unwrap_or("List reply"),
        ));
    }
    if base.album_message.is_set() {
        return None;
    }
    if base.group_invite_message.is_set() {
        return unsupported("group invite");
    }
    if base.event_message.is_set() {
        return unsupported("event");
    }
    if base.sticker_pack_message.is_set() {
        return unsupported("sticker pack");
    }
    if base.interactive_message.is_set()
        || base.template_message.is_set()
        || base.interactive_response_message.is_set()
        || base.template_button_reply_message.is_set()
    {
        return unsupported("interactive message");
    }
    if base.product_message.is_set() || base.order_message.is_set() {
        return unsupported("product");
    }
    if base.send_payment_message.is_set()
        || base.request_payment_message.is_set()
        || base.payment_invite_message.is_set()
        || base.invoice_message.is_set()
    {
        return unsupported("payment");
    }
    if base.call_log_messsage.is_set() || base.scheduled_call_creation_message.is_set() {
        return unsupported("call");
    }
    if base.lottie_sticker_message.is_set() {
        return unsupported("animated sticker");
    }
    if base.poll_update_message.is_set()
        || base.enc_reaction_message.is_set()
        || base.enc_comment_message.is_set()
        || base.enc_event_response_message.is_set()
        || base.keep_in_chat_message.is_set()
        || base.pin_in_chat_message.is_set()
        || base.sender_key_distribution_message.is_set()
        || base
            .fast_ratchet_key_sender_key_distribution_message
            .is_set()
        || base.sticker_sync_rmr_message.is_set()
        || base.message_context_info.is_set()
        || base.device_sent_message.is_set()
        || base.placeholder_message.is_set()
        || base.secret_encrypted_message.is_set()
        || base.message_history_bundle.is_set()
        || base.message_history_notice.is_set()
        || base.bot_invoke_message.is_set()
    {
        return None;
    }
    if *base == wa::Message::default() {
        return None;
    }
    unsupported("message")
}

fn interactive_text(text: &Option<String>) -> Option<String> {
    non_empty(text).map(|text| {
        if text.chars().count() <= MAX_INTERACTIVE_CHARS {
            return text;
        }
        let mut clipped: String = text.chars().take(MAX_INTERACTIVE_CHARS).collect();
        clipped.push('…');
        clipped
    })
}

/// Reply IDs are sent back unchanged, so reject oversized IDs rather than clip.
fn interactive_id(id: &Option<String>) -> Option<String> {
    non_empty(id).filter(|id| id.chars().count() <= 256)
}

/// Keep only answerable quick replies; return `None` when no text or buttons remain.
fn buttons_content(message: &wa::message::ButtonsMessage) -> Option<Content> {
    use wa::message::buttons_message::button::Type;
    let mut text = interactive_text(&message.content_text);
    if text.is_none()
        && let Some(wa::__buffa::oneof::message::buttons_message::Header::Text(header)) =
            &message.header
    {
        text = interactive_text(&Some(header.clone()));
    }
    let mut seen = HashSet::new();
    let buttons: Vec<_> = message
        .buttons
        .iter()
        .filter(|button| {
            // Native-flow buttons cannot be answered with a button reply.
            button.r#type != Some(Type::NATIVE_FLOW) && !button.native_flow_info.is_set()
        })
        .filter_map(|button| {
            // A repeated id could not say which button was chosen.
            let id = interactive_id(&button.button_id).filter(|id| seen.insert(id.clone()))?;
            let label = button
                .button_text
                .as_option()
                .and_then(|text| interactive_text(&text.display_text))?;
            Some(crate::model::QuickReply { id, label })
        })
        .take(MAX_INTERACTIVE_BUTTONS)
        .collect();
    (text.is_some() || !buttons.is_empty()).then(|| Content::Buttons {
        text: text.unwrap_or_default(),
        footer: interactive_text(&message.footer_text),
        buttons,
        answered: None,
    })
}

fn list_content(message: &wa::message::ListMessage) -> Option<Content> {
    use wa::message::list_message::ListType;
    // Product lists do not have a supported row-answer protocol.
    if message.list_type == Some(ListType::PRODUCT_LIST) || message.product_list_info.is_set() {
        return None;
    }
    let mut budget = MAX_INTERACTIVE_ITEMS;
    let mut seen = HashSet::new();
    let sections: Vec<_> = message
        .sections
        .iter()
        .filter_map(|section| {
            let rows: Vec<_> = section
                .rows
                .iter()
                .filter_map(|row| {
                    Some(crate::model::ListRow {
                        // A repeated id could not say which row was chosen.
                        id: interactive_id(&row.row_id).filter(|id| seen.insert(id.clone()))?,
                        title: interactive_text(&row.title)?,
                        description: interactive_text(&row.description),
                    })
                })
                .take(budget)
                .collect();
            budget -= rows.len();
            (!rows.is_empty()).then(|| crate::model::ListSection {
                title: interactive_text(&section.title),
                rows,
            })
        })
        .collect();
    (!sections.is_empty()).then(|| Content::List {
        title: interactive_text(&message.title).unwrap_or_default(),
        description: interactive_text(&message.description),
        button: interactive_text(&message.button_text).unwrap_or_default(),
        footer: interactive_text(&message.footer_text),
        sections,
        answered: None,
    })
}
