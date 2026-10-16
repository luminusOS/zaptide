//! Bounded page metadata and JPEG thumbnails for composer previews.
//! Fetching contacts the linked site before a message is sent.

use std::io::Read;
use std::net::IpAddr;
use std::time::Duration;

use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

const TIMEOUT: Duration = Duration::from_secs(2);
const MAX_BYTES: u64 = 256 * 1024;
const MAX_IMAGE_BYTES: u64 = 2 * 1024 * 1024;
const THUMBNAIL_EDGE: u32 = 192;

/// Display metadata with an optional center-cropped JPEG of at most 192 × 192 pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    pub title: Option<String>,
    pub description: Option<String>,
    pub thumbnail: Option<Vec<u8>>,
}

/// Fetch HTTP(S) metadata without redirects or credentials, rejecting non-public IPs.
/// Missing metadata or a failed page request returns `None`; image failure only omits the JPEG.
pub fn fetch(url: &str) -> Option<Preview> {
    let html = String::from_utf8_lossy(&download(url, MAX_BYTES)?).into_owned();
    let mut preview = parse(&html)?;
    preview.thumbnail = meta(&html, "og:image")
        .and_then(|source| url::Url::parse(url).ok()?.join(&source).ok())
        .and_then(|image| thumbnail(image.as_str()));
    Some(preview)
}

/// Read only the first `limit` bytes: page metadata is useful even when the full page is larger.
fn download(url: &str, limit: u64) -> Option<Vec<u8>> {
    let parsed = url::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return None;
    }
    // A proxy could resolve the target remotely and bypass the validated addresses.
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .max_redirects(0)
        .proxy(None)
        .build();
    let agent = ureq::Agent::with_parts(
        config,
        DefaultConnector::default(),
        PublicResolver(DefaultResolver::default()),
    );
    let mut response = agent.get(parsed.as_str()).call().ok()?;
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
    let small = reader.decode().ok()?.resize_to_fill(
        THUMBNAIL_EDGE,
        THUMBNAIL_EDGE,
        image::imageops::FilterType::Triangle,
    );
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

// Validate the actual connection addresses, not a separate DNS lookup vulnerable to rebinding.
#[derive(Debug)]
struct PublicResolver<R>(R);

impl<R: Resolver> Resolver for PublicResolver<R> {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let addresses = self.0.resolve(uri, config, timeout)?;
        if addresses.is_empty() || addresses.iter().any(|address| !is_public(address.ip())) {
            return Err(ureq::Error::HostNotFound);
        }
        Ok(addresses)
    }
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
                || (a == 192 && b == 168)
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 0 && c == 2)
                || (a == 198 && ((b == 18) || (b == 19) || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(address) => {
            let [first, second, ..] = address.segments();
            first & 0xe000 == 0x2000
                && !(first == 0x2001 && (second < 0x0200 || second == 0x0db8))
                && first != 0x2002
                && !(first == 0x3fff && second & 0xf000 == 0)
        }
    }
}

/// Extract Open Graph text with a `<title>` fallback, without network access.
/// Returns `None` when neither title nor description exists; the thumbnail is unset.
pub fn parse(html: &str) -> Option<Preview> {
    let title = meta(html, "og:title").or_else(|| element_text(html, "title"));
    let description = meta(html, "og:description");
    (title.is_some() || description.is_some()).then_some(Preview {
        title,
        description,
        thumbnail: None,
    })
}

/// Select the first whitespace-delimited HTTP(S) URL, trimming trailing punctuation.
/// Returns the original spelling rather than canonicalizing it; this does not resolve DNS.
pub fn first_url(text: &str) -> Option<String> {
    let token = text
        .split_whitespace()
        .map(|token| token.trim_end_matches(['.', ',', ')', ']', '!', '?', ';', ':']))
        .find(|token| token.starts_with("http://") || token.starts_with("https://"))?;
    crate::safety::preview_url(token)?;
    Some(token.to_owned())
}

