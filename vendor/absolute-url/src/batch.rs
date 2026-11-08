//! Experimental structural preprocessing for overlapping HTTPS-fallback ranges.
//!
//! This API does not validate hosts, ports or complete URLs, and does not decide
//! whether an initially parsed opaque URL prevents fallback.

use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;

use crate::batch_path::{encoded_byte, CompactPath};
use crate::parser::QUERY;
use crate::parser::{batch_authority_end, batch_host_end, batch_scheme_char};
use crate::parser::{batch_port_digit, batch_port_step, SPECIAL_QUERY, USERINFO};
use alloc::format;
use alloc::string::String;
use percent_encoding::CONTROLS;

/// Slow, uncounted parser-state oracle for differential tests, not a batch API.
pub fn test_parser_authority(value: &str) -> Result<(usize, usize), crate::ParseError> {
    crate::parser::batch_test_parser_authority(value)
}

/// Compare experimental path checkpoints with the original dense research index.
pub fn test_compact_path(word: &str) -> bool {
    crate::batch_path::verify_compact(word)
}

/// Exercise the real parser's insertion adjustment without allocating a large URL.
pub fn test_adjust_path_start(mut path_start: u32) -> u32 {
    crate::parser::batch_adjust_path_start(&mut path_start);
    path_start
}

/// Slow test helper for terminal path summaries; `start` must name a slash.
pub fn test_terminal_path(word: &str, start: usize, end: usize) -> (u128, bool) {
    let mut work = 0;
    let index = CompactPath::new(word, &mut work);
    let state = index.range(start, end);
    (state.len(), state.starts_double_slash())
}

/// Raw byte offsets into the input, before percent decoding or host processing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Split {
    /// Scheme delimiter if the initial parser's scheme state finds one.
    pub scheme_colon: Option<usize>,
    /// First special-authority delimiter or candidate end.
    pub authority_end: usize,
    /// The last authority `@`, if present.
    pub last_at: Option<usize>,
    /// The range that special-host parsing would inspect.
    pub host_start: usize,
    /// End of the raw host, before any port colon.
    pub host_end: usize,
    /// First control or backslash within this interval, rejected by preview validation.
    pub unsafe_char: Option<usize>,
}

/// Invalid input or interval for this deliberately restricted experiment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeError {
    /// Input contains a whitespace character.
    NonWord,
    /// Interval is invalid or violates the actual-trim endpoint contract.
    InvalidRange,
}

/// Counters for index passes and constant-time range queries, not IDNA work.
#[derive(Default, Clone, Copy, Debug)]
pub struct Work {
    /// UTF-8 bytes visited during input validation.
    pub validation_bytes: usize,
    /// Bytes visited in the backward index pass.
    pub indexed_bytes: usize,
    /// Bytes visited in the forward last-at pass.
    pub at_bytes: usize,
    /// Calls to [`AuthorityIndex::split`].
    pub queries: usize,
    /// Input bytes sent to the non-special authority parser.
    pub opaque_parser_bytes: usize,
    /// Operations building path suffix states.
    pub path_steps: usize,
    /// Port digits explicitly inspected and bracketed host bytes parsed.
    pub special_bytes: usize,
}

/// Shared structural state for a whitespace-free input word.
const BLOCK: usize = 64;
const AUTH: usize = 0;
const HOST: usize = 1;
const INSIDE: usize = 2;
const SCHEME: usize = 3;
const UNSAFE: usize = 4;
const NONSPECIAL: usize = 5;
const MARK: usize = 6;
const FRAGMENT: usize = 7;
const SLASH: usize = 8;
const NONSLASH: usize = 9;
const COLON: usize = 10;
const NONDIGIT: usize = 11;
const NONZERO: usize = 12;

#[derive(Clone)]
struct Block {
    next: [usize; 13],
    previous_at: usize,
    encoded: [u128; 4],
}

pub struct AuthorityIndex<'a> {
    word: &'a str,
    blocks: Vec<Block>,
    path: Option<CompactPath<'a>>,
}

