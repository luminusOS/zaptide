mod boundaries;
mod provider;
use boundaries::Boundaries;
use idna_batch::uts46::{PreparedFragment, Uts46};
use std::ops::Range;
use url_batch::batch::{AuthorityIndex, Work as UrlWork};

#[derive(Default, Debug)]
struct Work {
    #[cfg(test)]
    decode_raw: usize,
    #[cfg(test)]
    decoded: usize,
    #[cfg(test)]
    fragment_calls: usize,
    #[cfg(test)]
    fragment_input: usize,
    #[cfg(test)]
    fragment_output: usize,
    #[cfg(test)]
    span_input: usize,
    #[cfg(test)]
    bidi_tail_rejections: usize,
    #[cfg(test)]
    normalized_base_calls: usize,
    #[cfg(test)]
    normalized_base_bytes: usize,
    #[cfg(test)]
    base_validation_bytes: usize,
    #[cfg(test)]
    normalized_tail_calls: usize,
    #[cfg(test)]
    normalized_tail_bytes: usize,
    #[cfg(test)]
    normalized_revalidation_bytes: usize,
}
#[derive(Debug)]
struct Ready {
    start: usize,
    end: usize,
    www: bool,
    ascii_len: usize,
    partial: Option<Range<usize>>,
    prepared_partial: Option<PreparedFragment>,
    tail_prepared: Option<(usize, idna_batch::uts46::ActualTailResult)>,
    ascii_tail: Option<Range<usize>>,
}
#[derive(Debug)]
enum Decision {
    Ready(Ready),
    Invalid,
    EmptyHost,
    DeferredBracket,
    DeferredPartial,
    DeferredRawBoundary,
}
#[cfg(test)]
enum Offsets {
    Compact(Vec<u32>),
    Wide(Vec<usize>),
}
#[cfg(test)]
impl Offsets {
    fn new(len: usize, value: usize) -> Self {
        if len < u32::MAX as usize {
            let encoded = if value == usize::MAX {
                u32::MAX
            } else {
                u32::try_from(value).expect("compact initial value")
            };
            Self::Compact(vec![encoded; len])
        } else {
            Self::Wide(vec![value; len])
        }
    }
    fn get(&self, i: usize) -> usize {
        match self {
            Self::Compact(v) => {
                if v[i] == u32::MAX {
                    usize::MAX
                } else {
                    v[i] as usize
                }
            }
            Self::Wide(v) => v[i],
        }
    }
    fn set(&mut self, i: usize, value: usize) {
        match self {
            Self::Compact(v) => {
                v[i] = if value == usize::MAX {
                    u32::MAX
                } else {
                    u32::try_from(value).expect("compact offset")
                }
            }
            Self::Wide(v) => v[i] = value,
        }
    }
}
struct BitRanks {
    masks: Vec<u64>,
    ranks: Vec<usize>,
}
#[cfg(test)]
mod bit_rank_tests {
    use super::{Adapter, BitRanks, Decision, DecodeRanks, Work, hex};
    use idna_batch::uts46::Uts46;
    use url_batch::batch::Work as UrlWork;
    #[test]
    fn prefixes_match_scans_at_block_boundaries() {
        for len in [0, 1, 63, 64, 65, 127, 128, 129, 1025] {
            let bytes: Vec<_> = (0..len).map(|i| b"[a])%z"[i % 6]).collect();
            let ranks = BitRanks::new(&bytes, |b| matches!(b, b'[' | b']'));
            for end in 0..=len {
                assert_eq!(
                    ranks.get(end),
                    bytes[..end]
                        .iter()
                        .filter(|&&b| matches!(b, b'[' | b']'))
                        .count()
                );
            }
        }
    }
    #[test]
    fn greedy_percent_alignment_matches_decoder() {
        for input in [
            "%ff((www.é((,))).,)))",
            "%25ff%2e%%41%4a%4",
            &format!("{}%41%2e%ff", "a".repeat(63)),
        ] {
            let raw = input.as_bytes();
            let index = DecodeRanks::new(raw);
            let mut expected = vec![usize::MAX; raw.len() + 1];
            let (mut i, mut decoded) = (0, 0);
            expected[0] = 0;
            while i < raw.len() {
                i += if raw[i] == b'%'
                    && i + 2 < raw.len()
                    && hex(raw[i + 1]).is_some()
                    && hex(raw[i + 2]).is_some()
                {
                    3
                } else {
                    1
                };
                decoded += 1;
                expected[i] = decoded;
            }
            for (i, value) in expected.into_iter().enumerate() {
                assert_eq!(index.get(i), value, "{input:?} at {i}");
            }
        }
    }
    #[test]
    fn renderer_matches_pristine_on_tail_and_normalization_boundaries() {
        for input in [
            "www.%ff((www.é((,))).,)))",
            "www.example%2ecom www.example。com www...example",
            "http://%41%42.example/a www.%ff.example",
            "www.אבג.example www.אבג.123",
            "(www.example.com) (www.é((,))).,)))",
        ] {
            assert_eq!(
                crate::safety::linkify_markup(input),
                crate::safety::reference::linkify_markup(input),
                "{input:?}"
            );
        }
    }
    #[test]
    fn punctuation_tail_matches_pinned_plan() {
        let alphabet = b")!,;\"']";
        for &a in alphabet {
            for &b in alphabet {
                for &c in alphabet {
                    let trailer = [a, b, c, b')', b')'];
                    let mut input = String::from("www.a.");
                    input.push_str(std::str::from_utf8(&trailer).unwrap());
                    let mut work = Work::default();
                    let mut url_work = UrlWork::default();
                    let adapter = Adapter::new(&input, &mut work, &mut url_work).unwrap();
                    let plan = Uts46::new().prepare_actual_tail(b"", &trailer).unwrap();
                    let expected = Uts46::new().evaluate_actual_tail(&plan, 4).unwrap().is_ok();
                    let actual = matches!(
                        adapter.query_host(0..input.len() - 1, &mut work),
                        Decision::Ready(_)
                    );
                    assert_eq!(actual, expected, "{input:?}");
                }
            }
        }
    }
    #[test]
    fn ascii_base_tail_matches_pinned_plan() {
        use url_batch::batch::{DomainProvider, PreparedDomain};
        for (raw, base) in [
            ("Example", b"Example".as_slice()),
            ("%41bc", b"Abc"),
            ("xn--bcher-kva", b"xn--bcher-kva"),
            ("bad%ff", b"bad\xff"),
        ] {
            for trailer in [")))", ",)))", "])))", "!)))"] {
                let input = format!("www.{raw}{trailer}");
                let end = input.len() - 1;
                let mut work = Work::default();
                let mut url_work = UrlWork::default();
                let adapter = Adapter::new(&input, &mut work, &mut url_work).unwrap();
                let expected = Uts46::new()
                    .prepare_actual_tail(base, trailer.as_bytes())
                    .ok()
                    .and_then(|plan| {
                        Uts46::new()
                            .evaluate_actual_tail(&plan, trailer.len() - 1)
                            .map(|result| result.map(|r| plan.materialize(&r)))
                    });
                let mut provider = super::provider::IndexedProvider::new(&adapter);
                match (expected, provider.prepare(0..end)) {
                    (
                        Some(Ok(expected)),
                        PreparedDomain::Ready {
                            serialized_len,
                            key,
                        },
                    ) => {
                        let actual: Vec<u8> = (4..serialized_len as usize)
                            .map(|i| provider.byte(key, i))
                            .collect();
                        assert_eq!(actual, expected.as_bytes(), "{input:?}");
                    }
                    (Some(Err(_)) | None, PreparedDomain::Invalid) => {}
                    (expected, _) => {
                        panic!("{input:?} expected={expected:?}, unexpected provider decision")
                    }
                }
            }
        }
    }
    #[test]
    fn arena_markers_exclude_mapped_dots_and_preserve_empty_fragments() {
        let input = "www.é。a...end";
        let mut work = Work::default();
        let mut url_work = UrlWork::default();
        let adapter = Adapter::new(input, &mut work, &mut url_work).unwrap();
        let pieces: Vec<_> = (0..adapter.boundaries.len())
            .map(|i| &adapter.arena[adapter.arena_range(i, i + 1)])
            .collect();
        assert_eq!(pieces, ["www", "xn--9ca.a", "", "", "end"]);
        assert_eq!(
            &adapter.arena[adapter.arena_range(0, 5)],
            "www.xn--9ca.a...end"
        );
    }
    #[test]
    fn arena_select_handles_dense_and_sparse_marker_groups() {
        let long = "a".repeat(5000);
        let fragments: Vec<&str> = (0..150)
            .map(|i| {
                if i == 17 {
                    long.as_str()
                } else if i % 3 == 0 {
                    "é。b"
                } else if i % 3 == 1 {
                    ""
                } else {
                    "X"
                }
            })
            .collect();
        let input = fragments.join(".");
        let mut work = Work::default();
        let mut url_work = UrlWork::default();
        let adapter = Adapter::new(&input, &mut work, &mut url_work).unwrap();
        let expected: Vec<_> = fragments
            .iter()
            .map(|s| {
                Uts46::new()
                    .prepare_url_fragment(s.as_bytes())
                    .unwrap()
                    .ascii
            })
            .collect();
        assert_eq!(adapter.boundaries.len(), expected.len());
        for (i, fragment) in expected.iter().enumerate() {
            assert_eq!(&adapter.arena[adapter.arena_range(i, i + 1)], fragment);
        }
        for (start, end) in [(0, 64), (12, 19), (63, 65), (64, 128), (127, 150), (0, 150)] {
            assert_eq!(
                &adapter.arena[adapter.arena_range(start, end)],
                expected[start..end].join(".")
            );
        }
    }
    #[test]
    fn complex_tail_component_views_match_pinned_plan() {
        use url_batch::batch::{DomainProvider, PreparedDomain};
        let input = "www.é。abc)))";
        let end = input.len() - 1;
        let mut work = Work::default();
        let mut url_work = UrlWork::default();
        let adapter = Adapter::new(input, &mut work, &mut url_work).unwrap();
        let plan = Uts46::new()
            .prepare_actual_tail("é。abc".as_bytes(), b")))")
            .unwrap();
        let result = Uts46::new()
            .evaluate_actual_tail(&plan, 2)
            .unwrap()
            .unwrap();
        let expected = format!("www.{}", plan.materialize(&result));
        let mut provider = super::provider::IndexedProvider::new(&adapter);
        let PreparedDomain::Ready {
            serialized_len,
            key,
        } = provider.prepare(0..end)
        else {
            panic!("not ready")
        };
        let bytes: Vec<u8> = (0..serialized_len as usize)
            .map(|i| provider.byte(key, i))
            .collect();
        assert_eq!(bytes, expected.as_bytes());
    }
}
impl BitRanks {
    fn new(bytes: &[u8], predicate: impl Fn(u8) -> bool) -> Self {
        let mut masks = vec![0u64; bytes.len().div_ceil(64)];
        for (i, &byte) in bytes.iter().enumerate() {
            if predicate(byte) {
                masks[i / 64] |= 1u64 << (i % 64);
            }
        }
        let mut ranks = Vec::with_capacity(masks.len() + 1);
        ranks.push(0);
        for &mask in &masks {
            ranks.push(ranks.last().copied().unwrap_or(0) + mask.count_ones() as usize);
        }
        Self { masks, ranks }
    }
    fn get(&self, i: usize) -> usize {
        let block = i / 64;
        let shift = i % 64;
        self.ranks[block]
            + if shift == 0 {
                0
            } else {
                (self.masks[block] & ((1u64 << shift) - 1)).count_ones() as usize
            }
    }
    fn from_masks(masks: Vec<u64>) -> Self {
        let mut ranks = Vec::with_capacity(masks.len() + 1);
        ranks.push(0);
        for &mask in &masks {
            ranks.push(ranks.last().copied().unwrap_or(0) + mask.count_ones() as usize);
        }
        Self { masks, ranks }
    }
}
#[derive(Default)]
struct BitBuilder {
    masks: Vec<u64>,
    len: usize,
}
impl BitBuilder {
    fn push(&mut self, bit: bool) {
        if self.len.is_multiple_of(64) {
            self.masks.push(0)
        }
        if bit {
            self.masks[self.len / 64] |= 1u64 << (self.len % 64)
        }
        self.len += 1;
    }
    fn finish(self) -> BitRanks {
        BitRanks::from_masks(self.masks)
    }
}
struct MarkGroup {
    start: usize,
    sparse: usize,
}
struct MarkSelect {
    masks: Vec<u64>,
    groups: Vec<MarkGroup>,
    sparse: Vec<usize>,
}
impl MarkSelect {
    fn new(masks: Vec<u64>) -> Self {
        let mut groups = Vec::new();
        let mut sparse = Vec::new();
        let mut positions = [0usize; 64];
        let mut count = 0;
        for (block, &mask) in masks.iter().enumerate() {
            let mut bits = mask;
            while bits != 0 {
                positions[count] = block * 64 + bits.trailing_zeros() as usize;
                count += 1;
                bits &= bits - 1;
                if count == 64 {
                    Self::group(&mut groups, &mut sparse, &positions[..count]);
                    count = 0;
                }
            }
        }
        if count != 0 {
            Self::group(&mut groups, &mut sparse, &positions[..count]);
        }
        Self {
            masks,
            groups,
            sparse,
        }
    }
    fn group(groups: &mut Vec<MarkGroup>, sparse: &mut Vec<usize>, positions: &[usize]) {
        let start = positions[0];
        let offset = if positions[positions.len() - 1] - start >= 4096 {
            let n = sparse.len();
            sparse.extend_from_slice(positions);
            n
        } else {
            usize::MAX
        };
        groups.push(MarkGroup {
            start,
            sparse: offset,
        });
    }
    fn select(&self, number: usize) -> usize {
        let group = &self.groups[number / 64];
        let mut remaining = number % 64;
        if group.sparse != usize::MAX {
            return self.sparse[group.sparse + remaining];
        }
        let mut block = group.start / 64;
        let mut bits = self.masks[block] & (!0u64 << (group.start % 64));
        while remaining >= bits.count_ones() as usize {
            remaining -= bits.count_ones() as usize;
            block += 1;
            bits = self.masks[block];
        }
        for _ in 0..remaining {
            bits &= bits - 1;
        }
        block * 64 + bits.trailing_zeros() as usize
    }
}
#[derive(Default)]
struct ArenaBuilder {
    ascii: String,
    markers: Vec<u64>,
    invalid: BitBuilder,
    bidi: BitBuilder,
    bidi_invalid: BitBuilder,
}
impl ArenaBuilder {
    fn finish_fragment(&mut self, buf: &mut Vec<u8>, _work: &mut Work) {
        #[cfg(test)]
        {
            _work.fragment_calls += 1;
        }
        #[cfg(test)]
        {
            _work.fragment_input += buf.len();
        }
        let fragment = Uts46::new().prepare_url_fragment(buf).ok();
        self.invalid.push(fragment.is_none());
        self.bidi
            .push(fragment.as_ref().is_some_and(|p| p.has_bidi));
        self.bidi_invalid
            .push(fragment.as_ref().is_some_and(|p| !p.bidi_valid));
        if let Some(fragment) = fragment {
            #[cfg(test)]
            {
                _work.fragment_output += fragment.ascii.len();
            }
            self.ascii.push_str(&fragment.ascii)
        }
        let pos = self.ascii.len();
        self.markers.resize(pos / 64 + 1, 0);
        self.markers[pos / 64] |= 1u64 << (pos % 64);
        self.ascii.push('.');
        buf.clear();
    }
}
struct DecodeRanks {
    triplets: BitRanks,
    interior: Vec<u64>,
}
impl DecodeRanks {
    fn new(raw: &[u8]) -> Self {
        let mut masks = vec![0u64; raw.len().div_ceil(64)];
        let mut interior = vec![0u64; raw.len().div_ceil(64)];
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == b'%'
                && i + 2 < raw.len()
                && hex(raw[i + 1]).is_some()
                && hex(raw[i + 2]).is_some()
            {
                masks[i / 64] |= 1u64 << (i % 64);
                for p in i + 1..i + 3 {
                    interior[p / 64] |= 1u64 << (p % 64);
                }
                i += 3;
            } else {
                i += 1;
            }
        }
        let mut ranks = Vec::with_capacity(masks.len() + 1);
        ranks.push(0);
        for &mask in &masks {
            ranks.push(ranks.last().copied().unwrap_or(0) + mask.count_ones() as usize);
        }
        Self {
            triplets: BitRanks { masks, ranks },
            interior,
        }
    }
    fn get(&self, i: usize) -> usize {
        if self
            .interior
            .get(i / 64)
            .is_some_and(|mask| mask & (1u64 << (i % 64)) != 0)
        {
            usize::MAX
        } else {
            i - 2 * self.triplets.get(i)
        }
    }
}
struct Adapter<'a> {
    word: &'a str,
    index: AuthorityIndex<'a>,
    raw_decode: DecodeRanks,
    boundaries: Boundaries,
    brackets: BitRanks,
    ascii_bad: BitRanks,
    tail_plan: Option<(usize, Result<idna_batch::uts46::ActualTailPlan, ()>)>,
    tail_start: usize,
    punctuation_bad: BitRanks,
    ascii_tail: Option<(usize, Result<AsciiTailBase, ()>)>,
    arena: String,
    markers: MarkSelect,
    invalid: BitRanks,
    bidi: BitRanks,
    bidi_invalid: BitRanks,
}
struct AsciiTailBase {
    fragment: PreparedFragment,
    alabel: bool,
}
fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}
impl<'a> Adapter<'a> {
    fn arena_start(&self, i: usize) -> usize {
        if i == 0 {
            0
        } else {
            self.markers.select(i - 1) + 1
        }
    }
    fn arena_end(&self, i: usize) -> usize {
        self.markers.select(i)
    }
    fn arena_range(&self, start: usize, end: usize) -> Range<usize> {
        if start == end {
            0..0
        } else {
            self.arena_start(start)..self.arena_end(end - 1)
        }
    }
    fn new(
        s: &'a str,
        w: &mut Work,
        uw: &mut UrlWork,
    ) -> Result<Self, url_batch::batch::RangeError> {
        let index = AuthorityIndex::new(s, uw)?;
        #[cfg(test)]
        {
            w.span_input += s.len();
        }
        let raw = s.as_bytes();
        let n = raw.len();
        let raw_decode = DecodeRanks::new(raw);
        let boundaries = Boundaries::new(raw);
        let brackets = BitRanks::new(raw, |b| matches!(b, b'[' | b']'));
        let ascii_bad = BitRanks::new(raw, |b| {
            !(b.is_ascii_lowercase() || matches!(b, b'(' | b')'))
        });
        let mut arena = ArenaBuilder::default();
        let mut buf = Vec::new();
        let mut i = 0;
        #[cfg(test)]
        let mut decoded = 0;
        let mut decoded_word = Vec::with_capacity(n);
        while i < n {
            let start = i;
            let mut len = 1;
            let byte = if raw[i] == b'%' && i + 2 < n {
                if let (Some(a), Some(b)) = (hex(raw[i + 1]), hex(raw[i + 2])) {
                    len = 3;
                    a * 16 + b
                } else {
                    raw[i]
                }
            } else {
                raw[i]
            };
            i += len;
            #[cfg(test)]
            {
                w.decode_raw += len;
            }
            #[cfg(test)]
            {
                decoded += 1;
            }
            decoded_word.push(byte);
            let raw_delimiter =
                len == 1 && matches!(raw[start], b'@' | b':' | b'/' | b'?' | b'#' | b'\\');
            if byte == b'.' || raw_delimiter {
                arena.finish_fragment(&mut buf, w);
            } else {
                buf.push(byte)
            }
        }
        #[cfg(test)]
        {
            w.decoded += decoded;
        }
        arena.finish_fragment(&mut buf, w);
        debug_assert_eq!(arena.invalid.len, boundaries.len());
        let mut tail_start = n;
        while tail_start > 0
            && (raw[tail_start - 1] == b')' || b".,;:!?]\"'".contains(&raw[tail_start - 1]))
        {
            tail_start -= 1;
        }
        let mut punctuation_allowed = [false; 256];
        if tail_start < n {
            for &byte in b").,;:!?]\"'" {
                let sample = [byte, b')'];
                punctuation_allowed[byte as usize] = Uts46::new()
                    .prepare_actual_tail(b"", &sample)
                    .ok()
                    .and_then(|plan| Uts46::new().evaluate_actual_tail(&plan, 2))
                    .is_some_and(|result| result.is_ok());
            }
        }
        let punctuation_bad = BitRanks::new(raw, |b| !punctuation_allowed[b as usize]);
        let mut tail_plan = None;
        let mut ascii_tail = None;
        let part = if tail_start < n {
            boundaries.part_at(tail_start)
        } else {
            usize::MAX
        };
        if part != usize::MAX {
            let p = boundaries.part_start(part);
            let suffix_start = tail_start;
            let suffix_end = boundaries.part_end(part);
            if p < tail_start {
                let ds = raw_decode.get(p);
                let de = raw_decode.get(suffix_start);
                if ds != usize::MAX && de != usize::MAX {
                    #[cfg(test)]
                    {
                        w.normalized_base_calls += 1;
                    }
                    #[cfg(test)]
                    {
                        w.normalized_base_bytes += de - ds;
                    }
                    let base = &decoded_word[ds..de];
                    if base.is_ascii() {
                        let alabel = base.len() >= 4 && base[..4].eq_ignore_ascii_case(b"xn--");
                        ascii_tail = Some((
                            part,
                            Uts46::new()
                                .prepare_url_fragment(base)
                                .map(|fragment| AsciiTailBase { fragment, alabel })
                                .map_err(|_| ()),
                        ));
                    } else {
                        let result = Uts46::new()
                            .prepare_actual_tail(base, &raw[suffix_start..suffix_end])
                            .map_err(|_| ());
                        #[cfg(test)]
                        if let Ok(plan) = &result {
                            #[cfg(test)]
                            {
                                w.normalized_tail_bytes += plan.trailer_bytes();
                            }
                            #[cfg(test)]
                            {
                                w.base_validation_bytes += plan.base_validation_bytes();
                            }
                        }
                        tail_plan = Some((part, result));
                    }
                }
            }
        }
        Ok(Self {
            word: s,
            index,
            raw_decode,
            boundaries,
            brackets,
            ascii_bad,
            tail_plan,
            tail_start,
            punctuation_bad,
            ascii_tail,
            arena: arena.ascii,
            markers: MarkSelect::new(arena.markers),
            invalid: arena.invalid.finish(),
            bidi: arena.bidi.finish(),
            bidi_invalid: arena.bidi_invalid.finish(),
        })
    }
    fn query_host(&self, host: Range<usize>, _work: &mut Work) -> Decision {
        let (start, end) = (host.start, host.end);
        if start > end || end > self.word.len() {
            return Decision::DeferredRawBoundary;
        }
        if start == end {
            return Decision::EmptyHost;
        }
        if self.raw_decode.get(start) == usize::MAX || self.raw_decode.get(end) == usize::MAX {
            return Decision::DeferredRawBoundary;
        }
        if self.brackets.get(end) != self.brackets.get(start) {
            return if self.word.as_bytes()[start] == b'[' {
                Decision::DeferredBracket
            } else {
                Decision::Invalid
            };
        }
        let mut www = false;
        let begin = if self.boundaries.start_at(start) != usize::MAX {
            self.boundaries.start_at(start)
        } else if end >= start + 4
            && &self.word.as_bytes()[start..start + 4] == b"www."
            && self.boundaries.start_at(start + 4) != usize::MAX
        {
            www = true;
            self.boundaries.start_at(start + 4)
        } else if self.ascii_bad.get(end) == self.ascii_bad.get(start) {
            return Decision::Ready(Ready {
                start: 0,
                end: 0,
                www: false,
                ascii_len: end - start,
                partial: Some(start..end),
                prepared_partial: None,
                tail_prepared: None,
                ascii_tail: None,
            });
        } else {
            return Decision::DeferredPartial;
        };
        let mut finish = self.boundaries.end_at(end);
        let mut partial = None;
        let prepared_partial: Option<PreparedFragment> = None;
        let mut tail_prepared = None;
        let mut ascii_tail = None;
        let mut punctuation_partial = false;
        if finish == usize::MAX {
            let i = self.boundaries.part_at(end);
            if i == usize::MAX || i < begin {
                return Decision::DeferredPartial;
            }
            let p = self.boundaries.part_start(i);
            if self.ascii_bad.get(end) != self.ascii_bad.get(p) {
                if self.bidi.get(i) != self.bidi.get(begin) && self.word.as_bytes()[end - 1] == b')'
                {
                    #[cfg(test)]
                    {
                        _work.bidi_tail_rejections += 1;
                    }
                    return Decision::Invalid;
                }
                if p >= self.tail_start {
                    if self.word.as_bytes()[end - 1] != b')' {
                        return Decision::DeferredPartial;
                    }
                    if self.punctuation_bad.get(end) != self.punctuation_bad.get(p) {
                        return Decision::Invalid;
                    }
                    punctuation_partial = true;
                    partial = Some(p..end);
                } else if self.ascii_tail.as_ref().is_some_and(|(part, _)| *part == i) {
                    let Some((_, Ok(base))) = self.ascii_tail.as_ref() else {
                        return Decision::Invalid;
                    };
                    let suffix = self.tail_start;
                    if end < suffix {
                        return Decision::DeferredPartial;
                    }
                    if end > suffix && self.word.as_bytes()[end - 1] != b')' {
                        return Decision::DeferredPartial;
                    }
                    if self.punctuation_bad.get(end) != self.punctuation_bad.get(suffix)
                        || (end > suffix && base.alabel)
                    {
                        return Decision::Invalid;
                    }
                    ascii_tail = Some(suffix..end);
                } else {
                    let Some((_, plan)) = self.tail_plan.as_ref().filter(|(part, _)| *part == i)
                    else {
                        return Decision::DeferredPartial;
                    };
                    let Ok(plan) = plan else {
                        return Decision::Invalid;
                    };
                    let Some(tail_end) = end.checked_sub(self.tail_start) else {
                        return Decision::DeferredPartial;
                    };
                    #[cfg(test)]
                    {
                        _work.normalized_tail_calls += 1;
                    }
                    #[cfg(test)]
                    {
                        _work.normalized_revalidation_bytes += plan.revalidation_bytes(tail_end);
                    }
                    match Uts46::new().evaluate_actual_tail(plan, tail_end) {
                        Some(Ok(result)) => tail_prepared = Some((i, result)),
                        Some(Err(_)) => return Decision::Invalid,
                        None => return Decision::DeferredPartial,
                    }
                }
            } else {
                partial = Some(p..end)
            }
            finish = i;
        }
        if finish < begin {
            return Decision::DeferredPartial;
        }
        if self.invalid.get(finish) != self.invalid.get(begin) {
            return Decision::Invalid;
        }
        let ascii_tail_base = ascii_tail
            .as_ref()
            .and(self.ascii_tail.as_ref())
            .and_then(|(_, base)| base.as_ref().ok());
        let is_bidi = self.bidi.get(finish) != self.bidi.get(begin)
            || prepared_partial.as_ref().is_some_and(|x| x.has_bidi)
            || tail_prepared.as_ref().is_some_and(|(_, x)| x.has_bidi)
            || ascii_tail.as_ref().is_some_and(|tail| {
                tail.is_empty() && ascii_tail_base.is_some_and(|base| base.fragment.has_bidi)
            });
        if is_bidi && self.bidi_invalid.get(finish) != self.bidi_invalid.get(begin) {
            return Decision::Invalid;
        }
        if is_bidi && prepared_partial.as_ref().is_some_and(|p| !p.bidi_valid) {
            return Decision::Invalid;
        }
        if is_bidi && tail_prepared.as_ref().is_some_and(|(_, p)| !p.bidi_valid) {
            return Decision::Invalid;
        }
        if is_bidi && punctuation_partial {
            return Decision::Invalid;
        }
        if is_bidi
            && ascii_tail.as_ref().is_some_and(|tail| {
                !tail.is_empty() || ascii_tail_base.is_some_and(|base| !base.fragment.bidi_valid)
            })
        {
            return Decision::Invalid;
        }
        if is_bidi && let Some(p) = partial.as_ref() {
            let b = &self.word.as_bytes()[p.clone()];
            if !b.is_empty() && (!b[0].is_ascii_lowercase() || !b[b.len() - 1].is_ascii_lowercase())
            {
                return Decision::Invalid;
            }
        }
        let count = finish - begin
            + usize::from(
                partial.is_some()
                    || prepared_partial.is_some()
                    || tail_prepared.is_some()
                    || ascii_tail.is_some(),
            );
        let separators = count.saturating_sub(1) + usize::from(www);
        let full_count = finish - begin;
        let full_ascii = self.arena_range(begin, finish).len() - full_count.saturating_sub(1);
        let ascii_len = full_ascii
            + separators
            + if www { 3 } else { 0 }
            + partial.as_ref().map_or(0, |p| p.len())
            + prepared_partial.as_ref().map_or(0, |p| p.ascii.len())
            + tail_prepared.as_ref().map_or(0, |(_, p)| p.ascii_len)
            + ascii_tail.as_ref().map_or(0, |tail| {
                tail.len() + ascii_tail_base.map_or(0, |base| base.fragment.ascii.len())
            });
        Decision::Ready(Ready {
            start: begin,
            end: finish,
            www,
            ascii_len,
            partial,
            prepared_partial,
            tail_prepared,
            ascii_tail,
        })
    }
}

