use std::{
    cell::{Cell, OnceCell, RefCell},
    collections::HashMap,
};

use relm4::gtk::{self, glib, prelude::*, subclass::prelude::*};

#[derive(Debug, PartialEq, Eq)]
struct LinkRange {
    start: i32,
    end: i32,
    uri: String,
    mention: bool,
}

type LinkHandler = Box<dyn for<'a> Fn(crate::safety::LinkActivation<'a>)>;
type CodeHandler = Box<dyn Fn(&str)>;

// GtkTextView reserves one pixel for the cursor, even when hidden.
const CURSOR_SPACE: i32 = 1;

fn link_colors(
    background: crate::color::Color,
) -> (crate::color::Color, Option<crate::color::Color>) {
    use crate::color::Color;

    let dark_blue = Color::from_rgb(0x00, 0x54, 0xb8);
    let light_blue = Color::from_rgb(0x83, 0xc4, 0xff);
    if crate::native_theme::contrast_ratio(background, dark_blue) >= 4.5 {
        (dark_blue, None)
    } else if crate::native_theme::contrast_ratio(background, light_blue) >= 4.5 {
        (light_blue, None)
    } else {
        (dark_blue, Some(Color::WHITE))
    }
}

thread_local! {
    static BUBBLE_STYLE: gtk::CssProvider = {
        let provider = gtk::CssProvider::new();
        provider.load_from_string("textview.zaptide-message-text, textview.zaptide-message-text text { background-color: transparent; color: inherit; }");
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
        provider
    };
}

