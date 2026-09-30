//! Toolkit-neutral chat-list search, filtering, and stable selection.

use crate::model::{Chat, ChatId};

/// How archived chats participate in a projection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ArchiveFilter {
    /// Show only chats that are not archived.
    #[default]
    Exclude,
    /// Show archived chats only.
    Only,
}

/// How muted chats participate in a projection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MutedFilter {
    /// Do not filter by mute state.
    #[default]
    All,
    /// Show muted chats only.
    Only,
}

/// Chat kinds included in the sidebar projection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChatKindFilter {
    /// Show private chats and groups.
    #[default]
    All,
    /// Show private chats only.
    Private,
    /// Show groups only.
    Groups,
}

impl ChatKindFilter {
    /// Applies chat-kind selection to projection filters.
    pub fn apply(self, filters: &mut ChatListFilters) {
        filters.private_only = matches!(self, Self::Private);
        filters.groups_only = matches!(self, Self::Groups);
    }
}

/// Optional predicates applied to the chat snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChatListFilters {
    pub unread_only: bool,
    pub pinned_only: bool,
    pub private_only: bool,
    pub groups_only: bool,
    pub archive: ArchiveFilter,
    pub muted: MutedFilter,
}

/// Search and filter projection over an ordered chat snapshot.
///
/// Selection is stored by chat ID, not list position, so snapshot reorderings
/// and updates do not change which chat is selected. A removed chat clears
/// selection; filtering a selected chat out of view does not.
#[derive(Clone, Debug, Default)]
pub struct ChatListProjection {
    chats: Vec<Chat>,
    query: String,
    filters: ChatListFilters,
    selected_id: Option<ChatId>,
}

impl ChatListProjection {
    /// Replaces current snapshot, retaining selection when its ID still exists.
    /// Duplicate IDs in the input collapse in place.
    pub fn replace_snapshot(&mut self, chats: impl IntoIterator<Item = Chat>) {
        self.chats.clear();
        // Scanning the growing list per chat would make a 10k snapshot
        // replacement quadratic.
        let mut positions: std::collections::HashMap<ChatId, usize> =
            std::collections::HashMap::new();
        for chat in chats {
            match positions.get(&chat.id) {
                Some(&index) => self.chats[index] = chat,
                None => {
                    positions.insert(chat.id.clone(), self.chats.len());
                    self.chats.push(chat);
                }
            }
        }
        // Pinned chats lead, most recently pinned first; the rest keep the
        // snapshot's activity order.
        self.chats
            .sort_by_key(|chat| std::cmp::Reverse((chat.pinned, chat.pinned_at)));
        self.clear_selection_if_missing();
    }

    /// Sets free-text query. Matching is case-insensitive and all query words
    /// must occur in either the chat name or latest-message summary.
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn set_filters(&mut self, filters: ChatListFilters) {
        self.filters = filters;
    }

    /// Iterates visible chats in snapshot order. Locked chats are always
    /// excluded from this normal chat-list projection.
    pub fn visible(&self) -> impl Iterator<Item = &Chat> {
        let terms: Vec<String> = self
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let now = crate::util::now();
        self.chats.iter().filter(move |chat| {
            !chat.locked
                && terms_match(chat, &terms)
                && (!self.filters.unread_only || chat.unread > 0)
                && (!self.filters.pinned_only || chat.pinned)
                && (!self.filters.private_only || chat.kind == crate::model::ChatKind::Direct)
                && (!self.filters.groups_only || chat.kind == crate::model::ChatKind::Group)
                && match self.filters.archive {
                    ArchiveFilter::Exclude => !chat.archived,
                    ArchiveFilter::Only => chat.archived,
                }
                && match self.filters.muted {
                    MutedFilter::All => true,
                    MutedFilter::Only => chat.muted(now),
                }
        })
    }

    /// Selects a chat by ID if it exists in the snapshot, whether currently
    /// filtered out or visible.
    pub fn select(&mut self, id: impl Into<ChatId>) -> bool {
        let id = id.into();
        if self.chats.iter().any(|chat| chat.id == id) {
            self.selected_id = Some(id);
            true
        } else {
            false
        }
    }

    pub fn selected_id(&self) -> Option<&str> {
        self.selected_id.as_deref()
    }

    pub fn selected_chat(&self) -> Option<&Chat> {
        let id = self.selected_id.as_deref()?;
        self.chats.iter().find(|chat| chat.id == id)
    }

    fn clear_selection_if_missing(&mut self) {
        if self
            .selected_id
            .as_ref()
            .is_some_and(|selected| !self.chats.iter().any(|chat| &chat.id == selected))
        {
            self.selected_id = None;
        }
    }
}

