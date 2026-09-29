//! Validation at the boundary between message content and desktop handlers.

/// Preview metadata may supply a bare host, but never a desktop URI scheme.
pub fn preview_url(value: &str) -> Option<String> {
    if value.chars().any(char::is_control) || value.contains('\\') {
        return None;
    }
    let value = value.trim();
    let url = match url::Url::parse(value) {
        Ok(url) => url,
        Err(_) => url::Url::parse(&format!("https://{value}")).ok()?,
    };
    (matches!(url.scheme(), "http" | "https") && url.host_str().is_some()).then(|| url.to_string())
}

/// Scheme of in-app links that open a WhatsApp chat with a phone number.
pub const CHAT_SCHEME: &str = "zaptide-chat:";

/// Digits of the number a WhatsApp deep link (`wa.me/<n>`, `.../send?phone=<n>`,
/// `whatsapp://send?phone=<n>`) points at.
fn whatsapp_number(url: &url::Url) -> Option<String> {
    let digits = match (url.scheme(), url.host_str()?) {
        ("http" | "https", "wa.me") => url.path().trim_matches('/').to_owned(),
        ("http" | "https", "api.whatsapp.com" | "web.whatsapp.com") | ("whatsapp", "send") => url
            .query_pairs()
            .find(|(key, _)| key == "phone")?
            .1
            .into_owned(),
        _ => return None,
    };
    let digits: String = digits.chars().filter(char::is_ascii_digit).collect();
    (7..=15).contains(&digits.len()).then_some(digits)
}

/// Length of a `+<country code> ...` phone number at the start of `rest`, and its digits.
///
/// Digits come in groups joined by at most two separator characters. A group
/// that would pass 15 digits, or a lone 1–2 digit group after 10 digits (a
/// count or year following the number), ends the number; and a first group of
/// more than three digits followed by more groups is a date, not a country code.
fn phone_at(rest: &str) -> Option<(usize, String)> {
    let bytes = rest.as_bytes();
    let (mut digits, mut end, mut groups, mut first) = (String::new(), 0, 0, 0);
    let (mut index, mut separators) = (1, 0);
    while index < bytes.len() {
        if bytes[index].is_ascii_digit() {
            let start = index;
            while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                index += 1;
            }
            let group = &rest[start..index];
            if digits.len() + group.len() > 15
                || (groups > 0 && group.len() <= 2 && digits.len() >= 10)
            {
                break;
            }
            digits.push_str(group);
            end = index;
            groups += 1;
            if groups == 1 {
                first = group.len();
            }
            separators = 0;
        } else if matches!(bytes[index], b' ' | b'-' | b'(' | b')' | b'.') {
            separators += 1;
            if separators > 2 {
                break;
            }
            index += 1;
        } else {
            break;
        }
    }
    let valid =
        rest.starts_with('+') && (8..=15).contains(&digits.len()) && !(groups > 1 && first > 3);
    valid.then_some((end, digits))
}

/// Link at the start of `rest`: its byte length, target, and optional tooltip.
fn link_at(rest: &str) -> Option<(usize, String, Option<&'static str>)> {
    if rest.starts_with('+') {
        let (len, digits) = phone_at(rest)?;
        return Some((
            len,
            format!("{CHAT_SCHEME}{digits}"),
            Some("Message on WhatsApp"),
        ));
    }
    if !["http://", "https://", "www.", "whatsapp://"]
        .iter()
        .any(|prefix| rest.starts_with(prefix))
    {
        return None;
    }
    let word = rest.split(char::is_whitespace).next()?;
    // Trailing punctuation is not part of the link; a `)` is only when
    // unmatched, so `.../Foo_(bar)` keeps its own parenthesis.
    let mut link = word;
    loop {
        let trimmed = link.trim_end_matches(['.', ',', ';', ':', '!', '?', ']', '"', '\'']);
        let trimmed = if trimmed.ends_with(')')
            && trimmed.matches(')').count() > trimmed.matches('(').count()
        {
            &trimmed[..trimmed.len() - 1]
        } else {
            trimmed
        };
        if trimmed.len() == link.len() {
            break;
        }
        link = trimmed;
    }
    let len = link.len();
    if link.starts_with("whatsapp://") {
        let digits = whatsapp_number(&url::Url::parse(link).ok()?)?;
        return Some((
            len,
            format!("{CHAT_SCHEME}{digits}"),
            Some("Message on WhatsApp"),
        ));
    }
    let url = preview_url(link)?;
    match whatsapp_number(&url::Url::parse(&url).ok()?) {
        Some(digits) => Some((
            len,
            format!("{CHAT_SCHEME}{digits}"),
            Some("Message on WhatsApp"),
        )),
        None => Some((len, url, None)),
    }
}

