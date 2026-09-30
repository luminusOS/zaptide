use super::*;

pub(super) fn drain_and_convert(
    backend: Option<&Backend>,
    notifier: &EventNotifier,
) -> Vec<NativeEvent> {
    let mut events = Vec::new();
    if let Some(backend) = backend {
        drain_backend_events(backend, notifier, |event| match event {
            Event::Link(status) => events.push(NativeEvent::Link(status)),
            Event::Syncing(syncing) => events.push(NativeEvent::Syncing(syncing)),
            Event::Chats(rows) => events.push(NativeEvent::Chats(rows)),
            Event::ChatUpdated(chat) => events.push(NativeEvent::ChatUpdated(chat)),
            Event::Contacts(contacts) => events.push(NativeEvent::Contacts(contacts)),
            Event::Messages {
                chat,
                messages,
                older,
                complete,
            } => events.push(NativeEvent::Messages {
                chat,
                messages,
                older,
                complete,
            }),
            Event::OlderFetched { chat, more } => {
                events.push(NativeEvent::OlderFetched { chat, more })
            }
            Event::Stickers {
                saved,
                packs,
                recent,
            } => events.push(NativeEvent::Stickers {
                saved,
                packs,
                recent,
            }),
            Event::MessageUpdated(message) => events.push(NativeEvent::MessageUpdated(message)),
            Event::Edited { chat, id, success } => {
                events.push(NativeEvent::Edited { chat, id, success })
            }
            Event::Sent { chat, success } => events.push(NativeEvent::Sent { chat, success }),
            Event::AttachmentCompleted {
                chat,
                path,
                success,
                ..
            } => events.push(NativeEvent::AttachmentCompleted {
                chat,
                path,
                success,
            }),
            Event::Media {
                chat,
                message,
                result,
            } => events.push(NativeEvent::Media {
                chat,
                message,
                result,
            }),
            Event::ReceiptsPrivacy { disabled } => {
                events.push(NativeEvent::ReceiptsPrivacy { disabled })
            }
            Event::ContactReady { id, name } => events.push(NativeEvent::ContactReady { id, name }),
            Event::ContactAbout { id, about } => {
                events.push(NativeEvent::ContactAbout { id, about })
            }
            Event::Info(message) => events.push(NativeEvent::Info(message)),
            Event::MessageDeleted { chat, id } => {
                events.push(NativeEvent::MessageDeleted { chat, id })
            }
            Event::Incoming { chat, message } => {
                events.push(NativeEvent::Incoming { chat, message })
            }
            Event::Typing {
                chat,
                sender,
                composing,
            } => events.push(NativeEvent::Typing {
                chat,
                sender,
                composing,
            }),
            Event::Presence {
                id,
                online,
                last_seen,
            } => events.push(NativeEvent::Presence {
                id,
                online,
                last_seen,
            }),
            Event::Avatar {
                id,
                full: false,
                path,
            } => events.push(NativeEvent::Avatar { id, path }),
            Event::Error(error) => events.push(NativeEvent::Error(error)),
            _ => {}
        });
    }
    events
}