fn index_byte(word: &str, i: usize, next: &mut [usize; 13]) {
    let b = word.as_bytes()[i];
    let c = char::from(b);
    if batch_authority_end(c, true) {
        next[AUTH] = i;
    }
    let outside = if batch_host_end(c, false, true) {
        i
    } else {
        next[HOST]
    };
    let inside = if batch_host_end(c, true, true) {
        i
    } else {
        next[INSIDE]
    };
    match b {
        b'[' => {
            next[HOST] = next[INSIDE];
        }
        b']' => {
            next[INSIDE] = next[HOST];
        }
        _ => {
            next[HOST] = outside;
            next[INSIDE] = inside;
        }
    }
    if !batch_scheme_char(c) {
        next[SCHEME] = i;
    }
    if word.is_char_boundary(i)
        && word[i..]
            .chars()
            .next()
            .is_some_and(|ch| ch.is_control() || ch == '\\')
    {
        next[UNSAFE] = i;
    }
    if batch_authority_end(c, false) {
        next[NONSPECIAL] = i;
    }
    if matches!(b, b'?' | b'#') {
        next[MARK] = i;
    }
    if b == b'#' {
        next[FRAGMENT] = i;
    }
    if b == b'/' {
        next[SLASH] = i;
    } else {
        next[NONSLASH] = i;
    }
    if b == b':' {
        next[COLON] = i;
    }
    if batch_port_digit(c).is_none() {
        next[NONDIGIT] = i;
    }
    if b != b'0' {
        next[NONZERO] = i;
    }
}

impl<'a> AuthorityIndex<'a> {
    /// Capacities of experimental scalar and path indexes, excluding allocator overhead.
    pub fn research_capacities(&self) -> (usize, usize) {
        (
            self.blocks.capacity() * core::mem::size_of::<Block>(),
            self.path.as_ref().map_or(0, CompactPath::allocated),
        )
    }
    pub fn new(word: &'a str, work: &mut Work) -> Result<Self, RangeError> {
        for c in word.chars() {
            #[cfg(feature = "batch-stats")]
            {
                work.validation_bytes += c.len_utf8();
            }
            if c.is_whitespace() {
                return Err(RangeError::NonWord);
            }
        }
        let n = word.len();
        let count = n.div_ceil(BLOCK) + 1;
        let mut blocks = vec![
            Block {
                next: [n; 13],
                previous_at: n,
                encoded: [0; 4]
            };
            count
        ];
        let mut previous = n;
        let mut encoded = [0u128; 4];
        for (i, &b) in word.as_bytes().iter().enumerate() {
            if i % BLOCK == 0 {
                blocks[i / BLOCK].previous_at = previous;
                blocks[i / BLOCK].encoded = encoded;
            }
            if b == b'@' {
                previous = i;
            }
            for (k, set) in [CONTROLS, QUERY, USERINFO, SPECIAL_QUERY]
                .iter()
                .copied()
                .enumerate()
            {
                encoded[k] += encoded_byte(b, set);
            }
            #[cfg(feature = "batch-stats")]
            {
                work.at_bytes += 1;
            }
        }
        if n % BLOCK == 0 {
            blocks[n / BLOCK].previous_at = previous;
            blocks[n / BLOCK].encoded = encoded;
        }
        let mut next = [n; 13];
        blocks[n.div_ceil(BLOCK)].next = next;
        for i in (0..n).rev() {
            index_byte(word, i, &mut next);
            if i % BLOCK == 0 {
                blocks[i / BLOCK].next = next;
            }
            #[cfg(feature = "batch-stats")]
            {
                work.indexed_bytes += 1;
            }
        }
        let b = word.as_bytes();
        let needs_paths = b.contains(&b'/') && b.iter().any(|b| matches!(b, b':' | b'?' | b'#'));
        let path = needs_paths.then(|| CompactPath::new(word, &mut work.path_steps));
        Ok(Self { word, blocks, path })
    }

