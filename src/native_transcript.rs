//! Plain-text transcript of the loaded conversation for the clipboard.

/// A message body. `header` must use the phone sharing form,
/// `[time, date] Name: `.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptRow {
    pub header: String,
    pub text: String,
}

/// One message copies as plain text; several keep a sharing header each.
/// Emoji remain original Unicode text, never placeholders.
pub fn copied_text(rows: &[TranscriptRow]) -> Option<String> {
    match rows {
        [] => None,
        [row] => Some(row.text.clone()),
        rows => Some(
            rows.iter()
                .map(|row| format!("{}{}", row.header, row.text))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(header: &str, text: &str) -> TranscriptRow {
        TranscriptRow {
            header: header.into(),
            text: text.into(),
        }
    }

    #[test]
    fn copies_one_row_without_a_header_and_preserves_emoji() {
        assert_eq!(
            copied_text(&[row("[09:00, 1/2/2026] Ada: ", "hello 🎉")]).as_deref(),
            Some("hello 🎉")
        );
    }

    #[test]
    fn copies_rows_with_each_phone_header() {
        assert_eq!(
            copied_text(&[
                row("[09:00, 1/2/2026] Ada: ", "first"),
                row("[09:01, 1/2/2026] You: ", "second"),
            ])
            .as_deref(),
            Some("[09:00, 1/2/2026] Ada: first\n[09:01, 1/2/2026] You: second")
        );
        assert_eq!(copied_text(&[]), None);
    }
}
