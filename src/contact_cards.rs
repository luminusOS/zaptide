//! Bounded vCard metadata for native contact cards and sharing.
//!
//! The original vCard is kept for export and WhatsApp; only names and telephone
//! fields are interpreted here. Remote photos and other resources are never loaded.

use std::fmt;

pub const MAX_VCARD_BYTES: usize = 256 * 1024;
pub const MAX_CONTACTS: usize = 32;
const MAX_PHONES: usize = 8;

#[derive(Clone, PartialEq, Eq)]
pub struct ContactCard {
    pub name: String,
    pub phones: Vec<ContactPhone>,
    pub vcard: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ContactPhone {
    pub number: String,
    /// Only an explicit, valid `waid` parameter authorizes the Message action.
    pub whatsapp_id: Option<String>,
}

impl fmt::Debug for ContactCard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContactCard")
            .field("phone_count", &self.phones.len())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ContactPhone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ContactPhone(..)")
    }
}

impl ContactCard {
    /// Builds a minimal vCard from a synced, saved phone-number contact.
    pub fn from_saved(id: &str, name: &str) -> Option<Self> {
        let digits = crate::model::phone_of(id)?;
        let digits = whatsapp_digits(digits)?;
        let name = display_text(name, 200);
        if name.is_empty() {
            return None;
        }
        let vcard = format!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:{}\r\nTEL;TYPE=CELL;waid={digits}:+{digits}\r\nEND:VCARD\r\n",
            escape(&name)
        );
        Some(Self {
            name,
            phones: vec![ContactPhone {
                number: format!("+{digits}"),
                whatsapp_id: Some(id.to_owned()),
            }],
            vcard,
        })
    }
}

/// Parses complete vCards, including bundles and folded UTF-8 lines.
/// Invalid/incomplete records and oversized input are rejected, never exported as cards.
pub fn parse(vcards: &str) -> Vec<ContactCard> {
    if vcards.len() > MAX_VCARD_BYTES || vcards.contains('\0') {
        return Vec::new();
    }
    let mut cards = Vec::new();
    let mut record = None::<Vec<&str>>;
    for line in vcards.trim_start_matches('\u{feff}').lines() {
        if line.trim().eq_ignore_ascii_case("BEGIN:VCARD") {
            record = Some(vec![line]);
        } else if let Some(lines) = record.as_mut() {
            lines.push(line);
            if line.trim().eq_ignore_ascii_case("END:VCARD") {
                if let Some(card) = parse_record(lines) {
                    cards.push(card);
                }
                record = None;
                if cards.len() == MAX_CONTACTS {
                    break;
                }
            }
        }
    }
    cards
}

fn parse_record(lines: &[&str]) -> Option<ContactCard> {
    let mut unfolded = Vec::<String>::new();
    for line in lines {
        if let Some(previous) = unfolded.last_mut()
            && previous.ends_with('=')
            && previous.split_once(':').is_some_and(|(key, _)| {
                key.to_ascii_uppercase()
                    .contains("ENCODING=QUOTED-PRINTABLE")
            })
            && !line.trim().eq_ignore_ascii_case("END:VCARD")
        {
            previous.pop();
            previous.push_str(line.trim_start_matches([' ', '\t']));
        } else if let Some(rest) = line.strip_prefix([' ', '\t'])
            && let Some(previous) = unfolded.last_mut()
        {
            previous.push_str(rest);
        } else {
            unfolded.push((*line).to_owned());
        }
    }
    let mut name = String::new();
    let mut structured_name = String::new();
    let mut phones = Vec::<ContactPhone>::new();
    let mut version = false;
    for line in unfolded {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let mut fields = key.split(';');
        let property = fields.next()?.rsplit('.').next()?;
        if property.eq_ignore_ascii_case("VERSION") {
            version = matches!(value.trim(), "2.1" | "3.0" | "4.0");
        } else if property.eq_ignore_ascii_case("FN") {
            name = display_text(&unescape(&decode_text(key, value)?), 200);
        } else if property.eq_ignore_ascii_case("N") {
            structured_name =
                display_text(&structured_parts(&decode_text(key, value)?).join(" "), 200);
        } else if property.eq_ignore_ascii_case("TEL") && phones.len() < MAX_PHONES {
            let value = decode_text(key, value)?;
            let number = display_text(value.strip_prefix("tel:").unwrap_or(&value), 64);
            if number.is_empty() {
                continue;
            }
            let whatsapp_id = fields.find_map(|field| {
                let (parameter, digits) = field.split_once('=')?;
                parameter.eq_ignore_ascii_case("waid").then_some(())?;
                whatsapp_digits(digits.trim_matches('"'))
                    .map(|digits| format!("{digits}@s.whatsapp.net"))
            });
            if !phones.iter().any(|phone| phone.number == number) {
                phones.push(ContactPhone {
                    number,
                    whatsapp_id,
                });
            }
        }
    }
    if !version {
        return None;
    }
    if name.is_empty() {
        name = structured_name;
    }
    if name.is_empty() {
        name = phones
            .first()
            .map_or_else(|| "Contact".into(), |p| p.number.clone());
    }
    Some(ContactCard {
        name,
        phones,
        vcard: format!("{}\r\n", lines.join("\r\n")),
    })
}