    fn next_at(&self, pos: usize, kind: usize) -> usize {
        let boundary = pos.div_ceil(BLOCK).min(self.blocks.len() - 1);
        let mut next = self.blocks[boundary].next;
        let end = (boundary * BLOCK).min(self.word.len());
        for i in (pos..end).rev() {
            index_byte(self.word, i, &mut next);
        }
        next[kind]
    }
    fn previous_at(&self, pos: usize) -> usize {
        let base = pos / BLOCK;
        let mut previous = self.blocks[base].previous_at;
        for i in base * BLOCK..pos {
            if self.word.as_bytes()[i] == b'@' {
                previous = i
            }
        }
        previous
    }
    fn encoded_at(&self, pos: usize, kind: usize) -> u128 {
        let base = pos / BLOCK;
        let mut value = self.blocks[base].encoded[kind];
        let set = [CONTROLS, QUERY, USERINFO, SPECIAL_QUERY][kind];
        for &b in &self.word.as_bytes()[base * BLOCK..pos] {
            value += encoded_byte(b, set)
        }
        value
    }

    /// Split one UTF-8 aligned, ASCII-alpha-led range as an HTTPS authority.
    ///
    /// This only describes parser state. In particular, `scheme_colon` does not
    /// imply the initial parse succeeds, and no host validity is implied.
    pub fn split(&self, span: Range<usize>, _work: &mut Work) -> Result<Split, RangeError> {
        #[cfg(feature = "batch-stats")]
        {
            _work.queries += 1;
        }
        let n = self.word.len();
        if span.start >= span.end
            || span.end > n
            || !self.word.is_char_boundary(span.start)
            || !self.word.is_char_boundary(span.end)
        {
            return Err(RangeError::InvalidRange);
        }
        let stop = self.next_at(span.start, SCHEME);
        let scheme_colon = (stop < span.end && self.word.as_bytes()[stop] == b':').then_some(stop);
        let authority_end = self.next_at(span.start, AUTH).min(span.end);
        let at = self.previous_at(authority_end);
        let last_at = (at != n && at >= span.start).then_some(at);
        let host_start = last_at.map_or(span.start, |i| i + 1);
        let host_end = self.next_at(host_start, HOST).min(authority_end);
        let unsafe_at = self.next_at(span.start, UNSAFE);
        let unsafe_char = (unsafe_at < span.end).then_some(unsafe_at);
        Ok(Split {
            scheme_colon,
            authority_end,
            last_at,
            host_start,
            host_end,
            unsafe_char,
        })
    }

    /// Resolve safe `www.` initial parses without rescanning overlapping paths.
    ///
    /// Callers must supply the actual monotonically increasing renderer candidate
    /// starts when relying on amortized bounds for authority parsing. An end must
    /// be a renderer-trimmed end of this word, never before a subsequent slash.
    pub fn initial_www(&self, span: Range<usize>, work: &mut Work) -> Result<Initial, RangeError> {
        self.initial_www_with_limit(span, work, u32::MAX)
    }

