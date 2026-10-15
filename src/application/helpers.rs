//! Small pure helpers for the application component.

use super::*;

pub(super) fn selection_title(count: usize) -> String {
    match count {
        0 => "Select Messages".to_owned(),
        1 => "1 Selected".to_owned(),
        count => format!("{count} Selected"),
    }
}

pub(super) fn composer_draft_state_changed(before_empty: bool, after_empty: bool) -> bool {
    before_empty != after_empty
}

/// Start, removed count, and inserted count of the span where `new` differs
/// from an old list of `old_len` items, keeping their common prefix and suffix.
pub(super) fn changed_span<T>(
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

pub(super) fn translucent_sidebar_active(
    settings: &crate::settings::Settings,
    linked: bool,
    split_collapsed: bool,
) -> bool {
    settings.translucent_sidebar && linked && !split_collapsed
}

pub(super) fn should_auto_download(media: Option<&crate::model::Media>) -> bool {
    const MAX_AUTO_DOWNLOAD: u64 = 64 * 1024 * 1024;
    media.is_some_and(|media| {
        media.path.is_none()
            && media.size <= MAX_AUTO_DOWNLOAD
            && matches!(media.state, crate::model::MediaState::Idle)
    })
}

pub(super) fn should_send_on_enter(enter_sends: bool, control: bool, shift: bool) -> bool {
    !shift && (enter_sends || control)
}

/// A searchable country list for linking by phone number, starting at the
/// locale's country. The button shows the flag and code; the list, names.
pub(super) fn country_picker() -> gtk::DropDown {
    let labels: Vec<String> = crate::countries::COUNTRIES
        .iter()
        .map(crate::countries::Country::label)
        .collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let picker = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&labels))
        .enable_search(true)
        .search_match_mode(gtk::StringFilterMatchMode::Substring)
        .expression(gtk::PropertyExpression::new(
            gtk::StringObject::static_type(),
            None::<gtk::Expression>,
            "string",
        ))
        .tooltip_text(gettext("Country"))
        .build();
    picker.set_list_factory(Some(&label_factory(|text| text.to_owned())));
    picker.set_factory(Some(&label_factory(|text| {
        let flag = text.split(' ').next().unwrap_or_default();
        let code = text.rsplit(' ').next().unwrap_or_default();
        format!("{flag} {code}")
    })));
    let locale = ["LC_ALL", "LC_TELEPHONE", "LANG"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty())
        .unwrap_or_default();
    picker.set_selected(crate::countries::from_locale(&locale) as u32);
    picker
}

/// Labels for a string list, with `text` shaping what each row shows.
pub(super) fn label_factory(text: impl Fn(&str) -> String + 'static) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            item.set_child(Some(&gtk::Label::builder().xalign(0.0).build()));
        }
    });
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let (Some(label), Some(string)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<gtk::StringObject>(),
        ) {
            label.set_label(&text(&string.string()));
        }
    });
    factory
}

/// The number typed next to `picker`, with the chosen country's code.
pub(super) fn international_phone(picker: &gtk::DropDown, entry: &gtk::Entry) -> String {
    match crate::countries::COUNTRIES.get(picker.selected() as usize) {
        Some(country) => crate::countries::international(country, &entry.text()),
        None => entry.text().to_string(),
    }
}

pub(super) fn normalized_phone(input: &str) -> Option<String> {
    let digits: String = input.chars().filter(char::is_ascii_digit).collect();
    (7..=15).contains(&digits.len()).then_some(digits)
}

/// Notify unless the chat is on screen, muted, archived, or locked.
pub(super) fn notification_should_show(
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

/// Day separators, and the unread marker before the first of the `unread`
/// newest incoming messages. Only messages after your own last one can be
/// unread: replying means you saw them, even before the count catches up.
/// Text of the unread marker inside a row's prefix; the row draws it translated.
pub(super) const UNREAD_MARKER: &str = "Unread messages";

pub(super) fn conversation_prefixes(messages: &[(i64, bool)], unread: usize) -> Vec<String> {
    let after_own = messages
        .iter()
        .rposition(|(_, from_me)| *from_me)
        .map_or(0, |index| index + 1);
    let unread = unread.min(messages.len() - after_own);
    let unread_at = messages.len() - unread;
    let mut previous_day = None;
    messages
        .iter()
        .enumerate()
        .map(|(index, (timestamp, _))| {
            let day = crate::util::day_label(*timestamp);
            let mut prefix = String::new();
            if previous_day.as_deref() != Some(day.as_str()) {
                prefix.push_str(&format!("── {day} ──\n"));
                previous_day = Some(day);
            }
            if unread > 0 && index == unread_at {
                prefix.push_str(&format!("── {UNREAD_MARKER} ──\n"));
            }
            prefix
        })
        .collect()
}

pub(super) fn is_edit_completion(
    pending: Option<&(String, String, String)>,
    chat: &str,
    id: &str,
) -> bool {
    pending.is_some_and(|(pending_chat, pending_id, _)| pending_chat == chat && pending_id == id)
}

/// "photo.jpg", "photo.jpg and 2 more", or "2 attachments ready" when only
/// a clipboard image has no file name.
pub(super) fn attachment_summary(names: &[String], count: usize) -> String {
    match names {
        [] => format!(
            "{count} attachment{} ready",
            if count == 1 { "" } else { "s" }
        ),
        [name] if count == 1 => name.clone(),
        [name, ..] => format!("{name} and {} more", count - 1),
    }
}

pub(super) fn caption(text: &str) -> Option<String> {
    Some(text.trim().to_owned()).filter(|text| !text.is_empty())
}

pub(super) fn convert_premultiplied_bgra_to_rgba(pixels: &mut [u8]) {
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

pub(super) fn sanitized_error_feedback(error: &str) -> &'static str {
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

/// Contacts offered in New Chat as (id, name, formatted phone): only people
/// saved in the address book, so not everyone who has messaged or joined a
/// group with us.
pub(super) fn new_chat_contacts(
    contacts: &std::collections::HashMap<String, crate::model::Contact>,
) -> Vec<(String, String, String)> {
    let mut rows: Vec<_> = contacts
        .values()
        .filter_map(|contact| {
            let phone = crate::util::phone(crate::model::phone_of(&contact.id)?);
            let name = contact.full_name.as_deref()?.trim();
            (!name.is_empty()).then(|| (contact.id.clone(), name.to_owned(), phone))
        })
        .collect();
    rows.sort_by_cached_key(|(_, name, _)| name.to_lowercase());
    rows
}

pub(super) fn forwardable_chat(chat: &crate::model::Chat) -> bool {
    chat.kind != crate::model::ChatKind::Broadcast && chat.can_send()
}

/// Text a message shows, if any: its body or a media caption.
pub(super) fn message_text(message: &crate::model::Message) -> Option<String> {
    let text = match &message.content {
        crate::model::Content::Text { text, .. } => Some(text.clone()),
        crate::model::Content::Image { caption, .. }
        | crate::model::Content::Video { caption, .. }
        | crate::model::Content::Document { caption, .. } => caption.clone(),
        content => content.interactive_lines(),
    };
    text.filter(|text| !text.trim().is_empty())
}