fn unescape_attribute(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn text_view_markup(markup: &str) -> (String, Vec<LinkRange>, Vec<std::ops::Range<i32>>) {
    let mut output = String::with_capacity(markup.len());
    let mut links = Vec::new();
    let mut code = Vec::new();
    let mut active_link: Option<(i32, String, bool)> = None;
    let mut spans = Vec::new();
    let mut offset = 0i32;
    let mut remaining = markup;
    while !remaining.is_empty() {
        if remaining.starts_with('<')
            && let Some(end) = remaining.find('>')
        {
            let tag = &remaining[..=end];
            if let Some(rest) = tag.strip_prefix("<a href=\"") {
                let mut mention = false;
                if let Some(end) = rest.find('"') {
                    let uri = unescape_attribute(&rest[..end]);
                    mention = (uri.starts_with(crate::safety::CHAT_SCHEME)
                        || uri == crate::safety::MENTION_SCHEME)
                        && remaining[tag.len()..].starts_with('@');
                    active_link = Some((offset, uri, mention));
                }
                output.push_str(if mention {
                    "<span weight=\"bold\">"
                } else {
                    "<span underline=\"single\">"
                });
            } else if tag == "</a>" {
                if let Some((start, uri, mention)) = active_link.take() {
                    links.push(LinkRange {
                        start,
                        end: offset,
                        uri,
                        mention,
                    });
                }
                output.push_str("</span>");
            } else {
                if tag.starts_with("<span ") {
                    spans.push(tag.contains("bgcolor=\"#808080\"").then_some(offset));
                } else if tag == "</span>"
                    && let Some(Some(start)) = spans.pop()
                {
                    code.push(start..offset);
                }
                output.push_str(tag);
            }
            remaining = &remaining[end + 1..];
        } else if remaining.starts_with('&')
            && let Some(end) = remaining.find(';')
        {
            output.push_str(&remaining[..=end]);
            offset += 1;
            remaining = &remaining[end + 1..];
        } else {
            let ch = remaining.chars().next().expect("nonempty markup");
            output.push(ch);
            offset += 1;
            remaining = &remaining[ch.len_utf8()..];
        }
    }
    (output, links, code)
}

fn rendered_blocks(text: &str, displayed: &str) -> Vec<(std::ops::Range<i32>, String)> {
    use crate::safety::FormattingKind;

    let text: String = text
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect();
    let spans = crate::safety::formatting_spans(&text);
    let bytes = text.as_bytes();
    let mut removed = Vec::new();
    let mut blocks = Vec::new();
    for span in &spans {
        if span.kind == FormattingKind::Monospace {
            let mut before = span.range.start;
            while before > 0 && bytes[before - 1] == b'\n' {
                before -= 1;
            }
            let mut after = span.range.end;
            while after < bytes.len() && bytes[after] == b'\n' {
                after += 1;
            }
            removed.push(before..span.range.start);
            removed.push(span.range.end..after);
        }
        if let Some(block) = crate::safety::block_ranges(&text, span) {
            let marker = match span.kind {
                FormattingKind::Bullet => "• ".to_owned(),
                FormattingKind::Quote => "▎ ".to_owned(),
                FormattingKind::Numbered => text[block.marker].to_owned(),
                _ => continue,
            };
            blocks.push((span.range.start, marker));
        }
    }
    let mut starts = vec![0i32];
    let mut chars = 0i32;
    for ch in displayed.chars() {
        chars += 1;
        if ch == '\n' {
            starts.push(chars);
        }
    }
    let mut output = Vec::new();
    let (mut cursor, mut line, mut removed_index) = (0usize, 0usize, 0usize);
    for (position, marker) in blocks {
        while cursor < position {
            while removed_index < removed.len() && cursor >= removed[removed_index].end {
                removed_index += 1;
            }
            if bytes[cursor] == b'\n'
                && !removed
                    .get(removed_index)
                    .is_some_and(|range| range.contains(&cursor))
            {
                line += 1;
            }
            cursor += 1;
        }
        if let Some(&start) = starts.get(line) {
            let end = starts.get(line + 1).map_or(chars, |next| next - 1);
            output.push((start..end, marker));
        }
    }
    output
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TextBubble {
        pub(super) view: OnceCell<gtk::TextView>,
        pub(super) mention_color: OnceCell<gtk::Label>,
        pub(super) mention_tag: RefCell<Option<gtk::TextTag>>,
        pub(super) link_background: OnceCell<gtk::Label>,
        pub(super) link_tag: RefCell<Option<gtk::TextTag>>,
        pub(super) link_background_color: Cell<Option<gtk::gdk::RGBA>>,
        pub(super) width_attributes: RefCell<Option<gtk::pango::AttrList>>,
        pub(super) measured: Cell<Option<(i32, i32)>>,
        pub(super) resize_pending: Cell<bool>,
        pub(super) block_tags: RefCell<HashMap<i32, gtk::TextTag>>,
        pub(super) blocks: RefCell<Vec<(std::ops::Range<i32>, String)>>,
        pub(super) font_serial: Cell<u32>,
        pub(super) metrics_pending: Cell<bool>,
        pub(super) settings_handlers: RefCell<Vec<(gtk::Settings, glib::SignalHandlerId)>>,
        pub(super) markup_tags: RefCell<Vec<gtk::TextTag>>,
        pub(super) links: RefCell<Vec<LinkRange>>,
        pub(super) code: RefCell<Vec<std::ops::Range<i32>>>,
        pub(super) link_handler: RefCell<Option<LinkHandler>>,
        pub(super) code_handler: RefCell<Option<CodeHandler>>,
        pub(super) keyboard_link: Cell<Option<usize>>,
        pub(super) click_start: Cell<Option<i32>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TextBubble {
        const NAME: &'static str = "ZapTideTextBubble";
        type Type = super::TextBubble;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for TextBubble {
        fn constructed(&self) {
            self.parent_constructed();
            let view = gtk::TextView::builder()
                .editable(false)
                .cursor_visible(false)
                .wrap_mode(gtk::WrapMode::WordChar)
                .build();
            view.add_css_class("zaptide-message-text");
            BUBBLE_STYLE.with(|_| {});
            view.set_parent(&*self.obj());
            let mention_color = gtk::Label::new(None);
            mention_color.add_css_class("accent");
            mention_color.set_parent(&*self.obj());
            self.mention_color
                .set(mention_color)
                .expect("color initialized once");
            let link_background = gtk::Label::new(None);
            link_background.add_css_class("zaptide-link-background");
            link_background.set_parent(&*self.obj());
            self.link_background
                .set(link_background)
                .expect("color initialized once");
            let click = gtk::GestureClick::new();
            click.set_button(gtk::gdk::BUTTON_PRIMARY);
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            let owner = self.obj().downgrade();
            click.connect_pressed(move |gesture, presses, x, y| {
                if let Some(owner) = owner.upgrade() {
                    owner.imp().keyboard_link.set(None);
                    let offset = owner.offset_at(x, y);
                    owner.imp().click_start.set(offset);
                    if presses == 2 && offset.is_some_and(|offset| owner.copy_code_at(offset)) {
                        owner.imp().click_start.set(None);
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                    }
                }
            });
            let owner = self.obj().downgrade();
            click.connect_released(move |gesture, presses, x, y| {
                if let Some(owner) = owner.upgrade() {
                    let start = owner.imp().click_start.take();
                    let end = owner.offset_at(x, y);
                    if presses == 1
                        && start
                            .zip(end)
                            .is_some_and(|(start, end)| owner.same_link(start, end))
                        && owner.view().buffer().selection_bounds().is_none()
                        && owner.activate_link_at(start.unwrap())
                    {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                    }
                }
            });
            view.add_controller(click);
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            let owner = self.obj().downgrade();
            keys.connect_key_pressed(move |_, key, _, modifiers| {
                let Some(owner) = owner.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                owner.handle_key(key, modifiers)
            });
            view.add_controller(keys);
            let focus = gtk::EventControllerFocus::new();
            let owner = self.obj().downgrade();
            focus.connect_leave(move |_| {
                if let Some(owner) = owner.upgrade() {
                    owner.imp().keyboard_link.set(None);
                }
            });
            view.add_controller(focus);
            let owner = self.obj().downgrade();
            view.buffer().connect_changed(move |_| {
                if let Some(owner) = owner.upgrade() {
                    owner.imp().measured.set(None);
                    owner.queue_resize();
                }
            });
            if let Some(settings) = gtk::Settings::default() {
                for name in [
                    "gtk-font-name",
                    "gtk-theme-name",
                    "gtk-application-prefer-dark-theme",
                ] {
                    let owner = self.obj().downgrade();
                    let handler = settings.connect_notify_local(Some(name), move |_, _| {
                        if let Some(owner) = owner.upgrade() {
                            owner.schedule_metrics_refresh();
                        }
                    });
                    self.settings_handlers
                        .borrow_mut()
                        .push((settings.clone(), handler));
                }
            }
            self.view.set(view).expect("view initialized once");
        }

        fn dispose(&self) {
            for (settings, handler) in self.settings_handlers.borrow_mut().drain(..) {
                settings.disconnect(handler);
            }
            if let Some(view) = self.view.get() {
                view.unparent();
            }
            if let Some(color) = self.mention_color.get() {
                color.unparent();
            }
            if let Some(color) = self.link_background.get() {
                color.unparent();
            }
        }
    }

    impl WidgetImpl for TextBubble {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let view = self.view.get().expect("view initialized");
            if self.font_serial.get() != view.pango_context().serial() {
                self.obj().schedule_metrics_refresh();
            }
            if orientation == gtk::Orientation::Vertical
                && let Some((width, height)) = self.measured.get()
                && width == for_size
            {
                let (child_minimum, child_natural, _, _) = view.measure(orientation, for_size);
                let minimum = height.max(child_minimum);
                let natural = height.max(child_natural).max(minimum);
                return (minimum, natural, -1, -1);
            }
            let (minimum, natural, min_baseline, nat_baseline) =
                view.measure(orientation, for_size);
            if orientation == gtk::Orientation::Horizontal {
                let layout = self.obj().text_layout();
                let width =
                    layout.pixel_size().0 + view.left_margin() + view.right_margin() + CURSOR_SPACE;
                return (
                    minimum,
                    natural.max(width).max(minimum),
                    min_baseline,
                    nat_baseline,
                );
            }
            if orientation == gtk::Orientation::Vertical && minimum == 0 {
                let (_, line_height) = view.create_pango_layout(Some("Mg")).pixel_size();
                return (line_height, natural.max(line_height), -1, -1);
            }
            (minimum, natural.max(minimum), min_baseline, nat_baseline)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let view = self.view.get().expect("view initialized");
            view.allocate(width, height, baseline, None);
            let buffer = view.buffer();
            let rect = view.iter_location(&buffer.end_iter());
            let measured = rect.y() + rect.height() + view.bottom_margin();
            if self.measured.replace(Some((width, measured))) != Some((width, measured))
                && !self.resize_pending.replace(true)
            {
                let owner = self.obj().downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(owner) = owner.upgrade() {
                        owner.imp().resize_pending.set(false);
                        owner.queue_resize();
                    }
                });
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().refresh_mention_color();
            self.obj().refresh_link_color();
            if let Some(view) = self.view.get() {
                self.obj().snapshot_child(view, snapshot);
            }
        }
    }
}

glib::wrapper! {
    pub struct TextBubble(ObjectSubclass<imp::TextBubble>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TextBubble {
    fn refresh_link_color(&self) {
        if let Some(tag) = self.imp().link_tag.borrow().as_ref()
            && let Some(source) = self.imp().link_background.get()
        {
            let background = source.color();
            if self.imp().link_background_color.replace(Some(background)) == Some(background) {
                return;
            }
            let channel = |value: f32| (value * 255.0).round() as u8;
            let (color, backplate) = link_colors(crate::color::Color::from_rgb(
                channel(background.red()),
                channel(background.green()),
                channel(background.blue()),
            ));
            let rgba = |color: crate::color::Color| {
                gtk::gdk::RGBA::new(
                    color.r as f32 / 255.0,
                    color.g as f32 / 255.0,
                    color.b as f32 / 255.0,
                    1.0,
                )
            };
            let color = rgba(color);
            if tag.foreground_rgba() != Some(color) {
                tag.set_foreground_rgba(Some(&color));
            }
            let backplate = backplate.map(rgba);
            if tag.background_rgba() != backplate {
                tag.set_background_rgba(backplate.as_ref());
            }
        }
    }

    fn refresh_mention_color(&self) {
        if let Some(tag) = self.imp().mention_tag.borrow().as_ref()
            && let Some(source) = self.imp().mention_color.get()
        {
            let color = source.color();
            if tag.foreground_rgba() != Some(color) {
                tag.set_foreground_rgba(Some(&color));
            }
        }
    }

    pub(crate) fn new() -> Self {
        glib::Object::new()
    }

    pub(crate) fn view(&self) -> gtk::TextView {
        self.imp().view.get().expect("view initialized").clone()
    }

    pub(crate) fn selected_text(&self) -> Option<String> {
        let buffer = self.view().buffer();
        let (start, end) = buffer.selection_bounds()?;
        Some(buffer.text(&start, &end, true).to_string())
    }

    fn text_layout(&self) -> gtk::pango::Layout {
        let view = self.view();
        let buffer = view.buffer();
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
        let layout = view.create_pango_layout(Some(&text));
        layout.set_attributes(self.imp().width_attributes.borrow().as_ref());
        layout
    }

    pub(crate) fn wrapped_width(&self, available: i32) -> i32 {
        let view = self.view();
        let buffer = view.buffer();
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
        let margins = view.left_margin() + view.right_margin() + CURSOR_SPACE;
        let mut start = 0;
        let paragraphs: Vec<_> = text
            .split_inclusive('\n')
            .map(|paragraph| {
                let range = start..start + paragraph.len();
                start = range.end;
                (range, gtk::pango::AttrList::new())
            })
            .collect();
        if let Some(attributes) = self.imp().width_attributes.borrow().as_ref() {
            for attribute in attributes.attributes() {
                let start = attribute.start_index() as usize;
                let end = (attribute.end_index() as usize).min(text.len());
                let first = paragraphs
                    .partition_point(|(range, _)| range.start <= start)
                    .saturating_sub(1);
                for (range, attributes) in paragraphs[first..].iter() {
                    if range.start >= end {
                        break;
                    }
                    let mut attribute = attribute.clone();
                    attribute.set_start_index((start.max(range.start) - range.start) as u32);
                    attribute.set_end_index((end.min(range.end) - range.start) as u32);
                    attributes.insert(attribute);
                }
            }
        }
        let blocks = self.imp().blocks.borrow();
        let mut blocks = blocks.iter().peekable();
        let mut offset = 0;
        let mut widest = 0;
        for (range, attributes) in paragraphs {
            let paragraph = &text[range];
            let layout = view.create_pango_layout(Some(paragraph.trim_end_matches('\n')));
            layout.set_attributes(Some(&attributes));
            if let Some((_, marker)) = blocks.next_if(|(range, _)| range.start == offset) {
                let indent = view.create_pango_layout(Some(marker)).pixel_size().0;
                layout.set_indent(-indent * gtk::pango::SCALE);
            }
            layout.set_width((available - margins).max(1) * gtk::pango::SCALE);
            layout.set_wrap(gtk::pango::WrapMode::WordChar);
            widest = widest.max(layout.pixel_size().0);
            offset += paragraph.chars().count() as i32;
        }
        (widest + margins).min(available)
    }

    fn schedule_metrics_refresh(&self) {
        self.imp().measured.set(None);
        if self.imp().metrics_pending.replace(true) {
            return;
        }
        let weak = self.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(owner) = weak.upgrade() {
                owner.imp().metrics_pending.set(false);
                owner.refresh_block_metrics();
                owner.queue_resize();
            }
        });
    }

    fn refresh_block_metrics(&self) {
        let view = self.view();
        let buffer = view.buffer();
        let table = buffer.tag_table();
        for (_, tag) in self.imp().block_tags.borrow_mut().drain() {
            table.remove(&tag);
        }
        for (range, marker) in self.imp().blocks.borrow().iter() {
            let (width, _) = view.create_pango_layout(Some(marker)).pixel_size();
            let tag = self
                .imp()
                .block_tags
                .borrow_mut()
                .entry(width)
                .or_insert_with(|| {
                    let tag = gtk::TextTag::builder().indent(-width).build();
                    table.add(&tag);
                    tag
                })
                .clone();
            buffer.apply_tag(
                &tag,
                &buffer.iter_at_offset(range.start),
                &buffer.iter_at_offset(range.end),
            );
        }
        self.imp().font_serial.set(view.pango_context().serial());
        self.imp().measured.set(None);
    }

    pub(crate) fn connect_link_activation(
        &self,
        handler: impl for<'a> Fn(crate::safety::LinkActivation<'a>) + 'static,
    ) {
        *self.imp().link_handler.borrow_mut() = Some(Box::new(handler));
    }

    pub(crate) fn connect_code_copied(&self, handler: impl Fn(&str) + 'static) {
        *self.imp().code_handler.borrow_mut() = Some(Box::new(handler));
    }

    fn offset_at(&self, x: f64, y: f64) -> Option<i32> {
        let view = self.view();
        let (x, y) = view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        // `iter_at_location` returns the nearest cursor position, so the right
        // half of a character resolves to the next one and fails the bounds
        // check below. `iter_at_position` returns the character under the point.
        let (iter, _) = view.iter_at_position(x, y)?;
        let rect = view.iter_location(&iter);
        (x >= rect.x()
            && x < rect.x() + rect.width().max(1)
            && y >= rect.y()
            && y < rect.y() + rect.height())
        .then(|| iter.offset())
    }

    fn activate_link_at(&self, offset: i32) -> bool {
        let uri = self
            .imp()
            .links
            .borrow()
            .iter()
            .find(|link| (link.start..link.end).contains(&offset))
            .map(|link| link.uri.clone());
        let Some(uri) = uri else {
            return false;
        };
        let action = crate::safety::link_activation(&uri);
        if action == crate::safety::LinkActivation::Ignore {
            return true;
        }
        if let Some(handler) = self.imp().link_handler.borrow().as_ref() {
            handler(action);
        }
        true
    }

    fn same_link(&self, start: i32, end: i32) -> bool {
        self.imp().links.borrow().iter().any(|link| {
            (link.start..link.end).contains(&start) && (link.start..link.end).contains(&end)
        })
    }

    fn copy_code_at(&self, offset: i32) -> bool {
        let range = self
            .imp()
            .code
            .borrow()
            .iter()
            .find(|range| range.contains(&offset))
            .cloned();
        let Some(range) = range else {
            return false;
        };
        let view = self.view();
        let buffer = view.buffer();
        let start = buffer.iter_at_offset(range.start);
        let end = buffer.iter_at_offset(range.end);
        buffer.select_range(&start, &end);
        let code = buffer.text(&start, &end, true);
        view.clipboard().set_text(&code);
        if let Some(handler) = self.imp().code_handler.borrow().as_ref() {
            handler(&code);
        }
        true
    }

    fn handle_key(
        &self,
        key: gtk::gdk::Key,
        modifiers: gtk::gdk::ModifierType,
    ) -> glib::Propagation {
        if modifiers.intersects(
            gtk::gdk::ModifierType::CONTROL_MASK
                | gtk::gdk::ModifierType::ALT_MASK
                | gtk::gdk::ModifierType::SUPER_MASK,
        ) {
            self.imp().keyboard_link.set(None);
            return glib::Propagation::Proceed;
        }
        if key == gtk::gdk::Key::Tab || key == gtk::gdk::Key::ISO_Left_Tab {
            let backwards = key == gtk::gdk::Key::ISO_Left_Tab
                || modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK);
            let links = self.imp().links.borrow();
            let next = match (self.imp().keyboard_link.get(), backwards) {
                (None, false) => (!links.is_empty()).then_some(0),
                (None, true) => links.len().checked_sub(1),
                (Some(current), false) => (current + 1 < links.len()).then_some(current + 1),
                (Some(current), true) => current.checked_sub(1),
            };
            self.imp().keyboard_link.set(next);
            if let Some(index) = next {
                let buffer = self.view().buffer();
                buffer.select_range(
                    &buffer.iter_at_offset(links[index].start),
                    &buffer.iter_at_offset(links[index].end),
                );
                return glib::Propagation::Stop;
            }
        } else if matches!(
            key,
            gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter | gtk::gdk::Key::space
        ) {
            let offset = self
                .imp()
                .keyboard_link
                .get()
                .and_then(|index| self.imp().links.borrow().get(index).map(|link| link.start));
            if offset.is_some_and(|offset| self.activate_link_at(offset)) {
                return glib::Propagation::Stop;
            }
        } else {
            self.imp().keyboard_link.set(None);
        }
        glib::Propagation::Proceed
    }

    pub(crate) fn set_formatted(&self, text: &str, mentions: &[crate::safety::MentionLabel]) {
        self.imp().keyboard_link.set(None);
        self.imp().click_start.set(None);
        let markup = crate::safety::linkify_markup_with_mentions(text, mentions);
        let (markup, links, code) = text_view_markup(&markup);
        *self.imp().width_attributes.borrow_mut() = gtk::pango::parse_markup(&markup, '\0')
            .ok()
            .map(|(attributes, _, _)| attributes);
        let buffer = self.view().buffer();
        let table = buffer.tag_table();
        for (_, tag) in self.imp().block_tags.borrow_mut().drain() {
            table.remove(&tag);
        }
        for tag in self.imp().markup_tags.borrow_mut().drain(..) {
            table.remove(&tag);
        }
        buffer.set_text("");
        buffer.insert_markup(&mut buffer.start_iter(), &markup);
        let mention_tag = buffer.create_tag(None, &[]).expect("new mention tag");
        for link in links.iter().filter(|link| link.mention) {
            buffer.apply_tag(
                &mention_tag,
                &buffer.iter_at_offset(link.start),
                &buffer.iter_at_offset(link.end),
            );
        }
        *self.imp().mention_tag.borrow_mut() = Some(mention_tag);
        let link_tag = buffer.create_tag(None, &[]).expect("new link tag");
        for link in links.iter().filter(|link| !link.mention) {
            buffer.apply_tag(
                &link_tag,
                &buffer.iter_at_offset(link.start),
                &buffer.iter_at_offset(link.end),
            );
        }
        *self.imp().link_tag.borrow_mut() = Some(link_tag);
        self.imp().link_background_color.set(None);
        self.refresh_link_color();
        self.refresh_mention_color();
        table.foreach(|tag| self.imp().markup_tags.borrow_mut().push(tag.clone()));
        let displayed = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
        *self.imp().blocks.borrow_mut() = rendered_blocks(text, &displayed);
        self.refresh_block_metrics();
        *self.imp().links.borrow_mut() = links;
        *self.imp().code.borrow_mut() = code;
        self.imp().measured.set(None);
        self.queue_resize();
    }

    #[cfg(test)]
    pub(crate) fn set_block_text(&self, text: &str) {
        *self.imp().width_attributes.borrow_mut() = None;
        self.imp().keyboard_link.set(None);
        self.imp().click_start.set(None);
        self.imp().links.borrow_mut().clear();
        self.imp().code.borrow_mut().clear();
        self.imp().blocks.borrow_mut().clear();
        let view = self.view();
        let buffer = view.buffer();
        let table = buffer.tag_table();
        for tag in self.imp().markup_tags.borrow_mut().drain(..) {
            table.remove(&tag);
        }
        for (_, tag) in self.imp().block_tags.borrow_mut().drain() {
            table.remove(&tag);
        }
        buffer.set_text(text);
        let mut offsets = vec![0i32; text.len() + 1];
        let mut count = 0;
        for (byte, _) in text.char_indices() {
            offsets[byte] = count;
            count += 1;
        }
        offsets[text.len()] = count;
        for span in crate::safety::formatting_spans(text) {
            let Some(crate::safety::BlockRanges { marker, content }) =
                crate::safety::block_ranges(text, &span)
            else {
                continue;
            };
            let (width, _) = view
                .create_pango_layout(Some(&text[marker.clone()]))
                .pixel_size();
            self.imp().blocks.borrow_mut().push((
                offsets[marker.start]..offsets[content.end],
                text[marker.clone()].to_owned(),
            ));
            let tag = self
                .imp()
                .block_tags
                .borrow_mut()
                .entry(width)
                .or_insert_with(|| {
                    let tag = gtk::TextTag::builder().indent(-width).build();
                    table.add(&tag);
                    tag
                })
                .clone();
            buffer.apply_tag(
                &tag,
                &buffer.iter_at_offset(offsets[marker.start]),
                &buffer.iter_at_offset(offsets[content.end]),
            );
        }
        self.imp().font_serial.set(view.pango_context().serial());
        self.imp().measured.set(None);
        self.queue_resize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc, time::Duration};

    #[test]
    fn blue_links_contrast_with_light_dark_and_custom_bubbles() {
        for background in [
            crate::color::Color::from_rgb(255, 255, 255),
            crate::color::Color::from_rgb(30, 30, 30),
            crate::color::Color::from_rgb(120, 120, 120),
            crate::color::Color::from_rgb(117, 117, 117),
            crate::color::Color::from_rgb(118, 118, 118),
            crate::color::Color::from_rgb(50, 110, 150),
        ] {
            let (foreground, backplate) = link_colors(background);
            assert!(u16::from(foreground.b) - u16::from(foreground.r) >= 100);
            assert!(
                crate::native_theme::contrast_ratio(backplate.unwrap_or(background), foreground)
                    >= 4.5
            );
        }
    }

    #[test]
    #[ignore = "needs a display"]
    fn web_links_follow_bubble_colors_without_changing_selection_or_keyboard_activation() {
        gtk::init().unwrap();
        libadwaita::init().unwrap();
        let bubble = TextBubble::new();
        bubble.set_formatted("See https://example.com now", &[]);
        let other = TextBubble::new();
        other.set_formatted("See https://example.com now", &[]);
        let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
        for (child, class) in [(&bubble, "incoming"), (&other, "outgoing")] {
            let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
            container.add_css_class("zaptide-bubble");
            container.add_css_class(class);
            container.append(child);
            layout.append(&container);
        }
        let window = gtk::Window::builder()
            .default_width(400)
            .child(&layout)
            .build();
        let provider = gtk::CssProvider::new();
        provider.load_from_string(include_str!("../../data/style.css"));
        let palette_provider = gtk::CssProvider::new();
        let display = gtk::gdk::Display::default().unwrap();
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        gtk::style_context_add_provider_for_display(
            &display,
            &palette_provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
        window.present();
        let context = glib::MainContext::default();
        let settle = || {
            let until = std::time::Instant::now() + Duration::from_millis(200);
            while std::time::Instant::now() < until {
                while context.pending() {
                    context.iteration(false);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        palette_provider.load_from_string(include_str!("../../data/theme-defaults.css"));
        let manager = libadwaita::StyleManager::default();
        for scheme in [
            libadwaita::ColorScheme::ForceLight,
            libadwaita::ColorScheme::ForceDark,
        ] {
            manager.set_color_scheme(scheme);
            settle();
            for child in [&bubble, &other] {
                let background = child.imp().link_background.get().unwrap().color();
                let channel = |value: f32| (value * 255.0).round() as u8;
                let background = crate::color::Color::from_rgb(
                    channel(background.red()),
                    channel(background.green()),
                    channel(background.blue()),
                );
                let (foreground, backplate) = link_colors(background);
                let tag = child.imp().link_tag.borrow().clone().unwrap();
                let actual = tag.foreground_rgba().unwrap();
                assert_eq!(
                    (
                        channel(actual.red()),
                        channel(actual.green()),
                        channel(actual.blue())
                    ),
                    (foreground.r, foreground.g, foreground.b)
                );
                assert!(
                    crate::native_theme::contrast_ratio(
                        backplate.unwrap_or(background),
                        foreground
                    ) >= 4.5
                );
            }
        }
        manager.set_color_scheme(libadwaita::ColorScheme::Default);
        for (incoming, outgoing) in [
            (
                crate::color::Color::from_rgb(255, 255, 255),
                crate::color::Color::from_rgb(230, 245, 235),
            ),
            (
                crate::color::Color::from_rgb(34, 34, 34),
                crate::color::Color::from_rgb(25, 60, 45),
            ),
            (
                crate::color::Color::from_rgb(117, 117, 117),
                crate::color::Color::from_rgb(118, 118, 118),
            ),
            (
                crate::color::Color::from_rgb(255, 255, 255),
                crate::color::Color::from_rgb(34, 34, 34),
            ),
        ] {
            let mut palette = crate::theme::Palette::light();
            palette.bubble_in = incoming;
            palette.bubble_out = outgoing;
            palette_provider.load_from_string(&crate::native_theme::css_for_palette(&palette));
            settle();
            for (child, background) in [(&bubble, incoming), (&other, outgoing)] {
                let buffer = child.view().buffer();
                let tag = child.imp().link_tag.borrow().clone().unwrap();
                assert!(buffer.iter_at_offset(5).tags().contains(&tag));
                assert!(!buffer.iter_at_offset(0).tags().contains(&tag));
                let (foreground, backplate) = link_colors(background);
                let rgba = |c: crate::color::Color| {
                    gtk::gdk::RGBA::new(
                        c.r as f32 / 255.0,
                        c.g as f32 / 255.0,
                        c.b as f32 / 255.0,
                        1.0,
                    )
                };
                assert_eq!(tag.foreground_rgba(), Some(rgba(foreground)));
                assert_eq!(tag.background_rgba(), backplate.map(rgba));
            }
            let snapshot = gtk::Snapshot::new();
            gtk::WidgetPaintable::new(Some(&window)).snapshot(
                &snapshot,
                f64::from(window.width()),
                f64::from(window.height()),
            );
            let texture = window
                .renderer()
                .unwrap()
                .render_texture(snapshot.to_node().unwrap(), None);
            let (width, height) = (texture.width() as usize, texture.height() as usize);
            let mut pixels = vec![0; width * height * 4];
            texture.download(&mut pixels, width * 4);
            assert!(
                pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[0] > pixel[2].saturating_add(40) && pixel[3] == 255),
                "no blue pixels rendered"
            );
        }
        let activated = Rc::new(RefCell::new(false));
        let result = activated.clone();
        bubble.connect_link_activation(move |_| *result.borrow_mut() = true);
        assert_eq!(
            bubble.handle_key(gtk::gdk::Key::Tab, gtk::gdk::ModifierType::empty()),
            glib::Propagation::Stop
        );
        assert_eq!(
            bubble.selected_text().as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            bubble.handle_key(gtk::gdk::Key::Return, gtk::gdk::ModifierType::empty()),
            glib::Propagation::Stop
        );
        assert!(*activated.borrow());
        gtk::style_context_remove_provider_for_display(&display, &provider);
        gtk::style_context_remove_provider_for_display(&display, &palette_provider);
        window.close();
    }

    #[test]
    fn mention_links_are_bold_without_changing_web_links_or_offsets() {
        let mentions = [crate::safety::MentionLabel {
            user: "15551234567".into(),
            label: "Ana & Bia".into(),
            phone: Some("15551234567".into()),
            hint: None,
        }];
        let markup = crate::safety::linkify_markup_with_mentions(
            "😀 @15551234567 https://example.com",
            &mentions,
        );
        let (markup, links, _) = text_view_markup(&markup);
        assert!(markup.contains("<span weight=\"bold\">@Ana &amp; Bia</span>"));
        assert!(markup.contains("<span underline=\"single\">https://example.com</span>"));
        assert!(links[0].mention);
        assert_eq!(links[0].start, 2);
        assert_eq!(links[0].end, 12);
        assert!(!links[1].mention);
    }

    #[test]
    #[ignore = "needs a display"]
    fn mentions_follow_the_theme_accent_and_keep_keyboard_activation() {
        gtk::init().unwrap();
        libadwaita::init().unwrap();
        let bubble = TextBubble::new();
        let mentions = [crate::safety::MentionLabel {
            user: "15551234567".into(),
            label: "Synthetic Contact".into(),
            phone: Some("15551234567".into()),
            hint: None,
        }];
        bubble.set_formatted("Hi @15551234567", &mentions);
        let activated = Rc::new(RefCell::new(String::new()));
        let result = activated.clone();
        bubble.connect_link_activation(move |action| {
            if let crate::safety::LinkActivation::Chat(phone) = action {
                *result.borrow_mut() = phone.to_owned();
            }
        });
        let window = gtk::Window::builder()
            .default_width(360)
            .child(&bubble)
            .build();
        window.present();
        let context = glib::MainContext::default();
        let settle = || {
            let until = std::time::Instant::now() + Duration::from_millis(200);
            while std::time::Instant::now() < until {
                while context.pending() {
                    context.iteration(false);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        let provider = gtk::CssProvider::new();
        let display = gtk::gdk::Display::default().unwrap();
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
        for color in ["#3584e4", "#c061cb"] {
            provider.load_from_string(&format!(":root {{ --accent-color: {color}; }}"));
            bubble.queue_draw();
            settle();
            let tags = bubble.view().buffer().iter_at_offset(3).tags();
            assert!(tags.iter().any(|tag| tag.weight() >= 700));
            assert!(
                tags.iter()
                    .any(|tag| tag.foreground_rgba() == Some(color.parse().unwrap())),
                "expected {color}, source {:?}, tags {:?}",
                bubble.imp().mention_color.get().unwrap().color(),
                tags.iter()
                    .map(|tag| tag.foreground_rgba())
                    .collect::<Vec<_>>()
            );
            assert!(
                !tags
                    .iter()
                    .any(|tag| tag.underline() == gtk::pango::Underline::Single)
            );
            if let Some(directory) = std::env::var_os("RENDER_DIR") {
                let directory = std::path::PathBuf::from(directory);
                std::fs::create_dir_all(&directory).unwrap();
                let snapshot = gtk::Snapshot::new();
                gtk::WidgetPaintable::new(Some(&window)).snapshot(
                    &snapshot,
                    f64::from(window.width()),
                    f64::from(window.height()),
                );
                window
                    .renderer()
                    .unwrap()
                    .render_texture(snapshot.to_node().unwrap(), None)
                    .save_to_png(directory.join(format!("mention-{}.png", &color[1..])))
                    .unwrap();
            }
        }
        assert_eq!(
            bubble.handle_key(gtk::gdk::Key::Tab, gtk::gdk::ModifierType::empty()),
            glib::Propagation::Stop
        );
        assert_eq!(
            bubble.handle_key(gtk::gdk::Key::Return, gtk::gdk::ModifierType::empty()),
            glib::Propagation::Stop
        );
        assert_eq!(&*activated.borrow(), "15551234567");
        gtk::style_context_remove_provider_for_display(&display, &provider);
        window.close();
    }

    #[test]
    #[ignore = "needs a display"]
    fn message_text_requests_a_natural_width_that_fits_the_content() {
        gtk::init().unwrap();
        let bubble = TextBubble::new();
        bubble.set_formatted("Tinha esquecido de pagar o cabelo", &[]);
        let (_, natural, _, _) = bubble.measure(gtk::Orientation::Horizontal, -1);
        let (text_width, _) = bubble
            .view()
            .create_pango_layout(Some("Tinha esquecido de pagar o cabelo"))
            .pixel_size();
        assert!(
            natural >= text_width,
            "natural width {natural}, text width {text_width}"
        );

        bubble.set_formatted("*bold* `monospace`", &[]);
        let markup = crate::safety::linkify_markup_with_mentions("*bold* `monospace`", &[]);
        let (markup, _, _) = text_view_markup(&markup);
        let (attrs, text, _) = gtk::pango::parse_markup(&markup, '\0').unwrap();
        let layout = bubble.view().create_pango_layout(Some(&text));
        layout.set_attributes(Some(&attrs));
        let (_, natural, _, _) = bubble.measure(gtk::Orientation::Horizontal, -1);
        assert!(natural >= layout.pixel_size().0);
    }

    #[test]
    #[ignore = "needs a display"]
    fn media_card_renders_all_formats_and_remeasures_font_changes() {
        gtk::init().unwrap();
        let raw = "*bold* _it_ ~strike~ `é😀` ```mono```\n- bullet\n123456789. one two three four five six seven eight nine ten eleven twelve thirteen https://example.com\n> quote";
        let message = crate::model::Message {
            id: "synthetic".into(),
            chat: "synthetic".into(),
            sender: "synthetic".into(),
            sender_name: None,
            from_me: false,
            timestamp: 0,
            content: crate::model::Content::Buttons {
                text: raw.into(),
                footer: None,
                buttons: Vec::new(),
                answered: None,
            },
            status: crate::model::Delivery::None,
            delivered_at: None,
            read_at: None,
            quoted: None,
            reactions: Vec::new(),
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: None,
        };
        let rendered = super::super::build_media_widget_with_action(&message, |_| {});
        let parent = rendered.widget.clone();
        let bubble = parent
            .first_child()
            .unwrap()
            .downcast::<TextBubble>()
            .unwrap();
        let window = gtk::Window::builder()
            .default_width(240)
            .default_height(300)
            .child(&parent)
            .build();
        window.present();
        let context = glib::MainContext::default();
        let settle = || {
            let until = std::time::Instant::now() + Duration::from_millis(400);
            while std::time::Instant::now() < until {
                while context.pending() {
                    context.iteration(false);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        settle();
        let view = bubble.view();
        let buffer = view.buffer();
        let shown = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
        assert!(shown.contains("bold it strike é😀 mono\n• bullet\n123456789."));
        assert!(shown.ends_with("▎ quote"));
        assert!(
            buffer
                .iter_at_offset(0)
                .tags()
                .iter()
                .any(|tag| tag.weight() >= 700)
        );
        assert!(
            buffer
                .iter_at_offset(5)
                .tags()
                .iter()
                .any(|tag| tag.style() == gtk::pango::Style::Italic)
        );
        assert!(
            buffer
                .iter_at_offset(8)
                .tags()
                .iter()
                .any(|tag| tag.is_strikethrough())
        );
        assert_eq!(bubble.imp().links.borrow().len(), 1);
        assert_eq!(bubble.imp().code.borrow().len(), 2);
        let narrow_width = *bubble.imp().block_tags.borrow().keys().max().unwrap();
        let initial_height = bubble.height();
        let provider = gtk::CssProvider::new();
        provider.load_from_string("textview.synthetic-large-font { font-size: 30px; }");
        let display = gtk::gdk::Display::default().unwrap();
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
        view.add_css_class("synthetic-large-font");
        settle();
        let large_width = *bubble.imp().block_tags.borrow().keys().max().unwrap();
        assert!(
            large_width > narrow_width,
            "marker widths: {narrow_width}, {large_width}"
        );
        assert!(
            bubble.height() > initial_height,
            "heights: {initial_height}, {}",
            bubble.height()
        );
        view.remove_css_class("synthetic-large-font");
        gtk::style_context_remove_provider_for_display(&display, &provider);
        settle();
        assert_eq!(
            *bubble.imp().block_tags.borrow().keys().max().unwrap(),
            narrow_width
        );
        window.close();
    }

    #[test]
    #[ignore = "needs a display"]
    fn formatted_blocks_keep_links_code_and_measured_wrap_on_rebind() {
        gtk::init().unwrap();
        let bubble = TextBubble::new();
        let text = "```\nprelude\n```\n123456789. `é😀` https://example.com one two three four five six seven eight nine ten eleven twelve thirteen";
        bubble.set_formatted(text, &[]);
        let window = gtk::Window::builder()
            .default_width(220)
            .default_height(250)
            .child(&bubble)
            .build();
        window.present();
        let context = glib::MainContext::default();
        let until = std::time::Instant::now() + Duration::from_millis(400);
        while std::time::Instant::now() < until {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let view = bubble.view();
        let buffer = view.buffer();
        let displayed = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
        assert!(displayed.starts_with("prelude\n123456789. é😀 https://example.com"));
        assert_eq!(bubble.imp().links.borrow().len(), 1);
        assert_eq!(bubble.imp().code.borrow().len(), 2);
        let block = buffer.iter_at_line(1).unwrap();
        let content = buffer.iter_at_offset(block.offset() + 11);
        let mut continuation = block;
        assert!(view.forward_display_line(&mut continuation));
        assert_eq!(
            view.iter_location(&content).x(),
            view.iter_location(&continuation).x()
        );
        assert!(bubble.imp().block_tags.borrow().len() == 1);
        bubble.set_formatted("- fresh *bold*", &[]);
        assert_eq!(
            buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
            "• fresh bold"
        );
        assert!(bubble.imp().links.borrow().is_empty());
        assert!(bubble.imp().code.borrow().is_empty());
        assert_eq!(bubble.imp().block_tags.borrow().len(), 1);
        window.close();
    }

    #[test]
    #[ignore = "needs a display"]
    fn interaction_activates_safe_links_and_copies_unicode_code() {
        gtk::init().unwrap();
        let bubble = TextBubble::new();
        let mentions = [crate::safety::MentionLabel {
            user: "123".into(),
            label: "Éva".into(),
            phone: Some("123".into()),
            hint: None,
        }];
        bubble.set_formatted("😀 @123 https://example.com `é😀`", &mentions);
        let window = gtk::Window::builder()
            .default_width(500)
            .default_height(120)
            .child(&bubble)
            .build();
        window.present();
        let context = glib::MainContext::default();
        let until = std::time::Instant::now() + Duration::from_millis(300);
        while std::time::Instant::now() < until {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let activated = Rc::new(RefCell::new(Vec::new()));
        let found = activated.clone();
        bubble.connect_link_activation(move |action| match action {
            crate::safety::LinkActivation::Chat(phone) => {
                found.borrow_mut().push(format!("chat:{phone}"))
            }
            crate::safety::LinkActivation::Web(url) => found.borrow_mut().push(url.to_string()),
            crate::safety::LinkActivation::Ignore => panic!("rejected link reached handler"),
        });
        let copied = Rc::new(RefCell::new(Vec::new()));
        let found = copied.clone();
        bubble.connect_code_copied(move |code| found.borrow_mut().push(code.to_owned()));
        let view = bubble.view();
        let controllers = view.observe_controllers();
        let mut click = None;
        let mut keys = None;
        for index in 0..controllers.n_items() {
            let controller = controllers.item(index).unwrap();
            if let Ok(gesture) = controller.clone().downcast::<gtk::GestureClick>()
                && gesture.propagation_phase() == gtk::PropagationPhase::Capture
            {
                click = Some(gesture);
            }
            if let Ok(controller) = controller.downcast::<gtk::EventControllerKey>()
                && controller.propagation_phase() == gtk::PropagationPhase::Capture
            {
                keys = Some(controller);
            }
        }
        let click = click.unwrap();
        let coords = |offset| {
            let rect = view.iter_location(&view.buffer().iter_at_offset(offset));
            let (x, y) =
                view.buffer_to_window_coords(gtk::TextWindowType::Widget, rect.x(), rect.y());
            (f64::from(x + 1), f64::from(y + rect.height() / 2))
        };
        let send_click = |presses: i32, offset: i32| {
            let (x, y) = coords(offset);
            click.emit_by_name::<()>("pressed", &[&presses, &x, &y]);
            click.emit_by_name::<()>("released", &[&presses, &x, &y]);
        };
        let mention_offset = bubble.imp().links.borrow()[0].start;
        send_click(1, mention_offset - 1);
        assert!(activated.borrow().is_empty());
        send_click(1, mention_offset);
        assert_eq!(&*activated.borrow(), &["chat:123"]);
        let code_offset = bubble.imp().code.borrow()[0].start;
        send_click(2, code_offset);
        let (start, end) = view.buffer().selection_bounds().unwrap();
        assert_eq!(view.buffer().text(&start, &end, true), "é😀");
        assert_eq!(&*copied.borrow(), &["é😀"]);
        assert_eq!(
            context
                .block_on(view.clipboard().read_text_future())
                .unwrap()
                .unwrap(),
            "é😀"
        );
        view.buffer().place_cursor(&view.buffer().start_iter());
        let keys = keys.unwrap();
        let press_key = |key: gtk::gdk::Key| {
            keys.emit_by_name::<bool>(
                "key-pressed",
                &[&key, &0u32, &gtk::gdk::ModifierType::empty()],
            )
        };
        assert!(press_key(gtk::gdk::Key::Tab));
        assert!(press_key(gtk::gdk::Key::Return));
        assert!(press_key(gtk::gdk::Key::Tab));
        assert!(press_key(gtk::gdk::Key::Return));
        assert_eq!(
            &*activated.borrow(),
            &["chat:123", "chat:123", "https://example.com/"]
        );
        bubble.imp().links.borrow_mut()[0].uri = "file:///etc/passwd".into();
        view.buffer().place_cursor(&view.buffer().start_iter());
        send_click(1, mention_offset);
        assert_eq!(activated.borrow().len(), 3);
        bubble.set_formatted("replacement 😀 without links or code", &[]);
        assert!(!press_key(gtk::gdk::Key::Tab));
        assert!(!press_key(gtk::gdk::Key::Return));
        assert_eq!(activated.borrow().len(), 3);
        assert!(!bubble.copy_code_at(code_offset));
        assert_eq!(copied.borrow().len(), 1);
        window.close();
    }

    #[test]
    #[ignore = "needs a display"]
    fn formatted_buffer_matches_rendered_text_and_tracks_unicode_links() {
        gtk::init().unwrap();
        let bubble = TextBubble::new();
        let mentions = [crate::safety::MentionLabel {
            user: "123".into(),
            label: "Éva".into(),
            phone: Some("123".into()),
            hint: None,
        }];
        let text = "مرحبا *bold* `é😀` @123 https://example.com/?a=1&b=2";
        bubble.set_formatted(text, &mentions);
        let buffer = bubble.view().buffer();
        let displayed = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
        let label = gtk::Label::new(None);
        label.set_markup(&crate::safety::linkify_markup_with_mentions(
            text, &mentions,
        ));
        assert_eq!(displayed, label.text());
        let links = bubble.imp().links.borrow();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].uri, "zaptide-chat:123");
        assert_eq!(links[1].uri, "https://example.com/?a=1&b=2");
        for link in links.iter() {
            assert!(
                !buffer
                    .text(
                        &buffer.iter_at_offset(link.start),
                        &buffer.iter_at_offset(link.end),
                        true
                    )
                    .is_empty()
            );
        }
        let code = bubble.imp().code.borrow();
        assert_eq!(code.len(), 1);
        assert_eq!(
            buffer.text(
                &buffer.iter_at_offset(code[0].start),
                &buffer.iter_at_offset(code[0].end),
                true
            ),
            "é😀"
        );
    }

    #[test]
    #[ignore = "needs a display"]
    fn repeated_markers_share_tags_and_rebind_discards_old_tags() {
        gtk::init().unwrap();
        let bubble = TextBubble::new();
        bubble.set_block_text("- one\n- two\n- three");
        assert_eq!(bubble.imp().block_tags.borrow().len(), 1);
        let previous = bubble
            .imp()
            .block_tags
            .borrow()
            .values()
            .next()
            .unwrap()
            .clone();
        bubble.set_block_text("123456789. novo\n123456789. texto");
        assert_eq!(bubble.imp().block_tags.borrow().len(), 1);
        assert_ne!(
            &previous,
            bubble.imp().block_tags.borrow().values().next().unwrap()
        );
        let buffer = bubble.view().buffer();
        assert_eq!(
            buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
            "123456789. novo\n123456789. texto"
        );
    }

    #[test]
    #[ignore = "needs a display"]
    fn numbered_block_wraps_at_measured_marker_width() {
        gtk::init().unwrap();
        let bubble = TextBubble::new();
        let text = "123456789. one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen";
        bubble.set_block_text(text);
        let window = gtk::Window::builder()
            .default_width(220)
            .default_height(220)
            .child(&bubble)
            .build();
        window.present();
        let context = glib::MainContext::default();
        let until = std::time::Instant::now() + Duration::from_millis(400);
        while std::time::Instant::now() < until {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let view = bubble.view();
        let buffer = view.buffer();
        let content = buffer.iter_at_offset(11);
        let mut continuation = buffer.start_iter();
        assert!(view.forward_display_line(&mut continuation));
        let content_rect = view.iter_location(&content);
        let continuation_rect = view.iter_location(&continuation);
        assert!(continuation_rect.y() > content_rect.y());
        assert_eq!(continuation_rect.x(), content_rect.x());
        assert_eq!(
            buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
            text
        );
        window.close();
    }

    #[test]
    #[ignore = "needs a display"]
    fn list_view_remeasures_after_content_and_width_change() {
        gtk::init().unwrap();
        let current = Rc::new(RefCell::new(None::<TextBubble>));
        let factory = gtk::SignalListItemFactory::new();
        let setup_current = current.clone();
        factory.connect_setup(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let bubble = TextBubble::new();
            item.set_child(Some(&bubble));
            *setup_current.borrow_mut() = Some(bubble);
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let bubble = item.child().and_downcast::<TextBubble>().unwrap();
            let text = item.item().and_downcast::<gtk::StringObject>().unwrap();
            bubble.view().buffer().set_text(&text.string());
        });
        let strings = gtk::StringList::new(&[
            "123. A wrapped synthetic item repeated repeatedly repeatedly repeatedly repeatedly repeatedly repeatedly repeatedly.",
        ]);
        let selection = gtk::NoSelection::new(Some(strings.clone()));
        let list = gtk::ListView::new(Some(selection), Some(factory));
        let window = gtk::Window::builder()
            .default_width(240)
            .default_height(300)
            .child(&list)
            .build();
        window.present();
        let settle = || {
            let context = glib::MainContext::default();
            let until = std::time::Instant::now() + Duration::from_millis(400);
            while std::time::Instant::now() < until {
                while context.pending() {
                    context.iteration(false);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        settle();
        let bubble = current.borrow().as_ref().unwrap().clone();
        let narrow_height = bubble.height();
        let narrow_width = bubble.width();
        assert!(narrow_height > 0);
        window.set_default_size(480, 300);
        settle();
        eprintln!(
            "narrow={narrow_width}x{narrow_height} wide={}x{}",
            bubble.width(),
            bubble.height()
        );
        assert!(bubble.width() > narrow_width);
        assert!(bubble.height() > 0);
        assert!(bubble.height() <= narrow_height);
        strings.splice(0, 1, &["replacement"]);
        settle();
        let recycled = current.borrow().as_ref().unwrap().clone();
        assert_eq!(
            recycled.view().buffer().text(
                &recycled.view().buffer().start_iter(),
                &recycled.view().buffer().end_iter(),
                true
            ),
            "replacement"
        );
        assert!(recycled.height() > 0);
        window.close();
    }
}
