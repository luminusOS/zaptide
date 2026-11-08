//! Validation at the boundary between message content and desktop handlers.

#[cfg(test)]
thread_local! {
    static PARSE_BYTES: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

fn parse_url(value: &str) -> Result<url::Url, url::ParseError> {
    #[cfg(test)]
    PARSE_BYTES.with(|bytes| bytes.set(bytes.get().map(|total| total + value.len())));
    url::Url::parse(value)
}

/// Preview metadata may supply a bare host, but never a desktop URI scheme.
pub fn preview_url(value: &str) -> Option<String> {
    if value.chars().any(char::is_control) || value.contains('\\') {
        return None;
    }
    let value = value.trim();
    let url = match parse_url(value) {
        Ok(url) => url,
        Err(_) => parse_url(&format!("https://{value}")).ok()?,
    };
    (matches!(url.scheme(), "http" | "https") && url.host_str().is_some()).then(|| url.to_string())
}

/// A link from message content that may be handed to the desktop: an absolute
/// http or https URL with a host. This is the click-time half of a two-step
/// check; [`preview_url`] already filters template and preview URLs when a
/// message is classified, and this guards whatever stored content says when
/// it is opened. Unlike [`preview_url`] it never guesses a scheme, and every
/// other one (`file:`, `smb:`, `javascript:`, `data:`, app schemes) is
/// refused, since the default handler for it could be anything.
pub fn web_link(value: &str) -> Option<url::Url> {
    if value.chars().any(char::is_control) || value.contains('\\') {
        return None;
    }
    let url = url::Url::parse(value.trim()).ok()?;
    (matches!(url.scheme(), "http" | "https") && url.host_str().is_some()).then_some(url)
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

/// Characters dropped from the end of a link, with any unmatched `)`.
const LINK_TRAILERS: &[u8] = b".,;:!?]\"'";

/// Where the link candidates of one [`render`] call end, found in linear
/// time. Candidates are queried in increasing order, and those inside one word
/// share its end and trailing punctuation, so nothing is rescanned per candidate.
#[derive(Default)]
struct LinkScan {
    index: usize,
    whitespace: usize,
    unsafe_char: usize,
    question: usize,
    counted: usize,
    balance: isize,
    word: Option<WordTail>,
}

/// The run of trailing punctuation and `)` that ends a word.
struct WordTail {
    end: usize,
    start: usize,
    balance: isize,
    closes: Vec<usize>,
}

impl LinkScan {
    /// Count of `)` minus count of `(` in `text[..index]`.
    fn balance_to(&mut self, text: &str, index: usize) -> isize {
        for &byte in &text.as_bytes()[self.counted..index] {
            self.balance += isize::from(byte == b')') - isize::from(byte == b'(');
        }
        self.counted = index;
        self.balance
    }

    /// End of the link candidate at `index`: the end of its word without
    /// trailing punctuation, nor a `)` unless it closes a `(` in the link, so
    /// `.../Foo_(bar)` keeps its own parenthesis.
    ///
    /// `index` must not decrease between calls on the same `text`, and
    /// `text[index..]` must start with a letter.
    fn link_end(&mut self, text: &str, index: usize) -> usize {
        debug_assert!(index >= self.index, "link candidates must come in order");
        self.index = index;
        let end = resume_search(&mut self.whitespace, text.len(), index, |at| {
            text.is_char_boundary(at) && text[at..].starts_with(char::is_whitespace)
        })
        .unwrap_or(text.len());
        let balance = self.balance_to(text, index);
        let bytes = text.as_bytes();
        let tail = match self.word.take() {
            Some(tail) if tail.end == end => tail,
            _ => {
                let mut start = end;
                while start > index
                    && (bytes[start - 1] == b')' || LINK_TRAILERS.contains(&bytes[start - 1]))
                {
                    start -= 1;
                }
                let closes = (start..end).filter(|&at| bytes[at] == b')').collect();
                let word_balance: isize = bytes[index..start]
                    .iter()
                    .map(|&byte| isize::from(byte == b')') - isize::from(byte == b'('))
                    .sum();
                WordTail {
                    end,
                    start,
                    balance: balance + word_balance,
                    closes,
                }
            }
        };
        // Trimming from the end, the k-th `)` of the tail stays once the link
        // up to it has no more `)` than `(`; the tail itself holds no `(`.
        let unmatched = tail.balance - balance;
        let kept = usize::try_from(-unmatched)
            .unwrap_or(0)
            .min(tail.closes.len());
        let link_end = kept
            .checked_sub(1)
            .map_or(tail.start, |k| tail.closes[k] + 1);
        self.word = Some(tail);
        link_end
    }

    /// Whether `text[start..end]` holds a character [`preview_url`] rejects.
    /// `start` must not decrease between calls on the same `text`.
    fn has_unsafe_char(&mut self, text: &str, start: usize, end: usize) -> bool {
        resume_search(&mut self.unsafe_char, text.len(), start, |at| {
            text.is_char_boundary(at)
                && text[at..].starts_with(|c: char| c.is_control() || c == '\\')
        })
        .is_some_and(|at| at < end)
    }
}

/// Byte ranges of HTTP(S) and www link candidates, excluding trailing punctuation.
pub fn web_link_spans(text: &str) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    let mut scan = LinkScan::default();
    let mut chars = text.char_indices();
    let mut boundary = true;
    std::iter::from_fn(move || {
        for (index, c) in chars.by_ref() {
            let starts_link = boundary
                && ["http://", "https://", "www."]
                    .iter()
                    .any(|prefix| text[index..].starts_with(prefix));
            boundary = c.is_whitespace() || matches!(c, '(' | '[' | '"' | '\'');
            if starts_link {
                return Some(index..scan.link_end(text, index));
            }
        }
        None
    })
}

/// Link at `index` in `text`: its byte length, target, and optional tooltip.
///
/// Finding where a candidate ends is linear in `text`, but rejected `www.`
/// candidates and WhatsApp candidates containing `?` may still take quadratic time.
fn link_at(
    text: &str,
    index: usize,
    scan: &mut LinkScan,
    web_end: Option<usize>,
) -> Option<(usize, String, Option<&'static str>)> {
    let rest = &text[index..];
    if rest.starts_with('+') {
        let (len, digits) = phone_at(rest)?;
        return Some((
            len,
            format!("{CHAT_SCHEME}{digits}"),
            Some("Message on WhatsApp"),
        ));
    }
    let end = if rest.starts_with("whatsapp://") {
        scan.link_end(text, index)
    } else {
        web_end?
    };
    let len = end - index;
    let link = &rest[..len];
    if link.starts_with("whatsapp://") {
        // A WhatsApp chat requires a query, whose delimiter cannot be percent-encoded.
        if !resume_search(&mut scan.question, text.len(), index, |at| {
            text.as_bytes()[at] == b'?'
        })
        .is_some_and(|at| at < end)
        {
            return None;
        }
        let digits = whatsapp_number(&parse_url(link).ok()?)?;
        return Some((
            len,
            format!("{CHAT_SCHEME}{digits}"),
            Some("Message on WhatsApp"),
        ));
    }
    // The same rejection preview_url makes, answered without rescanning the
    // rest of a long word at every candidate inside it.
    if scan.has_unsafe_char(text, index, index + len) {
        return None;
    }
    let url = preview_url(link)?;
    match whatsapp_number(&parse_url(&url).ok()?) {
        Some(digits) => Some((
            len,
            format!("{CHAT_SCHEME}{digits}"),
            Some("Message on WhatsApp"),
        )),
        None => Some((len, url, None)),
    }
}

/// What activating a link in message text does.
#[derive(Debug, PartialEq)]
pub enum LinkActivation<'a> {
    /// Open or start the chat with this phone number.
    Chat(&'a str),
    /// Open this checked web link in the browser.
    Web(url::Url),
    /// Do nothing: a mention without a chat, or a link that is not safe to open.
    Ignore,
}

/// Decides what a link in a message label does. Labels never let GTK open
/// the raw href; the caller acts on the result and stops propagation.
pub fn link_activation(uri: &str) -> LinkActivation<'_> {
    if let Some(phone) = chat_link_number(uri).filter(|phone| !phone.is_empty()) {
        return LinkActivation::Chat(phone);
    }
    web_link(uri).map_or(LinkActivation::Ignore, LinkActivation::Web)
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
    let (end, mention) = mention_match(rest, mentions)?;
    let shown = format!("@{}", visible_mention_label(&mention.label));
    let title = mention.hint.as_deref().map_or_else(
        || "Message on WhatsApp".into(),
        |hint| esc(&hint.chars().filter(|c| !c.is_control()).collect::<String>()),
    );
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
    Some((end, markup))
}