    /// Test-only reduced offset model; the underlying parser still uses u32.
    pub fn initial_www_with_limit(
        &self,
        span: Range<usize>,
        work: &mut Work,
        limit: u32,
    ) -> Result<Initial, RangeError> {
        let split = self.split(span.clone(), work)?;
        if !self.word[span.start..span.end].starts_with("www.") {
            return Err(RangeError::InvalidRange);
        }
        if split.unsafe_char.is_some() {
            return Ok(Initial::Unsafe);
        }
        let Some(colon) = split.scheme_colon else {
            return Ok(Initial::Fallback);
        };
        let scheme_bytes = colon - span.start;
        if scheme_bytes as u128 + 1 > limit as u128 {
            return Ok(Initial::Fallback);
        }
        let after = colon + 1;
        let bytes = self.word.as_bytes();
        let mark = self.next_at(after, MARK).min(span.end);
        let before_mark = if !bytes[after..span.end].starts_with(b"//") {
            if mark == span.end {
                if self.next_at(span.end, SLASH) < self.word.len() {
                    return Err(RangeError::InvalidRange);
                }
                if after < span.end
                    && bytes[after] == b'/'
                    && self
                        .path
                        .as_ref()
                        .expect("slash index")
                        .range(after, span.end)
                        .starts_double_slash()
                {
                    let mut path_start = (scheme_bytes + 1) as u32;
                    crate::parser::batch_adjust_path_start(&mut path_start);
                }
                return Ok(Initial::SuppressFallback);
            }
            if after < mark && bytes[after] == b'/' {
                let path = self.path.as_ref().expect("slash index").suffix(after);
                let insertion = if path.starts_double_slash() { 2 } else { 0 };
                if insertion != 0 {
                    let mut path_start = (scheme_bytes + 1) as u32;
                    crate::parser::batch_adjust_path_start(&mut path_start);
                    let _ = path_start;
                }
                scheme_bytes as u128 + 1 + path.len() + insertion
            } else {
                scheme_bytes as u128 + 1 + self.encoded_at(mark, 0) - self.encoded_at(after, 0)
            }
        } else {
            let authority_start = after + 2;
            let authority_end = self.next_at(authority_start, NONSPECIAL).min(span.end);
            let synthetic_end = if authority_end < span.end {
                authority_end + 1
            } else {
                authority_end
            };
            #[cfg(feature = "batch-stats")]
            {
                work.opaque_parser_bytes += synthetic_end - span.start;
            }
            let Ok(parsed) = crate::Url::parse(&self.word[span.start..synthetic_end]) else {
                return Ok(Initial::Fallback);
            };
            let authority_len = parsed[..crate::Position::BeforePath].len() as u128;
            if authority_len > limit as u128 {
                return Ok(Initial::Fallback);
            }
            if mark == span.end {
                return Ok(Initial::SuppressFallback);
            }
            let path_len = if authority_end == mark {
                parsed.path().len() as u128
            } else {
                let prefix = parsed
                    .path()
                    .strip_suffix('/')
                    .expect("synthetic authority has trailing slash");
                let prefix_len = prefix.len() as u128;
                let suffix = self
                    .path
                    .as_ref()
                    .expect("slash index")
                    .suffix(authority_end);
                let extra = if prefix_len == 0 || suffix.skip > 0 {
                    0
                } else {
                    prefix_len - u128::from(suffix.segments == 0)
                };
                suffix.len() + extra
            };
            authority_len + path_len
        };
        if before_mark > limit as u128 {
            return Ok(Initial::Fallback);
        }
        if bytes[mark] == b'?' {
            let fragment = self.next_at(mark + 1, FRAGMENT).min(span.end);
            if fragment < span.end
                && before_mark + 1 + self.encoded_at(fragment, 1) - self.encoded_at(mark + 1, 1)
                    > limit as u128
            {
                return Ok(Initial::Fallback);
            }
        }
        Ok(Initial::SuppressFallback)
    }

    /// Decompose an HTTPS fallback without parsing a domain host per range.
    pub fn special_authority(
        &self,
        span: Range<usize>,
        work: &mut Work,
    ) -> Result<SpecialAuthority, RangeError> {
        self.scheme_authority(span, WebScheme::Https, work)
    }

