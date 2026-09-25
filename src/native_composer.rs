use std::collections::HashMap;
use std::fmt;

/// Toolkit-neutral composer state, keyed by chat ID.
///
/// Draft text and captions stay in memory only. Debug output deliberately omits
/// chat IDs, message IDs, and all user-authored text.
#[derive(Default)]
pub struct NativeComposerState {
    chats: HashMap<String, ChatComposerState>,
    next_operation: u64,
}

impl fmt::Debug for NativeComposerState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeComposerState")
            .field("chat_count", &self.chats.len())
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct ChatComposerState {
    draft: String,
    mode: ComposerMode,
    attachment_caption: String,
    revision: u64,
    caption_revision: u64,
    pending: Option<PendingCompletion>,
}

#[derive(Clone, Default, PartialEq, Eq)]
enum ComposerMode {
    #[default]
    Normal,
    Reply {
        message_id: String,
    },
    Edit {
        message_id: String,
    },
}

impl fmt::Debug for ComposerMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Normal => "Normal",
            Self::Reply { .. } => "Reply([redacted])",
            Self::Edit { .. } => "Edit([redacted])",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompletionKind {
    Send,
    Edit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingCompletion {
    operation_id: u64,
    kind: CompletionKind,
    revision: u64,
}

/// Resolves literal `@user` tokens against current group participants.
/// Returns canonical participant IDs once each, in text order.
pub fn mention_ids(text: &str, participants: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    for (start, _) in text.match_indices('@') {
        let end = text[start + 1..]
            .find(|character: char| !character.is_ascii_alphanumeric())
            .map_or(text.len(), |offset| start + 1 + offset);
        let token = &text[start + 1..end];
        if token.is_empty() {
            continue;
        }
        if let Some(id) = participants
            .iter()
            .find(|id| id.split('@').next() == Some(token))
            && !found.contains(id)
        {
            found.push(id.clone());
        }
    }
    found
}

/// Work handed to the application layer for sending or editing.
///
/// Fields are private so callers use accessors; `Debug` never reveals IDs or
/// message content.
pub struct ComposerRequest {
    chat_id: String,
    operation_id: u64,
    kind: CompletionKind,
    message_id: Option<String>,
    text: String,
    revision: u64,
}

impl fmt::Debug for ComposerRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComposerRequest")
            .field("kind", &self.kind)
            .field("text", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl ComposerRequest {
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl NativeComposerState {
    /// Read draft for one chat; drafts remain independent across chat switches.
    pub fn draft(&self, chat_id: &str) -> &str {
        self.chats
            .get(chat_id)
            .map(|state| state.draft.as_str())
            .unwrap_or_default()
    }

    /// Replace draft without normalizing whitespace or line endings.
    pub fn set_draft(&mut self, chat_id: &str, draft: impl Into<String>) {
        let state = self.chats.entry(chat_id.to_owned()).or_default();
        state.draft = draft.into();
        state.revision = state.revision.wrapping_add(1);
    }

    /// Enter reply context while retaining current draft and staged caption.
    pub fn begin_reply(&mut self, chat_id: &str, message_id: impl Into<String>) {
        self.chats.entry(chat_id.to_owned()).or_default().mode = ComposerMode::Reply {
            message_id: message_id.into(),
        };
    }

    /// Enter edit context and load selected message text into its chat draft.
    pub fn begin_edit(
        &mut self,
        chat_id: &str,
        message_id: impl Into<String>,
        message_text: impl Into<String>,
    ) {
        let state = self.chats.entry(chat_id.to_owned()).or_default();
        state.mode = ComposerMode::Edit {
            message_id: message_id.into(),
        };
        state.draft = message_text.into();
        state.revision = state.revision.wrapping_add(1);
    }

    /// Leave reply/edit mode without discarding the current draft.
    pub fn cancel_context(&mut self, chat_id: &str) {
        self.chats.entry(chat_id.to_owned()).or_default().mode = ComposerMode::Normal;
    }

    /// Completion must be reported through the matching method below.
    pub fn submit(&mut self, chat_id: &str) -> Option<ComposerRequest> {
        let state = self.chats.get(chat_id)?;
        if state.pending.is_some() || state.draft.trim().is_empty() {
            return None;
        }

        let (kind, message_id) = match &state.mode {
            ComposerMode::Normal => (CompletionKind::Send, None),
            ComposerMode::Reply { message_id } => (CompletionKind::Send, Some(message_id.clone())),
            ComposerMode::Edit { message_id } => (CompletionKind::Edit, Some(message_id.clone())),
        };
        let revision = state.revision;
        let text = state.draft.clone();
        let operation_id = self.allocate_operation();
        let state = self.chats.get_mut(chat_id).expect("state checked above");
        state.pending = Some(PendingCompletion {
            operation_id,
            kind,
            revision,
        });
        Some(ComposerRequest {
            chat_id: chat_id.to_owned(),
            operation_id,
            kind,
            message_id,
            text,
            revision,
        })
    }

    /// Complete a send only if request still matches chat and pending send.
    /// Newer text typed while request was in flight is preserved.
    pub fn complete_send(&mut self, request: &ComposerRequest) -> bool {
        self.complete_request(request, CompletionKind::Send)
    }

    /// Complete an edit only if request still matches chat and pending edit.
    pub fn complete_edit(&mut self, request: &ComposerRequest) -> bool {
        self.complete_request(request, CompletionKind::Edit)
    }

    /// Stage attachment caption independently from ordinary draft text.
    pub fn stage_attachment_caption(&mut self, chat_id: &str, caption: impl Into<String>) {
        let state = self.chats.entry(chat_id.to_owned()).or_default();
        state.attachment_caption = caption.into();
        state.caption_revision = state.caption_revision.wrapping_add(1);
    }

    fn complete_request(&mut self, request: &ComposerRequest, kind: CompletionKind) -> bool {
        if request.kind != kind {
            return false;
        }
        let Some(state) = self.chats.get_mut(&request.chat_id) else {
            return false;
        };
        if !matches_pending(state.pending, request.operation_id, kind, request.revision) {
            return false;
        }
        state.pending = None;
        if state.revision == request.revision {
            state.draft.clear();
            state.revision = state.revision.wrapping_add(1);
            if kind == CompletionKind::Edit {
                state.mode = ComposerMode::Normal;
            }
        }
        if kind == CompletionKind::Send
            && let Some(message_id) = request.message_id.as_deref()
            && matches!(&state.mode, ComposerMode::Reply { message_id: current } if current == message_id)
        {
            state.mode = ComposerMode::Normal;
        }
        true
    }

    fn allocate_operation(&mut self) -> u64 {
        self.next_operation = self.next_operation.wrapping_add(1);
        self.next_operation
    }
}

fn matches_pending(
    pending: Option<PendingCompletion>,
    operation_id: u64,
    kind: CompletionKind,
    revision: u64,
) -> bool {
    pending.is_some_and(|pending| {
        pending.operation_id == operation_id && pending.kind == kind && pending.revision == revision
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_drafts_are_retained_per_chat_without_normalization() {
        let mut state = NativeComposerState::default();
        state.set_draft("chat-a", "first line\nsecond line");
        state.set_draft("chat-b", "other draft");

        assert_eq!(state.draft("chat-a"), "first line\nsecond line");
        assert_eq!(state.draft("chat-b"), "other draft");
        assert_eq!(state.draft("missing"), "");
    }

    #[test]
    fn mention_tokens_resolve_only_to_known_participants() {
        let participants = vec![
            "491700000001@s.whatsapp.net".to_owned(),
            "491700000002@s.whatsapp.net".to_owned(),
        ];
        assert_eq!(
            mention_ids(
                "Hi @491700000002, @unknown and @491700000002!",
                &participants
            ),
            ["491700000002@s.whatsapp.net"]
        );
    }

    #[test]
    fn pending_submit_blocks_a_second_request() {
        let mut state = NativeComposerState::default();
        state.set_draft("chat", "one\ntwo");
        let request = state.submit("chat").expect("submit creates request");
        assert_eq!(request.text(), "one\ntwo");
        assert!(state.submit("chat").is_none());
    }
}