fn visible_mention_label(label: &str) -> String {
    label.chars().filter(|c| !c.is_control()).collect()
}

/// Conservatively keep Unicode marks and symbols next to a token from
/// becoming a link to only its numeric prefix.
pub(crate) fn mention_token_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || (!c.is_ascii() && !c.is_whitespace())
}

fn mention_match<'a>(
    rest: &str,
    mentions: &'a [MentionLabel],
) -> Option<(usize, &'a MentionLabel)> {
    let token = rest.strip_prefix('@')?;
    let end = token
        .find(|c: char| !mention_token_char(c))
        .unwrap_or(token.len());
    let mention = mentions
        .iter()
        .find(|mention| !mention.user.is_empty() && mention.user == token[..end])?;
    Some((1 + end, mention))
}

/// Visible mention names as plain text for accessibility and transcript copy.
pub fn display_mentions(text: &str, mentions: &[MentionLabel]) -> String {
    let mut out = String::with_capacity(text.len());
    let (mut index, mut boundary) = (0, true);
    while let Some(c) = text[index..].chars().next() {
        if let Some((len, mention)) = boundary
            .then(|| mention_match(&text[index..], mentions))
            .flatten()
        {
            out.push('@');
            out.push_str(&visible_mention_label(&mention.label));
            index += len;
            boundary = false;
        } else {
            out.push(c);
            boundary = c.is_whitespace() || matches!(c, '(' | '[' | '"' | '\'');
            index += c.len_utf8();
        }
    }
    out
}