fn whatsapp_digits(value: &str) -> Option<&str> {
    ((6..=15).contains(&value.len())
        && !value.starts_with('0')
        && value.bytes().all(|c| c.is_ascii_digit()))
    .then_some(value)
}

fn display_text(value: &str, limit: usize) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
}

/// Decode only the text properties used by the UI. Unrecognized transfer
/// encodings/charsets reject the card instead of displaying misleading data.
fn decode_text(key: &str, value: &str) -> Option<String> {
    let mut encoding = "";
    let mut charset = "UTF-8";
    for parameter in key.split(';').skip(1) {
        if let Some((name, value)) = parameter.split_once('=') {
            if name.eq_ignore_ascii_case("ENCODING") {
                encoding = value.trim_matches('"');
            }
            if name.eq_ignore_ascii_case("CHARSET") {
                charset = value.trim_matches('"');
            }
        }
    }
    let bytes = if encoding.eq_ignore_ascii_case("QUOTED-PRINTABLE") {
        let mut bytes = Vec::with_capacity(value.len());
        let mut input = value.bytes();
        while let Some(byte) = input.next() {
            if byte == b'=' {
                let high = char::from(input.next()?).to_digit(16)?;
                let low = char::from(input.next()?).to_digit(16)?;
                bytes.push((high * 16 + low) as u8);
            } else {
                bytes.push(byte);
            }
        }
        bytes
    } else if encoding.is_empty() || encoding.eq_ignore_ascii_case("8BIT") {
        value.as_bytes().to_vec()
    } else {
        return None;
    };
    if charset.eq_ignore_ascii_case("UTF-8") || charset.eq_ignore_ascii_case("UTF8") {
        String::from_utf8(bytes).ok()
    } else if charset.eq_ignore_ascii_case("US-ASCII") {
        String::from_utf8(bytes).ok().filter(|text| text.is_ascii())
    } else if charset.eq_ignore_ascii_case("ISO-8859-1") {
        Some(bytes.into_iter().map(char::from).collect())
    } else {
        None
    }
}

fn structured_parts(value: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut escaped = false;
    let mut start = 0;
    for (offset, c) in value.char_indices() {
        if c == ';' && !escaped {
            fields.push(unescape(&value[start..offset]));
            start = offset + 1;
        }
        escaped = c == '\\' && !escaped;
    }
    fields.push(unescape(&value[start..]));
    fields
}

