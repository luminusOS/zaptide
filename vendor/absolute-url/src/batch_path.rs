use alloc::vec;
use alloc::vec::Vec;
use percent_encoding::{percent_encode, AsciiSet};

use crate::parser::{batch_dot_segment, PATH};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Suffix {
    pub skip: usize,
    pub segments: usize,
    pub encoded: u128,
    pub first_empty: bool,
}

impl Suffix {
    pub fn len(self) -> u128 {
        1 + self.encoded + self.segments.saturating_sub(1) as u128
    }

    pub fn starts_double_slash(self) -> bool {
        self.segments >= 2 && self.first_empty
    }
}

pub(crate) fn encoded_byte(b: u8, set: &'static AsciiSet) -> u128 {
    percent_encode(&[b], set).map(str::len).sum::<usize>() as u128
}

pub(crate) fn suffixes(text: &str, _steps: &mut usize) -> Vec<Suffix> {
    let b = text.as_bytes();
    let n = b.len();
    let mut result = vec![Suffix::default(); n + 1];
    let mut end = n;
    while end > 0 {
        let mut begin = end;
        while begin > 0 && !matches!(b[begin - 1], b'?' | b'#') {
            begin -= 1;
            #[cfg(feature = "batch-stats")]
            {
                *_steps += 1;
            }
        }
        let mut segment_end = end;
        let mut pos = end;
        while pos > begin && b[pos - 1] != b'/' {
            pos -= 1;
            #[cfg(feature = "batch-stats")]
            {
                *_steps += 1;
            }
        }
        let terminal_dot = batch_dot_segment(&text[pos..end]) != 0;
        let mut state = Suffix {
            segments: usize::from(terminal_dot),
            first_empty: terminal_dot,
            ..Suffix::default()
        };
        while pos > begin {
            let slash = pos - 1;
            let segment = &text[pos..segment_end];
            #[cfg(feature = "batch-stats")]
            {
                *_steps += segment.len() + 1;
            }
            match batch_dot_segment(segment) {
                1 => {}
                2 => state.skip += 1,
                _ if state.skip > 0 => state.skip -= 1,
                _ => {
                    state.segments += 1;
                    state.first_empty = segment.is_empty();
                    state.encoded += segment
                        .bytes()
                        .map(|byte| encoded_byte(byte, PATH))
                        .sum::<u128>();
                }
            }
            result[slash] = state;
            segment_end = slash;
            pos = slash;
            while pos > begin && b[pos - 1] != b'/' {
                pos -= 1;
                #[cfg(feature = "batch-stats")]
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
    result
}

pub(crate) struct TailIndex {
    pub empty: Vec<Suffix>,
    pub ordinary: Vec<Suffix>,
    pub double_dot: Vec<Suffix>,
    pub last_slash: Vec<usize>,
    pub encoded: Vec<u128>,
}

pub(crate) fn terminal_suffixes(text: &str, _steps: &mut usize) -> TailIndex {
    let b = text.as_bytes();
    let n = b.len();
    let mut empty = vec![Suffix::default(); n + 1];
    let mut ordinary = vec![Suffix::default(); n + 1];
    let mut double_dot = vec![Suffix::default(); n + 1];
    let mut last_slash = vec![n; n + 1];
    let mut encoded = vec![0u128; n + 1];
    for i in 0..n {
        last_slash[i + 1] = if b[i] == b'/' { i } else { last_slash[i] };
        encoded[i + 1] = encoded[i] + encoded_byte(b[i], PATH);
        #[cfg(feature = "batch-stats")]
        {
            *_steps += 1;
        }
    }
    let mut end = n;
    while end > 0 {
        let mut begin = end;
        while begin > 0 && !matches!(b[begin - 1], b'?' | b'#') {
            begin -= 1;
            #[cfg(feature = "batch-stats")]
            {
                *_steps += 1;
            }
        }
        let mut pos = end;
        while pos > begin && b[pos - 1] != b'/' {
            pos -= 1;
            #[cfg(feature = "batch-stats")]
            {
                *_steps += 1;
            }
        }
        if pos > begin {
            let mut states = [
                Suffix {
                    segments: 1,
                    first_empty: true,
                    ..Suffix::default()
                },
                Suffix {
                    segments: 1,
                    first_empty: false,
                    ..Suffix::default()
                },
                Suffix {
                    segments: 1,
                    first_empty: true,
                    skip: 1,
                    ..Suffix::default()
                },
            ];
            loop {
                let slash = pos - 1;
                empty[slash] = states[0];
                ordinary[slash] = states[1];
                double_dot[slash] = states[2];
                let segment_end = slash;
                pos = slash;
                while pos > begin && b[pos - 1] != b'/' {
                    pos -= 1;
                    #[cfg(feature = "batch-stats")]
                    {
                        *_steps += 1;
                    }
                }
                if pos == begin {
                    break;
                }
                let segment = &text[pos..segment_end];
                #[cfg(feature = "batch-stats")]
                {
                    *_steps += segment.len() + 1;
                }
                let dot = batch_dot_segment(segment);
                let len = if dot == 0 {
                    segment.bytes().map(|c| encoded_byte(c, PATH)).sum()
                } else {
                    0
                };
                for state in &mut states {
                    match dot {
                        1 => {}
                        2 => state.skip += 1,
                        _ if state.skip > 0 => state.skip -= 1,
                        _ => {
                            state.segments += 1;
                            state.first_empty = segment.is_empty();
                            state.encoded += len;
                        }
                    }
                }
            }
        }
        if begin == 0 {
            break;
        }
        end = begin - 1;
    }
    TailIndex {
        empty,
        ordinary,
        double_dot,
        last_slash,
        encoded,
    }
}

impl TailIndex {
    pub fn range(&self, text: &str, start: usize, end: usize) -> Suffix {
        let slash = self.last_slash[end];
        debug_assert!(slash >= start);
        let segment = &text[slash + 1..end];
        match batch_dot_segment(segment) {
            1 => self.empty[start],
            2 => self.double_dot[start],
            _ if segment.is_empty() => self.empty[start],
            _ => {
                let mut state = self.ordinary[start];
                state.encoded += self.encoded[end] - self.encoded[slash + 1];
                state
            }
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Pending {
    len: usize,
    reverse: [u8; 6],
    encoded: u128,
}
impl Pending {
    fn add(&mut self, b: u8) {
        if self.len < 6 {
            self.reverse[self.len] = b;
        }
        self.len += 1;
        self.encoded += encoded_byte(b, PATH);
    }
    fn dot(self) -> u8 {
        if self.len > 6 {
            return 0;
        }
        let mut data = [0; 6];
        for (i, x) in data.iter_mut().enumerate().take(self.len) {
            *x = self.reverse[self.len - 1 - i];
        }
        let s = &data[..self.len];
        if s == b"." || s.eq_ignore_ascii_case(b"%2e") {
            1
        } else if s == b".."
            || s.eq_ignore_ascii_case(b"%2e%2e")
            || s.eq_ignore_ascii_case(b".%2e")
            || s.eq_ignore_ascii_case(b"%2e.")
        {
            2
        } else {
            0
        }
    }
}
#[derive(Clone, Copy, Default)]
struct PathState {
    normal: Suffix,
    empty: Suffix,
    ordinary: Suffix,
    double_dot: Suffix,
    pending: Pending,
    seen_slash: bool,
}
fn step(state: &mut Suffix, p: Pending) {
    match p.dot() {
        1 => {}
        2 => state.skip += 1,
        _ if state.skip > 0 => state.skip -= 1,
        _ => {
            state.segments += 1;
            state.first_empty = p.len == 0;
            state.encoded += p.encoded;
        }
    }
}
impl PathState {
    fn advance(&mut self, byte: u8) {
        if matches!(byte, b'?' | b'#') {
            *self = Self::default();
            return;
        }
        if byte != b'/' {
            self.pending.add(byte);
            return;
        }
        if !self.seen_slash {
            let terminal_dot = self.pending.dot() != 0;
            self.normal.segments = usize::from(terminal_dot);
            self.normal.first_empty = terminal_dot;
            self.empty = Suffix {
                segments: 1,
                first_empty: true,
                ..Suffix::default()
            };
            self.ordinary = Suffix {
                segments: 1,
                ..Suffix::default()
            };
            self.double_dot = Suffix {
                skip: 1,
                segments: 1,
                first_empty: true,
                ..Suffix::default()
            };
            self.seen_slash = true;
        } else {
            step(&mut self.empty, self.pending);
            step(&mut self.ordinary, self.pending);
            step(&mut self.double_dot, self.pending);
        }
        step(&mut self.normal, self.pending);
        self.pending = Pending::default();
    }
}
#[derive(Clone, Copy, Default)]
struct PathBlock {
    state: PathState,
    previous_slash: usize,
    encoded: u128,
}
pub(crate) struct CompactPath<'a> {
    word: &'a str,
    blocks: Vec<PathBlock>,
}
impl<'a> CompactPath<'a> {
    pub fn allocated(&self) -> usize {
        self.blocks.capacity() * core::mem::size_of::<PathBlock>()
    }
    pub fn new(word: &'a str, _steps: &mut usize) -> Self {
        const B: usize = 64;
        let n = word.len();
        let mut blocks = vec![PathBlock::default(); n.div_ceil(B) + 1];
        let mut previous = n;
        let mut encoded = 0;
        for (i, &b) in word.as_bytes().iter().enumerate() {
            if i % B == 0 {
                blocks[i / B].previous_slash = previous;
                blocks[i / B].encoded = encoded;
            }
            if b == b'/' {
                previous = i;
            }
            encoded += encoded_byte(b, PATH);
            #[cfg(feature = "batch-stats")]
            {
                *_steps += 1;
            }
        }
        if n % B == 0 {
            blocks[n / B].previous_slash = previous;
            blocks[n / B].encoded = encoded;
        }
        let mut state = PathState::default();
        blocks[n.div_ceil(B)].state = state;
        for i in (0..n).rev() {
            state.advance(word.as_bytes()[i]);
            if i % B == 0 {
                blocks[i / B].state = state;
            }
            #[cfg(feature = "batch-stats")]
            {
                *_steps += 1;
            }
        }
        Self { word, blocks }
    }
    fn state_at(&self, start: usize) -> PathState {
        const B: usize = 64;
        let boundary = start.div_ceil(B);
        let mut state = self.blocks[boundary].state;
        let end = (boundary * B).min(self.word.len());
        for i in (start..end).rev() {
            state.advance(self.word.as_bytes()[i]);
        }
        state
    }
    pub fn suffix(&self, start: usize) -> Suffix {
        self.state_at(start).normal
    }
    fn prefix(&self, pos: usize) -> u128 {
        const B: usize = 64;
        let base = pos / B;
        let mut result = self.blocks[base].encoded;
        for &b in &self.word.as_bytes()[base * B..pos] {
            result += encoded_byte(b, PATH)
        }
        result
    }
    fn previous_slash(&self, end: usize) -> usize {
        const B: usize = 64;
        let base = end / B;
        let mut result = self.blocks[base].previous_slash;
        for i in base * B..end {
            if self.word.as_bytes()[i] == b'/' {
                result = i
            }
        }
        result
    }
    pub fn range(&self, start: usize, end: usize) -> Suffix {
        let slash = self.previous_slash(end);
        debug_assert!(slash >= start);
        let x = &self.word[slash + 1..end];
        let state = self.state_at(start);
        match batch_dot_segment(x) {
            1 => state.empty,
            2 => state.double_dot,
            _ if x.is_empty() => state.empty,
            _ => {
                let mut s = state.ordinary;
                s.encoded += self.prefix(end) - self.prefix(slash + 1);
                s
            }
        }
    }
}

pub(crate) fn verify_compact(word: &str) -> bool {
    let mut work = 0;
    let original = suffixes(word, &mut work);
    let tail = terminal_suffixes(word, &mut work);
    let compact = CompactPath::new(word, &mut work);
    for (start, b) in word.bytes().enumerate() {
        if b != b'/' {
            continue;
        }
        if original[start] != compact.suffix(start) {
            return false;
        }
        let end = word[start..]
            .find(['?', '#'])
            .map_or(word.len(), |i| start + i);
        let Some(last) = word[start..end].rfind('/') else {
            return false;
        };
        let last = start + last;
        for e in last + 1..=end {
            if word.is_char_boundary(e) && tail.range(word, start, e) != compact.range(start, e) {
                return false;
            }
        }
    }
    true
}
