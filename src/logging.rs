//! Process logging with redaction of third-party records.
//!
//! whatsapp-rust logs raw protocol nodes at warning level, for example an
//! unrecognized top-level stanza with its sender's phone number and push
//! name. ZapTide shows library warnings by default, so those records are
//! reduced to their text and node tag, and long digit runs are masked,
//! before they reach the terminal or journal.

use std::fmt::Write as _;

/// Installs the logger: `RUST_LOG` when set, otherwise library warnings and
/// ZapTide information.
pub fn init() {
    let inner = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,zaptide=info"),
    )
    .build();
    let max_level = inner.filter();
    if log::set_boxed_logger(Box::new(RedactingLogger { inner })).is_ok() {
        log::set_max_level(max_level);
    }
}

struct RedactingLogger {
    inner: env_logger::Logger,
}

impl log::Log for RedactingLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.inner.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        if !self.inner.matches(record) {
            return;
        }
        if is_own(record.target()) {
            self.inner.log(record);
            return;
        }
        let message = redact(&record.args().to_string());
        self.inner.log(
            &log::Record::builder()
                .args(format_args!("{message}"))
                .metadata(record.metadata().clone())
                .module_path(record.module_path())
                .file(record.file())
                .line(record.line())
                .build(),
        );
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

fn is_own(target: &str) -> bool {
    target == "zaptide" || target.starts_with("zaptide::")
}

/// Digit runs at least this long are masked: phone numbers, user and group
/// JIDs and LIDs, while short counts, codes and versions stay readable. Long
/// timestamps, byte counts and identifiers are masked too, which is accepted.
/// Numbers split by spaces or dashes are not recognized; the library prints
/// identities as JIDs.
const MASKED_DIGITS: usize = 7;

fn redact(message: &str) -> String {
    let message = drop_node_attributes(message);
    let mut redacted = String::with_capacity(message.len());
    let mut digits = String::new();
    for character in message.chars().chain(std::iter::once('\0')) {
        if character.is_ascii_digit() {
            digits.push(character);
            continue;
        }
        if digits.len() >= MASKED_DIGITS {
            redacted.push_str("[number]");
        } else {
            redacted.push_str(&digits);
        }
        digits.clear();
        if character != '\0' {
            redacted.push(character);
        }
    }
    redacted
}

