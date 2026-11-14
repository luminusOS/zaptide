//! Quick reactions shown over the message under the pointer.
//!
//! One pill serves every row. It is built on the first hover and moved to
//! the bubble the pointer enters, so a long transcript pays nothing per row.

use super::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

pub(super) const QUICK_SLOTS: usize = 6;

const DEFAULT_REACTIONS: [&str; QUICK_SLOTS] = ["👍", "❤️", "😂", "😮", "😢", "🙏"];

/// Sweeping across the transcript should not flash a pill on every bubble.
const SHOW_DELAY: Duration = Duration::from_millis(120);

/// Long enough to cross from the bubble onto the pill, gap included.
const HIDE_DELAY: Duration = Duration::from_millis(250);

/// Space between the pill and the bubble's top edge, so the pill never covers
/// the sender's name or the first line.
const GAP: f32 = 8.0;

/// Keeps the pill inside the list when the bubble's top has scrolled away.
const MIN_TOP: i32 = 44;

/// The emoji for the quick reactions: the latest ones used, then WhatsApp's
/// usual six for any slot the history does not fill.
pub(super) fn quick_reactions(recent: &[String]) -> Vec<String> {
    let mut quick: Vec<String> = Vec::with_capacity(QUICK_SLOTS);
    for emoji in recent
        .iter()
        .map(String::as_str)
        .chain(DEFAULT_REACTIONS)
        .filter(|emoji| !emoji.trim().is_empty())
    {
        if quick.len() == QUICK_SLOTS {
            break;
        }
        if !quick.iter().any(|known| same_emoji(known, emoji)) {
            quick.push(emoji.to_owned());
        }
    }
    quick
}

/// Moves `emoji` to the front of `recent`, keeping only what the quick
/// reactions can show. Returns whether the list changed.
pub(super) fn remember_reaction(recent: &mut Vec<String>, emoji: &str) -> bool {
    if emoji.trim().is_empty() || recent.first().is_some_and(|first| first == emoji) {
        return false;
    }
    recent.retain(|known| !same_emoji(known, emoji));
    recent.insert(0, emoji.to_owned());
    recent.truncate(QUICK_SLOTS);
    true
}

/// Whether a reaction to `message` can be sent: not a call event, a deleted
/// message, or one that has not reached WhatsApp.
pub(super) fn can_react(message: &crate::model::Message) -> bool {
    use crate::model::{Content, Delivery};
    !matches!(message.content, Content::Revoked | Content::Call { .. })
        && !matches!(message.status, Delivery::Pending | Delivery::Failed)
}

/// The reaction we sent, if any.
pub(super) fn own_reaction(message: &crate::model::Message) -> Option<&str> {
    message
        .reactions
        .iter()
        .find(|reaction| reaction.from_me && !reaction.emoji.is_empty())
        .map(|reaction| reaction.emoji.as_str())
}

/// The presentation selector is optional, so "❤" and "❤️" are one emoji.
pub(super) fn same_emoji(a: &str, b: &str) -> bool {
    let bare = |text: &str| {
        text.chars()
            .filter(|c| *c != '\u{FE0F}')
            .collect::<String>()
    };
    bare(a) == bare(b)
}

/// The message under the pointer, as its row last bound it.
#[derive(Clone)]
pub(super) struct HoverTarget {
    pub id: String,
    pub outgoing: bool,
    pub mine: Option<String>,
    pub sender: ComponentSender<NativeApplication>,
}

/// Written by a row when it binds, read when the pointer enters its bubble.
pub(super) type HoverSlot = Rc<RefCell<Option<HoverTarget>>>;

struct Shown {
    target: HoverTarget,
    bubble: glib::WeakRef<gtk::Widget>,
}

#[derive(Default)]
struct Drawer {
    recent: RefCell<Vec<String>>,
    /// What the buttons currently offer, left to right.
    quick: RefCell<Vec<String>>,
    shown: RefCell<Option<Shown>>,
    timer: RefCell<Option<glib::SourceId>>,
    pill: RefCell<Option<glib::WeakRef<gtk::Box>>>,
    /// The message menu is open and already offers the same reactions.
    suspended: Cell<bool>,
}

