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

pub(super) fn message_row(
    message: crate::model::Message,
    contacts: &std::collections::HashMap<String, crate::model::Contact>,
    pointer_sender: ComponentSender<NativeApplication>,
    prefix: &str,
    avatar: Option<std::path::PathBuf>,
    boundaries: (bool, bool),
    audio_registry: AudioRegistry,
) -> MessageRow {
    let (show_sender, show_timestamp) = boundaries;
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
        sender_label(message.sender_name.as_deref(), &message.sender).into()
    };
    let separator = prefix
        .lines()
        .map(|line| line.trim_matches(|c| c == '─' || c == ' '))
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
                sender_label(quoted.sender_name.as_deref(), &quoted.sender),
                quoted.summary
            )
        })
        .unwrap_or_default();
    let clock = crate::util::clock(message.timestamp);
    let delivery = delivery_label(message.status);
    let mut footer = Vec::new();
    if message.forwarded {
        footer.push("Forwarded".to_owned());
    }
    if message.edited {
        footer.push("Edited".to_owned());
    }
    let reactions = reaction_summary(&message.reactions);
    footer.push(clock.clone());
    let mentions = mention_labels(&message, contacts);
    let accessible_label = format!(
        "{prefix}{sender}: {}{}{} · {clock}{delivery}",
        crate::safety::display_mentions(&transcript_text(&message), &mentions),
        if quote.is_empty() {
            String::new()
        } else {
            format!("\n↪ {quote}")
        },
        reactions,
    );
    MessageRow {
        id: message.id.clone(),
        separator,
        sender: sender.into_owned(),
        sender_class: SENDER_CLASSES
            [crate::util::hue(&message.sender) as usize * SENDER_CLASSES.len() / 360],
        avatar,
        quote,
        body,
        mentions,
        footer: footer.join(" · "),
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
    for emoji in ["👍", "❤️", "😂", "😮", "😢", "🙏"] {
        let chosen = current.as_deref() == Some(emoji);
        let button = gtk::Button::builder()
            .label(emoji)
            .tooltip_text(if chosen { "Remove reaction" } else { emoji })
            .css_classes(["flat", "circular", "zaptide-reaction"])
            .build();
        if chosen {
            button.add_css_class("chosen");
        }
        let (menu, sender) = (menu.clone(), sender.clone());
        let emoji = if chosen {
            String::new()
        } else {
            emoji.to_owned()
        };
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