#[derive(Debug)]
pub(crate) enum Verdict {
    Reject,
    Unsupported,
    Web { end: usize },
    Chat { end: usize, digits: String },
}

struct DecisionScope<'p, 'a, 'w>(&'p mut provider::IndexedProvider<'a, 'w>);
impl Drop for DecisionScope<'_, '_, '_> {
    fn drop(&mut self) {
        self.0.end_decision();
    }
}

pub(crate) struct Verdicts {
    // A zero end selects Reject or the sparse side table, preserving every usize offset.
    entries: Vec<(usize, usize)>,
    special: Vec<(usize, Verdict)>,
    cursor: usize,
    special_cursor: usize,
    scratch: Verdict,
}

impl Default for Verdicts {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            special: Vec::new(),
            cursor: 0,
            special_cursor: 0,
            scratch: Verdict::Reject,
        }
    }
}

impl Verdicts {
    pub(crate) fn push(&mut self, position: usize, verdict: Verdict) {
        debug_assert!(
            self.entries
                .last()
                .is_none_or(|(previous, _)| *previous < position)
        );
        let end = match verdict {
            Verdict::Reject => 0,
            Verdict::Web { end } if end != 0 => end,
            other => {
                self.special.push((position, other));
                0
            }
        };
        self.entries.push((position, end));
    }

