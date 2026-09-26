//! Emoji picker for the composer and message reactions.
//!
//! `GtkEmojiChooser` builds a widget for each of about two thousand emoji and
//! styles and lays them all out the first time it opens, a visible stall; the
//! reaction chooser was built afresh, and stalled, every time. This grid only
//! creates the cells on screen. It reads GTK's own emoji data, and GTK's
//! recent-emoji setting, so recents carry over from the old chooser.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};
use relm4::gtk;

const DATA: &str = "/org/gtk/libgtk/emoji/en.data";
const DATA_TYPE: &str = "a(aussasasu)";
const RECENT_SCHEMA: &str = "org.gtk.gtk4.Settings.EmojiChooser";
const RECENT_KEY: &str = "recently-used-emoji";
const MAX_RECENT: usize = 30;
const COLUMNS: u32 = 8;

/// Sections in GTK's order, with its icons. Group 2 holds bare skin-tone and
/// hair components, which GTK's chooser leaves out too.
const GROUPS: [(u32, &str, &str); 9] = [
    (0, "Smileys", "emoji-people-symbolic"),
    (1, "People", "emoji-body-symbolic"),
    (3, "Animals & Nature", "emoji-nature-symbolic"),
    (4, "Food & Drink", "emoji-food-symbolic"),
    (5, "Travel & Places", "emoji-travel-symbolic"),
    (6, "Activities", "emoji-activities-symbolic"),
    (7, "Objects", "emoji-objects-symbolic"),
    (8, "Symbols", "emoji-symbols-symbolic"),
    (9, "Flags", "emoji-flags-symbolic"),
];

/// Replaces the grid's items.
type ShowItems = dyn Fn(&[&str]);

struct Emoji {
    text: String,
    /// Lowercase name and keywords.
    search: String,
    group: u32,
    /// The entry as stored in GTK's data, for the recent-emoji setting.
    data: glib::Variant,
}

thread_local! {
    static EMOJI: Rc<Vec<Emoji>> = Rc::new(load());
}

/// Codepoints to text. GTK marks where a skin tone may go with a 0; without
/// a tone the emoji keeps its default yellow.
fn text(codepoints: &[u32], modifier: u32) -> String {
    codepoints
        .iter()
        .filter_map(|codepoint| match *codepoint {
            0 if modifier == 0 => None,
            0 => char::from_u32(modifier),
            codepoint => char::from_u32(codepoint),
        })
        .collect()
}

fn load() -> Vec<Emoji> {
    let (Ok(bytes), Ok(ty)) = (
        gio::resources_lookup_data(DATA, gio::ResourceLookupFlags::NONE),
        glib::VariantTy::new(DATA_TYPE),
    ) else {
        return Vec::new();
    };
    glib::Variant::from_bytes_with_type(&bytes, ty)
        .iter()
        .filter_map(|entry| {
            let codepoints: Vec<u32> = entry.child_value(0).get()?;
            let name: String = entry.child_value(1).get()?;
            let keywords: Vec<String> = entry.child_value(3).get()?;
            let group: u32 = entry.child_value(5).get()?;
            (group != 2).then(|| Emoji {
                text: text(&codepoints, 0),
                search: format!("{name} {}", keywords.join(" ")).to_lowercase(),
                group,
                data: entry,
            })
        })
        .collect()
}

fn recent_settings() -> Option<gio::Settings> {
    gio::SettingsSchemaSource::default()?.lookup(RECENT_SCHEMA, true)?;
    Some(gio::Settings::new(RECENT_SCHEMA))
}

fn recent(settings: &gio::Settings) -> Vec<String> {
    settings
        .value(RECENT_KEY)
        .iter()
        .filter_map(|item| {
            let codepoints: Vec<u32> = item.child_value(0).child_value(0).get()?;
            let modifier: u32 = item.child_value(1).get()?;
            Some(text(&codepoints, modifier))
        })
        .collect()
}

/// Puts `picked` first in GTK's recent emoji, as its chooser does.
fn remember(settings: &gio::Settings, emoji: &[Emoji], picked: &str) {
    let Some(entry) = emoji.iter().find(|entry| entry.text == picked) else {
        return;
    };
    let Ok(ty) = glib::VariantTy::new("((aussasasu)u)") else {
        return;
    };
    let first = glib::Variant::tuple_from_iter([entry.data.clone(), 0u32.to_variant()]);
    let rest = settings
        .value(RECENT_KEY)
        .iter()
        .filter(|item| item.child_value(0) != entry.data)
        .take(MAX_RECENT - 1)
        .collect::<Vec<_>>();
    let items = glib::Variant::array_from_iter_with_type(ty, std::iter::once(first).chain(rest));
    let _ = settings.set_value(RECENT_KEY, &items);
}

/// Emoji matching every word of `query` in their name or keywords.
fn matches<'a>(emoji: &'a [Emoji], query: &str) -> Vec<&'a str> {
    let words = query.to_lowercase();
    let words = words.split_whitespace().collect::<Vec<_>>();
    emoji
        .iter()
        .filter(|entry| words.iter().all(|word| entry.search.contains(word)))
        .map(|entry| entry.text.as_str())
        .collect()
}