thread_local! {
    static DRAWER: Rc<Drawer> = Rc::default();
}

fn drawer() -> Rc<Drawer> {
    DRAWER.with(Rc::clone)
}

/// Gives the pill the emoji reactions were last sent with.
pub(super) fn set_recent(recent: &[String]) {
    *drawer().recent.borrow_mut() = recent.to_vec();
}

/// The emoji the pill offers now, for the message menu to share.
pub(super) fn current_quick() -> Vec<String> {
    quick_reactions(&drawer().recent.borrow())
}

pub(super) fn pointer_entered(
    bubble: &gtk::Box,
    controller: &gtk::EventControllerMotion,
    slot: &HoverSlot,
) {
    let touch = controller
        .current_event_device()
        .is_some_and(|device| device.source() == gtk::gdk::InputSource::Touchscreen);
    if touch || slot.borrow().is_none() || drawer().suspended.get() {
        return;
    }
    let drawer = drawer();
    if drawer.shown.borrow().is_some() {
        drawer.cancel_timer();
        drawer.present(bubble.upcast_ref(), slot);
        return;
    }
    let (bubble, slot) = (bubble.downgrade(), slot.clone());
    drawer.after(SHOW_DELAY, move |drawer| {
        if let Some(bubble) = bubble.upgrade() {
            drawer.present(bubble.upcast_ref(), &slot);
        }
    });
}

pub(super) fn pointer_left(bubble: &gtk::Box) {
    let drawer = drawer();
    if drawer.shown_on(bubble.upcast_ref()) {
        drawer.hide_soon();
    } else {
        drawer.cancel_timer();
    }
}

/// A row bound a message while the pill may be showing for its bubble.
pub(super) fn rebound(bubble: &gtk::Box) {
    let drawer = drawer();
    if drawer.shown_on(bubble.upcast_ref()) {
        drawer.hide();
    }
}

pub(super) fn dismiss() {
    drawer().hide();
}

/// Hides the pill and keeps it hidden until [`resume`], while the message
/// menu shows its own reactions. Opening the menu moves the pointer grab,
/// which reports the pointer entering the bubble again.
pub(super) fn suspend() {
    let drawer = drawer();
    drawer.hide();
    drawer.suspended.set(true);
}

pub(super) fn resume() {
    drawer().suspended.set(false);
}

impl Drawer {
    fn cancel_timer(&self) {
        if let Some(source) = self.timer.borrow_mut().take() {
            source.remove();
        }
    }

    fn after(self: &Rc<Self>, delay: Duration, action: impl FnOnce(&Rc<Self>) + 'static) {
        self.cancel_timer();
        let drawer = self.clone();
        let source = glib::timeout_add_local_once(delay, move || {
            drawer.timer.borrow_mut().take();
            action(&drawer);
        });
        *self.timer.borrow_mut() = Some(source);
    }

    fn hide_soon(self: &Rc<Self>) {
        self.after(HIDE_DELAY, |drawer| drawer.hide());
    }

    fn hide(&self) {
        self.cancel_timer();
        self.shown.borrow_mut().take();
        let pill = self.pill.borrow().as_ref().and_then(|weak| weak.upgrade());
        if let Some(pill) = pill {
            pill.set_visible(false);
        }
    }

    fn shown_on(&self, bubble: &gtk::Widget) -> bool {
        self.shown
            .borrow()
            .as_ref()
            .and_then(|shown| shown.bubble.upgrade())
            .is_some_and(|shown| &shown == bubble)
    }

    fn present(self: &Rc<Self>, bubble: &gtk::Widget, slot: &HoverSlot) {
        if self.suspended.get() {
            return;
        }
        let Some(target) = slot.borrow().clone() else {
            return;
        };
        let Some(list) = bubble.ancestor(gtk::ListView::static_type()) else {
            return;
        };
        if bubble.compute_bounds(&list).is_none() {
            return;
        }
        let Some(pill) = self.pill_in(&list) else {
            return;
        };
        let quick = quick_reactions(&self.recent.borrow());
        refresh_buttons(&pill, &quick, target.mine.as_deref());
        *self.quick.borrow_mut() = quick;
        *self.shown.borrow_mut() = Some(Shown {
            target,
            bubble: bubble.downgrade(),
        });
        pill.set_visible(true);
        if let Some(parent) = pill.parent() {
            parent.queue_allocate();
        }
    }

