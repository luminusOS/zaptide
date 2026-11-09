use super::{FormattingKind as K, formatting_spans};

#[test]
fn parses_formats_and_raw_utf8_ranges() {
    let text = "שלום *bold _e\u{301}😀_* ~no~ `x`\n```\ncode\n```\n- one\n1. two\n> three";
    let spans = formatting_spans(text);
    for (kind, content) in [
        (K::Bold, "bold _e\u{301}😀_"),
        (K::Italic, "e\u{301}😀"),
        (K::Strike, "no"),
        (K::Code, "x"),
        (K::Monospace, "code"),
        (K::Bullet, "- one"),
        (K::Numbered, "1. two"),
        (K::Quote, "> three"),
    ] {
        assert!(
            spans
                .iter()
                .any(|span| span.kind == kind && &text[span.range.clone()] == content),
            "{kind:?}"
        );
    }
    assert!(
        spans
            .iter()
            .all(|s| text.is_char_boundary(s.range.start) && text.is_char_boundary(s.range.end))
    );
}

#[test]
fn malformed_delimiters_and_code_are_literal() {
    for text in ["*a\nb*", "snake_case_name", "x * *", "*open", "a*b*c"] {
        assert!(formatting_spans(text).is_empty(), "{text:?}");
    }
    let text = "`*x* https://example.com` ```\n_foo_\n```";
    let spans = formatting_spans(text);
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].kind, K::Code);
    assert_eq!(spans[1].kind, K::Monospace);
}

#[test]
fn adversarial_scans_have_linear_operation_counts() {
    for unit in [
        " *a",
        " ```   ",
        "*a _b ~c",
        "123456789.",
        "😀_x",
        "- item\n",
    ] {
        for n in [1_000, 2_000, 4_000] {
            let text = unit.repeat(n);
            super::super::FORMATTING_STEPS.with(|steps| steps.set(0));
            let spans = formatting_spans(&text);
            let work = super::super::FORMATTING_STEPS.with(std::cell::Cell::get);
            assert!(
                work <= 20 * text.len() + 10 * spans.len(),
                "{unit:?}: {work}"
            );
        }
    }
}

#[test]
fn links_and_controls_use_display_scanning_with_raw_offsets() {
    let text = "h\u{1}ttps://example.com/*link* *é\u{8} _😀_*\n*\u{1} item\r\n";
    let spans = formatting_spans(text);
    assert!(!spans.iter().any(|span| &text[span.range.clone()] == "link"));
    assert!(
        spans
            .iter()
            .any(|span| span.kind == K::Bold && &text[span.range.clone()] == "é\u{8} _😀_")
    );
    assert!(
        spans
            .iter()
            .any(|span| span.kind == K::Italic && &text[span.range.clone()] == "😀")
    );
    let block = spans.iter().find(|span| span.kind == K::Bullet).unwrap();
    let ranges = super::block_ranges(text, block).unwrap();
    assert_eq!(&text[ranges.marker], "*\u{1} ");
    assert_eq!(&text[ranges.content], "item");
}

#[test]
fn controls_do_not_create_an_italic_boundary_after_a_letter() {
    let text = "*é\u{8}_😀_*";
    let spans = formatting_spans(text);
    assert_eq!(super::super::linkify_markup(text), "<b>é_😀_</b>");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].kind, K::Bold);
    assert_eq!(&text[spans[0].range.clone()], "é\u{8}_😀_");
}

#[test]
fn blocks_preserve_newlines_and_empty_crlf_content() {
    for text in [
        "- \r\n",
        "- \n",
        "- ",
        "> content\r\n",
        "> content\n",
        "> content",
    ] {
        let spans = formatting_spans(text);
        let block = super::block_ranges(text, &spans[0]).unwrap();
        assert_eq!(
            &text[block.content],
            if text.starts_with('-') { "" } else { "content" }
        );
    }
}

#[test]
fn blocks_only_start_at_line_boundaries_and_keep_markers() {
    let text = "a 1. no\n12. yes\n> quote\n- item\n* item";
    let spans = formatting_spans(text);
    let blocks: Vec<_> = spans
        .iter()
        .filter(|s| matches!(s.kind, K::Numbered | K::Quote | K::Bullet))
        .map(|s| &text[s.range.clone()])
        .collect();
    assert_eq!(blocks, ["12. yes", "> quote", "- item", "* item"]);
}

#[test]
fn block_ranges_report_raw_marker_and_content_with_crlf() {
    let text = "😀\n12345678901234567890. é😀\r\n> quote\n- item\n* \n";
    let spans = formatting_spans(text);
    let blocks: Vec<_> = spans
        .iter()
        .filter_map(|span| super::block_ranges(text, span))
        .map(|ranges| (&text[ranges.marker], &text[ranges.content]))
        .collect();
    assert_eq!(
        blocks,
        [
            ("12345678901234567890. ", "é😀"),
            ("> ", "quote"),
            ("- ", "item"),
            ("* ", "")
        ]
    );
}

#[test]
fn block_ranges_reject_invalid_ranges_and_kinds() {
    use super::FormattingSpan;
    let text = "😀 12. x";
    for span in [
        FormattingSpan {
            kind: K::Numbered,
            range: 1..text.len(),
        },
        FormattingSpan {
            kind: K::Numbered,
            range: 5..text.len() + 1,
        },
        FormattingSpan {
            kind: K::Bold,
            range: 5..text.len(),
        },
        FormattingSpan {
            kind: K::Bullet,
            range: 5..text.len(),
        },
    ] {
        assert_eq!(super::block_ranges(text, &span), None);
    }
}

#[test]
fn numbered_markers_are_not_limited_by_integer_size() {
    let marker = "9".repeat(100_000);
    let text = format!("{marker}. a\n");
    let spans = formatting_spans(&text);
    let block = super::block_ranges(&text, &spans[0]).unwrap();
    assert_eq!(&text[block.marker], format!("{marker}. "));
    assert_eq!(&text[block.content], "a");
}

#[test]
fn controls_preserve_raw_offsets() {
    let text = "😀\u{8} *é\u{1}x*";
    let spans = formatting_spans(text);
    assert_eq!(&text[spans[0].range.clone()], "é\u{1}x");
}
