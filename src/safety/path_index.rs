#[derive(Clone, Copy, Debug, Default)]
pub struct Suffix {
    pub skip: usize,
    pub segments: usize,
    pub encoded_bytes: u128,
}

impl Suffix {
    pub fn path_len(self) -> u128 {
        1 + self.encoded_bytes + self.segments.saturating_sub(1) as u128
    }
}
