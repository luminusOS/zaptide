//! Title and description of a link, read from the page the way a chat preview
//! shows them. Pages are fetched while a link is staged in the composer.

use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(2);
const MAX_BYTES: u64 = 256 * 1024;
const MAX_IMAGE_BYTES: u64 = 2 * 1024 * 1024;
const THUMBNAIL_EDGE: u32 = 192;

#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    pub title: Option<String>,
    pub description: Option<String>,
    pub thumbnail: Option<Vec<u8>>,
}

pub fn fetch(url: &str) -> Option<Preview> {
    let html = String::from_utf8_lossy(&download(url, MAX_BYTES)?).into_owned();
    let mut preview = parse(&html)?;
    preview.thumbnail = meta(&html, "og:image")
        .and_then(|source| url::Url::parse(url).ok()?.join(&source).ok())
        .and_then(|image| thumbnail(image.as_str()));
    Some(preview)
}

/// Body of a public page, cut at `limit` bytes rather than failed: metadata
/// sits in `<head>` and most pages exceed the limit.
fn download(url: &str, limit: u64) -> Option<Vec<u8>> {
    if !public_destination(url) {
        return None;
    }
    let mut response = ureq::get(url)
        .config()
        .timeout_global(Some(TIMEOUT))
        .max_redirects(0)
        .build()
        .call()
        .ok()?;
    let mut body = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(limit)
        .read_to_end(&mut body)
        .ok()?;
    Some(body)
}

fn thumbnail(url: &str) -> Option<Vec<u8>> {
    encode_thumbnail(download(url, MAX_IMAGE_BYTES)?)
}

fn encode_thumbnail(bytes: Vec<u8>) -> Option<Vec<u8>> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    reader.limits(crate::native_media::image_limits());
    let small = reader
        .decode()
        .ok()?
        .thumbnail(THUMBNAIL_EDGE, THUMBNAIL_EDGE);
    let mut jpeg = Vec::new();
    small
        .to_rgb8()
        .write_to(
            &mut std::io::Cursor::new(&mut jpeg),
            image::ImageFormat::Jpeg,
        )
        .ok()?;
    Some(jpeg)
}

fn public_destination(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    let Some(port) = url.port_or_known_default() else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let Ok(addresses) = (host, port).to_socket_addrs() else {
        return false;
    };
    let addresses: Vec<_> = addresses.collect();
    !addresses.is_empty() && addresses.iter().all(|address| is_public(address.ip()))
}

fn is_public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let [a, b, c, _] = address.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 0 || b == 168))
                || (a == 192 && b == 0 && c == 2)
                || (a == 198 && ((b == 18) || (b == 19) || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(address) => {
            let [first, second, third, ..] = address.segments();
            first & 0xe000 == 0x2000 && !(first == 0x2001 && second == 0x0db8 && third == 0)
        }
    }
}

pub fn parse(html: &str) -> Option<Preview> {
    let title = meta(html, "og:title").or_else(|| element_text(html, "title"));
    let description = meta(html, "og:description");
    (title.is_some() || description.is_some()).then_some(Preview {
        title,
        description,
        thumbnail: None,
    })
}

pub fn first_url(text: &str) -> Option<String> {
    let token = text
        .split_whitespace()
        .map(|token| token.trim_end_matches(['.', ',', ')', ']', '!', '?', ';', ':']))
        .find(|token| token.starts_with("http://") || token.starts_with("https://"))?;
    crate::safety::preview_url(token)?;
    Some(token.to_owned())
}

pub fn matches_text(text: &str, preview: &crate::model::LinkPreview) -> bool {
    first_url(text).as_deref() == Some(preview.url.as_str())
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
    fn page_images_become_small_jpegs_the_bubble_can_decode() {
        let mut png = Vec::new();
        image::DynamicImage::new_rgb8(800, 400)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let jpeg = encode_thumbnail(png).unwrap();
        assert!(matches!(
            crate::native_media::decode_thumbnail(&jpeg),
            crate::native_media::ThumbnailResult::Image(_)
        ));
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

    #[test]
    fn extracts_normalized_http_urls_without_trailing_punctuation() {
        assert_eq!(
            first_url("See https://example.com/path)."),
            Some("https://example.com/path".to_owned())
        );
        assert_eq!(
            first_url("http://127.0.0.1/"),
            Some("http://127.0.0.1/".to_owned())
        );
    }

    #[test]
    fn rejects_non_public_addresses_before_fetching() {
        assert!(!is_public("127.0.0.1".parse().unwrap()));
        assert!(!is_public("192.168.1.1".parse().unwrap()));
        assert!(!is_public("::1".parse().unwrap()));
        assert!(is_public("8.8.8.8".parse().unwrap()));
        assert!(is_public("2606:4700:4700::1111".parse().unwrap()));
        assert!(!public_destination("http://127.0.0.1/"));
    }

    #[test]
    fn only_sends_a_preview_for_its_current_draft_url() {
        let preview = crate::model::LinkPreview {
            url: "https://example.com".to_owned(),
            title: Some("Example".to_owned()),
            description: None,
            thumbnail: None,
        };
        assert!(matches_text("See https://example.com", &preview));
        assert!(!matches_text("See https://other.example", &preview));
    }
}