/// Pango markup for message text with web addresses, WhatsApp links and
/// `+` phone numbers turned into links. Everything else is escaped, so
/// message text can never inject markup.
pub fn linkify_markup(text: &str) -> String {
    linkify_markup_with_mentions(text, &[])
}

/// [`linkify_markup`] that also highlights the given `@user` mentions.
pub fn linkify_markup_with_mentions(text: &str, mentions: &[MentionLabel]) -> String {
    // Pango rejects the character references GLib emits for control characters.
    let text: String = text
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect();
    let mut out = String::with_capacity(text.len());
    render(&text, mentions, true, &mut out);
    out
}

const SPAN_MARKERS: [u8; 4] = *b"*_~`";

/// Finds span closers for one [`render`] call in linear time. Openers are
/// queried in increasing order and whether a marker can close a span does not
/// depend on its opener, so each search resumes where the previous one stopped
/// instead of rescanning the rest of the line from every opener.
#[derive(Default)]
struct SpanClosers {
    index: usize,
    markers: [usize; SPAN_MARKERS.len()],
    newline: usize,
}

impl SpanClosers {
    /// Byte offset, from `index`, of the marker closing the `marker` span that
    /// starts at `index` in `text`. As in WhatsApp, a span stays on one line,
    /// holds something, does not start or end with whitespace, and ends before
    /// a character that is not a letter or digit.
    ///
    /// `index` must not decrease between calls on the same `text`.
    fn closing(&mut self, text: &str, index: usize, marker: char) -> Option<usize> {
        debug_assert!(index >= self.index, "span openers must come in order");
        self.index = index;
        let slot = SPAN_MARKERS
            .iter()
            .position(|&byte| char::from(byte) == marker)?;
        let byte = SPAN_MARKERS[slot];
        let first = text[index + 1..].chars().next()?;
        if first.is_whitespace() || first == marker {
            return None;
        }
        let start = index + 1 + first.len_utf8();
        let bytes = text.as_bytes();
        let close = resume_search(&mut self.markers[slot], bytes.len(), start, |at| {
            bytes[at] == byte
                && !text[..at]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_whitespace)
                && text[at + 1..]
                    .chars()
                    .next()
                    .is_none_or(|after| !after.is_alphanumeric())
        })?;
        let newline = resume_search(&mut self.newline, bytes.len(), start, |at| {
            bytes[at] == b'\n'
        });
        newline
            .is_none_or(|newline| close < newline)
            .then_some(close - index)
    }
}

