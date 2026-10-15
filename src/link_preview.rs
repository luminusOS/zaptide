//! Title and description of a link, read from the page the way a chat preview
//! shows them. The fetch runs only when a message with a link is sent.

use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(2);
const MAX_BYTES: u64 = 256 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    pub title: Option<String>,
    pub description: Option<String>,
}

pub async fn fetch(url: String) -> Option<Preview> {
    let page = tokio::task::spawn_blocking(move || read_page(&url));
    tokio::time::timeout(TIMEOUT, page).await.ok()?.ok()?
}

fn read_page(url: &str) -> Option<Preview> {
    let mut response = ureq::get(url)
        .config()
        .timeout_global(Some(TIMEOUT))
        .build()
        .call()
        .ok()?;
    let html = response
        .body_mut()
        .with_config()
        .limit(MAX_BYTES)
        .read_to_string()
        .ok()?;
    parse(&html)
}

pub fn parse(html: &str) -> Option<Preview> {
    let title = meta(html, "og:title").or_else(|| element_text(html, "title"));
    let description = meta(html, "og:description");
    (title.is_some() || description.is_some()).then_some(Preview { title, description })
}

/// Character ranges of the `http://` and `https://` words in `text`.
pub fn link_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut offset = 0;
    for piece in text.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end();
        if word.starts_with("http://") || word.starts_with("https://") {
            ranges.push((offset, offset + word.chars().count()));
        }
        offset += piece.chars().count();
    }
    ranges
}

fn meta(html: &str, key: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let quoted = [format!("\"{key}\""), format!("'{key}'")];
    let mut from = 0;
    while let Some(found) = lower[from..].find("<meta") {
        let start = from + found;
        let end = start + lower[start..].find('>')?;
        let tag = &lower[start..end];
        if quoted.iter().any(|name| tag.contains(name)) {
            return attribute(&html[start..end], "content");
        }
        from = end;
    }
    None
}

fn element_text(html: &str, name: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let open = lower.find(&format!("<{name}"))?;
    let start = open + lower[open..].find('>')? + 1;
    let end = start + lower[start..].find(&format!("</{name}"))?;
    clean(&html[start..end])
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let at = lower.find(&format!("{name}="))? + name.len() + 1;
    let quote = tag[at..]
        .chars()
        .next()
        .filter(|c| *c == '"' || *c == '\'')?;
    let value = &tag[at + 1..];
    clean(&value[..value.find(quote)?])
}

fn clean(raw: &str) -> Option<String> {
    let text = raw
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_open_graph_tags_and_decodes_entities() {
        let html = r#"<html><head><title>Fallback</title>
            <meta property="og:title" content="Tom &amp; Jerry">
            <meta name='og:description' content='A &quot;classic&quot; show'>
            </head></html>"#;
        let preview = parse(html).unwrap();
        assert_eq!(preview.title.as_deref(), Some("Tom & Jerry"));
        assert_eq!(preview.description.as_deref(), Some("A \"classic\" show"));
    }

    #[test]
    fn falls_back_to_the_page_title() {
        let preview = parse("<title> Plain page </title>").unwrap();
        assert_eq!(preview.title.as_deref(), Some("Plain page"));
        assert_eq!(preview.description, None);
        assert_eq!(parse("<p>no metadata</p>"), None);
    }

    #[test]
    fn finds_link_words_with_character_offsets() {
        assert_eq!(
            link_ranges("ação https://a.io/x e\nhttp://b.io"),
            vec![(5, 19), (22, 33)]
        );
        assert!(link_ranges("www.a.io").is_empty());
    }
}