    pub fn scheme_authority(
        &self,
        span: Range<usize>,
        scheme: WebScheme,
        work: &mut Work,
    ) -> Result<SpecialAuthority, RangeError> {
        let split = self.split(span.clone(), work)?;
        if split.unsafe_char.is_some() {
            return Ok(SpecialAuthority::Unsafe);
        }
        let bytes = self.word.as_bytes();
        let mut prefix = scheme.serialized_prefix_len();
        if let Some(at) = split.last_at {
            if at > span.start {
                let colon = self.next_at(span.start, COLON);
                let mut value = self.encoded_at(at, 2) - self.encoded_at(span.start, 2);
                let has_content;
                if colon < at {
                    value -= encoded_byte(b':', USERINFO);
                    let password = colon + 1 < at;
                    value += u128::from(password);
                    has_content = colon > span.start || password;
                } else {
                    has_content = true;
                }
                prefix += value + u128::from(has_content);
            }
        }
        let host = split.host_start..split.host_end;
        if host.is_empty() {
            return Ok(SpecialAuthority::Invalid(crate::ParseError::EmptyHost));
        }
        let port_start = split.host_end + usize::from(split.host_end < split.authority_end);
        let port = if split.host_end == split.authority_end {
            Ok(None)
        } else if self.next_at(port_start, NONDIGIT) < split.authority_end {
            Err(crate::ParseError::InvalidPort)
        } else if port_start == split.authority_end {
            Ok(None)
        } else {
            let significant = self.next_at(port_start, NONZERO).min(split.authority_end);
            if split.authority_end - significant > 5 {
                Err(crate::ParseError::InvalidPort)
            } else {
                let mut value = 0u32;
                for &byte in &bytes[significant..split.authority_end] {
                    #[cfg(feature = "batch-stats")]
                    {
                        work.special_bytes += 1;
                    }
                    match batch_port_step(value, u32::from(byte - b'0')) {
                        Ok(next) => value = next,
                        Err(_) => {
                            value = 65536;
                            break;
                        }
                    }
                }
                if value > 65535 {
                    Err(crate::ParseError::InvalidPort)
                } else if Some(value as u16) == crate::parser::default_port(scheme.name()) {
                    Ok(None)
                } else {
                    Ok(Some(value as u16))
                }
            }
        };
        let summary = AuthoritySummary {
            host: host.clone(),
            authority_end: split.authority_end,
            prefix_len: prefix,
            port,
        };
        if bytes[host.start] != b'[' {
            return Ok(SpecialAuthority::Domain(summary));
        }
        if host.len() > 47 {
            return Ok(SpecialAuthority::Invalid(
                crate::ParseError::InvalidIpv6Address,
            ));
        }
        #[cfg(feature = "batch-stats")]
        {
            work.special_bytes += host.len();
        }
        match crate::Host::parse(&self.word[host]) {
            Ok(crate::Host::Ipv6(address)) => {
                let serialized = format!("{}", crate::Host::<String>::Ipv6(address));
                match summary.port {
                    Ok(_) => Ok(SpecialAuthority::Ipv6 {
                        summary,
                        serialized,
                    }),
                    Err(e) => Ok(SpecialAuthority::Invalid(e)),
                }
            }
            _ => Ok(SpecialAuthority::Invalid(
                crate::ParseError::InvalidIpv6Address,
            )),
        }
    }

    /// Check URL offset conversions after a provider has validated a domain host.
    pub fn special_offsets(
        &self,
        span: Range<usize>,
        authority: &AuthoritySummary,
        host_len: u128,
        limit: u32,
    ) -> bool {
        let Ok(port) = authority.port else {
            return false;
        };
        let port_len = port.map_or(0, |p| {
            1 + if p < 10 {
                1
            } else if p < 100 {
                2
            } else if p < 1000 {
                3
            } else if p < 10000 {
                4
            } else {
                5
            }
        });
        let base = authority.prefix_len + host_len + port_len;
        if base > u128::from(limit) {
            return false;
        }
        let end = span.end;
        let mark = self.next_at(authority.authority_end, MARK).min(end);
        if mark == end {
            return true;
        }
        let path_len = if self.word.as_bytes()[authority.authority_end] == b'/' {
            self.path
                .as_ref()
                .expect("slash index")
                .suffix(authority.authority_end)
                .len()
        } else {
            1
        };
        let before_mark = base + path_len;
        if before_mark > u128::from(limit) {
            return false;
        }
        if self.word.as_bytes()[mark] == b'?' {
            let fragment = self.next_at(mark + 1, FRAGMENT).min(end);
            if fragment < end
                && before_mark + 1 + self.encoded_at(fragment, 3) - self.encoded_at(mark + 1, 3)
                    > u128::from(limit)
            {
                return false;
            }
        }
        true
    }

    /// Decide the implemented HTTPS fallback slice using an indexed host provider.
    pub fn special_fallback<P: DomainProvider>(
        &self,
        span: Range<usize>,
        provider: &mut P,
        work: &mut Work,
    ) -> Result<FallbackDecision, RangeError> {
        self.scheme_fallback(span, WebScheme::Https, provider, work)
    }