/// A popover of emoji; picking one calls `on_pick` and closes it.
pub fn picker(on_pick: impl Fn(&str) + 'static) -> gtk::Popover {
    let emoji = EMOJI.with(Rc::clone);
    let settings = recent_settings();
    let on_pick: Rc<dyn Fn(&str)> = Rc::new(on_pick);

    let popover = gtk::Popover::new();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search emoji")
        .build();
    let grid = gtk::GridView::builder()
        .min_columns(COLUMNS)
        .max_columns(COLUMNS)
        .single_click_activate(true)
        .build();
    grid.add_css_class("zaptide-emoji-grid");
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let label = gtk::Label::new(None);
        label.add_css_class("zaptide-emoji-cell");
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            item.set_child(Some(&label));
        }
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let (Some(label), Some(text)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<gtk::StringObject>(),
        ) {
            label.set_label(&text.string());
        }
    });
    grid.set_factory(Some(&factory));
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(300)
        .vexpand(true)
        .child(&grid)
        .build();
    let categories = gtk::Box::builder().homogeneous(true).build();
    content.append(&search);
    content.append(&scroller);
    content.append(&categories);
    popover.set_child(Some(&content));

    let show: Rc<ShowItems> = Rc::new({
        let grid = grid.downgrade();
        move |items| {
            if let Some(grid) = grid.upgrade() {
                let model = gtk::StringList::new(items);
                grid.set_model(Some(&gtk::NoSelection::new(Some(model))));
            }
        }
    });

    // Where each category starts in the full list: recents, then GTK's groups.
    let starts: Rc<RefCell<Vec<Option<u32>>>> = Rc::default();
    let recent_button = category_button("Recent", "emoji-recent-symbolic");
    categories.append(&recent_button);
    let mut buttons = vec![recent_button];
    for (_, title, icon) in GROUPS {
        let button = category_button(title, icon);
        categories.append(&button);
        buttons.push(button);
    }
    let show_all: Rc<dyn Fn()> = Rc::new({
        let (emoji, settings, starts, show) = (
            emoji.clone(),
            settings.clone(),
            starts.clone(),
            show.clone(),
        );
        let recent_button = buttons[0].downgrade();
        move || {
            let recents = settings.as_ref().map(recent).unwrap_or_default();
            let mut items: Vec<&str> = recents.iter().map(String::as_str).collect();
            let mut section_starts = vec![(!recents.is_empty()).then_some(0)];
            for (group, _, _) in GROUPS {
                section_starts.push(Some(items.len() as u32));
                items.extend(
                    emoji
                        .iter()
                        .filter(|entry| entry.group == group)
                        .map(|entry| entry.text.as_str()),
                );
            }
            if let Some(button) = recent_button.upgrade() {
                button.set_visible(!recents.is_empty());
            }
            *starts.borrow_mut() = section_starts;
            show(&items);
        }
    });
    for (index, button) in buttons.iter().enumerate() {
        let (starts, grid, scroller) = (starts.clone(), grid.downgrade(), scroller.downgrade());
        button.connect_clicked(move |_| {
            let (Some(grid), Some(scroller), Some(Some(start))) = (
                grid.upgrade(),
                scroller.upgrade(),
                starts.borrow().get(index).copied(),
            ) else {
                return;
            };
            // Cells are the same height, so a row's offset is proportional.
            let rows = grid
                .model()
                .map_or(1, |model| model.n_items().div_ceil(COLUMNS).max(1));
            let adjustment = scroller.vadjustment();
            let row_height = adjustment.upper() / f64::from(rows);
            adjustment.set_value(f64::from(start / COLUMNS) * row_height);
        });
    }

    let pick: Rc<dyn Fn(&str)> = Rc::new({
        let (emoji, popover) = (emoji.clone(), popover.downgrade());
        move |picked| {
            on_pick(picked);
            if let Some(settings) = &settings {
                remember(settings, &emoji, picked);
            }
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
        }
    });
    let item_text = |grid: &gtk::GridView, position: u32| {
        grid.model()?
            .item(position)
            .and_downcast::<gtk::StringObject>()
            .map(|text| text.string().to_string())
    };
    grid.connect_activate({
        let pick = pick.clone();
        move |grid, position| {
            if let Some(text) = item_text(grid, position) {
                pick(&text);
            }
        }
    });
    search.connect_activate({
        let (pick, grid) = (pick.clone(), grid.downgrade());
        move |_| {
            if let Some(text) = grid.upgrade().and_then(|grid| item_text(&grid, 0)) {
                pick(&text);
            }
        }
    });
    search.connect_search_changed({
        let (emoji, show, show_all, categories) = (
            emoji.clone(),
            show.clone(),
            show_all.clone(),
            categories.downgrade(),
        );
        move |search| {
            let query = search.text();
            let query = query.trim();
            if let Some(categories) = categories.upgrade() {
                categories.set_sensitive(query.is_empty());
            }
            if query.is_empty() {
                show_all();
            } else {
                show(&matches(&emoji, query));
            }
        }
    });
    popover.connect_show({
        let (search, scroller) = (search.downgrade(), scroller.downgrade());
        move |_| {
            // Refreshes the recents too.
            if let Some(search) = search.upgrade() {
                search.set_text("");
                search.grab_focus();
            }
            show_all();
            if let Some(scroller) = scroller.upgrade() {
                scroller.vadjustment().set_value(0.0);
            }
        }
    });
    popover
}

fn category_button(title: &str, icon: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(title)
        .css_classes(["flat"])
        .build();
    button.update_property(&[gtk::accessible::Property::Label(title)]);
    button
}

#[cfg(test)]
mod tests {
    #[test]
    fn skin_tone_slots_are_dropped_or_filled() {
        assert_eq!(super::text(&[0x1F44B, 0], 0), "👋");
        assert_eq!(super::text(&[0x1F44B, 0], 0x1F3FD), "👋🏽");
    }
}