    fn pill_in(self: &Rc<Self>, list: &gtk::Widget) -> Option<gtk::Box> {
        let overlay = list
            .ancestor(gtk::Overlay::static_type())?
            .downcast::<gtk::Overlay>()
            .ok()?;
        let existing = self.pill.borrow().as_ref().and_then(|weak| weak.upgrade());
        if let Some(pill) = existing {
            if pill.parent().as_ref() == Some(overlay.upcast_ref()) {
                return Some(pill);
            }
            if let Some(parent) = pill.parent().and_downcast::<gtk::Overlay>() {
                parent.remove_overlay(&pill);
            }
        }
        let pill = self.build(list, &overlay);
        *self.pill.borrow_mut() = Some(pill.downgrade());
        Some(pill)
    }

    fn build(self: &Rc<Self>, list: &gtk::Widget, overlay: &gtk::Overlay) -> gtk::Box {
        let pill = gtk::Box::builder()
            .spacing(2)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .can_focus(false)
            .visible(false)
            .css_classes(["card", "zaptide-quick-reactions"])
            .build();
        for index in 0..QUICK_SLOTS {
            let button = pill_button();
            button.connect_clicked(move |_| drawer().choose(index));
            pill.append(&button);
        }
        let more = pill_button();
        more.set_icon_name("list-add-symbolic");
        let label = gettext("More Reactions");
        more.set_tooltip_text(Some(&label));
        more.update_property(&[gtk::accessible::Property::Label(&label)]);
        more.connect_clicked(|button| drawer().choose_another(button));
        pill.append(&more);

        overlay.add_overlay(&pill);
        overlay.set_measure_overlay(&pill, false);

        let drawer = Rc::downgrade(self);
        let weak_pill = pill.downgrade();
        let position = overlay.connect_get_child_position(move |overlay, child| {
            let pill = weak_pill.upgrade()?;
            if child != pill.upcast_ref::<gtk::Widget>() {
                return None;
            }
            let drawer = drawer.upgrade()?;
            let shown = drawer.shown.borrow();
            let shown = shown.as_ref()?;
            let bubble = shown.bubble.upgrade()?;
            let list = bubble.ancestor(gtk::ListView::static_type())?;
            let bounds = bubble.compute_bounds(&list)?;
            let anchor = list.compute_point(
                overlay,
                &gtk::graphene::Point::new(bounds.x(), pill_anchor_top(bounds.y()) as f32),
            )?;
            let (_, width, _, _) = pill.measure(gtk::Orientation::Horizontal, -1);
            let (_, height, _, _) = pill.measure(gtk::Orientation::Vertical, width);
            Some(pill_bounds(
                anchor,
                bounds.width(),
                (overlay.width(), overlay.height()),
                (width, height),
                shown.target.outgoing != (overlay.direction() == gtk::TextDirection::Rtl),
            ))
        });
        let weak_overlay = overlay.downgrade();
        let position = RefCell::new(Some(position));
        pill.connect_parent_notify(move |pill| {
            if let Some(overlay) = weak_overlay.upgrade()
                && pill.parent().as_ref() != Some(overlay.upcast_ref())
                && let Some(position) = position.borrow_mut().take()
            {
                overlay.disconnect(position);
            }
        });

        let motion = gtk::EventControllerMotion::new();
        let (entered, left) = (self.clone(), self.clone());
        motion.connect_enter(move |_, _, _| entered.cancel_timer());
        motion.connect_leave(move |_| {
            if left.shown.borrow().is_some() {
                left.hide_soon();
            }
        });
        pill.add_controller(motion);

        if let Some(scroll) = list
            .dynamic_cast_ref::<gtk::Scrollable>()
            .and_then(|scrollable| scrollable.vadjustment())
        {
            scroll.connect_value_changed(|_| dismiss());
        }
        pill
    }