    pub(crate) fn get(&mut self, position: usize) -> Option<&Verdict> {
        while self.cursor < self.entries.len() && self.entries[self.cursor].0 < position {
            self.cursor += 1;
        }
        let &(offset, end) = self.entries.get(self.cursor)?;
        if offset != position {
            return None;
        }
        if end != 0 {
            self.scratch = Verdict::Web { end };
            return Some(&self.scratch);
        }
        while self.special_cursor < self.special.len()
            && self.special[self.special_cursor].0 < position
        {
            self.special_cursor += 1;
        }
        if let Some((_, verdict)) = self
            .special
            .get(self.special_cursor)
            .filter(|(offset, _)| *offset == position)
        {
            return Some(verdict);
        }
        self.scratch = Verdict::Reject;
        Some(&self.scratch)
    }
}

pub(crate) fn decisions(text: &str) -> Verdicts {
    use url_batch::batch::FallbackDecision;
    let mut result = Verdicts::default();
    let mut cursor = 0;
    while cursor < text.len() {
        let c = text[cursor..].chars().next().unwrap();
        if c.is_whitespace() {
            cursor += c.len_utf8();
            continue;
        }
        let start = cursor;
        cursor = text[start..]
            .find(char::is_whitespace)
            .map_or(text.len(), |x| start + x);
        let word = &text[start..cursor];
        let has_web =
            word.contains("www.") || word.contains("http://") || word.contains("https://");
        let mut boundary = true;
        let has_whatsapp = word.char_indices().any(|(i, c)| {
            let candidate = boundary && word[i..].starts_with("whatsapp://");
            boundary = c.is_whitespace() || matches!(c, '(' | '[' | '"' | '\'');
            candidate
        });
        if !has_web && !has_whatsapp {
            continue;
        }
        if !has_web {
            let mut ww = crate::safety::interval::Work::default();
            let whatsapp = crate::safety::interval::Index::new(word, &mut ww);
            let mut scan = crate::safety::LinkScan::default();
            let mut boundary = true;
            #[cfg(test)]
            let mut candidates = 0;
            for (i, c) in word.char_indices() {
                let candidate = boundary && word[i..].starts_with("whatsapp://");
                boundary = c.is_whitespace() || matches!(c, '(' | '[' | '"' | '\'');
                if !candidate {
                    continue;
                }
                let end = scan.link_end(word, i);
                let verdict = whatsapp
                    .query(i..end, &mut ww)
                    .map_or(Verdict::Reject, |digits| Verdict::Chat {
                        end: start + end,
                        digits,
                    });
                result.push(start + i, verdict);
                #[cfg(test)]
                {
                    candidates += 1;
                }
            }
            #[cfg(test)]
            crate::safety::stats::update(|s| {
                s.indexed_word_bytes += word.len();
                s.whatsapp_preprocessing += ww.preprocessing;
                s.whatsapp_authority_bytes += ww.authority_bytes;
                s.whatsapp_authority_scanned += ww.authority_scanned;
                s.whatsapp_parser_input_bytes += ww.parser_input_bytes;
                s.whatsapp_parser_calls += ww.parser_calls;
                s.whatsapp_path_steps += ww.path_steps;
                s.whatsapp_query_bytes += ww.query_bytes;
                s.candidates += candidates;
            });
            continue;
        }
        let mut aw = Work::default();
        let mut uw = UrlWork::default();
        let adapter = Adapter::new(word, &mut aw, &mut uw).expect("word index");
        let mut provider = provider::IndexedProvider::new(&adapter);
        let mut ww = crate::safety::interval::Work::default();
        let whatsapp = has_whatsapp.then(|| crate::safety::interval::Index::new(word, &mut ww));
        let mut scan = crate::safety::LinkScan::default();
        let mut boundary = true;
        #[cfg(test)]
        let mut candidates = 0;
        for (i, c) in word.char_indices() {
            let rest = &word[i..];
            let kind = if boundary && rest.starts_with("www.") {
                1
            } else if boundary && rest.starts_with("http://") {
                2
            } else if boundary && rest.starts_with("https://") {
                3
            } else if boundary && rest.starts_with("whatsapp://") {
                4
            } else {
                0
            };
            boundary = c.is_whitespace() || matches!(c, '(' | '[' | '"' | '\'');
            if kind == 0 {
                continue;
            }
            let end = scan.link_end(word, i);
            let verdict = if kind == 4 {
                whatsapp
                    .as_ref()
                    .expect("WhatsApp index")
                    .query(i..end, &mut ww)
                    .map_or(Verdict::Reject, |digits| Verdict::Chat {
                        end: start + end,
                        digits,
                    })
            } else {
                let scope = DecisionScope(&mut provider);
                let decision = if kind == 1 {
                    if end - i < 4 {
                        adapter.index.special_fallback(i..end, scope.0, &mut uw)
                    } else {
                        adapter.index.preview_www(i..end, scope.0, &mut uw)
                    }
                } else {
                    adapter.index.preview_absolute(i..end, scope.0, &mut uw)
                }
                .expect("LinkScan candidate interval is valid");
                match decision {
                    FallbackDecision::Accept => Verdict::Web { end: start + end },
                    FallbackDecision::Reject
                    | FallbackDecision::Unsafe
                    | FallbackDecision::NotWeb => Verdict::Reject,
                    FallbackDecision::ProviderDeferred => Verdict::Unsupported,
                }
            };
            result.push(start + i, verdict);
            #[cfg(test)]
            {
                candidates += 1;
            }
        }
        #[cfg(test)]
        crate::safety::stats::update(|s| {
            s.indexed_word_bytes += word.len();
            s.raw_decoded += aw.decode_raw;
            s.decoded_bytes += aw.decoded;
            s.fragment_input += aw.fragment_input;
            s.fragment_output += aw.fragment_output;
            s.normalized_base_bytes += aw.normalized_base_bytes;
            s.normalized_tail_bytes +=
                aw.normalized_tail_bytes + provider.work.normalized_tail_bytes;
            s.normalized_revalidation_bytes += provider.work.normalized_revalidation_bytes;
            s.url_indexed_bytes += uw.indexed_bytes;
            s.url_validation_bytes += uw.validation_bytes;
            s.url_at_bytes += uw.at_bytes;
            s.url_path_steps += uw.path_steps;
            s.url_queries += uw.queries;
            s.url_opaque_parser_bytes += uw.opaque_parser_bytes;
            s.url_special_bytes += uw.special_bytes;
            s.provider_prepare_calls += provider.prepare_calls;
            s.max_prepares_per_decision = s
                .max_prepares_per_decision
                .max(provider.max_decision_prepares);
            s.provider_operations += provider.operations.get();
            s.provider_segment_probes += provider.segment_probes.get();
            s.whatsapp_preprocessing += ww.preprocessing;
            s.whatsapp_authority_bytes += ww.authority_bytes;
            s.whatsapp_authority_scanned += ww.authority_scanned;
            s.whatsapp_parser_input_bytes += ww.parser_input_bytes;
            s.whatsapp_parser_calls += ww.parser_calls;
            s.whatsapp_path_steps += ww.path_steps;
            s.whatsapp_query_bytes += ww.query_bytes;
            s.provider_index_bytes += provider.build_bytes;
            s.provider_local_bytes += provider.local_bytes;
            s.max_provider_shared_capacity = s
                .max_provider_shared_capacity
                .max(provider.shared_allocated);
            s.max_provider_local_capacity = s
                .max_provider_local_capacity
                .max(provider.peak_local_allocated);
            s.candidates += candidates;
        });
    }
    result
}

