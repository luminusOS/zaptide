//! Lines in the transcript that report something that happened in a chat,
//! such as a pin or a change of the disappearing-messages timer. They are
//! stored like messages so they keep their place, but they are not read from
//! anyone: they never count as unread and never raise a notification.

use super::*;
use crate::model::SystemEvent;

impl Worker {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn store_system_line(
        &mut self,
        chat: &str,
        id: &str,
        sender: String,
        sender_name: Option<String>,
        from_me: bool,
        timestamp: i64,
        event: SystemEvent,
    ) {
        let chat = self.canonical_str(chat);
        self.ensure_chat(&chat, None);
        let row = Message {
            id: id.to_owned(),
            chat: chat.clone(),
            sender,
            sender_name: if from_me { None } else { sender_name },
            from_me,
            timestamp,
            content: Content::System { event },
            status: Delivery::None,
            quoted: None,
            reactions: Vec::new(),
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: None,
            delivered_at: None,
            read_at: None,
        };
        if let Err(error) = self.archive.insert_message(&row, None) {
            log::warn!("could not store a chat event: {error}");
            return;
        }
        let mut stored = self
            .archive
            .message(&chat, id)
            .ok()
            .flatten()
            .unwrap_or(row);
        self.polish(&mut stored);
        self.emit(Event::Messages {
            request: None,
            chat: chat.clone(),
            messages: vec![stored],
            keep_states: Vec::new(),
            older: false,
            complete: false,
        });
        self.emit_chat(&chat);
    }
}