/// Phone number from an in-app chat link produced by [`linkify_markup`].
pub fn chat_link_number(uri: &str) -> Option<&str> {
    uri.strip_prefix(CHAT_SCHEME)
}

/// A person mentioned in a message: the `@user` token in its text, the name
/// to show, and their phone digits when the chat can be opened from it.
#[derive(Clone, Debug, PartialEq)]
pub struct MentionLabel {
    pub user: String,
    pub label: String,
    pub phone: Option<String>,
    /// Shown on hover: who this is on WhatsApp when the label is not a name.
    pub hint: Option<String>,
}

/// Scheme of a mention link with no destination, kept only for its tooltip.
pub const MENTION_SCHEME: &str = "zaptide-mention:";

/// Markup for the `@user` token at the start of `rest` when it names a known
/// mention: a chat link when the phone is known, bold otherwise.
fn mention_at(rest: &str, mentions: &[MentionLabel]) -> Option<(usize, String)> {
    use gtk4::glib::markup_escape_text as esc;
    let token = rest.strip_prefix('@')?;
    let end = token
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(token.len());
    let mention = mentions
        .iter()
        .find(|mention| !mention.user.is_empty() && mention.user == token[..end])?;
    let shown = format!("@{}", mention.label);
    let title = mention
        .hint
        .as_deref()
        .map_or_else(|| "Message on WhatsApp".into(), esc);
    let markup = match (&mention.phone, &mention.hint) {
        (Some(phone), _) => format!(
            "<a href=\"{CHAT_SCHEME}{}\" title=\"{title}\">{}</a>",
            esc(phone),
            esc(&shown)
        ),
        (None, Some(_)) => format!(
            "<a href=\"{MENTION_SCHEME}\" title=\"{title}\">{}</a>",
            esc(&shown)
        ),
        (None, None) => format!("<b>{}</b>", esc(&shown)),
    };
    Some((1 + end, markup))
}

/// Pango markup for message text with web addresses, WhatsApp links and
/// `+` phone numbers turned into links. Everything else is escaped, so
/// message text can never inject markup.
pub fn linkify_markup(text: &str) -> String {
    linkify_markup_with_mentions(text, &[])
}

/// [`linkify_markup`] that also highlights the given `@user` mentions.
pub fn linkify_markup_with_mentions(text: &str, mentions: &[MentionLabel]) -> String {
    use gtk4::glib::markup_escape_text as esc;
    // Pango rejects the character references GLib emits for control characters.
    let text: String = text
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect();
    let text = text.as_str();
    let mut out = String::with_capacity(text.len());
    let (mut plain, mut index, mut boundary) = (0, 0, true);
    while let Some(c) = text[index..].chars().next() {
        let rest = &text[index..];
        // Only after whitespace or an opening mark, so `a@15581.com` stays an address.
        if let Some((len, markup)) = boundary.then(|| mention_at(rest, mentions)).flatten() {
            out.push_str(&esc(&text[plain..index]));
            out.push_str(&markup);
            index += len;
            plain = index;
            boundary = false;
        } else if let Some((len, href, title)) = boundary.then(|| link_at(rest)).flatten() {
            out.push_str(&esc(&text[plain..index]));
            let title = title.map_or_else(String::new, |t| format!(" title=\"{t}\""));
            out.push_str(&format!(
                "<a href=\"{}\"{title}>{}</a>",
                esc(&href),
                esc(&rest[..len])
            ));
            index += len;
            plain = index;
            boundary = false;
        } else {
            boundary = c.is_whitespace() || matches!(c, '(' | '[' | '"' | '\'');
            index += c.len_utf8();
        }
    }
    out.push_str(&esc(&text[plain..]));
    out
}

#[cfg(test)]
mod tests {
    use super::linkify_markup;

