use gtk::prelude::*;
use gtk4 as gtk;

use crate::safety::{FormattingKind, formatting_spans};

#[cfg(test)]
pub(crate) mod probe {
    use std::{cell::RefCell, io::Write, time::Instant};
    struct State {
        start: Instant,
        file: std::fs::File,
        depth: usize,
        count: u64,
    }
    thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }
    pub(crate) fn start() {
        let file = std::fs::File::create(std::env::var("ZMD_LOAD_EVENTS").unwrap()).unwrap();
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                start: Instant::now(),
                file,
                depth: 0,
                count: 0,
            })
        });
    }
    pub(crate) fn event(stage: &str, value: usize) {
        STATE.with(|s| if let Some(s) = s.borrow_mut().as_mut() {
            writeln!(s.file, "{}", serde_json::json!({"stage":stage,"value":value,"elapsed_us":s.start.elapsed().as_micros(),"depth":s.depth,"changed_count":s.count})).unwrap();
            s.file.flush().unwrap();
        });
    }
    pub(crate) fn changed(enter: bool) {
        STATE.with(|s| if let Some(s) = s.borrow_mut().as_mut() {
            if enter { s.depth += 1; s.count += 1; }
            writeln!(s.file, "{}", serde_json::json!({"stage":if enter { "changed_enter" } else { "changed_exit" },"elapsed_us":s.start.elapsed().as_micros(),"depth":s.depth,"changed_count":s.count})).unwrap();
            s.file.flush().unwrap();
            if !enter { s.depth -= 1; }
        });
    }
}

const TAGS: [&str; 7] = [
    "zaptide-format-bold",
    "zaptide-format-italic",
    "zaptide-format-strike",
    "zaptide-format-code",
    "zaptide-format-quote",
    "zaptide-format-bullet",
    "zaptide-format-numbered",
];

pub(crate) fn install(buffer: &gtk::TextBuffer) {
    let tags = [
        gtk::TextTag::builder().name(TAGS[0]).weight(700).build(),
        gtk::TextTag::builder()
            .name(TAGS[1])
            .style(gtk::pango::Style::Italic)
            .build(),
        gtk::TextTag::builder()
            .name(TAGS[2])
            .strikethrough(true)
            .build(),
        gtk::TextTag::builder()
            .name(TAGS[3])
            .family("monospace")
            .build(),
        gtk::TextTag::builder()
            .name(TAGS[4])
            .left_margin(16)
            .build(),
        gtk::TextTag::builder()
            .name(TAGS[5])
            .left_margin(24)
            .indent(-16)
            .build(),
        gtk::TextTag::builder()
            .name(TAGS[6])
            .left_margin(24)
            .indent(-16)
            .build(),
    ];
    let table = buffer.tag_table();
    for tag in tags {
        if table
            .lookup(tag.name().as_deref().unwrap_or_default())
            .is_none()
        {
            table.add(&tag);
        }
    }
}

pub(crate) fn refresh(buffer: &gtk::TextBuffer) {
    let (start, end) = buffer.bounds();
    #[cfg(test)]
    probe::event("format_text_enter", 0);
    let text = buffer.text(&start, &end, true);
    #[cfg(test)]
    probe::event("format_text_exit", text.len());
    #[cfg(test)]
    probe::event("format_remove_enter", 0);
    for name in TAGS {
        buffer.remove_tag_by_name(name, &start, &end);
    }
    #[cfg(test)]
    probe::event("format_remove_exit", 7);
    #[cfg(test)]
    probe::event("format_map_enter", 0);
    let offsets = character_offsets(&text);
    #[cfg(test)]
    probe::event("format_map_exit", offsets.len());
    #[cfg(test)]
    probe::event("format_parser_enter", 0);
    let spans = formatting_spans(&text);
    #[cfg(test)]
    probe::event("format_parser_exit", spans.len());
    #[cfg(test)]
    probe::event("format_apply_enter", spans.len());
    #[cfg(test)]
    let mut applied = 0usize;
    for span in spans {
        let name = match span.kind {
            FormattingKind::Bold => TAGS[0],
            FormattingKind::Italic => TAGS[1],
            FormattingKind::Strike => TAGS[2],
            FormattingKind::Code | FormattingKind::Monospace => TAGS[3],
            FormattingKind::Quote => TAGS[4],
            FormattingKind::Bullet => TAGS[5],
            FormattingKind::Numbered => TAGS[6],
        };
        buffer.apply_tag_by_name(
            name,
            &buffer.iter_at_offset(offsets[span.range.start]),
            &buffer.iter_at_offset(offsets[span.range.end]),
        );
        #[cfg(test)]
        {
            applied += 1;
            if applied.is_multiple_of(1024) {
                probe::event("format_apply_checkpoint", applied);
            }
        }
    }
    #[cfg(test)]
    probe::event("format_apply_exit", applied);
}

fn character_offsets(text: &str) -> Vec<i32> {
    let mut offsets = vec![0; text.len() + 1];
    let mut count = 0;
    for (byte, _) in text.char_indices() {
        offsets[byte] = count;
        count += 1;
    }
    offsets[text.len()] = count;
    offsets
}

#[cfg(test)]
mod tests {
    use super::gtk;
    use super::{TAGS, character_offsets, install, refresh};
    use gtk::prelude::*;

    #[test]
    fn maps_original_utf8_boundaries_to_gtk_character_offsets() {
        let text = "aא🙂界";
        let offsets = character_offsets(text);
        let actual: Vec<_> = text
            .char_indices()
            .map(|(byte, _)| offsets[byte])
            .chain([offsets[text.len()]])
            .collect();
        assert_eq!(actual, [0, 1, 2, 3, 4]);
    }

    #[test]
    fn maps_empty_text() {
        assert_eq!(character_offsets(""), [0]);
    }

    #[test]
    #[ignore = "needs a display"]
    fn formatting_keeps_raw_text_selection_and_unrelated_tags() {
        gtk::init().unwrap();
        let buffer = gtk::TextBuffer::new(None);
        buffer.set_enable_undo(true);
        buffer.set_text("🙂 *bold* and `code`");
        let foreign = gtk::TextTag::builder().name("other-feature").build();
        buffer.tag_table().add(&foreign);
        buffer.apply_tag(&foreign, &buffer.start_iter(), &buffer.end_iter());
        buffer.select_range(&buffer.iter_at_offset(4), &buffer.iter_at_offset(3));
        install(&buffer);
        install(&buffer);
        refresh(&buffer);

        assert_eq!(
            buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
            "🙂 *bold* and `code`"
        );
        let (start, end) = buffer.selection_bounds().unwrap();
        assert_eq!((start.offset(), end.offset()), (3, 4));
        assert!(buffer.iter_at_offset(3).has_tag(&foreign));
        let bold = buffer.tag_table().lookup(TAGS[0]).unwrap();
        assert!(buffer.iter_at_offset(3).has_tag(&bold));
        assert!(!buffer.iter_at_offset(2).has_tag(&bold));

        buffer.begin_user_action();
        buffer.insert_at_cursor("界");
        buffer.end_user_action();
        let edited = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
        refresh(&buffer);
        buffer.undo();
        assert_eq!(
            buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
            "🙂 *bold* and `code`"
        );
        buffer.redo();
        assert_eq!(
            buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
            edited
        );
    }
}
