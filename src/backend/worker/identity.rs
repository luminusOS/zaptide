use super::*;

impl Worker {
    pub(super) fn learn_lid(&mut self, lid: &str, pn: &str) {
        if lid.is_empty() || pn.is_empty() {
            return;
        }
        if self
            .identity
            .lid_to_pn
            .get(lid)
            .is_some_and(|known| known == pn)
        {
            return;
        }
        self.identity
            .lid_to_pn
            .insert(lid.to_owned(), pn.to_owned());
        match self.archive.put_lid(lid, pn) {
            Ok(true) => self.emit_chats(),
            Ok(false) => {}
            Err(error) => log::warn!("could not remember an id mapping: {error}"),
        }
    }

    pub(super) fn learn_pair(&mut self, a: &Jid, b: &Jid) {
        if a.is_lid() && b.is_pn() {
            self.learn_lid(a.user_base(), b.user_base());
        } else if a.is_pn() && b.is_lid() {
            self.learn_lid(b.user_base(), a.user_base());
        }
    }

    pub(super) fn learn_source(&mut self, source: &MessageSource) {
        if let Some(alt) = &source.sender_alt {
            let sender = source.sender.clone();
            self.learn_pair(&sender, alt);
        }
        if let Some(alt) = &source.recipient_alt {
            let chat = source
                .recipient
                .clone()
                .unwrap_or_else(|| source.chat.clone());
            self.learn_pair(&chat, alt);
        }
    }

    /// Returns the archive id for a JID, resolving known privacy ids.
    pub(super) fn canonical(&self, jid: &Jid) -> String {
        if jid.is_lid()
            && let Some(pn) = self.identity.lid_to_pn.get(jid.user_base())
        {
            let pn = format!("{pn}@s.whatsapp.net");
            return if self.is_me(&pn) { self.me() } else { pn };
        }
        let id = jid.to_non_ad_string();
        if self.is_me(&id) {
            return self.me();
        }
        id
    }

    pub(super) fn canonical_str(&self, id: &str) -> String {
        match id.parse::<Jid>() {
            Ok(jid) => self.canonical(&jid),
            Err(_) => id.to_owned(),
        }
    }

    pub(super) fn jid_of(id: &str) -> Option<Jid> {
        id.parse().ok()
    }

    // --- names -----------------------------------------------------------
}

impl Worker {
    pub(super) fn saved_name(&self, id: &str) -> Option<String> {
        self.identity
            .contacts
            .get(id)
            .and_then(|contact| contact.full_name.clone())
            .filter(|name| !name.is_empty())
    }

    pub(super) fn contact_name(&self, id: &str) -> Option<String> {
        self.identity.contacts.get(id).and_then(Contact::label)
    }

    /// Resolves a name for a quote or mention.
    pub(super) fn name_for(&self, id: &str) -> Option<String> {
        if self.is_me(id) || id == self.me() {
            return Some("You".to_owned());
        }
        if let Some(name) = self.contact_name(id) {
            return Some(name);
        }
        crate::model::phone_of(id).map(crate::util::phone)
    }

    /// Returns the best current chat name.
    pub(super) fn chat_name(&self, id: &str, push_name: Option<&str>) -> String {
        if id == self.me() {
            return "You".to_owned();
        }
        if let Some(name) = self
            .identity
            .contacts
            .get(id)
            .and_then(|contact| contact.full_name.clone())
            .filter(|name| !name.is_empty())
        {
            return name;
        }
        if let Some(digits) = crate::model::phone_of(id) {
            return crate::util::phone(digits);
        }
        if let Some(name) = push_name
            .filter(|name| !name.is_empty())
            .or_else(|| self.identity.contacts.get(id)?.push_name.as_deref())
        {
            return format!("~{name}");
        }
        fallback_name(id)
    }

    pub(super) fn remember_push_name(&mut self, id: &str, push_name: &str) {
        if let Some(contact) = self.learn_push_name(id, push_name) {
            self.emit(Event::Contacts(vec![contact]));
            self.refresh_chat_name(id);
        }
    }

    /// Saves a changed push name and returns the contact for the caller to
    /// send, so a history chunk can send all of its names in one event.
    pub(super) fn learn_push_name(&mut self, id: &str, push_name: &str) -> Option<Contact> {
        if push_name.is_empty() || id == self.me() {
            return None;
        }
        let contact = self
            .identity
            .contacts
            .entry(id.to_owned())
            .or_insert_with(|| Contact {
                id: id.to_owned(),
                full_name: None,
                push_name: None,
            });
        if contact.push_name.as_deref() == Some(push_name) {
            return None;
        }
        contact.push_name = Some(push_name.to_owned());
        let contact = contact.clone();
        if let Err(error) = self.archive.upsert_contact(&contact) {
            log::warn!("could not save a contact: {error}");
        }
        Some(contact)
    }

    /// Replaces a fallback chat name when a better one is known.
    pub(super) fn refresh_chat_name(&mut self, id: &str) {
        let Ok(Some(chat)) = self.archive.chat(id) else {
            return;
        };
        if chat.kind == ChatKind::Group {
            return;
        }
        let name = self.chat_name(id, None);
        if name != chat.name {
            let _ = self.archive.rename_chat(id, &name);
            self.emit_chat(id);
        }
    }

    pub(super) fn ensure_chat(&mut self, id: &str, push_name: Option<&str>) {
        match self.archive.chat(id) {
            Ok(Some(chat)) => {
                if chat.kind != ChatKind::Group {
                    let name = self.chat_name(id, push_name);
                    if name != chat.name {
                        let _ = self.archive.rename_chat(id, &name);
                    }
                }
            }
            Ok(None) => {
                let name = self.chat_name(id, push_name);
                if self.archive.ensure_chat(id, &name).is_err() {
                    log::warn!("could not create a chat");
                }
            }
            Err(_error) => log::warn!("could not read a chat"),
        }
        if ChatKind::from_id(id) == ChatKind::Group {
            self.request_group_info(id, false);
        }
    }

    /// Queues a group metadata request, at the front when `force` is true.
    pub(super) fn remember_identity(
        &mut self,
        pn: Option<Jid>,
        lid: Option<Jid>,
        name: Option<String>,
    ) {
        if let Some(pn) = pn {
            let pn = pn.to_non_ad_string();
            let _ = self.archive.set_meta("me_pn", &pn);
            self.identity.me_pn = Some(pn);
        }
        if let Some(lid) = lid {
            let lid = lid.to_non_ad_string();
            let _ = self.archive.set_meta("me_lid", &lid);
            self.identity.me_lid = Some(lid);
        }
        if let (Some(pn), Some(lid)) = (self.identity.me_pn.clone(), self.identity.me_lid.clone())
            && let (Some(pn), Some(lid)) = (Self::jid_of(&pn), Self::jid_of(&lid))
        {
            self.learn_pair(&lid, &pn);
        }
        if let Some(name) = name.filter(|name| !name.is_empty()) {
            let _ = self.archive.set_meta("me_name", &name);
            self.identity.me_name = Some(name);
        }
        self.emit_profile();
    }
}