    #[test]
    fn links_web_addresses_and_escapes_the_rest() {
        assert_eq!(
            linkify_markup("see https://a.io/x?a=1&b=2, <b>ok</b> www.c.com."),
            "see <a href=\"https://a.io/x?a=1&amp;b=2\">https://a.io/x?a=1&amp;b=2</a>, \
             &lt;b&gt;ok&lt;/b&gt; <a href=\"https://www.c.com/\">www.c.com</a>."
        );
        assert_eq!(linkify_markup("javascript:x a&b"), "javascript:x a&amp;b");
        assert_eq!(
            linkify_markup("(https://a.io)"),
            "(<a href=\"https://a.io/\">https://a.io</a>)"
        );
    }

    #[test]
    fn mentions_show_the_name_and_open_the_chat_when_the_phone_is_known() {
        use super::{MentionLabel, linkify_markup_with_mentions};
        let mentions = [
            MentionLabel {
                user: "15581".into(),
                label: "Ana <b>".into(),
                phone: Some("5511912345678".into()),
                hint: None,
            },
            MentionLabel {
                user: "99999".into(),
                label: "99999".into(),
                phone: None,
                hint: None,
            },
            MentionLabel {
                user: "42424".into(),
                label: "42424".into(),
                phone: None,
                hint: Some("~Bia \"B\" <x>".into()),
            },
        ];
        assert_eq!(
            linkify_markup_with_mentions("hi @15581, and @99999! @12345 a@15581.com", &mentions),
            "hi <a href=\"zaptide-chat:5511912345678\" title=\"Message on WhatsApp\">@Ana &lt;b&gt;</a>, \
             and <b>@99999</b>! @12345 a@15581.com"
        );
        // The WhatsApp name shows on hover, escaped, for someone with no saved name.
        assert_eq!(
            linkify_markup_with_mentions("@42424", &mentions),
            "<a href=\"zaptide-mention:\" title=\"~Bia &quot;B&quot; &lt;x&gt;\">@42424</a>"
        );
        // No mentions known: text is unchanged apart from escaping.
        assert_eq!(linkify_markup_with_mentions("@15581", &[]), "@15581");
    }

    #[test]
    fn keeps_balanced_parentheses_and_drops_control_characters() {
        assert_eq!(
            linkify_markup("https://en.wikipedia.org/wiki/Foo_(bar)."),
            "<a href=\"https://en.wikipedia.org/wiki/Foo_(bar)\">https://en.wikipedia.org/wiki/Foo_(bar)</a>."
        );
        assert_eq!(linkify_markup("a\u{8}b\nc"), "ab\nc");
    }

    #[test]
    fn phone_numbers_and_whatsapp_links_open_a_chat() {
        let chat = |digits: &str, shown: &str| {
            format!("<a href=\"zaptide-chat:{digits}\" title=\"Message on WhatsApp\">{shown}</a>")
        };
        assert_eq!(
            linkify_markup("call +55 (11) 91234-5678."),
            format!("call {}.", chat("5511912345678", "+55 (11) 91234-5678"))
        );
        assert_eq!(
            linkify_markup("https://wa.me/5511912345678?text=hi"),
            chat("5511912345678", "https://wa.me/5511912345678?text=hi")
        );
        assert_eq!(
            linkify_markup("whatsapp://send?phone=+5511912345678"),
            chat("5511912345678", "whatsapp://send?phone=+5511912345678")
        );
        assert_eq!(
            linkify_markup("https://api.whatsapp.com/send?phone=5511912345678"),
            chat(
                "5511912345678",
                "https://api.whatsapp.com/send?phone=5511912345678"
            )
        );
        // A count or year after a number, and dates, are not part of it.
        assert_eq!(
            linkify_markup("+55 11 91234-5678 3 people"),
            format!("{} 3 people", chat("5511912345678", "+55 11 91234-5678"))
        );
        assert_eq!(linkify_markup("on +2024-01-15"), "on +2024-01-15");
        assert_eq!(
            linkify_markup("+5511912345678 2025"),
            format!("{} 2025", chat("5511912345678", "+5511912345678"))
        );
        // Too short to be a number, and bare digits are left alone.
        assert_eq!(
            linkify_markup("+123 and 11912345678"),
            "+123 and 11912345678"
        );
    }
}
