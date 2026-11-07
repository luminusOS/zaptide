use super::*;

impl NativeApplication {
    pub(super) fn selected_message_id(&self) -> Option<String> {
        self.message_target.clone()
    }

    pub(super) fn selected_message(&self) -> Option<&crate::model::Message> {
        self.selected_message_id()
            .and_then(|id| self.open_chat.message_snapshots.get(&id))
    }

    pub(super) fn selected_poll_options(&self) -> Option<&[String]> {
        match &self.selected_message()?.content {
            crate::model::Content::Poll { options, .. } => Some(options),
            _ => None,
        }
    }

    pub(super) fn selected_poll_can_vote(&self) -> bool {
        matches!(&self.selected_message().map(|message| &message.content),
            Some(crate::model::Content::Poll { state, .. }) if state.can_vote)
    }

    pub(super) fn react_selected(&mut self, emoji: String) {
        let Some((chat, message)) = self
            .open_chat
            .active_chat
            .clone()
            .zip(self.selected_message_id())
        else {
            return;
        };
        if reaction_drawer::remember_reaction(&mut self.settings.recent_reactions, &emoji) {
            reaction_drawer::set_recent(&self.settings.recent_reactions);
            if let Err(error) = self.save_settings() {
                log::warn!("could not save recent reactions: {error}");
            }
        }
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

    /// Picked rows, counting an album once.
    pub(super) fn selected_rows(&self) -> usize {
        (0..self.messages.len())
            .filter_map(|position| self.messages.get(position))
            .filter(|item| {
                let row = item.borrow();
                row.selected == Some(true) && !row.collapsed
            })
            .count()
    }

    /// Picks or drops the message at `position`; an album goes as a whole.
    pub(super) fn toggle_message_selection(&mut self, position: u32) {
        let Some(item) = self.messages.get(position) else {
            return;
        };
        let ids: Vec<String> = {
            let row = item.borrow();
            if row.album.is_empty() {
                vec![row.id.clone()]
            } else {
                row.album.iter().map(|message| message.id.clone()).collect()
            }
        };
        let Some(selection) = self.message_selection.as_mut() else {
            return;
        };
        if selection.contains(&ids[0]) {
            selection.retain(|id| !ids.contains(id));
        } else {
            selection.extend(ids);
            let order = &self.open_chat.message_ids;
            selection.sort_by_cached_key(|id| order.iter().position(|known| known == id));
        }
        self.rebuild_message_rows(false);
    }

    pub(super) fn forward_selected(&mut self, destinations: Vec<String>) {
        let Some(source) = self.open_chat.active_chat.clone() else {
            return;
        };
        let messages = match self.message_selection.as_ref() {
            Some(selection) => selection.clone(),
            None => self.selected_message_id().into_iter().collect(),
        };
        if messages.is_empty() {
            return;
        }
        let destinations: Vec<_> = destinations
            .into_iter()
            .filter(|destination| {
                *destination != source
                    && self
                        .chat_snapshots
                        .iter()
                        .any(|chat| chat.id == *destination && !chat.archived && !chat.locked)
            })
            .collect();
        if destinations.is_empty() {
            self.status = gettext("Choose another available conversation");
            return;
        }
        let Some(backend) = &self.backend else {
            return;
        };
        for destination in &destinations {
            for message in &messages {
                if let crate::model::Action::Forward {
                    from_chat,
                    message,
                    to_chat,
                } = crate::native_actions::forward(
                    source.clone(),
                    message.clone(),
                    destination.clone(),
                ) {
                    backend.send(crate::backend::Command::Forward {
                        from_chat,
                        message,
                        to_chat,
                    });
                }
            }
        }
        let rows = if self.message_selection.is_some() {
            self.selected_rows()
        } else {
            1
        };
        let what = match rows {
            1 => "message".to_owned(),
            count => format!("{count} messages"),
        };
        self.status = match destinations.len() {
            1 => format!("Forwarding {what}"),
            chats => format!("Forwarding {what} to {chats} chats"),
        };
        if self.message_selection.take().is_some() {
            self.rebuild_message_rows(false);
        }
    }

    pub(super) fn delete_selected(&mut self, everyone: bool) {
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
                    self.status = gettext("Deleting message for everyone");
                }
                crate::model::Action::DeleteForMe { chat, message } => {
                    backend.send(crate::backend::Command::DeleteLocal { chat, id: message });
                    self.status = gettext("Deleting message");
                }
                _ => {}
            }
        }
    }

    pub(super) fn vote_selected(&mut self) {
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
}