/// Cuts the message at the first protocol node with content, whose attributes
/// and children carry identities, and keeps only its tag. Nodes render as
/// `<tag attr="…">` or, through a derived `Debug`, as `Node { tag: "…", … }`.
/// A bare `<tag>` without a matching `</tag>` names a stanza in prose and
/// stays, and a `<` that
/// continues a word, as in a generic type name, does not start a node.
fn drop_node_attributes(message: &str) -> std::borrow::Cow<'_, str> {
    let mut search = 0;
    while let Some(offset) = message[search..].find(['<', 'N']) {
        let index = search + offset;
        let rest = &message[index..];
        search = index + 1;
        let follows_word = message[..index]
            .chars()
            .next_back()
            .is_some_and(|previous| previous.is_alphanumeric() || previous == '_');
        if follows_word {
            continue;
        }
        if let Some(debug) = rest.strip_prefix("Node { tag: \"") {
            let tag = debug.split('"').next().unwrap_or_default();
            return format!("{}Node {{ tag: \"{tag}\" … }}", &message[..index]).into();
        }
        let Some(element) = rest.strip_prefix('<') else {
            continue;
        };
        let tag: String = element
            .chars()
            .take_while(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | ':' | '-' | '.')
            })
            .collect();
        if tag.is_empty() || !tag.starts_with(|first: char| first.is_ascii_alphabetic()) {
            continue;
        }
        let after = &element[tag.len()..];
        let has_content = match after.chars().next() {
            Some(next) => next.is_whitespace() || next == '/',
            None => false,
        } || (after.starts_with('>') && after.contains(&format!("</{tag}>")));
        if has_content {
            let mut kept = message[..index].to_owned();
            let _ = write!(kept, "<{tag} …>");
            return kept.into();
        }
    }
    message.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_dumps_keep_only_their_tag() {
        let message = r#"Received unknown top-level node: <status from="120363000000000000@g.us" participant_pn="5500900000000@s.whatsapp.net" notify="Synthetic Name"><enc type="pkmsg"/></status>"#;
        assert_eq!(
            redact(message),
            "Received unknown top-level node: <status …>"
        );
    }

    #[test]
    fn namespaced_and_text_only_nodes_keep_only_their_tag() {
        assert_eq!(
            redact(r#"Unknown stream error: <stream:error code="515" reason="Synthetic"/>"#),
            "Unknown stream error: <stream:error …>"
        );
        assert_eq!(
            redact("Received unknown top-level node: <status>Synthetic Name</status>"),
            "Received unknown top-level node: <status …>"
        );
    }

    #[test]
    fn debug_rendered_nodes_keep_only_their_tag() {
        let node = whatsapp_rust::wacore_binary::builder::NodeBuilder::new("failure")
            .attr("from", "5500900000000@s.whatsapp.net")
            .attr("notify", "Synthetic Name")
            .build();
        let error = whatsapp_rust::IqError::Disconnected(Box::new(node));
        assert_eq!(
            redact(&error.to_string()),
            "received disconnect node during IQ wait: <failure …>"
        );
        assert_eq!(
            redact(&format!("Failed to send post-connect active IQ: {error:?}")),
            "Failed to send post-connect active IQ: Disconnected(<failure …>"
        );
        assert_eq!(
            redact(
                r#"Disconnected(Node { tag: "failure", attrs: {"from": "5500900000000@s.whatsapp.net"} })"#
            ),
            "Disconnected(Node { tag: \"failure\" … }"
        );
    }

    #[test]
    fn long_digit_runs_are_masked() {
        assert_eq!(
            redact("Failed to parse LID from success stanza: 100000000000001@lid"),
            "Failed to parse LID from success stanza: [number]@lid"
        );
        assert_eq!(
            redact("Received unhandled IQ for 5500900000000:12@s.whatsapp.net"),
            "Received unhandled IQ for [number]:12@s.whatsapp.net"
        );
    }

    #[test]
    fn ordinary_messages_are_unchanged() {
        for message in [
            "Transport disconnected: timeout; reconnecting.",
            "app state sync from server_sync: incomplete (fatal=[] retryable=[] skipped=[RegularLow])",
            "retry 3 of 5 after 250 ms",
            "x < 3 and a <= b",
            "invalid payload for Vec<u8>: unexpected end",
            "LID not found in <success> stanza. Group messaging may fail.",
            "ServerError { code: 400, text: \"bad\", response: <failure> }",
            "NodeKind changed",
        ] {
            assert_eq!(redact(message), message);
        }
    }

    #[derive(Clone, Default)]
    struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn logger_redacts_library_records_and_keeps_their_metadata() {
        use log::Log as _;
        use std::io::Write as _;
        let captured = Captured::default();
        let logger = RedactingLogger {
            inner: env_logger::Builder::new()
                .parse_filters("warn,zaptide=info")
                .target(env_logger::Target::Pipe(Box::new(captured.clone())))
                .format(|out, record| {
                    writeln!(
                        out,
                        "{} {} {}",
                        record.level(),
                        record.target(),
                        record.args()
                    )
                })
                .build(),
        };
        let record = |level, target, message: &str| {
            logger.log(
                &log::Record::builder()
                    .level(level)
                    .target(target)
                    .args(format_args!("{message}"))
                    .build(),
            );
        };
        record(
            log::Level::Warn,
            "whatsapp_rust::client::node_io",
            r#"Received unknown top-level node: <status notify="Synthetic Name">"#,
        );
        record(
            log::Level::Info,
            "zaptide::backend::worker",
            "chat list holds 1234567 chats",
        );
        record(log::Level::Info, "whatsapp_rust::client", "filtered out");
        logger.flush();
        assert_eq!(
            String::from_utf8(captured.0.lock().unwrap().clone()).unwrap(),
            "WARN whatsapp_rust::client::node_io Received unknown top-level node: <status …>\n\
             INFO zaptide::backend::worker chat list holds 1234567 chats\n"
        );
    }

    #[test]
    fn own_records_are_recognized_by_crate_target() {
        assert!(is_own("zaptide"));
        assert!(is_own("zaptide::backend::worker"));
        assert!(!is_own("zaptide_extra"));
        assert!(!is_own("whatsapp_rust::client::node_io"));
        assert!(!is_own("Client/AppState"));
    }
}