    fn choose(&self, index: usize) {
        let Some(emoji) = self.quick.borrow().get(index).cloned() else {
            return;
        };
        let Some(Shown { target, .. }) = self.shown.borrow_mut().take() else {
            return;
        };
        self.hide();
        let emoji = if target
            .mine
            .as_deref()
            .is_some_and(|mine| same_emoji(mine, &emoji))
        {
            String::new()
        } else {
            emoji
        };
        target.sender.input(Input::ReactTo {
            id: target.id,
            emoji,
        });
    }

    fn choose_another(&self, button: &gtk::Button) {
        let Some(Shown { target, bubble }) = self.shown.borrow_mut().take() else {
            return;
        };
        let anchor = bubble
            .upgrade()
            .and_then(|bubble| bubble.ancestor(gtk::ListView::static_type()))
            .and_then(|list| button.compute_bounds(&list).map(|bounds| (list, bounds)));
        self.hide();
        let Some((list, bounds)) = anchor else {
            return;
        };
        let HoverTarget { id, sender, .. } = target;
        let chooser = crate::native_emoji::picker(move |emoji| {
            sender.input(Input::ReactTo {
                id: id.clone(),
                emoji: emoji.to_owned(),
            });
        });
        chooser.set_parent(&list);
        chooser.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
            bounds.x() as i32,
            bounds.y() as i32,
            bounds.width() as i32,
            bounds.height() as i32,
        )));
        chooser.connect_closed(|chooser| {
            let chooser = chooser.clone();
            glib::idle_add_local_once(move || chooser.unparent());
        });
        chooser.popup();
    }
}

/// Where the pill points, given the bubble's top in list coordinates.
fn pill_anchor_top(bubble_top: f32) -> i32 {
    ((bubble_top - GAP) as i32).max(MIN_TOP)
}

fn pill_bounds(
    anchor: gtk::graphene::Point,
    bubble_width: f32,
    available: (i32, i32),
    size: (i32, i32),
    align_end: bool,
) -> gtk::gdk::Rectangle {
    let x = if align_end {
        anchor.x() as i32 + bubble_width as i32 - size.0
    } else {
        anchor.x() as i32
    };
    gtk::gdk::Rectangle::new(
        x.clamp(0, (available.0 - size.0).max(0)),
        (anchor.y() as i32 - size.1).clamp(0, (available.1 - size.1).max(0)),
        size.0,
        size.1,
    )
}

/// Not focusable: clicking a quick reaction must leave the composer's focus alone.
fn pill_button() -> gtk::Button {
    gtk::Button::builder()
        .css_classes(["flat", "circular", "zaptide-reaction"])
        .focus_on_click(false)
        .can_focus(false)
        .build()
}

