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

/// Pango markup for message text with web addresses turned into links.
/// Everything else is escaped, so message text can never inject markup.
pub fn linkify_markup(text: &str) -> String {
    use gtk4::glib::markup_escape_text as esc;
    let mut out = String::with_capacity(text.len());
    for piece in text.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end();
        let word_end = word
            .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '"', '\''])
            .len();
        let (link, rest) = piece.split_at(word_end);
        let url = (link.starts_with("http://")
            || link.starts_with("https://")
            || link.starts_with("www."))
        .then(|| preview_url(link))
        .flatten();
        match url {
            Some(url) => out.push_str(&format!("<a href=\"{}\">{}</a>", esc(&url), esc(link))),
            None => out.push_str(&esc(link)),
        }
        out.push_str(&esc(rest));
    }
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
    }
}
