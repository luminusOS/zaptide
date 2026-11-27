//! Widgets for one message: bubble rows, delivery ticks, reactions.

use super::*;

/// WhatsApp-style check marks, drawn so the double tick overlaps like the
/// original instead of depending on the font's check glyph.
pub(super) fn delivery_ticks() -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(16)
        .content_height(11)
        .valign(gtk::Align::Center)
        .css_classes(["zaptide-delivery"])
        .build();
    area.set_draw_func(|area, cr, _, height| {
        let color = area.color();
        let bottom = f64::from(height) - 2.0;
        cr.set_source_rgba(
            color.red().into(),
            color.green().into(),
            color.blue().into(),
            color.alpha().into(),
        );
        cr.set_line_width(1.5);
        cr.set_line_cap(gtk::cairo::LineCap::Round);
        cr.set_line_join(gtk::cairo::LineJoin::Round);
        let double = area.has_css_class("double");
        let first = if double { 1.0 } else { 3.0 };
        cr.move_to(first, bottom - 3.5);
        cr.line_to(first + 3.5, bottom);
        cr.line_to(first + 10.0, 1.5);
        if double {
            // The second tick's short stroke hides behind the first one.
            cr.move_to(first + 6.0, bottom - 1.5);
            cr.line_to(first + 7.5, bottom);
            cr.line_to(first + 14.0, 1.5);
        }
        let _ = cr.stroke();
    });
    area
}

pub(super) fn set_delivery_ticks(area: &gtk::DrawingArea, glyph: &str) {
    area.set_visible(!glyph.is_empty());
    if glyph.chars().count() == 2 {
        area.add_css_class("double");
    } else {
        area.remove_css_class("double");
    }
    area.queue_draw();
}

fn view_once_label(message: &crate::model::Message) -> Option<String> {
    message.content.view_once().then(|| gettext("View once"))
}

/// The marks that precede the time in a bubble's footer, each followed by a
/// separator, or nothing for a plain message.
pub(super) fn footer_notes(message: &crate::model::Message) -> String {
    let mut notes: Vec<String> = view_once_label(message).into_iter().collect();
    if message.forwarded {
        notes.push("Forwarded".to_owned());
    }
    if message.edited {
        notes.push("Edited".to_owned());
    }
    if notes.is_empty() {
        String::new()
    } else {
        format!("{} ·", notes.join(" · "))
    }
}

pub(super) const STAR_ICON_SIZE: i32 = 13;

/// A grey star that only decorates: its row or heading already says what it is.
pub(super) fn decorative_star(size: i32) -> gtk::Image {
    gtk::Image::builder()
        .icon_name("starred-symbolic")
        .pixel_size(size)
        .accessible_role(gtk::AccessibleRole::Presentation)
        .css_classes(["zaptide-starred-indicator"])
        .build()
}

/// What a screen reader announces for a bubble: the content (including the
/// view-once mark when present), the time, the starred mark, and delivery.
pub(super) fn accessible_label(
    prefix: &str,
    sender: &str,
    content: &str,
    clock: &str,
    starred: Option<&str>,
    delivery: &str,
) -> String {
    let starred = starred.map_or_else(String::new, |words| format!(" · {words}"));
    format!("{prefix}{sender}: {content} · {clock}{starred}{delivery}")
}

/// The text and star in front of a bubble's delivery mark: marks such as
/// "Forwarded", the starred indicator, then the time.
pub(super) struct FooterLabels {
    notes: gtk::Label,
    star: gtk::Image,
    clock: gtk::Label,
}

impl FooterLabels {
    pub(super) fn new() -> Self {
        let notes = gtk::Label::builder()
            .xalign(1.0)
            .wrap(true)
            .max_width_chars(52)
            .css_classes(["dim-label", "caption"])
            .build();
        let star = gtk::Image::builder()
            .icon_name("starred-symbolic")
            .pixel_size(STAR_ICON_SIZE)
            .visible(false)
            .tooltip_text(gettext("Starred"))
            .css_classes(["zaptide-starred-indicator"])
            .build();
        star.update_property(&[gtk::accessible::Property::Label(&gettext("Starred"))]);
        let clock = gtk::Label::builder()
            .xalign(1.0)
            .css_classes(["dim-label", "caption"])
            .build();
        Self { notes, star, clock }
    }

    pub(super) fn append_to(&self, row: &gtk::Box) {
        row.append(&self.notes);
        row.append(&self.star);
        row.append(&self.clock);
    }

    pub(super) fn set(&self, notes: &str, clock: &str, starred: bool) {
        self.notes.set_label(notes);
        self.notes.set_visible(!notes.is_empty());
        self.star.set_visible(starred);
        self.clock.set_label(clock);
        self.clock.set_visible(!clock.is_empty());
    }
}

