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
        ("http" | "https", "api.whatsapp.com" | "web.whatsapp.com") | ("whatsapp", "send") => {
            url.query_pairs().find(|(key, _)| key == "phone")?.1.into_owned()
        }
        _ => return None,
    };
    let digits: String = digits.chars().filter(char::is_ascii_digit).collect();
    (7..=15).contains(&digits.len()).then_some(digits)
}

/// Length of a `+<country code> ...` phone number at the start of `rest`, and its digits.
fn phone_at(rest: &str) -> Option<(usize, String)> {
    let mut digits = String::new();
    let mut end = 0;
    for (index, c) in rest.char_indices().skip(1) {
        if c.is_ascii_digit() {
            digits.push(c);
            end = index + 1;
        } else if !matches!(c, ' ' | '-' | '(' | ')' | '.') {
            break;
        }
    }
    (rest.starts_with('+') && (8..=15).contains(&digits.len())).then_some((end, digits))
}

/// Link at the start of `rest`: its byte length, target, and optional tooltip.
fn link_at(rest: &str) -> Option<(usize, String, Option<&'static str>)> {
    if rest.starts_with('+') {
        let (len, digits) = phone_at(rest)?;
        return Some((len, format!("{CHAT_SCHEME}{digits}"), Some("Message on WhatsApp")));
    }
    if !["http://", "https://", "www.", "whatsapp://"]
        .iter()
        .any(|prefix| rest.starts_with(prefix))
    {
        return None;
    }
    let word = rest.split(char::is_whitespace).next()?;
    let len = word
        .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '"', '\''])
        .len();
    let link = &rest[..len];
    if link.starts_with("whatsapp://") {
        let digits = whatsapp_number(&url::Url::parse(link).ok()?)?;
        return Some((len, format!("{CHAT_SCHEME}{digits}"), Some("Message on WhatsApp")));
    }
    let url = preview_url(link)?;
    match whatsapp_number(&url::Url::parse(&url).ok()?) {
        Some(digits) => Some((len, format!("{CHAT_SCHEME}{digits}"), Some("Message on WhatsApp"))),
        None => Some((len, url, None)),
    }
}

/// Phone number from an in-app chat link produced by [`linkify_markup`].
pub fn chat_link_number(uri: &str) -> Option<&str> {
    uri.strip_prefix(CHAT_SCHEME)
}

/// Pango markup for message text with web addresses, WhatsApp links and
/// `+` phone numbers turned into links. Everything else is escaped, so
/// message text can never inject markup.
pub fn linkify_markup(text: &str) -> String {
    use gtk4::glib::markup_escape_text as esc;
    let mut out = String::with_capacity(text.len());
    let (mut plain, mut index, mut boundary) = (0, 0, true);
    while let Some(c) = text[index..].chars().next() {
        let rest = &text[index..];
        if let Some((len, href, title)) = boundary.then(|| link_at(rest)).flatten() {
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
    fn phone_numbers_and_whatsapp_links_open_a_chat() {
        let chat = |digits: &str, shown: &str| {
            format!(
                "<a href=\"zaptide-chat:{digits}\" title=\"Message on WhatsApp\">{shown}</a>"
            )
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
        // Too short to be a number, and bare digits are left alone.
        assert_eq!(linkify_markup("+123 and 11912345678"), "+123 and 11912345678");
    }
}