    pub fn scheme_fallback<P: DomainProvider>(
        &self,
        span: Range<usize>,
        scheme: WebScheme,
        provider: &mut P,
        work: &mut Work,
    ) -> Result<FallbackDecision, RangeError> {
        match self.scheme_authority(span.clone(), scheme, work)? {
            SpecialAuthority::Unsafe => Ok(FallbackDecision::Unsafe),
            SpecialAuthority::Invalid(_) => Ok(FallbackDecision::Reject),
            SpecialAuthority::Ipv6 {
                summary,
                serialized,
            } => Ok(
                if self.special_offsets(span, &summary, serialized.len() as u128, u32::MAX) {
                    FallbackDecision::Accept
                } else {
                    FallbackDecision::Reject
                },
            ),
            SpecialAuthority::Domain(summary) => {
                if summary.prefix_len > u32::MAX as u128 {
                    return Ok(FallbackDecision::Reject);
                }
                match provider.prepare(summary.host.clone()) {
                    PreparedDomain::Deferred => Ok(FallbackDecision::ProviderDeferred),
                    PreparedDomain::Invalid => Ok(FallbackDecision::Reject),
                    PreparedDomain::Ready {
                        serialized_len,
                        key,
                    } => {
                        let host_len = match numeric_host(provider, key) {
                            Ok(Some(len)) => len as u128,
                            Ok(None) => serialized_len,
                            Err(()) => return Ok(FallbackDecision::Reject),
                        };
                        Ok(
                            if self.special_offsets(span, &summary, host_len, u32::MAX) {
                                FallbackDecision::Accept
                            } else {
                                FallbackDecision::Reject
                            },
                        )
                    }
                }
            }
        }
    }

    /// Preview a lowercase absolute HTTP(S) candidate, including HTTPS retry.
    pub fn preview_absolute<P: DomainProvider>(
        &self,
        span: Range<usize>,
        provider: &mut P,
        work: &mut Work,
    ) -> Result<FallbackDecision, RangeError> {
        let split = self.split(span.clone(), work)?;
        if split.unsafe_char.is_some() {
            return Ok(FallbackDecision::Unsafe);
        }
        let candidate = &self.word[span.clone()];
        let (scheme, after) = if candidate.starts_with("http://") {
            (WebScheme::Http, span.start + 7)
        } else if candidate.starts_with("https://") {
            (WebScheme::Https, span.start + 8)
        } else {
            return Err(RangeError::InvalidRange);
        };
        let host_start = self.next_at(after, NONSLASH).min(span.end);
        let first = if host_start == span.end {
            FallbackDecision::Reject
        } else {
            self.scheme_fallback(host_start..span.end, scheme, provider, work)?
        };
        if first == FallbackDecision::Reject {
            self.special_fallback(span, provider, work)
        } else {
            Ok(first)
        }
    }

