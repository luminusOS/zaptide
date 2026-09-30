//! Message labels and text projections shared by rows and transcript actions.

/// Best label for a sender: saved or pushed name, else formatted phone number.
pub(super) fn sender_label(name: Option<&str>, id: &str) -> String {
    if let Some(name) = name.filter(|name| !name.is_empty()) {
        return name.to_owned();
    }
    crate::model::phone_of(id)
        .map(crate::util::phone)
        .unwrap_or_else(|| id.to_owned())
}

pub(super) fn delivery_label(delivery: crate::model::Delivery) -> &'static str {
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

/// Compact delivery indicator: check glyphs, highlighted once read, or an icon.
pub(super) fn delivery_mark(
    delivery: crate::model::Delivery,
) -> (&'static str, Option<&'static str>, bool) {
    match delivery {
        crate::model::Delivery::Pending => ("", Some("document-open-recent-symbolic"), false),
        crate::model::Delivery::Failed => ("", Some("dialog-error-symbolic"), false),
        crate::model::Delivery::Sent => ("✓", None, false),
        crate::model::Delivery::Delivered => ("✓✓", None, false),
        crate::model::Delivery::Read | crate::model::Delivery::Played => ("✓✓", None, true),
        crate::model::Delivery::None => ("", None, false),
    }
}

/// Names for mentioned people: saved or WhatsApp name, formatted phone, or token.
pub(super) fn mention_labels(
    message: &crate::model::Message,
    contacts: &std::collections::HashMap<String, crate::model::Contact>,
) -> Vec<crate::safety::MentionLabel> {
    message
        .mentions
        .iter()
        .map(|mention| {
            let phone = crate::model::phone_of(&mention.id);
            let saved = contacts
                .get(&mention.id)
                .and_then(|contact| contact.full_name.as_deref())
                .filter(|name| !name.is_empty());
            let label = saved
                .map(str::to_owned)
                .or_else(|| phone.map(crate::util::phone))
                .unwrap_or_else(|| mention.user.clone());
            let hint = match (saved, &mention.name) {
                (None, Some(name)) => Some(format!("~{name}")),
                _ => None,
            };
            crate::safety::MentionLabel {
                user: mention.user.clone(),
                label,
                phone: phone.map(str::to_owned),
                hint,
            }
        })
        .collect()
}

pub(super) fn message_group_boundaries(messages: &[crate::model::Message]) -> Vec<(bool, bool)> {
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

/// Reactions grouped by emoji, in first-seen order: (emoji, count, ours).
pub(super) fn reaction_counts(reactions: &[crate::model::Reaction]) -> Vec<(String, usize, bool)> {
    let mut counts = Vec::<(String, usize, bool)>::new();
    for reaction in reactions
        .iter()
        .filter(|reaction| !reaction.emoji.is_empty())
    {
        match counts.iter_mut().find(|entry| entry.0 == reaction.emoji) {
            Some(entry) => {
                entry.1 += 1;
                entry.2 |= reaction.from_me;
            }
            None => counts.push((reaction.emoji.clone(), 1, reaction.from_me)),
        }
    }
    counts
}

pub(super) fn transcript_row(
    message: &crate::model::Message,
    contacts: &std::collections::HashMap<String, crate::model::Contact>,
) -> crate::native_transcript::TranscriptRow {
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
        text: crate::safety::display_mentions(
            &transcript_text(message),
            &mention_labels(message, contacts),
        ),
    }
}

pub(super) fn transcript_text(message: &crate::model::Message) -> String {
    match &message.content {
        crate::model::Content::Text { text, .. } => text.clone(),
        content => content
            .interactive_lines()
            .unwrap_or_else(|| message.summary()),
    }
}

pub(super) fn editable_text(message: &crate::model::Message) -> Option<String> {
    match (&message.from_me, &message.content) {
        (true, crate::model::Content::Text { text, .. }) => Some(text.clone()),
        _ => None,
    }
}
