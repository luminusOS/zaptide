const GROUP: usize = 64;
const DENSE_SPAN: usize = 4096;

struct SelectGroup {
    first: usize,
    sparse: usize,
}

struct Bitmap {
    words: Vec<u64>,
    ranks: Vec<usize>,
    groups: Vec<SelectGroup>,
    sparse: Vec<usize>,
}

impl Bitmap {
    fn new(len: usize) -> Self {
        Self {
            words: vec![0; len / 64 + 1],
            ranks: Vec::new(),
            groups: Vec::new(),
            sparse: Vec::new(),
        }
    }

    fn insert(&mut self, position: usize) {
        self.words[position / 64] |= 1u64 << (position % 64);
    }

    fn finish(&mut self) {
        self.ranks = Vec::with_capacity(self.words.len() + 1);
        self.ranks.push(0);
        let mut positions = [0usize; GROUP];
        let mut used = 0;
        for word_index in 0..self.words.len() {
            let mut word = self.words[word_index];
            self.ranks
                .push(self.ranks[word_index] + word.count_ones() as usize);
            while word != 0 {
                positions[used] = word_index * 64 + word.trailing_zeros() as usize;
                used += 1;
                word &= word - 1;
                if used == GROUP {
                    self.push_group(&positions);
                    used = 0;
                }
            }
        }
        if used != 0 {
            self.push_group(&positions[..used]);
        }
    }

    fn push_group(&mut self, positions: &[usize]) {
        let sparse = if positions[positions.len() - 1] - positions[0] >= DENSE_SPAN {
            let offset = self.sparse.len();
            self.sparse.extend_from_slice(positions);
            offset
        } else {
            usize::MAX
        };
        self.groups.push(SelectGroup {
            first: positions[0],
            sparse,
        });
    }

    fn contains(&self, position: usize) -> bool {
        self.words[position / 64] & (1u64 << (position % 64)) != 0
    }

    fn rank(&self, position: usize) -> usize {
        let shift = position % 64;
        self.ranks[position / 64]
            + if shift == 0 {
                0
            } else {
                (self.words[position / 64] & ((1u64 << shift) - 1)).count_ones() as usize
            }
    }

    fn select(&self, number: usize) -> usize {
        let group = &self.groups[number / GROUP];
        let mut remaining = number % GROUP;
        if group.sparse != usize::MAX {
            return self.sparse[group.sparse + remaining];
        }
        let mut block = group.first / 64;
        let mut bits = self.words[block] & (u64::MAX << (group.first % 64));
        while remaining >= bits.count_ones() as usize {
            remaining -= bits.count_ones() as usize;
            block += 1;
            bits = self.words[block];
        }
        for _ in 0..remaining {
            bits &= bits - 1;
        }
        block * 64 + bits.trailing_zeros() as usize
    }
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(super) struct Boundaries {
    separator_starts: Bitmap,
    separator_ends: Bitmap,
    raw_len: usize,
    parts: usize,
}

impl Boundaries {
    pub(super) fn new(raw: &[u8]) -> Self {
        let mut starts = Bitmap::new(raw.len());
        let mut ends = Bitmap::new(raw.len());
        let mut i = 0;
        let mut parts = 1;
        while i < raw.len() {
            let start = i;
            let decoded = if raw[i] == b'%' && raw.len() - i >= 3 {
                hex(raw[i + 1])
                    .zip(hex(raw[i + 2]))
                    .map(|(a, b)| a * 16 + b)
            } else {
                None
            };
            let byte = decoded.unwrap_or(raw[i]);
            i += if decoded.is_some() { 3 } else { 1 };
            if byte == b'.'
                || (decoded.is_none() && matches!(byte, b'@' | b':' | b'/' | b'?' | b'#' | b'\\'))
            {
                starts.insert(start);
                ends.insert(i);
                parts += 1;
            }
        }
        starts.finish();
        ends.finish();
        Self {
            separator_starts: starts,
            separator_ends: ends,
            raw_len: raw.len(),
            parts,
        }
    }

    pub(super) fn start_at(&self, pos: usize) -> usize {
        if pos > self.raw_len {
            return usize::MAX;
        }
        if pos == 0 {
            0
        } else if self.separator_ends.contains(pos) {
            self.separator_ends.rank(pos) + 1
        } else {
            usize::MAX
        }
    }

    pub(super) fn end_at(&self, pos: usize) -> usize {
        if pos > self.raw_len {
            return usize::MAX;
        }
        if pos == self.raw_len {
            self.parts
        } else if self.separator_starts.contains(pos) {
            self.separator_starts.rank(pos) + 1
        } else {
            usize::MAX
        }
    }

    pub(super) fn part_at(&self, pos: usize) -> usize {
        if pos >= self.raw_len {
            return usize::MAX;
        }
        let finished =
            self.separator_ends.rank(pos) + usize::from(self.separator_ends.contains(pos));
        let begun =
            self.separator_starts.rank(pos) + usize::from(self.separator_starts.contains(pos));
        if begun != finished {
            usize::MAX
        } else {
            finished
        }
    }

    pub(super) fn part_start(&self, part: usize) -> usize {
        if part >= self.parts {
            usize::MAX
        } else if part == 0 {
            0
        } else {
            self.separator_ends.select(part - 1)
        }
    }

    pub(super) fn part_end(&self, part: usize) -> usize {
        if part >= self.parts {
            usize::MAX
        } else if part + 1 == self.parts {
            self.raw_len
        } else {
            self.separator_starts.select(part)
        }
    }

    pub(super) fn len(&self) -> usize {
        self.parts
    }
}
