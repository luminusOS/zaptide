//! Peak heap use while decoding a hostile embedded thumbnail. Kept in its own
//! test binary so the counting allocator sees no other test's allocations.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use zaptide::native_media::{ThumbnailResult, decode_inline_thumbnail};

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call forwards to `System` with the caller's layout and
// pointer unchanged; the counters do not affect the returned memory.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: same contract as the caller's.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let now = CURRENT.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(pointer, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: same contract as the caller's.
        let moved = unsafe { System.realloc(pointer, layout, new_size) };
        if !moved.is_null() {
            if new_size >= layout.size() {
                let grown = new_size - layout.size();
                let now = CURRENT.fetch_add(grown, Ordering::Relaxed) + grown;
                PEAK.fetch_max(now, Ordering::Relaxed);
            } else {
                CURRENT.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

struct Bits {
    bytes: Vec<u8>,
    used: u32,
}

impl Bits {
    fn push(&mut self, value: u32, count: u32) {
        for bit in 0..count {
            if self.used.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if value >> bit & 1 == 1 {
                *self.bytes.last_mut().unwrap() |= 1 << (self.used % 8);
            }
            self.used += 1;
        }
    }

    /// Fixed Huffman codes are stored most significant bit first.
    fn push_code(&mut self, code: u32, count: u32) {
        let reversed = code.reverse_bits() >> (32 - count);
        self.push(reversed, count);
    }
}

/// A zlib stream inflating to `copies * 258 + 1` zero bytes: one literal
/// zero, then copies of length 258 at distance 1.
fn zeros_zlib(copies: usize) -> Vec<u8> {
    let mut bits = Bits {
        bytes: Vec::new(),
        used: 0,
    };
    bits.push(1, 1);
    bits.push(1, 2);
    bits.push_code(0b0011_0000, 8);
    for _ in 0..copies {
        bits.push_code(0b1100_0101, 8);
        bits.push_code(0, 5);
    }
    bits.push_code(0, 7);
    let inflated = copies * 258 + 1;
    let adler = (((inflated % 65_521) as u32) << 16) | 1;
    let mut zlib = vec![0x78, 0x01];
    zlib.extend_from_slice(&bits.bytes);
    zlib.extend_from_slice(&adler.to_be_bytes());
    zlib
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut chunk = (data.len() as u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(kind);
    chunk.extend_from_slice(data);
    let crc = crc32(&chunk[4..]);
    chunk.extend_from_slice(&crc.to_be_bytes());
    chunk
}

/// A 1x1 PNG whose iCCP chunk holds about 200 KiB of deflated zeros that
/// inflate to over 30 MB.
fn png_with_huge_icc_profile() -> Vec<u8> {
    let mut one_pixel = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(1, 1, image::Rgb([1, 2, 3]))
        .write_to(&mut one_pixel, image::ImageFormat::Png)
        .unwrap();
    let one_pixel = one_pixel.into_inner();
    let after_ihdr = 8 + 8 + 13 + 4;
    let mut icc = b"icc\0\0".to_vec();
    icc.extend_from_slice(&zeros_zlib(126_000));
    let mut png = one_pixel[..after_ihdr].to_vec();
    png.extend_from_slice(&chunk(b"iCCP", &icc));
    png.extend_from_slice(&one_pixel[after_ihdr..]);
    png
}

#[test]
fn icc_profile_bombs_stay_under_the_decode_allocation_cap() {
    let png = png_with_huge_icc_profile();
    assert!(png.len() <= zaptide::native_media::MAX_INLINE_THUMBNAIL_BYTES);

    let before = CURRENT.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let started = Instant::now();
    let result = decode_inline_thumbnail(&png);
    let elapsed = started.elapsed();
    let peak = PEAK.load(Ordering::Relaxed) - before;

    let cap = zaptide::native_media::MAX_INLINE_THUMBNAIL_PIXELS as usize * 4;
    eprintln!("result {result:?}, peak {peak} bytes, cap {cap} bytes, {elapsed:?}");
    // png drops a profile that exceeds its byte limit and still decodes the
    // pixels, so the one pixel may come back; what matters is the bound.
    assert!(match result {
        ThumbnailResult::Placeholder(_) => true,
        ThumbnailResult::Image(image) => (image.width, image.height) == (1, 1),
    });
    let decoder_buffers = 1024 * 1024;
    assert!(peak < cap + decoder_buffers, "peak {peak} bytes");
    assert!(elapsed < Duration::from_secs(1), "took {elapsed:?}");
}