fn refresh_buttons(pill: &gtk::Box, quick: &[String], mine: Option<&str>) {
    let mut child = pill.first_child();
    for emoji in quick {
        let Some(button) = child.take().and_downcast::<gtk::Button>() else {
            return;
        };
        child = button.next_sibling();
        let chosen = mine.is_some_and(|mine| same_emoji(mine, emoji));
        let words = if chosen {
            gettext("Remove reaction")
        } else {
            gettext("React with {emoji}").replace("{emoji}", emoji)
        };
        button.set_label(emoji);
        button.set_tooltip_text(Some(&words));
        button.update_property(&[gtk::accessible::Property::Label(&words)]);
        if chosen {
            button.add_css_class("chosen");
        } else {
            button.remove_css_class("chosen");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    fn message(
        content: crate::model::Content,
        status: crate::model::Delivery,
    ) -> crate::model::Message {
        crate::model::Message {
            id: "m1".into(),
            chat: "chat".into(),
            sender: "ana".into(),
            sender_name: None,
            from_me: false,
            timestamp: 0,
            content,
            status,
            delivered_at: None,
            read_at: None,
            quoted: None,
            reactions: Vec::new(),
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: None,
        }
    }

    #[test]
    fn without_history_the_usual_six_are_offered() {
        assert_eq!(quick_reactions(&[]), strings(&DEFAULT_REACTIONS));
    }

    #[test]
    fn recent_reactions_come_first_and_defaults_fill_the_rest() {
        assert_eq!(
            quick_reactions(&strings(&["🎉", "😂"])),
            strings(&["🎉", "😂", "👍", "❤️", "😮", "😢"])
        );
    }

    #[test]
    fn six_recent_reactions_replace_every_default() {
        let recent = strings(&["🎉", "🔥", "👏", "🤔", "😍", "💯", "🙏"]);
        assert_eq!(quick_reactions(&recent), recent[..QUICK_SLOTS]);
    }

    #[test]
    fn blank_and_repeated_entries_never_take_a_slot() {
        let quick = quick_reactions(&strings(&["", " ", "🎉", "🎉", "❤"]));
        assert_eq!(quick, strings(&["🎉", "❤", "👍", "😂", "😮", "😢"]));
        assert_eq!(quick.len(), QUICK_SLOTS);
    }

    #[test]
    fn the_presentation_selector_does_not_make_a_different_emoji() {
        assert!(same_emoji("❤", "❤️"));
        assert!(!same_emoji("❤️", "👍"));
    }

    #[test]
    fn remembering_moves_an_emoji_to_the_front_without_repeats() {
        let mut recent = strings(&["🎉", "😂", "🔥"]);
        assert!(remember_reaction(&mut recent, "🔥"));
        assert_eq!(recent, strings(&["🔥", "🎉", "😂"]));
        assert!(!remember_reaction(&mut recent, "🔥"));
        assert!(remember_reaction(&mut recent, "😂"));
        assert_eq!(recent, strings(&["😂", "🔥", "🎉"]));
    }

    #[test]
    fn remembering_keeps_only_what_the_pill_can_show() {
        let mut recent = Vec::new();
        for emoji in ["1️⃣", "2️⃣", "3️⃣", "4️⃣", "5️⃣", "6️⃣", "7️⃣"] {
            assert!(remember_reaction(&mut recent, emoji));
        }
        assert_eq!(recent.len(), QUICK_SLOTS);
        assert_eq!(recent.first().map(String::as_str), Some("7️⃣"));
        assert!(!recent.iter().any(|emoji| emoji == "1️⃣"));
    }

    #[test]
    fn removing_a_reaction_is_not_remembered() {
        let mut recent = strings(&["🎉"]);
        assert!(!remember_reaction(&mut recent, ""));
        assert!(!remember_reaction(&mut recent, "  "));
        assert_eq!(recent, strings(&["🎉"]));
    }

    #[test]
    fn only_delivered_content_can_be_reacted_to() {
        use crate::model::{CallOutcome, Content, Delivery};
        assert!(can_react(&message(Content::text("hi"), Delivery::None)));
        assert!(can_react(&message(Content::text("hi"), Delivery::Sent)));
        assert!(can_react(&message(Content::text("hi"), Delivery::Read)));
        assert!(!can_react(&message(Content::Revoked, Delivery::None)));
        assert!(!can_react(&message(Content::text("hi"), Delivery::Pending)));
        assert!(!can_react(&message(Content::text("hi"), Delivery::Failed)));
        let call = Content::Call {
            video: false,
            outgoing: false,
            outcome: CallOutcome::Missed,
            seconds: None,
        };
        assert!(!can_react(&message(call, Delivery::None)));
    }

    /// Run under a display: `GSETTINGS_BACKEND=memory cargo test pill_buttons -- --ignored`.
    #[test]
    #[ignore = "needs a display"]
    fn pill_buttons_follow_the_recent_reactions_and_mark_ours() {
        gtk::init().expect("display");
        let list = gtk::ListView::new(None::<gtk::SelectionModel>, None::<gtk::ListItemFactory>);
        let overlay = gtk::Overlay::builder().child(&list).build();
        let window = gtk::Window::builder().child(&overlay).build();
        let drawer = Rc::new(Drawer::default());
        let pill = drawer.pill_in(list.upcast_ref()).expect("overlay");
        assert!(!pill.can_focus());
        assert!(!overlay.is_measure_overlay(&pill));
        assert_eq!(
            drawer.pill_in(list.upcast_ref()).expect("overlay"),
            pill,
            "the one pill is reused"
        );

        let quick = quick_reactions(&strings(&["🎉", "😂"]));
        refresh_buttons(&pill, &quick, Some("🎉"));
        let mut buttons = Vec::new();
        let mut child = pill.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            buttons.push(widget.downcast::<gtk::Button>().expect("button"));
        }
        assert_eq!(buttons.len(), QUICK_SLOTS + 1);
        let labels: Vec<_> = buttons
            .iter()
            .take(QUICK_SLOTS)
            .map(|button| button.label().unwrap_or_default().to_string())
            .collect();
        assert_eq!(labels, quick);
        assert!(buttons[0].has_css_class("chosen"));
        assert!(!buttons[1].has_css_class("chosen"));
        assert!(buttons.iter().all(|button| !button.can_focus()));
        assert_eq!(
            buttons[QUICK_SLOTS].icon_name().as_deref(),
            Some("list-add-symbolic")
        );

        refresh_buttons(&pill, &quick, None);
        assert!(!buttons[0].has_css_class("chosen"));

        let provider = gtk::CssProvider::new();
        provider.load_from_string(include_str!("../../data/style.css"));
        gtk::style_context_add_provider_for_display(
            &list.display(),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        window.present();
        pill.set_visible(true);
        let (_, size) = pill.preferred_size();
        assert!(size.width() < 600, "pill is {} px wide", size.width());
        assert!(size.height() < 200, "pill is {} px tall", size.height());

        drawer.hide_soon();
        assert!(drawer.timer.borrow().is_some());
        drawer.hide();
        assert!(drawer.timer.borrow().is_none());
        assert!(!pill.is_visible());

        overlay.remove_overlay(&pill);
        let replacement = drawer.pill_in(list.upcast_ref()).expect("overlay");
        assert_ne!(replacement, pill);
        let weak = pill.downgrade();
        drop(pill);
        assert!(
            weak.upgrade().is_none(),
            "position callback retains the pill"
        );
    }

    #[test]
    fn pill_alignment_follows_the_bubble_without_covering_it() {
        let anchor = gtk::graphene::Point::new(50.0, 100.0);
        assert_eq!(
            pill_bounds(anchor, 180.0, (400, 300), (100, 40), false),
            gtk::gdk::Rectangle::new(50, 60, 100, 40)
        );
        assert_eq!(
            pill_bounds(anchor, 180.0, (400, 300), (100, 40), true),
            gtk::gdk::Rectangle::new(130, 60, 100, 40)
        );
    }

    #[test]
    fn pill_position_stays_inside_the_available_viewport() {
        assert_eq!(
            pill_bounds(
                gtk::graphene::Point::new(500.0, 10.0),
                180.0,
                (300, 200),
                (100, 40),
                false,
            ),
            gtk::gdk::Rectangle::new(200, 0, 100, 40)
        );
        assert_eq!(
            pill_bounds(
                gtk::graphene::Point::new(-50.0, 300.0),
                80.0,
                (300, 200),
                (100, 40),
                true,
            ),
            gtk::gdk::Rectangle::new(0, 160, 100, 40)
        );
    }

    #[test]
    fn the_pill_points_above_the_bubble_and_stays_inside_the_list() {
        assert_eq!(pill_anchor_top(200.0), 192);
        assert!(pill_anchor_top(200.0) < 200);
        assert_eq!(pill_anchor_top(10.0), MIN_TOP);
    }

    #[test]
    fn a_suspended_drawer_shows_nothing_until_resumed() {
        suspend();
        assert!(drawer().suspended.get());
        assert!(drawer().shown.borrow().is_none());
        resume();
        assert!(!drawer().suspended.get());
    }

    #[test]
    fn our_reaction_is_the_one_marked_as_ours() {
        let mut message = message(
            crate::model::Content::text("hi"),
            crate::model::Delivery::None,
        );
        assert_eq!(own_reaction(&message), None);
        message.reactions = vec![
            crate::model::Reaction {
                sender: "ana".into(),
                from_me: false,
                emoji: "👍".into(),
            },
            crate::model::Reaction {
                sender: "me".into(),
                from_me: true,
                emoji: "🎉".into(),
            },
        ];
        assert_eq!(own_reaction(&message), Some("🎉"));
    }
}