/// How a message sits among its neighbours and what marks it.
pub(super) struct RowFlags {
    pub show_sender: bool,
    pub show_timestamp: bool,
    pub starred: bool,
    /// Kept from disappearing by someone in the chat.
    pub kept: bool,
}

pub(super) fn message_row<S>(
    message: crate::model::Message,
    contacts: &std::collections::HashMap<String, crate::model::Contact>,
    pointer_sender: S,
    prefix: &str,
    avatar: Option<std::path::PathBuf>,
    flags: RowFlags,
    audio_registry: AudioRegistry,
) -> MessageRow<S> {
    let RowFlags {
        show_sender,
        show_timestamp,
        starred,
        kept,
    } = flags;
    const SENDER_CLASSES: [&str; 6] = [
        "zaptide-sender-blue",
        "zaptide-sender-green",
        "zaptide-sender-yellow",
        "zaptide-sender-orange",
        "zaptide-sender-red",
        "zaptide-sender-purple",
    ];
    let sender: std::borrow::Cow<'_, str> = if message.from_me {
        "You".into()
    } else {
        contact_sender_label(message.sender_name.as_deref(), &message.sender, contacts).into()
    };
    let mut unread_divider = false;
    let separator = prefix
        .lines()
        .map(|line| line.trim_matches(|c| c == '─' || c == ' '))
        .filter(|line| {
            let marker = *line == UNREAD_MARKER;
            unread_divider |= marker;
            !marker
        })
        .collect::<Vec<_>>()
        .join("\n");
    let body = match &message.content {
        crate::model::Content::Text { text, .. } => text.clone(),
        crate::model::Content::Image { caption, .. }
        | crate::model::Content::Video { caption, .. }
        | crate::model::Content::Document { caption, .. } => caption.clone().unwrap_or_default(),
        _ => String::new(),
    };
    let quote = message
        .quoted
        .as_ref()
        .map(|quoted| {
            format!(
                "{}: {}",
                if quoted.sender_name.as_deref() == Some("You") {
                    "You".into()
                } else {
                    contact_sender_label(quoted.sender_name.as_deref(), &quoted.sender, contacts)
                },
                crate::safety::display_mentions(
                    &quoted.summary,
                    &mention_ref_labels(&quoted.mentions, contacts)
                )
            )
        })
        .unwrap_or_default();
    let clock = crate::util::clock(message.timestamp);
    let delivery = delivery_label(message.status);
    let mut notes = footer_notes(&message);
    let kept_label = kept.then(|| gettext("Kept"));
    if let Some(label) = &kept_label {
        notes = if notes.is_empty() {
            format!("{label} ·")
        } else {
            format!("{label} · {notes}")
        };
    }
    let reactions = reaction_summary(&message.reactions);
    let mentions = mention_labels(&message, contacts);
    let mut content = format!(
        "{}{}{}",
        crate::safety::display_mentions(&transcript_text(&message), &mentions),
        if quote.is_empty() {
            String::new()
        } else {
            format!("\n↪ {quote}")
        },
        reactions,
    );
    if let Some(marker) = view_once_label(&message) {
        content.push_str(" · ");
        content.push_str(&marker);
    }
    if let Some(marker) = kept_label {
        content.push_str(" · ");
        content.push_str(&marker);
    }
    let accessible_label = accessible_label(
        prefix,
        &sender,
        &content,
        &clock,
        starred.then(|| gettext("Starred")).as_deref(),
        delivery,
    );
    MessageRow {
        id: message.id.clone(),
        account: crate::account::AccountId::FIRST,
        separator,
        unread_divider,
        sender: sender.into_owned(),
        sender_class: SENDER_CLASSES
            [crate::util::hue(&message.sender) as usize * SENDER_CLASSES.len() / 360],
        avatar,
        quote,
        body,
        mentions,
        notes,
        clock,
        starred,
        accessible_label,
        show_sender,
        show_timestamp,
        pointer_sender,
        message,
        album: Vec::new(),
        collapsed: false,
        audio: None,
        audio_registry,
        selected: None,
    }
}

#[cfg(test)]
#[test]
fn view_once_marker_is_announced_only_for_view_once_bubbles() {
    let row_for = |view_once| {
        let mut message = super::tests::grouping_message("synthetic", "Ana", false, 0);
        message.content = crate::model::Content::Image {
            caption: Some("synthetic caption".into()),
            media: crate::model::Media {
                mime: "image/jpeg".into(),
                size: 0,
                width: None,
                height: None,
                path: None,
                state: Default::default(),
                view_once,
            },
        };
        message_row(
            message,
            &Default::default(),
            (),
            "",
            None,
            RowFlags {
                kept: false,
                show_sender: true,
                show_timestamp: true,
                starred: false,
            },
            Default::default(),
        )
    };
    let plain = row_for(false);
    let marked = row_for(true);
    let marker = gettext("View once");
    assert!(!plain.accessible_label.contains(&marker));
    assert!(marked.accessible_label.contains(&marker));
    assert_eq!(plain.message.summary(), marked.message.summary());
}