#[cfg(test)]
mod lifecycle_tests {
    #[test]
    fn absolute_retry_keeps_keys_through_both_preparations() {
        let text = "http://09";
        let mut aw = super::Work::default();
        let mut uw = super::UrlWork::default();
        let adapter = super::Adapter::new(text, &mut aw, &mut uw).unwrap();
        let mut provider = super::provider::IndexedProvider::new(&adapter);
        {
            let scope = super::DecisionScope(&mut provider);
            let result = adapter
                .index
                .preview_absolute(0..text.len(), scope.0, &mut uw)
                .unwrap();
            assert_eq!(result, url_batch::batch::FallbackDecision::Accept);
            assert_eq!(scope.0.decision_prepares, 2);
        }
        assert_eq!(provider.decision_prepares, 0);
        assert_eq!(provider.max_decision_prepares, 2);
    }
}

#[cfg(test)]
mod interval_totality_tests {
    use super::{Verdict, decisions};

    #[test]
    fn link_scan_candidates_never_defer_for_mixed_terminal_fragments() {
        for count in [1, 2, 8, 64] {
            for tail in [").,)))", ")...,,)))", ")..é.,)))", ")..?)))"] {
                let text = format!("{}www.é{tail}", "www.a(".repeat(count));
                let verdicts = decisions(&text);
                assert!(
                    verdicts
                        .special
                        .iter()
                        .all(|(_, verdict)| !matches!(verdict, Verdict::Unsupported)),
                    "LinkScan-generated candidate was deferred"
                );
                assert_eq!(
                    crate::safety::linkify_markup(&text),
                    crate::safety::reference::linkify_markup(&text)
                );
            }
        }
    }
}

