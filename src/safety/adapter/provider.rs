use super::{Adapter, Decision, Ready, Work};
use std::borrow::Cow;
#[cfg(test)]
use std::cell::Cell;
use std::ops::Range;
use url_batch::batch::{AsciiClass, DomainProvider, PreparedDomain};

struct Index<'a> {
    bytes: Cow<'a, [u8]>,
    len: usize,
    masks: Vec<[u64; 5]>,
    ranks: Vec<[usize; 4]>,
    dot_groups: Vec<DotGroup>,
    sparse_dots: Vec<usize>,
    next_block: Vec<usize>,
}
struct DotGroup {
    start: usize,
    sparse: usize,
}
impl<'a> Index<'a> {
    #[cfg(test)]
    fn allocated(&self) -> usize {
        (match &self.bytes {
            Cow::Borrowed(_) => 0,
            Cow::Owned(bytes) => bytes.capacity(),
        }) + self.masks.capacity() * core::mem::size_of::<[u64; 5]>()
            + self.ranks.capacity() * core::mem::size_of::<[usize; 4]>()
            + self.next_block.capacity() * core::mem::size_of::<usize>()
            + self.dot_groups.capacity() * core::mem::size_of::<DotGroup>()
            + self.sparse_dots.capacity() * core::mem::size_of::<usize>()
    }
    fn new(data: &'a [u8]) -> Self {
        Self::build(Cow::Borrowed(data))
    }
    fn owned(data: Vec<u8>) -> Self {
        Self::build(Cow::Owned(data))
    }
    fn build(data: Cow<'a, [u8]>) -> Self {
        let blocks = data.len().div_ceil(64);
        let mut masks = vec![[0u64; 5]; blocks];
        for (i, &b) in data.iter().enumerate() {
            let m = &mut masks[i / 64];
            let bit = 1u64 << (i % 64);
            if !b.is_ascii_digit() {
                m[0] |= bit
            }
            if !(b'0'..=b'7').contains(&b) {
                m[1] |= bit
            }
            if !b.is_ascii_hexdigit() {
                m[2] |= bit
            }
            if b == b'.' {
                m[3] |= bit
            }
            if b != b'0' {
                m[4] |= bit
            }
        }
        let mut ranks = vec![[0usize; 4]; blocks + 1];
        for i in 0..blocks {
            for k in 0..4 {
                ranks[i + 1][k] = ranks[i][k] + masks[i][k].count_ones() as usize;
            }
        }
        let mut dot_groups = Vec::new();
        let mut sparse_dots = Vec::new();
        let mut group = [0usize; 64];
        let mut used = 0;
        for (block, mask) in masks.iter().enumerate() {
            let mut bits = mask[3];
            while bits != 0 {
                group[used] = block * 64 + bits.trailing_zeros() as usize;
                used += 1;
                bits &= bits - 1;
                if used == 64 {
                    Self::push_dot_group(&mut dot_groups, &mut sparse_dots, &group[..used]);
                    used = 0;
                }
            }
        }
        if used != 0 {
            Self::push_dot_group(&mut dot_groups, &mut sparse_dots, &group[..used]);
        }
        let mut next_block = vec![blocks; blocks + 1];
        for i in (0..blocks).rev() {
            next_block[i] = if masks[i][4] != 0 {
                i
            } else {
                next_block[i + 1]
            };
        }
        let len = data.len();
        Self {
            bytes: data,
            len,
            masks,
            ranks,
            dot_groups,
            sparse_dots,
            next_block,
        }
    }
    fn without_bytes(self) -> Index<'static> {
        Index {
            bytes: Cow::Borrowed(b""),
            len: self.len,
            masks: self.masks,
            ranks: self.ranks,
            dot_groups: self.dot_groups,
            sparse_dots: self.sparse_dots,
            next_block: self.next_block,
        }
    }
    fn push_dot_group(
        groups: &mut Vec<DotGroup>,
        sparse_dots: &mut Vec<usize>,
        positions: &[usize],
    ) {
        let start = positions[0];
        let sparse = if positions[positions.len() - 1] - start >= 4096 {
            let i = sparse_dots.len();
            sparse_dots.extend_from_slice(positions);
            i
        } else {
            usize::MAX
        };
        groups.push(DotGroup { start, sparse });
    }
    fn rank(&self, k: usize, pos: usize) -> usize {
        let b = pos / 64;
        let shift = pos % 64;
        self.ranks[b][k]
            + if shift == 0 {
                0
            } else {
                (self.masks[b][k] & ((1u64 << shift) - 1)).count_ones() as usize
            }
    }
    fn select_dot(&self, mut number: usize) -> usize {
        assert!(number < self.ranks[self.masks.len()][3]);
        let group = &self.dot_groups[number / 64];
        number %= 64;
        if group.sparse != usize::MAX {
            return self.sparse_dots[group.sparse + number];
        }
        let mut block = group.start / 64;
        let mut bits = self.masks[block][3] & (!0u64 << (group.start % 64));
        while number >= bits.count_ones() as usize {
            number -= bits.count_ones() as usize;
            block += 1;
            bits = self.masks[block][3];
        }
        for _ in 0..number {
            bits &= bits - 1;
        }
        block * 64 + bits.trailing_zeros() as usize
    }
    fn first_nonzero(&self, start: usize, end: usize) -> usize {
        if start == end {
            return end;
        }
        let block = start / 64;
        let masked = self.masks[block][4] & (!0u64 << (start % 64));
        let i = if masked != 0 {
            block * 64 + masked.trailing_zeros() as usize
        } else {
            let next = self.next_block[block + 1];
            if next == self.masks.len() {
                end
            } else {
                next * 64 + self.masks[next][4].trailing_zeros() as usize
            }
        };
        i.min(end)
    }
}
#[derive(Clone, Copy)]
enum Source {
    Shared(usize),
    Local(usize),
}
struct Segment {
    source: Source,
    range: Range<usize>,
    virtual_start: usize,
    dots_before: usize,
}
struct Key {
    segments: Vec<Segment>,
    locals: Vec<Index<'static>>,
    len: usize,
    dots: usize,
}
struct Component {
    start: usize,
    end: usize,
    id: Option<usize>,
}
pub struct IndexedProvider<'a, 'w> {
    adapter: &'a Adapter<'w>,
    shared: Vec<Index<'a>>,
    components: Vec<Component>,
    keys: Vec<Key>,
    pub work: Work,
    #[cfg(test)]
    pub build_bytes: usize,
    #[cfg(test)]
    pub local_bytes: usize,
    #[cfg(test)]
    pub shared_allocated: usize,
    #[cfg(test)]
    pub local_allocated: usize,
    #[cfg(test)]
    pub peak_local_allocated: usize,
    #[cfg(test)]
    pub active_local_allocated: usize,
    #[cfg(test)]
    pub operations: Cell<usize>,
    #[cfg(test)]
    pub segment_probes: Cell<usize>,
    #[cfg(test)]
    pub prepare_calls: usize,
    #[cfg(test)]
    pub decision_prepares: usize,
    #[cfg(test)]
    pub max_decision_prepares: usize,
}
impl<'a, 'w> IndexedProvider<'a, 'w> {
    pub fn new(adapter: &'a Adapter<'w>) -> Self {
        let mut shared = vec![
            Index::new(b"."),
            Index::new(b"www."),
            Index::new(adapter.word.as_bytes()),
        ];
        shared.push(Index::new(adapter.arena.as_bytes()));
        shared.push(Index::new(
            adapter
                .ascii_tail
                .as_ref()
                .and_then(|(_, base)| base.as_ref().ok())
                .map_or(b"".as_slice(), |base| base.fragment.ascii.as_bytes()),
        ));
        let mut components = Vec::new();
        if let Some((_, Ok(plan))) = &adapter.tail_plan {
            plan.visit_components(|s| {
                if !s.is_empty() {
                    let start = s.as_ptr() as usize;
                    components.push(Component {
                        start,
                        end: start + s.len(),
                        id: None,
                    });
                }
            })
        }
        #[cfg(test)]
        let build_bytes = shared.iter().map(|x| x.len).sum();
        #[cfg(test)]
        let shared_allocated = shared.iter().map(Index::allocated).sum();
        Self {
            adapter,
            shared,
            components,
            keys: Vec::new(),
            work: Work::default(),
            #[cfg(test)]
            build_bytes,
            #[cfg(test)]
            local_bytes: 0,
            #[cfg(test)]
            shared_allocated,
            #[cfg(test)]
            local_allocated: 0,
            #[cfg(test)]
            peak_local_allocated: 0,
            #[cfg(test)]
            active_local_allocated: 0,
            #[cfg(test)]
            operations: Cell::new(0),
            #[cfg(test)]
            segment_probes: Cell::new(0),
            #[cfg(test)]
            prepare_calls: 0,
            #[cfg(test)]
            decision_prepares: 0,
            #[cfg(test)]
            max_decision_prepares: 0,
        }
    }
    pub fn end_decision(&mut self) {
        self.keys.clear();
        #[cfg(test)]
        {
            self.active_local_allocated = 0;
        }
        #[cfg(test)]
        {
            self.decision_prepares = 0;
        }
    }
    fn push(key: &mut Key, shared: &[Index], source: Source, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        let idx = match source {
            Source::Shared(i) => &shared[i],
            Source::Local(i) => &key.locals[i],
        };
        let count = idx.rank(3, range.end) - idx.rank(3, range.start);
        key.segments.push(Segment {
            source,
            virtual_start: key.len,
            dots_before: key.dots,
            range: range.clone(),
        });
        key.len += range.len();
        key.dots += count;
    }
    fn build_key(&mut self, r: &Ready) -> Key {
        let mut key = Key {
            segments: Vec::with_capacity(8),
            locals: Vec::new(),
            len: 0,
            dots: 0,
        };
        if r.www {
            Self::push(&mut key, &self.shared, Source::Shared(1), 0..4)
        }
        if r.end > r.start {
            Self::push(
                &mut key,
                &self.shared,
                Source::Shared(3),
                self.adapter.arena_range(r.start, r.end),
            );
        }
        if let Some(p) = r.partial.as_ref() {
            if r.end > r.start {
                Self::push(&mut key, &self.shared, Source::Shared(0), 0..1)
            }
            Self::push(&mut key, &self.shared, Source::Shared(2), p.clone());
        }
        if let Some(p) = r.prepared_partial.as_ref() {
            if r.end > r.start {
                Self::push(&mut key, &self.shared, Source::Shared(0), 0..1)
            }
            let id = key.locals.len();
            #[cfg(test)]
            {
                self.local_bytes += p.ascii.len();
            }
            key.locals.push(Index::owned(p.ascii.as_bytes().to_vec()));
            #[cfg(test)]
            {
                self.local_allocated += key.locals[id].allocated();
            }
            Self::push(&mut key, &self.shared, Source::Local(id), 0..p.ascii.len());
        }
        if let Some(tail) = r.ascii_tail.as_ref() {
            if r.end > r.start {
                Self::push(&mut key, &self.shared, Source::Shared(0), 0..1)
            }
            Self::push(
                &mut key,
                &self.shared,
                Source::Shared(4),
                0..self.shared[4].bytes.len(),
            );
            Self::push(&mut key, &self.shared, Source::Shared(2), tail.clone());
        }
        if let Some((i, result)) = r.tail_prepared.as_ref() {
            if r.end > r.start {
                Self::push(&mut key, &self.shared, Source::Shared(0), 0..1)
            }
            let plan = self
                .adapter
                .tail_plan
                .as_ref()
                .filter(|(part, _)| part == i)
                .unwrap()
                .1
                .as_ref()
                .unwrap();
            plan.visit_ascii(result, |chunk| {
                if chunk.is_empty() {
                    return;
                }
                if chunk == "." {
                    Self::push(&mut key, &self.shared, Source::Shared(0), 0..1);
                    return;
                }
                let ptr = chunk.as_ptr() as usize;
                if let Some(c) = self
                    .components
                    .iter_mut()
                    .find(|c| ptr >= c.start && ptr + chunk.len() <= c.end)
                {
                    let start = ptr - c.start;
                    let id = match c.id {
                        Some(id) => id,
                        None => {
                            let mut built = None;
                            plan.visit_components(|component| {
                                if component.as_ptr() as usize == c.start {
                                    built = Some(Index::new(component.as_bytes()).without_bytes());
                                }
                            });
                            let index = built.unwrap();
                            #[cfg(test)]
                            {
                                self.build_bytes += index.len;
                            }
                            #[cfg(test)]
                            {
                                self.shared_allocated += index.allocated();
                            }
                            let id = self.shared.len();
                            self.shared.push(index);
                            c.id = Some(id);
                            id
                        }
                    };
                    Self::push(
                        &mut key,
                        &self.shared,
                        Source::Shared(id),
                        start..start + chunk.len(),
                    )
                } else {
                    let id = key.locals.len();
                    #[cfg(test)]
                    {
                        self.local_bytes += chunk.len();
                    }
                    key.locals.push(Index::owned(chunk.as_bytes().to_vec()));
                    #[cfg(test)]
                    {
                        self.local_allocated += key.locals[id].allocated();
                    }
                    Self::push(&mut key, &self.shared, Source::Local(id), 0..chunk.len());
                }
            });
        }
        assert_eq!(key.len, r.ascii_len);
        assert!(key.segments.len() <= 8);
        key
    }
    #[cfg(test)]
    fn record(&self, key: usize, passes: usize) {
        self.operations.set(self.operations.get() + 1);
        self.segment_probes
            .set(self.segment_probes.get() + passes * self.keys[key].segments.len());
    }
    fn index<'b>(&'b self, key: &'b Key, source: Source) -> &'b Index<'b> {
        match source {
            Source::Shared(i) => &self.shared[i],
            Source::Local(i) => &key.locals[i],
        }
    }
    fn source_byte(&self, key: &Key, source: Source, offset: usize) -> u8 {
        if let Source::Shared(id) = source
            && let Some(component) = self
                .components
                .iter()
                .find(|component| component.id == Some(id))
        {
            let mut result = None;
            if let Some((_, Ok(plan))) = &self.adapter.tail_plan {
                plan.visit_components(|chunk| {
                    if chunk.as_ptr() as usize == component.start {
                        result = chunk.as_bytes().get(offset).copied();
                    }
                });
            }
            return result.unwrap();
        }
        self.index(key, source).bytes[offset]
    }
    fn dot(&self, key: &Key, number: usize) -> usize {
        for s in &key.segments {
            let idx = self.index(key, s.source);
            let n = idx.rank(3, s.range.end) - idx.rank(3, s.range.start);
            if number >= s.dots_before && number < s.dots_before + n {
                let pos = idx.select_dot(idx.rank(3, s.range.start) + number - s.dots_before);
                return s.virtual_start + pos - s.range.start;
            }
        }
        panic!("invalid dot")
    }
    fn pieces<'b>(
        &'b self,
        key: &'b Key,
        range: Range<usize>,
    ) -> impl Iterator<Item = (&'b Index<'b>, Range<usize>, usize)> + 'b {
        key.segments.iter().filter_map(move |s| {
            let start = range.start.max(s.virtual_start);
            let end = range.end.min(s.virtual_start + s.range.len());
            if start >= end {
                return None;
            }
            Some((
                self.index(key, s.source),
                s.range.start + start - s.virtual_start..s.range.start + end - s.virtual_start,
                start,
            ))
        })
    }
}
impl DomainProvider for IndexedProvider<'_, '_> {
    fn prepare(&mut self, host: Range<usize>) -> PreparedDomain {
        #[cfg(test)]
        {
            self.prepare_calls += 1;
        }
        #[cfg(test)]
        {
            self.decision_prepares += 1;
        }
        #[cfg(test)]
        {
            self.max_decision_prepares = self.max_decision_prepares.max(self.decision_prepares);
        }
        match self.adapter.query_host(host, &mut self.work) {
            Decision::Ready(r) => {
                let key = self.build_key(&r);
                #[cfg(test)]
                {
                    self.active_local_allocated +=
                        key.locals.iter().map(Index::allocated).sum::<usize>();
                    self.peak_local_allocated =
                        self.peak_local_allocated.max(self.active_local_allocated);
                }
                let len = key.len;
                let id = self.keys.len();
                self.keys.push(key);
                PreparedDomain::Ready {
                    serialized_len: len as u128,
                    key: id,
                }
            }
            Decision::Invalid | Decision::EmptyHost => PreparedDomain::Invalid,
            _ => PreparedDomain::Deferred,
        }
    }
    fn label_count(&self, key: usize) -> usize {
        #[cfg(test)]
        self.record(key, 1);
        self.keys[key].dots + 1
    }
    fn label(&self, key: usize, index: usize) -> Range<usize> {
        #[cfg(test)]
        self.record(key, 2);
        let k = &self.keys[key];
        assert!(index <= k.dots);
        let start = if index == 0 {
            0
        } else {
            self.dot(k, index - 1) + 1
        };
        let end = if index == k.dots {
            k.len
        } else {
            self.dot(k, index)
        };
        start..end
    }
    fn byte(&self, key: usize, index: usize) -> u8 {
        #[cfg(test)]
        self.record(key, 1);
        let k = &self.keys[key];
        assert!(index < k.len);
        for s in &k.segments {
            if index >= s.virtual_start && index < s.virtual_start + s.range.len() {
                return self.source_byte(k, s.source, s.range.start + index - s.virtual_start);
            }
        }
        unreachable!()
    }
    fn all(&self, key: usize, range: Range<usize>, class: AsciiClass) -> bool {
        #[cfg(test)]
        self.record(key, 1);
        let k = &self.keys[key];
        assert!(range.start <= range.end && range.end <= k.len);
        self.pieces(k, range).all(|(i, r, _)| {
            let k = match class {
                AsciiClass::Decimal => 0,
                AsciiClass::Octal => 1,
                AsciiClass::Hex => 2,
            };
            i.rank(k, r.end) == i.rank(k, r.start)
        })
    }
    fn first_nonzero(&self, key: usize, range: Range<usize>) -> usize {
        #[cfg(test)]
        self.record(key, 1);
        let k = &self.keys[key];
        assert!(range.start <= range.end && range.end <= k.len);
        let end = range.end;
        for (i, r, virtual_start) in self.pieces(k, range) {
            let n = i.first_nonzero(r.start, r.end);
            if n < r.end {
                return virtual_start + n - r.start;
            }
        }
        end
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Adapter, Work};
    use super::{Index, IndexedProvider};
    use url_batch::batch::{DomainProvider, PreparedDomain, Work as UrlWork};
    #[test]
    fn rejected_expansion_tail_never_indexes_plan_components() {
        let mut input = String::from("www.");
        input.push_str(&"ﷺ".repeat((1024 * 1024 - 5) / 3));
        while input.len() < 1024 * 1024 - 1 {
            input.push('a');
        }
        input.push(')');
        let mut work = Work::default();
        let mut url_work = UrlWork::default();
        let adapter = Adapter::new(&input, &mut work, &mut url_work).unwrap();
        let mut provider = IndexedProvider::new(&adapter);
        assert!(!provider.components.is_empty());
        assert!(
            provider
                .components
                .iter()
                .all(|component| component.id.is_none())
        );
        let shared = provider.shared_allocated;
        let bytes = provider.build_bytes;
        let mut eager_payload = 0;
        if let Some((_, Ok(plan))) = &adapter.tail_plan {
            plan.visit_components(|component| {
                if !component.is_empty() {
                    eager_payload += Index::new(component.as_bytes()).allocated();
                }
            });
        }
        assert!(eager_payload > 1024 * 1024);
        assert!(matches!(
            provider.prepare(0..input.len() - 1),
            PreparedDomain::Invalid
        ));
        assert_eq!(provider.shared_allocated, shared);
        assert_eq!(provider.build_bytes, bytes);
        assert!(
            provider
                .components
                .iter()
                .all(|component| component.id.is_none())
        );
        println!(
            "expansion fixture: shared={shared} eager_component_payload={eager_payload} lazy_component_payload=0"
        );
    }
    #[test]
    fn actual_components_are_cached_and_retry_keys_remain_live() {
        let input = "www.é。abc)))";
        let end = input.len() - 1;
        let mut work = Work::default();
        let mut url_work = UrlWork::default();
        let adapter = Adapter::new(input, &mut work, &mut url_work).unwrap();
        let mut provider = IndexedProvider::new(&adapter);
        assert!(
            provider
                .components
                .iter()
                .all(|component| component.id.is_none())
        );
        let before = provider.shared_allocated;
        let PreparedDomain::Ready {
            serialized_len,
            key: first,
        } = provider.prepare(0..end)
        else {
            panic!("first key")
        };
        let first_bytes: Vec<_> = (0..serialized_len as usize)
            .map(|i| provider.byte(first, i))
            .collect();
        let after = provider.shared_allocated;
        let shared_len = provider.shared.len();
        assert!(after > before);
        assert!(
            provider
                .components
                .iter()
                .any(|component| component.id.is_none())
        );
        let PreparedDomain::Ready {
            serialized_len: second_len,
            key: second,
        } = provider.prepare(0..end)
        else {
            panic!("retry key")
        };
        assert_eq!(provider.shared_allocated, after);
        assert_eq!(provider.shared.len(), shared_len);
        println!(
            "valid fixture: shared_before={before} shared_after={after} component_count={} indexed_count={}",
            provider.components.len(),
            provider
                .components
                .iter()
                .filter(|component| component.id.is_some())
                .count()
        );
        assert_eq!(serialized_len, second_len);
        assert_eq!(
            first_bytes,
            (0..second_len as usize)
                .map(|i| provider.byte(second, i))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            first_bytes,
            (0..serialized_len as usize)
                .map(|i| provider.byte(first, i))
                .collect::<Vec<_>>()
        );
        provider.end_decision();
        let PreparedDomain::Ready { .. } = provider.prepare(0..end) else {
            panic!("new decision")
        };
        assert_eq!(provider.shared_allocated, after);
        assert_eq!(provider.shared.len(), shared_len);
    }
    #[test]
    fn packed_index_matches_byte_scans() {
        for n in [0, 1, 63, 64, 65, 127, 128, 129, 257] {
            let bytes: Vec<_> = (0..n)
                .map(|i| b"00012389abcdef.xG"[(i * 11 + i / 7) % 17])
                .collect();
            let index = Index::new(&bytes);
            for (number, position) in bytes
                .iter()
                .enumerate()
                .filter_map(|(i, &b)| (b == b'.').then_some(i))
                .enumerate()
            {
                assert_eq!(index.select_dot(number), position);
            }
            for start in 0..=n {
                for end in start..=n {
                    for k in 0..4 {
                        let count = bytes[start..end]
                            .iter()
                            .filter(|&&b| match k {
                                0 => !b.is_ascii_digit(),
                                1 => !(b'0'..=b'7').contains(&b),
                                2 => !b.is_ascii_hexdigit(),
                                _ => b == b'.',
                            })
                            .count();
                        assert_eq!(
                            index.rank(k, end) - index.rank(k, start),
                            count,
                            "n={n} range={start}..{end} k={k}"
                        );
                    }
                    assert_eq!(
                        index.first_nonzero(start, end),
                        (start..end).find(|&i| bytes[i] != b'0').unwrap_or(end)
                    );
                }
            }
        }
    }
    #[test]
    fn dense_and_sparse_dot_selection() {
        for bytes in [vec![b'.'; 8193], {
            let mut b = vec![b'0'; 8193];
            b[0] = b'.';
            b[4096] = b'.';
            b[8192] = b'.';
            b
        }] {
            let index = Index::new(&bytes);
            for (number, position) in bytes
                .iter()
                .enumerate()
                .filter_map(|(i, &b)| (b == b'.').then_some(i))
                .enumerate()
            {
                assert_eq!(index.select_dot(number), position);
            }
            assert!(index.allocated() < bytes.len() * 3);
        }
    }
    #[test]
    fn dot_selection_across_sparse_and_dense_groups() {
        let mut bytes = vec![b'0'; 64 * 64 * 65 + 1];
        for i in 0..64 {
            bytes[i * 100] = b'.';
        }
        for i in 0..64 {
            bytes[10000 + i] = b'.';
        }
        for &i in &[262143, 262144, 266240] {
            bytes[i] = b'.';
        }
        let index = Index::new(&bytes);
        assert_ne!(index.dot_groups[0].sparse, usize::MAX);
        assert_eq!(index.dot_groups[1].sparse, usize::MAX);
        for (number, position) in bytes
            .iter()
            .enumerate()
            .filter_map(|(i, &b)| (b == b'.').then_some(i))
            .enumerate()
        {
            assert_eq!(index.select_dot(number), position);
        }
    }
}