#[cfg(test)]
#[test]
fn kept_marker_is_visible_and_announced_without_changing_the_message() {
    let row_for = |kept| {
        message_row(
            super::tests::grouping_message("synthetic", "Ana", false, 0),
            &Default::default(),
            (),
            "",
            None,
            RowFlags {
                kept,
                show_sender: true,
                show_timestamp: true,
                starred: false,
            },
            Default::default(),
        )
    };
    let plain = row_for(false);
    let marked = row_for(true);
    let marker = gettext("Kept");
    assert!(marked.notes.contains(&marker));
    assert!(marked.accessible_label.contains(&marker));
    assert!(!plain.accessible_label.contains(&marker));
    assert_eq!(plain.message.summary(), marked.message.summary());
}

/// A pill under the bubble; clicking it adds this reaction, or removes ours.
pub(super) fn reaction_chip(
    id: &str,
    emoji: &str,
    count: usize,
    from_me: bool,
    sender: &ComponentSender<NativeApplication>,
) -> gtk::Button {
    let others = count - usize::from(from_me);
    let who = match (from_me, others) {
        (true, 0) => "You".to_owned(),
        (true, 1) => "You and 1 other".to_owned(),
        (true, n) => format!("You and {n} others"),
        (false, 1) => "1 person".to_owned(),
        (false, n) => format!("{n} people"),
    };
    let button = gtk::Button::builder()
        .label(if count > 1 {
            format!("{emoji} {count}")
        } else {
            emoji.to_owned()
        })
        .tooltip_text(format!(
            "{who} reacted with {emoji}\n{}",
            if from_me {
                "Click to remove"
            } else {
                "Click to react"
            }
        ))
        .css_classes(["zaptide-reaction-chip"])
        .build();
    if from_me {
        button.add_css_class("chosen");
    }
    let (id, emoji, sender) = (id.to_owned(), emoji.to_owned(), sender.clone());
    button.connect_clicked(move |_| {
        sender.input(Input::ReactTo {
            id: id.clone(),
            emoji: if from_me {
                String::new()
            } else {
                emoji.clone()
            },
        });
    });
    button
}

pub(super) fn reaction_summary(reactions: &[crate::model::Reaction]) -> String {
    let counts = reaction_counts(reactions);
    if counts.is_empty() {
        return String::new();
    }
    let summary = counts
        .into_iter()
        .map(|(emoji, count, from_me)| {
            format!("{emoji} × {count}{}", if from_me { " · You" } else { "" })
        })
        .collect::<Vec<_>>()
        .join("   ");
    format!("\n{summary}")
}

/// WhatsApp-style quick reactions above the message menu. Picking the
/// reaction already sent removes it; "+" opens the full emoji chooser.
pub(super) fn reaction_bar(
    menu: &gtk::PopoverMenu,
    parent: &gtk::Widget,
    rect: gtk::gdk::Rectangle,
    current: Option<String>,
    sender: &ComponentSender<NativeApplication>,
) -> gtk::Box {
    let bar = gtk::Box::builder()
        .spacing(2)
        .css_classes(["zaptide-reactions"])
        .build();
    for emoji in reaction_drawer::current_quick() {
        let chosen = current
            .as_deref()
            .is_some_and(|current| reaction_drawer::same_emoji(current, &emoji));
        let button = gtk::Button::builder()
            .label(&emoji)
            .tooltip_text(if chosen {
                gettext("Remove reaction")
            } else {
                emoji.clone()
            })
            .css_classes(["flat", "circular", "zaptide-reaction"])
            .build();
        if chosen {
            button.add_css_class("chosen");
        }
        let (menu, sender) = (menu.clone(), sender.clone());
        let emoji = if chosen { String::new() } else { emoji };
        button.connect_clicked(move |_| {
            menu.popdown();
            sender.input(Input::ReactSelected(emoji.clone()));
        });
        bar.append(&button);
    }
    let more = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text(gettext("More Reactions"))
        .css_classes(["flat", "circular", "zaptide-reaction"])
        .build();
    let (menu, parent, sender) = (menu.clone(), parent.clone(), sender.clone());
    more.connect_clicked(move |_| {
        menu.popdown();
        let sender = sender.clone();
        let chooser = crate::native_emoji::picker(move |emoji| {
            sender.input(Input::ReactSelected(emoji.to_owned()));
        });
        chooser.set_parent(&parent);
        chooser.set_pointing_to(Some(&rect));
        chooser.connect_closed(|chooser| {
            let chooser = chooser.clone();
            gtk::glib::idle_add_local_once(move || chooser.unparent());
        });
        chooser.popup();
    });
    bar.append(&more);
    bar
}