#[cfg(test)]
mod verdict_index_tests {
    use super::{Verdict, Verdicts};

    #[test]
    fn compact_entries_preserve_sparse_verdicts_and_maximum_offsets() {
        let mut v = Verdicts::default();
        v.push(0, Verdict::Reject);
        v.push(1, Verdict::Web { end: usize::MAX });
        v.push(
            2,
            Verdict::Chat {
                end: 8,
                digits: "123".into(),
            },
        );
        v.push(3, Verdict::Unsupported);
        v.push(4, Verdict::Web { end: 0 });
        v.push(usize::MAX, Verdict::Reject);
        assert!(matches!(v.get(0), Some(Verdict::Reject)));
        assert!(matches!(v.get(1), Some(Verdict::Web { end: usize::MAX })));
        assert!(matches!(v.get(2), Some(Verdict::Chat { end: 8, digits }) if digits == "123"));
        assert!(matches!(v.get(3), Some(Verdict::Unsupported)));
        assert!(matches!(v.get(4), Some(Verdict::Web { end: 0 })));
        assert!(v.get(usize::MAX - 1).is_none());
        assert!(matches!(v.get(usize::MAX), Some(Verdict::Reject)));
        assert!(matches!(v.get(usize::MAX), Some(Verdict::Reject)));
        assert_eq!(
            std::mem::size_of_val(&v.entries[0]),
            2 * std::mem::size_of::<usize>()
        );
    }

