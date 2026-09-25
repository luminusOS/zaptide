use serde::{Deserialize, Serialize};

const STICKER_TAG: u16 = 0x5741;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct StickerInfo {
    #[serde(
        rename = "sticker-pack-id",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub pack_id: String,
    #[serde(
        rename = "sticker-pack-name",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub pack_name: String,
    #[serde(
        rename = "sticker-pack-publisher",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub publisher: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emojis: Vec<String>,
}

struct Chunk<'a> {
    id: [u8; 4],
    data: &'a [u8],
}

fn chunks(bytes: &[u8]) -> Option<Vec<Chunk<'_>>> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return None;
    }
    let mut chunks = Vec::new();
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id: [u8; 4] = bytes[pos..pos + 4].try_into().ok()?;
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().ok()?) as usize;
        let start = pos + 8;
        let end = start.checked_add(size)?;
        if end > bytes.len() {
            return None;
        }
        chunks.push(Chunk {
            id,
            data: &bytes[start..end],
        });
        pos = end + (size & 1);
    }
    Some(chunks)
}

pub fn read(bytes: &[u8]) -> Option<StickerInfo> {
    let exif = chunks(bytes)?
        .into_iter()
        .find(|chunk| &chunk.id == b"EXIF")?
        .data;
    tiff_entry(exif, STICKER_TAG)
        .and_then(json_object)
        .or_else(|| json_object(exif))
}

fn json_object(bytes: &[u8]) -> Option<StickerInfo> {
    let start = bytes.iter().position(|byte| *byte == b'{')?;
    let end = bytes.iter().rposition(|byte| *byte == b'}')?;
    serde_json::from_slice(bytes.get(start..=end)?).ok()
}

pub fn emojis(bytes: &[u8]) -> Vec<String> {
    read(bytes)
        .map(|info| info.emojis)
        .unwrap_or_default()
        .into_iter()
        .map(|emoji| emoji.trim().to_owned())
        .filter(|emoji| !emoji.is_empty())
        .collect()
}

fn tiff_entry(tiff: &[u8], tag: u16) -> Option<&[u8]> {
    let little = match tiff.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let bytes: [u8; 2] = tiff.get(at..at + 2)?.try_into().ok()?;
        Some(if little {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let bytes: [u8; 4] = tiff.get(at..at + 4)?.try_into().ok()?;
        Some(if little {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    };
    let ifd = u32_at(4)? as usize;
    let count = u16_at(ifd)? as usize;
    for index in 0..count {
        let entry = ifd + 2 + index * 12;
        if u16_at(entry)? == tag {
            let length = u32_at(entry + 4)? as usize;
            if length <= 4 {
                return tiff.get(entry + 8..entry + 8 + length);
            }
            let offset = u32_at(entry + 8)? as usize;
            return tiff.get(offset..offset.checked_add(length)?);
        }
    }
    None
}
