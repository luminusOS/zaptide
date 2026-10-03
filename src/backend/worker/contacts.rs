use super::*;
use crate::contact_cards::ContactCard;

fn contact_message(contact: &ContactCard, context: Option<wa::ContextInfo>) -> wa::Message {
    wa::Message {
        contact_message: MessageField::some(wa::message::ContactMessage {
            display_name: Some(contact.name.clone()),
            vcard: Some(contact.vcard.clone()),
            context_info: context.map_or_else(MessageField::none, MessageField::some),
            ..Default::default()
        }),
        ..Default::default()
    }
}

impl Worker {
    pub(super) fn send_contact(
        &mut self,
        chat: ChatId,
        contact: ContactCard,
        quoting: Option<String>,
    ) {
        // Re-parse at the backend boundary instead of trusting renderer metadata.
        let cards = crate::contact_cards::parse(&contact.vcard);
        let [contact] = cards.as_slice() else {
            self.emit(Event::Error("Choose one valid contact to send".into()));
            return;
        };
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            self.emit(Event::Error("Not connected to WhatsApp".into()));
            return;
        };
        let (context, quoted) = self.quote_context(&chat, quoting.as_deref());
        let mut message = contact_message(contact, context);
        let expiration = self.apply_ephemeral(&chat, &mut message);
        let id = client.generate_message_id();
        let row = Message {
            id: id.clone(),
            chat: chat.clone(),
            sender: self.me(),
            sender_name: None,
            from_me: true,
            timestamp: crate::util::now(),
            content: Content::Contact {
                display_name: contact.name.clone(),
                vcard: contact.vcard.clone(),
            },
            status: Delivery::Pending,
            delivered_at: None,
            read_at: None,
            quoted,
            reactions: Vec::new(),
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: None,
        };
        self.store_message(row, Some(message.encode_to_vec()), None);
        tokio::spawn(send_outgoing_with_kind(
            OutgoingSession {
                client,
                commands: self.commands.clone(),
                generation: self.session_generation,
                generation_shared: self.session_generation_shared.clone(),
            },
            chat,
            jid,
            id,
            message,
            expiration,
            true,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_wire_round_trip_preserves_vcard_quote_and_expiration() {
        let contact = ContactCard::from_saved("15555550123@s.whatsapp.net", "Ada Example").unwrap();
        let context = wa::ContextInfo {
            stanza_id: Some("QUOTED".into()),
            ..Default::default()
        };
        let mut wire = contact_message(&contact, Some(context));
        assert_eq!(
            apply_ephemeral_expiration(&mut wire, Some(3600)),
            Some(3600)
        );
        let decoded = wa::Message::decode(&mut wire.encode_to_vec().as_slice()).unwrap();
        assert_eq!(
            classify(&decoded),
            Some(Content::Contact {
                display_name: contact.name,
                vcard: contact.vcard
            })
        );
        let context = &decoded.contact_message.as_option().unwrap().context_info;
        assert_eq!(context.stanza_id.as_deref(), Some("QUOTED"));
        assert_eq!(context.expiration, Some(3600));
    }
}