    #[test]
    fn dense_nested_live_verdict_capacity() {
        let prefix = "*_~";
        let suffix = "x~_*";
        let count = (1024 * 1024 - prefix.len() - suffix.len()) / 5;
        let padding = 1024 * 1024 - prefix.len() - suffix.len() - count * 5;
        let text = format!(
            "{prefix}{}{}{suffix}",
            "www. ".repeat(count),
            "x".repeat(padding)
        );
        let spans = [
            &text[..],
            &text[1..text.len() - 1],
            &text[2..text.len() - 2],
            &text[3..text.len() - 3],
        ];
        let verdicts: Vec<_> = spans.iter().map(|span| super::decisions(span)).collect();
        let capacities: Vec<_> = verdicts.iter().map(|v| v.entries.capacity()).collect();
        let lengths: Vec<_> = verdicts.iter().map(|v| v.entries.len()).collect();
        let special: usize = verdicts.iter().map(|v| v.special.capacity()).sum();
        let bytes: usize = capacities.iter().sum::<usize>() * std::mem::size_of::<(usize, usize)>();
        println!(
            "entry_bytes={} lengths={lengths:?} capacities={capacities:?} special_capacity={special} four_frame_bytes={bytes}",
            std::mem::size_of::<(usize, usize)>()
        );
        assert!(lengths.iter().all(|&len| len >= count - 1));
        assert_eq!(special, 0);
        assert!(bytes <= 16 * 1024 * 1024);
    }