fn terms_match(chat: &Chat, terms: &[String]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let name = chat.name.to_lowercase();
    let summary = chat
        .last
        .as_ref()
        .map_or_else(String::new, |message| message.summary.to_lowercase());
    terms
        .iter()
        .all(|term| name.contains(term) || summary.contains(term))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LastMessage;

    fn chat(id: &str, name: &str) -> Chat {
        Chat::new(id.into(), name.into())
    }

    fn projection(chats: impl IntoIterator<Item = Chat>) -> ChatListProjection {
        let mut projection = ChatListProjection::default();
        projection.replace_snapshot(chats);
        projection
    }

    fn ids(projection: &ChatListProjection) -> Vec<&str> {
        projection.visible().map(|chat| chat.id.as_str()).collect()
    }

    #[test]
    fn searches_names_and_latest_message_summaries() {
        let mut ada = chat("ada@s.whatsapp.net", "Ada Lovelace");
        ada.last = Some(LastMessage {
            from_me: false,
            sender: "ada@s.whatsapp.net".into(),
            sender_name: None,
            summary: "Analytical engine notes".into(),
            status: Default::default(),
        });
        let mut projection = projection([ada, chat("bob@s.whatsapp.net", "Bob")]);

        projection.set_query("LOVELACE");
        assert_eq!(ids(&projection), ["ada@s.whatsapp.net"]);
        projection.set_query("engine notes");
        assert_eq!(ids(&projection), ["ada@s.whatsapp.net"]);
        projection.set_query("ada engine");
        assert_eq!(ids(&projection), ["ada@s.whatsapp.net"]);
        projection.set_query("missing");
        assert!(ids(&projection).is_empty());
    }

    #[test]
    fn pinned_chats_lead_in_pin_order_and_the_rest_keep_activity_order() {
        let mut old_pin = chat("old-pin", "Old pin");
        old_pin.pinned = true;
        old_pin.pinned_at = 100;
        let mut new_pin = chat("new-pin", "New pin");
        new_pin.pinned = true;
        new_pin.pinned_at = 200;
        let projection = projection([
            chat("recent", "Recent"),
            old_pin,
            chat("older", "Older"),
            new_pin,
        ]);
        assert_eq!(ids(&projection), ["new-pin", "old-pin", "recent", "older"]);
    }

    #[test]
    fn applies_unread_pinned_archive_and_muted_filters() {
        let now = crate::util::now();
        let mut unread_pinned = chat("one", "One");
        unread_pinned.unread = 2;
        unread_pinned.pinned = true;
        unread_pinned.muted_until = Some(now + 3600);
        let mut archived = chat("two", "Two");
        archived.archived = true;
        let mut expired = chat("three", "Three");
        expired.muted_until = Some(now - 3600);
        let mut projection = projection([unread_pinned, archived, expired]);

        projection.set_filters(ChatListFilters {
            unread_only: true,
            pinned_only: true,
            ..Default::default()
        });
        assert_eq!(ids(&projection), ["one"]);
        projection.set_filters(ChatListFilters {
            archive: ArchiveFilter::Only,
            ..Default::default()
        });
        assert_eq!(ids(&projection), ["two"]);
        projection.set_filters(ChatListFilters {
            muted: MutedFilter::Only,
            ..Default::default()
        });
        assert_eq!(ids(&projection), ["one"]);
    }

    #[test]
    fn filters_private_chats_and_groups_independently() {
        let mut projection = projection([
            chat("one@s.whatsapp.net", "Private"),
            chat("123@g.us", "Group"),
        ]);
        projection.set_filters(ChatListFilters {
            private_only: true,
            ..Default::default()
        });
        assert_eq!(ids(&projection), ["one@s.whatsapp.net"]);
        projection.set_filters(ChatListFilters {
            groups_only: true,
            ..Default::default()
        });
        assert_eq!(ids(&projection), ["123@g.us"]);
    }

    #[test]
    fn chat_kind_filter_maps_to_projection_flags() {
        let cases = [
            (ChatKindFilter::All, false, false),
            (ChatKindFilter::Private, true, false),
            (ChatKindFilter::Groups, false, true),
        ];
        for (kind, private_only, groups_only) in cases {
            let mut filters = ChatListFilters {
                private_only: true,
                groups_only: true,
                ..Default::default()
            };
            kind.apply(&mut filters);
            assert_eq!(filters.private_only, private_only);
            assert_eq!(filters.groups_only, groups_only);
        }
    }

    #[test]
    fn locked_chats_stay_hidden_and_selection_tracks_id_across_updates() {
        let mut locked = chat("secret", "Secret");
        locked.locked = true;
        let one = chat("one", "One");
        let two = chat("two", "Two");
        let mut projection = projection([one.clone(), locked, two.clone()]);
        assert_eq!(ids(&projection), ["one", "two"]);
        assert!(projection.select("two"));

        let mut updated_two = two;
        updated_two.name = "Updated Two".into();
        projection.replace_snapshot([updated_two, one.clone()]);
        assert_eq!(projection.selected_id(), Some("two"));
        assert_eq!(projection.selected_chat().unwrap().name, "Updated Two");

        projection.set_query("no match");
        assert_eq!(projection.selected_id(), Some("two"));
        projection.replace_snapshot([one]);
        assert_eq!(projection.selected_id(), None);
    }

    #[test]
    fn replaces_10k_chat_snapshots_quickly_and_in_order() {
        use std::time::{Duration, Instant};

        let fixture: Vec<Chat> = (0..10_000)
            .map(|index| chat(&format!("{index}@s.whatsapp.net"), &format!("Chat {index}")))
            .collect();
        let selected = fixture[5_000].id.clone();
        let mut projection = projection(fixture.iter().cloned());
        assert!(projection.select(selected.clone()));

        let mut reversed = fixture;
        reversed.reverse();
        let start = Instant::now();
        projection.replace_snapshot(reversed.iter().cloned());
        assert!(start.elapsed() < Duration::from_millis(500));
        assert_eq!(projection.selected_id(), Some(selected.as_str()));
        assert!(
            ids(&projection)
                .iter()
                .eq(reversed.iter().map(|chat| &chat.id))
        );

        projection.set_query("chat 777");
        assert_eq!(ids(&projection).len(), 19);
    }
}
