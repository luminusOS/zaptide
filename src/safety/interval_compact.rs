use crate::safety::path_index::Suffix;

const BLOCK: usize = 256;

pub struct Bits {
    words: Vec<u64>,
    next: Vec<usize>,
    prev: Vec<usize>,
    len: usize,
}

impl Bits {
    pub fn new(len: usize) -> Self {
        Self {
            words: vec![0; len.div_ceil(64)],
            next: Vec::new(),
            prev: Vec::new(),
            len,
        }
    }
    pub fn set(&mut self, at: usize) {
        self.words[at / 64] |= 1 << (at % 64);
    }
    pub fn finish(&mut self) {
        let blocks = self.len.div_ceil(BLOCK);
        self.next = vec![self.len; blocks + 1];
        self.prev = vec![self.len; blocks + 1];
        for block in (0..blocks).rev() {
            self.next[block] = self.next[block + 1];
            for word in (block * 4..((block + 1) * 4).min(self.words.len())).rev() {
                if self.words[word] != 0 {
                    self.next[block] = word * 64 + self.words[word].trailing_zeros() as usize;
                }
            }
        }
        let mut last = self.len;
        for block in 0..blocks {
            self.prev[block] = last;
            for word in block * 4..((block + 1) * 4).min(self.words.len()) {
                if self.words[word] != 0 {
                    last = word * 64 + 63 - self.words[word].leading_zeros() as usize;
                }
            }
        }
        self.prev[blocks] = last;
    }
    pub fn contains(&self, at: usize) -> bool {
        at < self.len && self.words[at / 64] & (1 << (at % 64)) != 0
    }
    pub fn next(&self, at: usize) -> usize {
        if at >= self.len {
            return self.len;
        }
        let block = at / BLOCK;
        let first = at / 64;
        for word in first..((block + 1) * 4).min(self.words.len()) {
            let mask = if word == first {
                u64::MAX << (at % 64)
            } else {
                u64::MAX
            };
            let v = self.words[word] & mask;
            if v != 0 {
                return word * 64 + v.trailing_zeros() as usize;
            }
        }
        self.next[block + 1]
    }
    pub fn previous(&self, end: usize) -> Option<usize> {
        if end == 0 {
            return None;
        }
        let at = end - 1;
        let block = at / BLOCK;
        let first = at / 64;
        for word in (block * 4..=first).rev() {
            let mask = if word == first {
                u64::MAX >> (63 - at % 64)
            } else {
                u64::MAX
            };
            let v = self.words[word] & mask;
            if v != 0 {
                return Some(word * 64 + 63 - v.leading_zeros() as usize);
            }
        }
        (self.prev[block] != self.len).then_some(self.prev[block])
    }
}

fn size(b: u8, path: bool) -> u128 {
    if matches!(b, b'\t' | b'\r' | b'\n') {
        0
    } else if b <= 0x20
        || b >= 0x7f
        || matches!(b, b'"' | b'<' | b'>')
        || (path && matches!(b, b'`' | b'#' | b'?' | b'{' | b'}'))
    {
        3
    } else {
        1
    }
}

pub struct Prefix {
    checkpoints: Vec<u128>,
    path: bool,
}
impl Prefix {
    pub fn new(bytes: &[u8], path: bool) -> Self {
        let mut checkpoints = Vec::with_capacity(bytes.len() / BLOCK + 1);
        let mut sum = 0;
        checkpoints.push(0);
        for (i, &b) in bytes.iter().enumerate() {
            sum += size(b, path);
            if (i + 1) % BLOCK == 0 {
                checkpoints.push(sum);
            }
        }
        Self { checkpoints, path }
    }
    pub fn at(&self, bytes: &[u8], at: usize) -> u128 {
        let block = at / BLOCK;
        self.checkpoints[block]
            + bytes[block * BLOCK..at]
                .iter()
                .map(|&b| size(b, self.path))
                .sum::<u128>()
    }
}

fn dot(s: &[u8]) -> u8 {
    if s == b"." || s.eq_ignore_ascii_case(b"%2e") {
        1
    } else if s == b".."
        || s.eq_ignore_ascii_case(b"%2e%2e")
        || s.eq_ignore_ascii_case(b"%2e.")
        || s.eq_ignore_ascii_case(b".%2e")
    {
        2
    } else {
        0
    }
}

fn apply(state: &mut Suffix, segment: &[u8], encoded: u128) {
    match dot(segment) {
        1 => {}
        2 => state.skip += 1,
        _ if state.skip > 0 => state.skip -= 1,
        _ => {
            state.segments += 1;
            state.encoded_bytes += encoded;
        }
    }
}

pub struct Paths {
    checkpoints: Vec<Option<(usize, Suffix)>>,
    slash: Bits,
    prefix: Prefix,
}
impl Paths {
    pub fn new(bytes: &[u8], _steps: &mut usize) -> Self {
        let n = bytes.len();
        let mut slash = Bits::new(n);
        let mut checkpoints = vec![None; n.div_ceil(BLOCK)];
        let prefix = Prefix::new(bytes, true);
        let mut end = n;
        while end > 0 {
            let mut begin = end;
            while begin > 0 && !matches!(bytes[begin - 1], b'?' | b'#') {
                begin -= 1;
                #[cfg(test)]
                {
                    *_steps += 1;
                }
            }
            let mut segment_end = end;
            let mut pos = end;
            while pos > begin && bytes[pos - 1] != b'/' {
                pos -= 1;
                #[cfg(test)]
                {
                    *_steps += 1;
                }
            }
            let mut state = Suffix {
                skip: 0,
                segments: usize::from(dot(&bytes[pos..end]) != 0),
                encoded_bytes: 0,
            };
            while pos > begin {
                let s = pos - 1;
                let segment = &bytes[pos..segment_end];
                #[cfg(test)]
                {
                    *_steps += segment.len() + 1;
                }
                let encoded = segment.iter().map(|&b| size(b, true)).sum();
                apply(&mut state, segment, encoded);
                slash.set(s);
                checkpoints[s / BLOCK] = Some((s, state));
                segment_end = s;
                pos = s;
                while pos > begin && bytes[pos - 1] != b'/' {
                    pos -= 1;
                    #[cfg(test)]
                    {
                        *_steps += 1;
                    }
                }
            }
            if begin == 0 {
                break;
            }
            end = begin - 1;
        }
        slash.finish();
        Self {
            checkpoints,
            slash,
            prefix,
        }
    }
    pub fn at(&self, bytes: &[u8], start: usize, query: &Bits, fragment: &Bits) -> Suffix {
        if !self.slash.contains(start) {
            return Suffix::default();
        }
        let mut pending = Vec::new();
        let mut pos = start;
        let mut state;
        loop {
            if let Some((first, checkpoint)) = self.checkpoints[pos / BLOCK]
                && pos == first
            {
                state = checkpoint;
                break;
            }
            let next = self.slash.next(pos + 1);
            let boundary = query.next(pos + 1).min(fragment.next(pos + 1));
            if boundary <= next {
                let segment = &bytes[pos + 1..boundary];
                state = Suffix {
                    skip: 0,
                    segments: usize::from(dot(segment) != 0),
                    encoded_bytes: 0,
                };
                let encoded = self.prefix.at(bytes, boundary) - self.prefix.at(bytes, pos + 1);
                apply(&mut state, segment, encoded);
                break;
            }
            pending.push((pos + 1, next));
            pos = next;
        }
        for (begin, end) in pending.into_iter().rev() {
            let encoded = self.prefix.at(bytes, end) - self.prefix.at(bytes, begin);
            apply(&mut state, &bytes[begin..end], encoded);
        }
        state
    }
}