fn unescape(value: &str) -> String {
    let mut text = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => text.push(' '),
                Some(c) => text.push(c),
                None => text.push('\\'),
            }
        } else {
            text.push(c);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_and_structured_names_preserve_international_and_escaped_text() {
        let encoded = "\u{feff}BEGIN:VCARD\r\nVERSION:2.1\r\nFN;CHARSET=UTF-8;ENCODING=QUOTED-PRINTABLE:=C3=89va=20=\r\nExample\r\nEND:VCARD\r\n";
        let cards = parse(encoded);
        assert_eq!(cards[0].name, "Éva Example");
        assert_eq!(parse(&cards[0].vcard)[0].name, "Éva Example");
        let structured = r"BEGIN:VCARD
VERSION:2.1
N:Doe\;Jr.;Jane;\\Middle;;
END:VCARD";
        assert_eq!(parse(structured)[0].name, "Doe;Jr. Jane \\Middle");
        let latin = "BEGIN:VCARD\nVERSION:2.1\nFN;CHARSET=ISO-8859-1;ENCODING=QUOTED-PRINTABLE:=C9va\nEND:VCARD";
        assert_eq!(parse(latin)[0].name, "Éva");
        assert!(parse("BEGIN:VCARD\nVERSION:2.1\nFN;ENCODING=B:bmFtZQ==\nEND:VCARD").is_empty());
    }

    #[test]
    fn saved_contacts_round_trip_without_property_injection() {
        let card =
            ContactCard::from_saved("15555550123@s.whatsapp.net", "Ada, Example; Jr.\nTEL:bad")
                .unwrap();
        let parsed = parse(&card.vcard);
        assert_eq!(parsed, vec![card]);
        assert!(ContactCard::from_saved("123456@lid", "Ada").is_none());
        assert!(ContactCard::from_saved("@s.whatsapp.net", "Ada").is_none());
        assert!(!format!("{parsed:?}").contains("Ada"));
        assert!(!format!("{parsed:?}").contains("15555550123"));
    }

    #[test]
    fn bundles_unfold_and_keep_distinct_numbers_and_explicit_whatsapp_ids() {
        let input = "BEGIN:VCARD\nVERSION:3.0\nFN:Long\\, Na\n me\nitem1.TEL;TYPE=CELL;WAID=15555550123:+1 555 555 0123\nTEL:555-0124\nEND:VCARD\nBEGIN:VCARD\nVERSION:4.0\nFN:Other\nTEL;VALUE=uri:tel:+15555550125\nEND:VCARD";
        let cards = parse(input);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].name, "Long, Name");
        assert_eq!(cards[0].phones.len(), 2);
        assert_eq!(
            cards[0].phones[0].whatsapp_id.as_deref(),
            Some("15555550123@s.whatsapp.net")
        );
        assert!(cards[0].phones[1].whatsapp_id.is_none());
        assert_eq!(cards[1].phones[0].number, "+15555550125");
        assert!(cards[1].phones[0].whatsapp_id.is_none());
        assert_eq!(parse(&cards[0].vcard), vec![cards[0].clone()]);
    }

    #[test]
    fn rejects_incomplete_oversized_and_invalid_identifiers() {
        assert!(parse("BEGIN:VCARD\nVERSION:3.0\nFN:Incomplete").is_empty());
        assert!(parse("BEGIN:VCARD\nFN:No version\nEND:VCARD").is_empty());
        assert!(parse(&"x".repeat(MAX_VCARD_BYTES + 1)).is_empty());
        let card = parse(
            "BEGIN:VCARD\nVERSION:3.0\nTEL;waid=123@g.us:123\nTEL;waid=0123456789:456\nEND:VCARD",
        );
        assert!(card[0].phones.iter().all(|p| p.whatsapp_id.is_none()));
    }

    #[test]
    fn bounds_card_phone_and_name_counts() {
        let one = format!(
            "BEGIN:VCARD\nVERSION:3.0\nFN:{}\n{}END:VCARD\n",
            "é".repeat(500),
            (0..20).map(|i| format!("TEL:{i}\n")).collect::<String>()
        );
        let cards = parse(&one.repeat(40));
        assert_eq!(cards.len(), MAX_CONTACTS);
        assert_eq!(cards[0].name.chars().count(), 200);
        assert_eq!(cards[0].phones.len(), MAX_PHONES);
    }
}