/// First position from `start` on that `matches`, skipping the positions an
/// earlier call already ruled out. `start` must not decrease between calls.
fn resume_search(
    cursor: &mut usize,
    len: usize,
    start: usize,
    matches: impl Fn(usize) -> bool,
) -> Option<usize> {
    *cursor = (*cursor).max(start);
    while *cursor < len {
        if matches(*cursor) {
            return Some(*cursor);
        }
        *cursor += 1;
    }
    None
}

/// Monospace on a faint grey that reads on light and dark bubbles alike.
const CODE_OPEN: &str = "<span font_family=\"monospace\" bgcolor=\"#808080\" bgalpha=\"22%\">";

/// Byte range of the code span covering `index` in text laid out from
/// [`linkify_markup`] markup. Code is the only markup with a background.
pub fn code_range_at(
    attrs: &gtk4::pango::AttrList,
    index: usize,
) -> Option<std::ops::Range<usize>> {
    attrs
        .attributes()
        .into_iter()
        .filter(|attr| attr.type_() == gtk4::pango::AttrType::Background)
        .map(|attr| attr.start_index() as usize..attr.end_index() as usize)
        .find(|range| range.contains(&index))
}

/// Appends the Pango markup of `text` to `out`: WhatsApp formatting (`*bold*`,
/// `_italic_`, `~strike~`, `` `code` ``, code blocks, `>` quotes, `-` lists),
/// links and mentions. Everything else is escaped.
fn render(text: &str, mentions: &[MentionLabel], mut line_start: bool, out: &mut String) {
    use gtk4::glib::markup_escape_text as esc;
    let (mut plain, mut index, mut boundary) = (0, 0, true);
    let mut closers = SpanClosers::default();
    let mut links = LinkScan::default();
    let mut web_links = web_link_spans(text).peekable();
    while let Some(c) = text[index..].chars().next() {
        let rest = &text[index..];
        if line_start {
            let block = if rest.starts_with("> ") {
                Some("<span alpha=\"60%\">▎</span> ")
            } else if rest.starts_with("- ") || rest.starts_with("* ") {
                Some("• ")
            } else {
                None
            };
            if let Some(markup) = block {
                out.push_str(&esc(&text[plain..index]));
                out.push_str(markup);
                index += 2;
                plain = index;
                line_start = false;
                boundary = true;
                continue;
            }
        }
        if let Some(code) = rest.strip_prefix("```")
            && let Some(end) = code.find("```")
            && !code[..end].trim().is_empty()
        {
            out.push_str(&esc(&text[plain..index]));
            out.push_str(&format!(
                "{CODE_OPEN}{}</span>",
                esc(code[..end].trim_matches('\n'))
            ));
            index += 6 + end;
            plain = index;
            boundary = false;
            line_start = false;
            continue;
        }
        // Only after a character that is not a letter or digit, so `snake_case` stays plain.
        let opens = text[..index]
            .chars()
            .next_back()
            .is_none_or(|before| !before.is_alphanumeric());
        let tag = match c {
            '*' => Some("b"),
            '_' => Some("i"),
            '~' => Some("s"),
            '`' => Some("span"),
            _ => None,
        };
        if opens
            && let Some(tag) = tag
            && let Some(end) = closers.closing(text, index, c)
        {
            out.push_str(&esc(&text[plain..index]));
            let inner = &rest[c.len_utf8()..end];
            if tag == "span" {
                out.push_str(CODE_OPEN);
            } else {
                out.push_str(&format!("<{tag}>"));
            }
            if tag == "span" {
                out.push_str(&esc(inner));
            } else {
                render(inner, mentions, false, out);
            }
            out.push_str(&format!("</{tag}>"));
            index += end + c.len_utf8();
            plain = index;
            boundary = false;
            line_start = false;
            continue;
        }
        // Only after whitespace or an opening mark, so `a@15581.com` stays an address.
        if let Some((len, markup)) = boundary.then(|| mention_at(rest, mentions)).flatten() {
            out.push_str(&esc(&text[plain..index]));
            out.push_str(&markup);
            index += len;
            plain = index;
            boundary = false;
        } else if let Some((len, href, title)) = boundary
            .then(|| {
                while web_links.peek().is_some_and(|span| span.start < index) {
                    web_links.next();
                }
                let end = web_links
                    .peek()
                    .filter(|span| span.start == index)
                    .map(|span| span.end);
                link_at(text, index, &mut links, end)
            })
            .flatten()
        {
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
            line_start = c == '\n';
            index += c.len_utf8();
            continue;
        }
        line_start = false;
    }
    out.push_str(&esc(&text[plain..]));
}

