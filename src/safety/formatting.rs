use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FormattingKind {
    Bold,
    Italic,
    Strike,
    Code,
    Monospace,
    Quote,
    Bullet,
    Numbered,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FormattingSpan {
    pub(crate) kind: FormattingKind,
    pub(crate) range: Range<usize>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct BlockRanges {
    pub(crate) marker: Range<usize>,
    pub(crate) content: Range<usize>,
}

pub(crate) fn block_ranges(text: &str, span: &FormattingSpan) -> Option<BlockRanges> {
    let line = text.get(span.range.clone())?;
    let mut chars = line.char_indices().filter(|&(_, c)| visible(c));
    let (_, first) = chars.next()?;
    let marker_end = match span.kind {
        FormattingKind::Quote if first == '>' => chars.next().filter(|&(_, c)| c == ' ')?.0 + 1,
        FormattingKind::Bullet if matches!(first, '-' | '*') => {
            chars.next().filter(|&(_, c)| c == ' ')?.0 + 1
        }
        FormattingKind::Numbered if first.is_ascii_digit() => {
            let mut next = chars.next()?;
            while next.1.is_ascii_digit() {
                next = chars.next()?;
            }
            if next.1 != '.' {
                return None;
            }
            chars.next().filter(|&(_, c)| c == ' ')?.0 + 1
        }
        _ => return None,
    };
    let content_end = span.range.end - usize::from(line.ends_with('\r'));
    let marker_len = line[marker_end..]
        .char_indices()
        .find(|&(_, c)| visible(c))
        .map_or(content_end - span.range.start, |(offset, _)| {
            marker_end + offset
        });
    Some(BlockRanges {
        marker: span.range.start..span.range.start + marker_len,
        content: span.range.start + marker_len..content_end,
    })
}

pub(crate) fn formatting_spans(text: &str) -> Vec<FormattingSpan> {
    let filtered;
    let display = if text.chars().all(visible) {
        text
    } else {
        filtered = text.chars().filter(|&c| visible(c)).collect::<String>();
        &filtered
    };
    let mut spans = Vec::new();
    super::scan_formatting(display, &[], true, None, Some(&mut spans), 0);
    if display.len() != text.len() {
        project_ranges(text, &mut spans);
    }
    spans
}

fn visible(c: char) -> bool {
    !c.is_control() || matches!(c, '\n' | '\t')
}

#[derive(Clone, Copy, Default)]
struct Projection {
    raw: usize,
    display: usize,
}

impl Projection {
    fn advance(&mut self, text: &str, target: usize) -> usize {
        debug_assert!(target >= self.display);
        while let Some(c) = text[self.raw..].chars().next() {
            if visible(c) {
                if self.display == target {
                    break;
                }
                self.display += c.len_utf8();
            }
            self.raw += c.len_utf8();
        }
        debug_assert_eq!(self.display, target);
        self.raw
    }
}

fn project_ranges(text: &str, spans: &mut [FormattingSpan]) {
    // Scanner output is preorder: nested ranges close in reverse order, while
    // ranges at the same nesting depth end in increasing order.
    let mut starts = Projection::default();
    let mut ends: Vec<Projection> = Vec::new();
    let mut enclosing = Vec::new();
    for span in spans {
        let start = span.range.start;
        let end = span.range.end;
        while enclosing.last().is_some_and(|&outer| start >= outer) {
            enclosing.pop();
        }
        let depth = enclosing.len();
        if depth == ends.len() {
            ends.push(Projection::default());
        }
        span.range = starts.advance(text, start)..ends[depth].advance(text, end);
        enclosing.push(end);
    }
}

#[cfg(test)]
#[path = "formatting_tests.rs"]
mod tests;
