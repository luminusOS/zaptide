//! Our own picture at the top of the chat list: it opens the linked
//! accounts, each with its unread chats, and a way to add another. A dot on
//! it says another account has unread chats.

use crate::account::AccountId;
use relm4::adw::prelude::*;
use relm4::{adw, gtk};

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) enum RowState {
    #[default]
    Ready,
    SignedOut,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct SwitcherRow {
    pub(super) id: AccountId,
    pub(super) label: String,
    pub(super) phone: Option<String>,
    pub(super) avatar: Option<std::path::PathBuf>,
    pub(super) unread: usize,
    pub(super) state: RowState,
}

impl SwitcherRow {
    pub(super) fn subtitle(&self) -> String {
        match self.state {
            RowState::SignedOut => "Signed out".into(),
            RowState::Failed => "Couldn't start".into(),
            RowState::Ready => self.phone.clone().unwrap_or_default(),
        }
    }

    /// What a screen reader hears after the row's title.
    pub(super) fn spoken(&self, active: bool) -> String {
        let subtitle = self.subtitle();
        let unread = crate::native_tray::unread_label(self.unread);
        [
            Some(subtitle).filter(|text| !text.is_empty()),
            unread,
            active.then(|| "current account".to_owned()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ")
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct SwitcherState {
    pub(super) rows: Vec<SwitcherRow>,
    pub(super) active: Option<AccountId>,
    pub(super) active_label: String,
    pub(super) active_avatar: Option<std::path::PathBuf>,
    pub(super) unread_elsewhere: usize,
    pub(super) can_add: bool,
}

impl SwitcherState {
    /// The name stays fixed; who is signed in and unread chats elsewhere
    /// go in the description, so nothing is read twice.
    pub(super) fn button_label(&self) -> &'static str {
        "Switch Account"
    }

    fn active_name(&self) -> &str {
        self.rows
            .iter()
            .find(|row| Some(row.id) == self.active)
            .map_or(self.active_label.as_str(), |row| row.label.as_str())
    }

    fn unread_elsewhere_text(&self) -> Option<String> {
        crate::native_tray::unread_label(self.unread_elsewhere)
            .map(|count| format!("{count} in other accounts"))
    }

    pub(super) fn tooltip(&self) -> String {
        let name = self.active_name();
        let mut tooltip = if name.is_empty() {
            "Switch Account".to_owned()
        } else {
            format!("Switch Account — {name}")
        };
        if let Some(unread) = self.unread_elsewhere_text() {
            tooltip.push('\n');
            tooltip.push_str(&unread);
        }
        tooltip
    }

    /// Who is signed in, and unread chats elsewhere, for screen readers.
    pub(super) fn spoken(&self) -> String {
        [
            Some(self.active_name().to_owned()).filter(|name| !name.is_empty()),
            self.unread_elsewhere_text(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ")
    }
}

struct Widgets {
    button: gtk::MenuButton,
    avatar: adw::Avatar,
    dot: gtk::Box,
    list: gtk::ListBox,
    popover: gtk::Popover,
    on_switch: Box<dyn Fn(AccountId)>,
    on_add: Box<dyn Fn()>,
}

pub(super) struct AccountSwitcher {
    pub(super) button: gtk::MenuButton,
    #[cfg(test)]
    list: gtk::ListBox,
    #[cfg(test)]
    popover: gtk::Popover,
    widgets: std::rc::Rc<Widgets>,
    shown: std::rc::Rc<std::cell::RefCell<Option<SwitcherState>>>,
    /// What to show once the open popover closes: rows are never rebuilt
    /// under the pointer or keyboard focus.
    deferred: std::rc::Rc<std::cell::RefCell<Option<SwitcherState>>>,
}

fn set_picture(avatar: &adw::Avatar, path: Option<&std::path::Path>) {
    let texture = path.and_then(|path| gtk::gdk::Texture::from_filename(path).ok());
    avatar.set_custom_image(texture.as_ref());
}

impl AccountSwitcher {
    pub(super) fn new(
        on_switch: impl Fn(AccountId) + 'static,
        on_add: impl Fn() + 'static,
    ) -> Self {
        let avatar = adw::Avatar::new(24, None, true);
        let dot = gtk::Box::builder()
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .can_target(false)
            .visible(false)
            .build();
        dot.add_css_class("zaptide-account-dot");
        let overlay = gtk::Overlay::builder().child(&avatar).build();
        overlay.add_overlay(&dot);
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        list.add_css_class("navigation-sidebar");
        let popover = gtk::Popover::builder().child(&list).build();
        let button = gtk::MenuButton::builder()
            .child(&overlay)
            .popover(&popover)
            .build();
        button.add_css_class("flat");
        let widgets = std::rc::Rc::new(Widgets {
            button: button.clone(),
            avatar,
            dot,
            list: list.clone(),
            popover: popover.clone(),
            on_switch: Box::new(on_switch),
            on_add: Box::new(on_add),
        });
        let shown = std::rc::Rc::new(std::cell::RefCell::new(None));
        let deferred: std::rc::Rc<std::cell::RefCell<Option<SwitcherState>>> =
            std::rc::Rc::default();
        {
            let (widgets, shown, deferred) = (
                std::rc::Rc::downgrade(&widgets),
                shown.clone(),
                deferred.clone(),
            );
            popover.connect_closed(move |_| {
                if let (Some(widgets), Some(state)) = (widgets.upgrade(), deferred.take()) {
                    apply(&widgets, &state);
                    shown.replace(Some(state));
                }
            });
        }
        Self {
            button,
            #[cfg(test)]
            list,
            #[cfg(test)]
            popover,
            widgets,
            shown,
            deferred,
        }
    }

    /// Redraws only when what it shows changed, and never while open.
    pub(super) fn sync(&self, state: SwitcherState) {
        if self.shown.borrow().as_ref() == Some(&state) {
            self.deferred.take();
            return;
        }
        if self.widgets.popover.is_visible() {
            self.deferred.replace(Some(state));
            return;
        }
        apply(&self.widgets, &state);
        self.shown.replace(Some(state));
    }
}

fn apply(widgets: &std::rc::Rc<Widgets>, state: &SwitcherState) {
    widgets.avatar.set_text(Some(&state.active_label));
    set_picture(&widgets.avatar, state.active_avatar.as_deref());
    widgets.dot.set_visible(state.unread_elsewhere > 0);
    widgets.button.set_tooltip_text(Some(&state.tooltip()));
    widgets.button.update_property(&[
        gtk::accessible::Property::Label(state.button_label()),
        gtk::accessible::Property::Description(&state.spoken()),
    ]);
    while let Some(row) = widgets.list.first_child() {
        widgets.list.remove(&row);
    }
    for account in &state.rows {
        widgets
            .list
            .append(&row(widgets, account, Some(account.id) == state.active));
    }
    if state.can_add {
        let add = adw::ActionRow::builder()
            .title("Add Account…")
            .activatable(true)
            .build();
        // Same column as the account pictures.
        let icon = gtk::Image::builder()
            .icon_name("list-add-symbolic")
            .width_request(32)
            .height_request(32)
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        add.add_prefix(&icon);
        let weak = std::rc::Rc::downgrade(widgets);
        add.connect_activated(move |_| {
            if let Some(widgets) = weak.upgrade() {
                widgets.popover.popdown();
                (widgets.on_add)();
            }
        });
        widgets.list.append(&add);
    }
}

fn row(widgets: &std::rc::Rc<Widgets>, account: &SwitcherRow, active: bool) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(account.label.as_str())
        .subtitle(account.subtitle())
        .activatable(!active)
        .build();
    row.upcast_ref::<gtk::Widget>()
        .update_property(&[gtk::accessible::Property::Description(
            &account.spoken(active),
        )]);
    let avatar = adw::Avatar::new(32, Some(&account.label), true);
    set_picture(&avatar, account.avatar.as_deref());
    row.add_prefix(&avatar);
    if account.unread > 0 {
        let unread = gtk::Label::builder()
            .label(account.unread.to_string())
            .valign(gtk::Align::Center)
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        unread.add_css_class("zaptide-unread-pill");
        row.add_suffix(&unread);
    }
    if active {
        let check = gtk::Image::builder()
            .icon_name("object-select-symbolic")
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        row.add_suffix(&check);
    }
    let (weak, id) = (std::rc::Rc::downgrade(widgets), account.id);
    row.connect_activated(move |_| {
        if let Some(widgets) = weak.upgrade() {
            widgets.popover.popdown();
            (widgets.on_switch)(id);
        }
    });
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessible_label_mentions_unread_elsewhere_without_relying_on_colour() {
        let mut state = SwitcherState::default();
        state.rows.push(SwitcherRow {
            id: AccountId(1),
            label: "Work".into(),
            ..Default::default()
        });
        state.active = Some(AccountId(1));
        assert_eq!(state.button_label(), "Switch Account");
        assert_eq!(state.tooltip(), "Switch Account — Work");
        state.unread_elsewhere = 2;
        assert_eq!(state.button_label(), "Switch Account");
        assert_eq!(state.spoken(), "Work, 2 unread chats in other accounts");
    }

    /// Renders the sidebar header with the account button, and the
    /// popover's list, to PNGs under `$RENDER_DIR`, light and dark, at the
    /// narrowest supported width. Run under a display, e.g. broadway:
    /// `gtk4-broadwayd :5 & GDK_BACKEND=broadway BROADWAY_DISPLAY=:5
    /// RENDER_DIR=/tmp/r cargo test --lib render_account_switcher -- --ignored`.
    #[test]
    #[ignore = "needs a display"]
    fn render_account_switcher() {
        use relm4::gtk::prelude::*;
        gtk::init().unwrap();
        adw::init().unwrap();
        let dir = std::path::PathBuf::from(std::env::var("RENDER_DIR").expect("RENDER_DIR"));
        let css = gtk::CssProvider::new();
        css.load_from_string(
            ".zaptide-unread-pill { background: var(--accent-bg-color); color: var(--accent-fg-color); border-radius: 9999px; padding: 0 6px; } .zaptide-account-dot { min-width: 8px; min-height: 8px; border-radius: 9999px; background: var(--accent-bg-color); box-shadow: 0 0 0 2px var(--headerbar-bg-color); }",
        );
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().unwrap(),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let one = SwitcherState {
            rows: vec![SwitcherRow {
                id: AccountId(1),
                label: "Alice Example".into(),
                phone: Some("+1 555 0100".into()),
                ..Default::default()
            }],
            active: Some(AccountId(1)),
            active_label: "Alice Example".into(),
            can_add: true,
            ..Default::default()
        };
        let mut two = one.clone();
        two.rows.push(SwitcherRow {
            id: AccountId(2),
            label: "Work".into(),
            phone: Some("+1 555 0199".into()),
            unread: 3,
            ..Default::default()
        });
        two.rows.push(SwitcherRow {
            id: AccountId(3),
            label: "Old phone".into(),
            phone: Some("+1 555 0142".into()),
            state: RowState::SignedOut,
            ..Default::default()
        });
        two.unread_elsewhere = 3;
        let context = gtk::glib::MainContext::default();
        let render = |name: &str, child: &gtk::Widget, width: i32| {
            let window = adw::Window::builder()
                .default_width(width)
                .content(child)
                .build();
            window.present();
            let end = std::time::Instant::now() + std::time::Duration::from_millis(400);
            while std::time::Instant::now() < end {
                context.iteration(false);
            }
            let paintable = gtk::WidgetPaintable::new(Some(&window));
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(
                &snapshot,
                f64::from(window.width()),
                f64::from(window.height()),
            );
            let node = snapshot.to_node().unwrap();
            let texture = window
                .native()
                .unwrap()
                .renderer()
                .unwrap()
                .render_texture(&node, None);
            let path = dir.join(format!("{name}.png"));
            std::fs::write(&path, texture.save_to_png_bytes()).unwrap();
            window.close();
        };
        for (scheme_name, scheme) in [
            ("light", adw::ColorScheme::ForceLight),
            ("dark", adw::ColorScheme::ForceDark),
        ] {
            adw::StyleManager::default().set_color_scheme(scheme);
            for (count, state) in [("one", &one), ("two", &two)] {
                let switcher = AccountSwitcher::new(|_| {}, || {});
                switcher.sync(state.clone());
                assert_eq!(
                    switcher.button.tooltip_text().as_deref(),
                    Some(state.tooltip().as_str())
                );
                let header = adw::HeaderBar::new();
                header.set_title_widget(Some(&adw::WindowTitle::new("ZapTide", "")));
                header.pack_start(&switcher.button);
                header.pack_start(&gtk::Button::from_icon_name("chat-message-new-symbolic"));
                header.pack_end(
                    &gtk::MenuButton::builder()
                        .icon_name("open-menu-symbolic")
                        .build(),
                );
                let page = adw::ToolbarView::new();
                page.add_top_bar(&header);
                page.set_content(Some(&gtk::Label::new(Some("Chats"))));
                render(
                    &format!("header-{count}-{scheme_name}"),
                    page.upcast_ref(),
                    360,
                );
                switcher.popover.set_child(None::<&gtk::Widget>);
                let list = gtk::Box::builder()
                    .margin_top(12)
                    .margin_bottom(12)
                    .margin_start(12)
                    .margin_end(12)
                    .build();
                list.append(&switcher.list);
                render(
                    &format!("popover-{count}-{scheme_name}"),
                    list.upcast_ref(),
                    320,
                );
            }
        }
    }

    #[test]
    fn screen_readers_hear_who_is_signed_in_and_the_unread_count() {
        let mut state = SwitcherState {
            rows: vec![SwitcherRow {
                id: AccountId(1),
                label: "Work".into(),
                ..Default::default()
            }],
            active: Some(AccountId(1)),
            ..Default::default()
        };
        assert_eq!(state.spoken(), "Work");
        state.unread_elsewhere = 1;
        assert_eq!(state.spoken(), "Work, 1 unread chat in other accounts");
        assert_eq!(
            state.tooltip(),
            "Switch Account — Work\n1 unread chat in other accounts"
        );
        let row = SwitcherRow {
            phone: Some("+1 555 0100".into()),
            unread: 3,
            ..Default::default()
        };
        assert_eq!(
            row.spoken(true),
            "+1 555 0100, 3 unread chats, current account"
        );
        assert_eq!(SwitcherRow::default().spoken(false), "");
    }

    #[test]
    fn row_subtitle_says_when_an_account_cannot_be_used() {
        let mut row = SwitcherRow {
            phone: Some("+1 555 0100".into()),
            ..Default::default()
        };
        assert_eq!(row.subtitle(), "+1 555 0100");
        row.state = RowState::SignedOut;
        assert_eq!(row.subtitle(), "Signed out");
        row.state = RowState::Failed;
        assert_eq!(row.subtitle(), "Couldn't start");
    }
}