#[cfg(test)]
mod tests {
    use super::{CODE_OPEN, LinkActivation, link_activation, linkify_markup, web_link};

    #[test]
    fn label_links_open_chats_and_checked_web_links_only() {
        assert_eq!(link_activation("zaptide-chat:1"), LinkActivation::Chat("1"));
        assert_eq!(link_activation("zaptide-chat:"), LinkActivation::Ignore);
        assert_eq!(link_activation("zaptide-mention:"), LinkActivation::Ignore);
        assert_eq!(link_activation("file:///x"), LinkActivation::Ignore);
        assert_eq!(
            link_activation("javascript:alert(1)"),
            LinkActivation::Ignore
        );
        assert_eq!(
            link_activation("https://a.io/"),
            LinkActivation::Web(url::Url::parse("https://a.io/").unwrap())
        );
    }

    #[test]
    fn only_absolute_web_links_reach_the_desktop() {
        assert_eq!(
            web_link("https://example.com/path?q=1").map(String::from),
            Some("https://example.com/path?q=1".to_owned())
        );
        assert_eq!(
            web_link("HTTPS://Example.COM/Path").map(String::from),
            Some("https://example.com/Path".to_owned())
        );
        assert!(web_link("http://example.com").is_some());
        for refused in [
            "file:///etc/passwd",
            "smb://server/share",
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "steam://run/1",
            "zaptide-chat:15555550123",
            "example.com",
            "https://",
            "https://exa\nmple.com",
            "https://example.com\\evil",
            "",
        ] {
            assert_eq!(web_link(refused), None, "{refused}");
        }
    }

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
    fn code_spans_are_found_by_layout_index() {
        let (attrs, text, _) =
            gtk4::pango::parse_markup(&linkify_markup("ab `cd` *ef*\n```\ng h\n```"), '\0')
                .expect("valid markup");
        assert_eq!(text, "ab cd ef\ng h");
        assert_eq!(super::code_range_at(&attrs, 3), Some(3..5));
        assert_eq!(super::code_range_at(&attrs, 10), Some(9..12));
        assert_eq!(super::code_range_at(&attrs, 0), None);
        assert_eq!(super::code_range_at(&attrs, 6), None);
    }