    #[test]
    fn monotonic_queries_skip_unconsumed_candidates() {
        let mut verdicts = Verdicts::default();
        verdicts.push(0, Verdict::Reject);
        verdicts.push(4, Verdict::Web { end: 9 });
        verdicts.push(12, Verdict::Reject);
        assert!(verdicts.get(1).is_none());
        assert!(matches!(verdicts.get(4), Some(Verdict::Web { end: 9 })));
        assert!(matches!(verdicts.get(4), Some(Verdict::Web { end: 9 })));
        assert!(verdicts.get(11).is_none());
        assert!(matches!(verdicts.get(12), Some(Verdict::Reject)));
        assert!(verdicts.get(13).is_none());
    }
}

#[cfg(test)]
mod lazy_whatsapp_tests {
    #[test]
    fn embedded_non_candidate_does_not_construct_whatsapp_index() {
        let input = "www.whatsapp://example";
        crate::safety::stats::reset();
        let actual = crate::safety::linkify_markup(input);
        assert_eq!(actual, crate::safety::reference::linkify_markup(input));
        assert_eq!(crate::safety::stats::snapshot().whatsapp_preprocessing, 0);
    }

    #[test]
    fn boundary_candidate_still_builds_whatsapp_index() {
        let input = "www.a(whatsapp://send?phone=12345678)";
        crate::safety::stats::reset();
        let actual = crate::safety::linkify_markup(input);
        assert_eq!(actual, crate::safety::reference::linkify_markup(input));
        assert!(crate::safety::stats::snapshot().whatsapp_preprocessing > 0);
    }
}

#[cfg(test)]
mod offsets_tests {
    use super::Offsets;

    #[test]
    fn compact_and_wide_store_positions_and_missing_values() {
        for mut values in [
            Offsets::new(3, usize::MAX),
            Offsets::Wide(vec![usize::MAX; 3]),
        ] {
            assert_eq!(values.get(0), usize::MAX);
            values.set(0, 0);
            values.set(1, 123_456);
            values.set(2, usize::MAX);
            assert_eq!(
                (values.get(0), values.get(1), values.get(2)),
                (0, 123_456, usize::MAX)
            );
        }
    }
}
