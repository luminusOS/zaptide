//! Toolkit-neutral retention window for the active conversation.
//!
//! The native shell keeps only a bounded slice of the loaded messages in its
//! GTK models; everything older or newer is evicted until the user pages back.
//! The cap and eviction rule live here so the retention budget can be measured
//! without a display server.

/// Maximum decoded messages retained for the active conversation.
pub const ACTIVE_MESSAGE_LIMIT: usize = 2_000;

/// Trims `ids` to at most `limit` entries and returns the evicted ids.
///
/// Pages of older messages are prepended, so eviction drops the opposite end:
/// the newest ids when older history was prepended, the oldest ids when newer
/// messages were appended.
pub fn trim_message_window(
    ids: &mut Vec<String>,
    prepending_older: bool,
    limit: usize,
) -> Vec<String> {
    let mut removed = Vec::new();
    while ids.len() > limit {
        let index = if prepending_older { ids.len() - 1 } else { 0 };
        removed.push(ids.remove(index));
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Content, Delivery, Message};
    use std::collections::HashMap;
    use std::time::Instant;

    fn synthetic_message(chat: &str, index: usize) -> Message {
        Message {
            id: format!("MSG{index:06}"),
            chat: chat.into(),
            sender: format!("{index}@s.whatsapp.net"),
            sender_name: None,
            from_me: index.is_multiple_of(2),
            timestamp: 1_700_000_000 + index as i64,
            content: Content::text(format!("Synthetic message {index}")),
            status: Delivery::None,
            delivered_at: None,
            read_at: None,
            quoted: None,
            reactions: Vec::new(),
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: None,
        }
    }

    #[test]
    fn older_page_window_keeps_new_rows_and_existing_anchor() {
        let mut ids = vec!["old-a".into(), "old-b".into(), "old-c".into()];
        ids.splice(0..0, ["older-a".into(), "older-b".into()]);
        let removed = trim_message_window(&mut ids, true, 3);

        assert_eq!(removed, ["old-c", "old-b"]);
        assert_eq!(ids, ["older-a", "older-b", "old-a"]);
    }

    /// Feeds a 100,000-message synthetic fixture through the same id window
    /// and snapshot map that `application.rs` maintains for the active chat,
    /// confirming only `ACTIVE_MESSAGE_LIMIT` entries are ever retained.
    #[test]
    fn active_window_caps_100k_synthetic_messages_at_limit() {
        const TOTAL: usize = 100_000;
        const PAGE: usize = 2_500;
        const CHAT: &str = "1234@s.whatsapp.net";

        let mut ids: Vec<String> = Vec::new();
        let mut snapshots: HashMap<String, Message> = HashMap::new();
        let start = Instant::now();
        for page in (0..TOTAL).step_by(PAGE) {
            for index in page..page + PAGE {
                let message = synthetic_message(CHAT, index);
                ids.push(message.id.clone());
                snapshots.insert(message.id.clone(), message);
            }
            for id in trim_message_window(&mut ids, false, ACTIVE_MESSAGE_LIMIT) {
                snapshots.remove(&id);
            }
            assert!(
                ids.len() <= ACTIVE_MESSAGE_LIMIT,
                "window exceeded the cap after page {page}"
            );
            assert_eq!(
                snapshots.len(),
                ids.len(),
                "evicted snapshots must not outlive their ids"
            );
        }
        let elapsed = start.elapsed();

        assert_eq!(ids.len(), ACTIVE_MESSAGE_LIMIT);
        assert_eq!(snapshots.len(), ACTIVE_MESSAGE_LIMIT);
        assert_eq!(ids.first().map(String::as_str), Some("MSG098000"));
        assert_eq!(ids.last().map(String::as_str), Some("MSG099999"));
        assert!(
            ids.windows(2).all(|pair| pair[0] < pair[1]),
            "retained window keeps chronological order"
        );

        eprintln!(
            "message_window_100k: feed+trim total={elapsed:?} for {TOTAL} messages, retained={}",
            ids.len()
        );
    }
}