/// Whether the preview URL exactly matches the draft's first selected URL.
pub fn matches_text(text: &str, preview: &crate::model::LinkPreview) -> bool {
    first_url(text).as_deref() == Some(preview.url.as_str())
}

/// Half-open Unicode character ranges, not byte offsets, for HTTP(S) words in `text`.
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
        let crate::native_media::ThumbnailResult::Image(thumbnail) =
            crate::native_media::decode_thumbnail(&jpeg)
        else {
            panic!("expected thumbnail image");
        };
        assert_eq!(thumbnail.width, thumbnail.height);
        assert!(thumbnail.width <= crate::native_media::MAX_THUMBNAIL_EDGE);
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
        assert!(download("http://127.0.0.1/", MAX_BYTES).is_none());
    }

    #[test]
    fn rejects_unsupported_schemes_and_embedded_credentials() {
        for url in [
            "file:///etc/hosts",
            "ftp://127.0.0.1/file",
            "http://user:password@127.0.0.1/",
            "https://user@127.0.0.1/",
        ] {
            assert!(download(url, MAX_BYTES).is_none());
        }
    }

    #[test]
    fn rejects_ipv6_documentation_and_transition_ranges() {
        for address in ["2001:db8:1234::1", "2002:7f00:1::1", "2001::1", "3fff::1"] {
            assert!(!is_public(address.parse().unwrap()), "{address}");
        }
    }

    #[test]
    fn resolver_rejects_private_answers_and_keeps_the_validated_addresses() {
        use ureq::unversioned::resolver::Resolver;
        let answers = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let resolver = PublicResolver(RebindingResolver(answers.clone()));
        let uri = "https://example.com".parse().unwrap();
        let config = ureq::config::Config::default();
        let timeout = || ureq::unversioned::transport::NextTimeout {
            after: TIMEOUT.into(),
            reason: ureq::Timeout::Global,
        };
        let addresses = resolver.resolve(&uri, &config, timeout()).unwrap();
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0], "8.8.8.8:443".parse().unwrap());
        assert_eq!(answers.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(resolver.resolve(&uri, &config, timeout()).is_err());
    }

    #[test]
    fn http_connection_uses_validated_addresses_without_resolving_again() {
        let lookups = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let agent = ureq::Agent::with_parts(
            ureq::Agent::config_builder().proxy(None).build(),
            InspectConnection(connections.clone()),
            PublicResolver(RebindingResolver(lookups.clone())),
        );
        assert!(matches!(
            agent.get("https://example.com").call(),
            Err(ureq::Error::ConnectionFailed)
        ));
        assert_eq!(lookups.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(connections.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(matches!(
            agent.get("https://example.com").call(),
            Err(ureq::Error::HostNotFound)
        ));
        assert_eq!(connections.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[derive(Debug)]
    struct InspectConnection(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl ureq::unversioned::transport::Connector for InspectConnection {
        type Out = ();

        fn connect(
            &self,
            details: &ureq::unversioned::transport::ConnectionDetails<'_>,
            _chained: Option<()>,
        ) -> Result<Option<()>, ureq::Error> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert!(details.config.proxy().is_none());
            assert_eq!(details.uri.host(), Some("example.com"));
            assert_eq!(details.addrs.len(), 1);
            assert_eq!(details.addrs[0], "8.8.8.8:443".parse().unwrap());
            Err(ureq::Error::ConnectionFailed)
        }
    }

    #[derive(Debug)]
    struct RebindingResolver(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl ureq::unversioned::resolver::Resolver for RebindingResolver {
        fn resolve(
            &self,
            _uri: &ureq::http::Uri,
            _config: &ureq::config::Config,
            _timeout: ureq::unversioned::transport::NextTimeout,
        ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
            let mut addresses = self.empty();
            let call = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            addresses.push(if call == 0 {
                "8.8.8.8:443".parse().unwrap()
            } else {
                "127.0.0.1:443".parse().unwrap()
            });
            if call > 0 {
                addresses.push("8.8.8.8:443".parse().unwrap());
            }
            Ok(addresses)
        }
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