    #[test]
    fn whatsapp_formatting_becomes_markup() {
        assert_eq!(
            linkify_markup("Hello, *Name*! _hi_ ~no~ `a<b` ok"),
            format!("Hello, <b>Name</b>! <i>hi</i> <s>no</s> {CODE_OPEN}a&lt;b</span> ok")
        );
        // Spans nest, and links inside them stay links.
        assert_eq!(
            linkify_markup("*bold _both_* _see https://a.io_"),
            "<b>bold <i>both</i></b> <i>see <a href=\"https://a.io/\">https://a.io</a></i>"
        );
        // Code is literal.
        assert_eq!(linkify_markup("`*x*`"), format!("{CODE_OPEN}*x*</span>"));
        assert_eq!(
            linkify_markup("```\n*x*\nmore\n```"),
            format!("{CODE_OPEN}*x*\nmore</span>")
        );
        // Not spans: spaces inside, empty, inside a word, unclosed, across lines.
        for plain in [
            "2 * 3 * 4",
            "** x",
            "snake_case_name",
            "a*b*c",
            "*open",
            "*a\nb*",
            "* *",
        ] {
            assert!(
                !linkify_markup(plain).contains("<b>") && !linkify_markup(plain).contains("<i>"),
                "{plain}"
            );
        }
        assert_eq!(linkify_markup("foo_bar_ and _x_y"), "foo_bar_ and _x_y");
        // A closing mark must not be followed by a letter or digit.
        assert_eq!(linkify_markup("*a*b"), "*a*b");
        assert_eq!(linkify_markup("(*a*)"), "(<b>a</b>)");
        assert_eq!(
            linkify_markup("> quoted\n- one\n* two\n1. three"),
            "<span alpha=\"60%\">▎</span> quoted\n• one\n• two\n1. three"
        );
        assert_eq!(linkify_markup("a - b"), "a - b");
        // Pango accepts the code styling.
        assert!(gtk4::pango::parse_markup(&linkify_markup("`a` ```\nb\n```"), '\0').is_ok());
        // Markup characters in the text stay escaped inside a span.
        assert_eq!(linkify_markup("*<b>*"), "<b>&lt;b&gt;</b>");
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
        assert_eq!(
            linkify_markup_with_mentions("@15581_foo @15581abc @15581é @15581\u{0301}", &mentions),
            "@15581_foo @15581abc @15581é @15581\u{0301}"
        );
        assert_eq!(
            super::display_mentions("hi @15581, a@15581.com @15581_foo", &mentions),
            "hi @Ana <b>, a@15581.com @15581_foo"
        );
        let unsafe_hint = [MentionLabel {
            user: "77777".into(),
            label: "77777".into(),
            phone: None,
            hint: Some("~Bia\u{8}\" <friend>".into()),
        }];
        assert_eq!(
            linkify_markup_with_mentions("@77777", &unsafe_hint),
            "<a href=\"zaptide-mention:\" title=\"~Bia&quot; &lt;friend&gt;\">@77777</a>"
        );
        let bad_label = [MentionLabel {
            user: "77777".into(),
            label: "A\u{8}na".into(),
            phone: None,
            hint: None,
        }];
        assert_eq!(
            linkify_markup_with_mentions("@77777", &bad_label),
            "<b>@Ana</b>"
        );
        assert_eq!(super::display_mentions("@77777", &bad_label), "@Ana");
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

    mod quadratic {
        //! The renderer before span closers were found in linear time, kept
        //! verbatim as the reference the linear one must match exactly.
        use super::super::{
            CHAT_SCHEME, CODE_OPEN, MentionLabel, mention_at, phone_at, preview_url,
            whatsapp_number,
        };

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

        fn closing_marker(rest: &str, marker: char) -> Option<usize> {
            let mut chars = rest.char_indices().skip(1);
            let (_, first) = chars.next()?;
            if first.is_whitespace() || first == marker {
                return None;
            }
            let mut previous = first;
            for (index, c) in chars {
                if c == '\n' {
                    return None;
                }
                if c == marker && !previous.is_whitespace() {
                    let after = rest[index + c.len_utf8()..].chars().next();
                    if after.is_none_or(|after| !after.is_alphanumeric()) {
                        return Some(index);
                    }
                }
                previous = c;
            }
            None
        }

        pub(super) fn render(
            text: &str,
            mentions: &[MentionLabel],
            mut line_start: bool,
            out: &mut String,
        ) {
            use gtk4::glib::markup_escape_text as esc;
            let (mut plain, mut index, mut boundary) = (0, 0, true);
            while let Some(c) = text[index..].chars().next() {
                let rest = &text[index..];
                if line_start {
                    let block = if rest.starts_with("> ") {
                        Some("<span alpha=\"60%\">▎</span> ")
                    } else if rest.starts_with("- ") || rest.starts_with("* ") {
                        Some("• ")
                    } else {
                        None
                    };
                    if let Some(markup) = block {
                        out.push_str(&esc(&text[plain..index]));
                        out.push_str(markup);
                        index += 2;
                        plain = index;
                        line_start = false;
                        boundary = true;
                        continue;
                    }
                }
                if let Some(code) = rest.strip_prefix("```")
                    && let Some(end) = code.find("```")
                    && !code[..end].trim().is_empty()
                {
                    out.push_str(&esc(&text[plain..index]));
                    out.push_str(&format!(
                        "{CODE_OPEN}{}</span>",
                        esc(code[..end].trim_matches('\n'))
                    ));
                    index += 6 + end;
                    plain = index;
                    boundary = false;
                    line_start = false;
                    continue;
                }
                let opens = text[..index]
                    .chars()
                    .next_back()
                    .is_none_or(|before| !before.is_alphanumeric());
                let tag = match c {
                    '*' => Some("b"),
                    '_' => Some("i"),
                    '~' => Some("s"),
                    '`' => Some("span"),
                    _ => None,
                };
                if let (true, Some(tag), Some(end)) =
                    (opens, tag, tag.and_then(|_| closing_marker(rest, c)))
                {
                    out.push_str(&esc(&text[plain..index]));
                    let inner = &rest[c.len_utf8()..end];
                    if tag == "span" {
                        out.push_str(CODE_OPEN);
                    } else {
                        out.push_str(&format!("<{tag}>"));
                    }
                    if tag == "span" {
                        out.push_str(&esc(inner));
                    } else {
                        render(inner, mentions, false, out);
                    }
                    out.push_str(&format!("</{tag}>"));
                    index += end + c.len_utf8();
                    plain = index;
                    boundary = false;
                    line_start = false;
                    continue;
                }
                if let Some((len, markup)) = boundary.then(|| mention_at(rest, mentions)).flatten()
                {
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
                    line_start = c == '\n';
                    index += c.len_utf8();
                    continue;
                }
                line_start = false;
            }
            out.push_str(&esc(&text[plain..]));
        }
    }

    #[test]
    fn whatsapp_certificate_matches_reference() {
        for text in [
            "whatsapp://send%3Fphone=1234567",
            "whatsapp://send?%70hone=%31%32%33%34%35%36%37",
            "whatsapp://x@send?phone=1234567",
            "whatsapp://send#fragment?phone=1234567",
            "whatsapp://send?phone=1234567#fragment",
            "whatsapp://[::1]?phone=1234567",
            "whatsapp://é?phone=1234567",
            "(whatsapp://x(whatsapp://send?phone=1234567)).?!",
            "whatsapp://send?phone=1234567).?!",
            "whatsapp://send\t?phone=1234567",
        ] {
            let mut expected = String::new();
            quadratic::render(text, &[], true, &mut expected);
            assert_eq!(linkify_markup(text), expected, "{text:?}");
        }
    }

    #[test]
    fn whatsapp_parse_work_is_bounded_without_question_mark() {
        let mut whatsapp_work = Vec::new();
        for candidate in ["whatsapp://x1234567(", "www.a:bad1234567("] {
            for count in [1_000, 2_000, 4_000] {
                let text = candidate.repeat(count);
                super::PARSE_BYTES.with(|bytes| bytes.set(Some(0)));
                let started = std::time::Instant::now();
                assert_eq!(linkify_markup(&text), text);
                let elapsed = started.elapsed();
                let work = super::PARSE_BYTES.with(|bytes| bytes.replace(None).unwrap());
                eprintln!("{candidate:?} x{count}: {work} parse bytes, {elapsed:?}");
                if candidate.starts_with("whatsapp:") {
                    whatsapp_work.push(work);
                } else {
                    assert!(work > 0);
                }
            }
        }
        assert_eq!(whatsapp_work, [0, 0, 0]);
    }

    #[test]
    fn linear_render_matches_the_quadratic_reference() {
        use super::MentionLabel;
        const PIECES: &[&str] = &[
            "*",
            "_",
            "~",
            "`",
            "```",
            "**",
            "__",
            "*_",
            "_~*",
            " ",
            "  ",
            "\n",
            "\t",
            "a",
            "Z",
            "9",
            "é",
            "e\u{301}",
            "😀",
            "👍🏽",
            "日本",
            "<",
            ">",
            "&",
            "\"",
            "'",
            "(",
            ")",
            "[",
            "]",
            ".",
            ",",
            "!",
            "\\",
            "\u{8}",
            "\u{7f}",
            "\u{200b}",
            "> ",
            "- ",
            "* ",
            "https://a.io",
            "https://a.io/x_(y)",
            "www.b.com.",
            "http://c.d/*e*",
            "whatsapp://send?phone=+5511912345678",
            "https://wa.me/5511912345678",
            "+55 11 91234-5678",
            "+2024-01-15",
            "+123",
            "@15581",
            "@42424",
            "@12345",
            "word",
            "snake_case",
            "\u{a0}",
            "\u{3000}",
            "\r",
            "\u{2028}",
            "٣",
            "*😀",
            "_𝔸",
            "~🀄",
            "`😀",
            "https://\\(",
            "https://a.io/)))",
            "(www.a:",
            "(whatsapp://x",
            "(https://a.io",
        ];
        // No whitespace, so these build long words full of link candidates.
        const WORD_PIECES: &[&str] = &[
            "https://",
            "http://x",
            "www.",
            "www.a:",
            "whatsapp://",
            "whatsapp://send?phone=",
            "5511912345678",
            "+55",
            "a.io",
            "/",
            "?",
            "(",
            ")",
            "))",
            "[",
            "]",
            "\"",
            "'",
            ".",
            ",",
            ":",
            "!",
            "\\",
            "\u{8}",
            "@15581",
            "*",
            "_",
            "é",
            "😀",
        ];
        let mentions = [
            MentionLabel {
                user: "15581".into(),
                label: "Ana <b>".into(),
                phone: Some("5511912345678".into()),
                hint: None,
            },
            MentionLabel {
                user: "42424".into(),
                label: "42424".into(),
                phone: None,
                hint: Some("~Bia".into()),
            },
        ];
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..20_000 {
            let len = next() % if case % 16 == 0 { 400 } else { 48 };
            let pieces = if case % 3 == 1 { WORD_PIECES } else { PIECES };
            let text: String = (0..len)
                .map(|_| pieces[(next() % pieces.len() as u64) as usize])
                .collect();
            let known: &[MentionLabel] = if case % 2 == 0 { &mentions } else { &[] };
            for line_start in [true, false] {
                let (mut linear, mut reference) = (String::new(), String::new());
                super::render(&text, known, line_start, &mut linear);
                quadratic::render(&text, known, line_start, &mut reference);
                assert_eq!(linear, reference, "{text:?}");
            }
        }
    }

    #[test]
    fn long_lines_of_unclosed_markers_render_quickly() {
        for marker in ["*", "_", "~", "`"] {
            let text = format!(" {marker}a").repeat(20_000);
            let started = std::time::Instant::now();
            let markup = linkify_markup(&text);
            let elapsed = started.elapsed();
            assert_eq!(markup, text);
            assert!(
                elapsed < std::time::Duration::from_millis(200),
                "{marker}: {elapsed:?}"
            );
        }
    }

    #[test]
    fn long_words_of_rejected_or_trimmed_links_render_quickly() {
        let rejected = "https://\\(".repeat(6_500);
        let started = std::time::Instant::now();
        let markup = linkify_markup(&rejected);
        let elapsed = started.elapsed();
        assert_eq!(markup, rejected);
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "rejected: {elapsed:?}"
        );

        let closers = ")".repeat(20_000);
        let started = std::time::Instant::now();
        let markup = linkify_markup(&format!("https://a.io/{closers}"));
        let elapsed = started.elapsed();
        assert_eq!(
            markup,
            format!("<a href=\"https://a.io/\">https://a.io/</a>{closers}")
        );
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "trimmed: {elapsed:?}"
        );
    }

    #[test]
    #[ignore = "synthetic timing evidence for the deferred rejected-candidate scanner"]
    fn rejected_candidates_with_irrelevant_digits_timing() {
        for candidate in ["www.a:bad1234567(", "whatsapp://x1234567("] {
            for count in [1_000, 2_000, 4_000] {
                let text = candidate.repeat(count);
                let started = std::time::Instant::now();
                assert_eq!(linkify_markup(&text), text);
                eprintln!("{candidate:?} x{count}: {:?}", started.elapsed());
            }
        }
    }
}