    /// Apply the initial-parse rule before attempting the HTTPS fallback slice.
    pub fn preview_www<P: DomainProvider>(
        &self,
        span: Range<usize>,
        provider: &mut P,
        work: &mut Work,
    ) -> Result<FallbackDecision, RangeError> {
        match self.initial_www(span.clone(), work)? {
            Initial::Unsafe => Ok(FallbackDecision::Unsafe),
            Initial::SuppressFallback => Ok(FallbackDecision::NotWeb),
            Initial::Fallback => self.special_fallback(span, provider, work),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebScheme {
    Http,
    Https,
}
impl WebScheme {
    fn name(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
    fn serialized_prefix_len(self) -> u128 {
        self.name().len() as u128 + 3
    }
}

/// A provider must reproduce pinned domain normalization and URL validity.
pub trait DomainProvider {
    fn prepare(&mut self, host: Range<usize>) -> PreparedDomain;
    fn label_count(&self, key: usize) -> usize;
    fn label(&self, key: usize, index: usize) -> Range<usize>;
    fn byte(&self, key: usize, index: usize) -> u8;
    fn all(&self, key: usize, range: Range<usize>, class: AsciiClass) -> bool;
    fn first_nonzero(&self, key: usize, range: Range<usize>) -> usize;
}

#[derive(Clone, Copy)]
pub enum AsciiClass {
    Decimal,
    Octal,
    Hex,
}

/// Prepared host status with an indexed normalized-ASCII host handle.
pub enum PreparedDomain {
    Deferred,
    Invalid,
    Ready { serialized_len: u128, key: usize },
}

/// Deferred outcomes are not acceptances or rejections.
#[derive(Debug, PartialEq, Eq)]
pub enum FallbackDecision {
    Unsafe,
    NotWeb,
    Reject,
    Accept,
    ProviderDeferred,
}

fn numeric_label<P: DomainProvider>(provider: &P, key: usize, range: Range<usize>) -> String {
    let len = range.len();
    if len == 0 {
        return String::new();
    }
    let first = provider.byte(key, range.start);
    let hex =
        len >= 2 && first == b'0' && matches!(provider.byte(key, range.start + 1), b'x' | b'X');
    let octal = !hex && len >= 2 && first == b'0';
    let prefix = if hex {
        2
    } else if octal {
        1
    } else {
        0
    };
    let class = if hex {
        AsciiClass::Hex
    } else if octal {
        AsciiClass::Octal
    } else {
        AsciiClass::Decimal
    };
    if !provider.all(key, range.start + prefix..range.end, class) {
        return if provider.all(key, range, AsciiClass::Decimal) {
            String::from("09")
        } else {
            String::from("g")
        };
    }
    let nonzero = provider.first_nonzero(key, range.start + prefix..range.end);
    if range.end - nonzero > 11 {
        return String::from(if hex {
            "0x100000000"
        } else if octal {
            "040000000000"
        } else {
            "4294967296"
        });
    }
    let mut result = String::from(if hex {
        "0x"
    } else if octal {
        "0"
    } else {
        ""
    });
    for i in nonzero..range.end {
        result.push(char::from(provider.byte(key, i)));
    }
    if result.is_empty() {
        result.push('0');
    }
    result
}

fn numeric_host<P: DomainProvider>(provider: &P, key: usize) -> Result<Option<usize>, ()> {
    let count = provider.label_count(key);
    if count == 0 {
        return Ok(None);
    }
    let trailing = provider.label(key, count - 1).is_empty();
    let effective = count - usize::from(trailing);
    if effective == 0 {
        return crate::host::batch_numeric_host("");
    }
    let last = numeric_label(provider, key, provider.label(key, effective - 1));
    if effective > 4 {
        return match crate::host::batch_numeric_host(&last) {
            Ok(None) => Ok(None),
            _ => Err(()),
        };
    }
    let mut host = String::new();
    for i in 0..effective {
        if i != 0 {
            host.push('.');
        }
        host.push_str(&numeric_label(provider, key, provider.label(key, i)));
    }
    if trailing {
        host.push('.');
    }
    crate::host::batch_numeric_host(&host)
}

/// Unvalidated domain or parser-validated bracketed host and port.
#[derive(Clone, Debug)]
pub enum SpecialAuthority {
    /// Preview validation rejects a control or backslash.
    Unsafe,
    /// The pinned host or port parser rejects before any domain provider is needed.
    Invalid(crate::ParseError),
    /// A domain requires the IDNA provider; port errors must wait until it answers.
    Domain(AuthoritySummary),
    /// A validated IPv6 host and normalized serialization.
    Ipv6 {
        summary: AuthoritySummary,
        serialized: String,
    },
}

/// Raw domain range and already indexed credentials and port.
#[derive(Clone, Debug)]
pub struct AuthoritySummary {
    pub host: Range<usize>,
    pub authority_end: usize,
    pub prefix_len: u128,
    pub port: Result<Option<u16>, crate::ParseError>,
}

/// Outcome of the partial initial-parse decision. No variant certifies a web host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Initial {
    /// The renderer's preview rejects this range before URL parsing.
    Unsafe,
    /// The unprefixed parse fails; HTTPS fallback is required by the caller's policy.
    Fallback,
    /// The unprefixed parse succeeds, so HTTPS fallback must not be attempted.
    SuppressFallback,
}
