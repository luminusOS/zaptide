//! Serialize and back off automatic phone-history requests for poll results.

use crate::model::ChatId;
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

type Key = (ChatId, String);
#[derive(Default)]
pub(super) struct Requests {
    queue: VecDeque<(Key, Instant)>,
    active: Option<(Key, Instant)>,
    tried: HashMap<Key, u32>,
}

impl Requests {
    /// Pending, requested this session, and waiting after a failed attempt.
    pub fn state(&self, chat: &str, poll: &str) -> (bool, bool, bool) {
        let key = (chat.to_owned(), poll.to_owned());
        let queued = self.queue.iter().any(|(pending, _)| pending == &key);
        (
            queued
                || self
                    .active
                    .as_ref()
                    .is_some_and(|(active, _)| active == &key),
            self.tried.contains_key(&key),
            queued && self.tried.get(&key).is_some_and(|&failures| failures > 0),
        )
    }

    pub fn next(&mut self, now: Instant) -> Option<Key> {
        if self.active.is_some() {
            return None;
        }
        let index = self.queue.iter().position(|(_, due)| *due <= now)?;
        let (key, _) = self.queue.remove(index)?;
        self.active = Some((key.clone(), now));
        Some(key)
    }

    pub fn finish(&mut self, chat: &str, poll: &str) {
        let key = (chat.to_owned(), poll.to_owned());
        self.queue.retain(|(pending, _)| pending != &key);
        if self
            .active
            .as_ref()
            .is_some_and(|(active, _)| active == &key)
        {
            self.active = None;
        }
        self.tried.insert(key, 0);
    }

    pub fn fail(&mut self, chat: &str, poll: &str, requested: Instant, now: Instant) {
        if self
            .active
            .as_ref()
            .is_some_and(|((active_chat, active_poll), started)| {
                active_chat == chat && active_poll == poll && *started == requested
            })
        {
            self.defer(now);
        }
    }

    fn defer(&mut self, now: Instant) -> Option<Key> {
        let (key, _) = self.active.take()?;
        let failures = self.tried.entry(key.clone()).or_default();
        *failures = failures.saturating_add(1);
        let delay = (30 * (1_u64 << (*failures - 1).min(5))).min(900);
        self.queue
            .push_back((key.clone(), now + Duration::from_secs(delay)));
        Some(key)
    }

    pub fn expire(&mut self, now: Instant) -> Option<Key> {
        if self.active.as_ref().is_some_and(|(_, started)| {
            now.saturating_duration_since(*started) >= Duration::from_secs(30)
        }) {
            return self.defer(now);
        }
        None
    }

    pub fn reconnect(&mut self, now: Instant) {
        for (_, due) in &mut self.queue {
            *due = now;
        }
    }
}
