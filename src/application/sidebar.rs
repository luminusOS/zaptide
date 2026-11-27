use crate::i18n::gettext;
use gtk::prelude::*;
use relm4::prelude::*;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct SidebarState {
    /// Changes when search is cleared from outside the entry.
    pub(super) search_resets: u64,
    pub(super) filters: crate::native_chat_list::ChatListFilters,
    pub(super) archived: bool,
    pub(super) archived_unread: usize,
    pub(super) calls: bool,
    pub(super) calls_unseen: usize,
    pub(super) calls_empty: bool,
    pub(super) live_call: bool,
    pub(super) refreshing: bool,
    pub(super) empty: bool,
    pub(super) empty_title: &'static str,
    pub(super) empty_description: &'static str,
}

impl Default for SidebarState {
    fn default() -> Self {
        Self {
            search_resets: 0,
            filters: Default::default(),
            archived: false,
            archived_unread: 0,
            calls: false,
            calls_unseen: 0,
            calls_empty: true,
            live_call: false,
            refreshing: false,
            empty: true,
            empty_title: "No Chats Yet",
            empty_description: "Conversations appear here as WhatsApp syncs.",
        }
    }
}

pub(super) struct SidebarInit {
    pub(super) account_button: gtk::MenuButton,
    pub(super) state: SidebarState,
    pub(super) chat_view: gtk::ListView,
    pub(super) calls_view: gtk::ListBox,
    pub(super) chat_targets: std::rc::Rc<std::cell::RefCell<super::chats::ChatTargets>>,
    pub(super) menu: gtk::gio::Menu,
}

pub(super) struct Sidebar {
    state: SidebarState,
    search_entry: Option<gtk::SearchEntry>,
}

#[derive(Debug)]
pub(super) enum SidebarInput {
    Sync(SidebarState),
}

#[derive(Debug)]
pub(super) enum SidebarOutput {
    SearchChats(String),
    SetUnreadFilter(bool),
    SetPinnedFilter(bool),
    SetChatKindFilter(crate::native_chat_list::ChatKindFilter),
    SetArchivedFilter(bool),
    SetMutedFilter(bool),
    ShowCalls(bool),
    SelectChat(crate::account::AccountId, String),
    /// A popup for the pointed or focused row, in chat list coordinates.
    ChatMenu {
        target: super::ChatTarget,
        x: f64,
        y: f64,
    },
    NewChat,
    ShowLiveCall,
}

fn chat_list_row(list: &gtk::ListView, mut widget: gtk::Widget) -> Option<gtk::Widget> {
    if &widget == list.upcast_ref::<gtk::Widget>() {
        return None;
    }
    loop {
        let parent = widget.parent()?;
        if &parent == list.upcast_ref::<gtk::Widget>() {
            return Some(widget);
        }
        widget = parent;
    }
}

fn row_target(row: &gtk::Widget, targets: &super::chats::ChatTargets) -> Option<super::ChatTarget> {
    targets.for_id(&row.first_child()?.widget_name())
}

fn keyboard_menu_key(key: gtk::gdk::Key, modifiers: gtk::gdk::ModifierType) -> bool {
    let modifiers = modifiers & gtk::accelerator_get_default_mod_mask();
    (key == gtk::gdk::Key::Menu && modifiers.is_empty())
        || (key == gtk::gdk::Key::F10 && modifiers == gtk::gdk::ModifierType::SHIFT_MASK)
}

#[relm4::component(pub(super))]
impl SimpleComponent for Sidebar {
    type Init = SidebarInit;
    type Input = SidebarInput;
    type Output = SidebarOutput;

    view! {
        adw::ToolbarView {
            add_top_bar = &adw::HeaderBar {
                #[wrap(Some)]
                set_title_widget = &gtk::Box {},
                pack_start: &init.account_button,
                pack_start = &adw::Spinner {
                    set_tooltip_text: Some(&gettext("Updating messages")),
                    #[watch]
                    set_visible: model.state.refreshing,
                },
                pack_end = &gtk::MenuButton {
                    set_icon_name: "open-menu-symbolic",
                    set_tooltip_text: Some(&gettext("Main menu")),
                    update_property: &[gtk::accessible::Property::Label(&gettext("Main menu"))],
                    set_primary: true,
                    set_menu_model: Some(&init.menu),
                },
                pack_end = &gtk::Button {
                    set_icon_name: "chat-message-new-symbolic",
                    set_tooltip_text: Some(&gettext("New chat")),
                    update_property: &[gtk::accessible::Property::Label(&gettext("New chat"))],
                    connect_clicked[sender] => move |_| sender.output(SidebarOutput::NewChat).unwrap(),
                },
            },
            #[wrap(Some)]
            set_content = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                append = &gtk::Button {
                    set_tooltip_text: Some(&gettext("Return to call")),
                    update_property: &[gtk::accessible::Property::Label(&gettext("Return to call"))],
                    set_margin_start: 12,
                    set_margin_end: 12,
                    set_margin_top: 6,
                    set_margin_bottom: 6,
                    #[wrap(Some)]
                    set_child = &gtk::Box {
                        set_halign: gtk::Align::Center,
                        set_spacing: 6,
                        append = &gtk::Image {
                            set_icon_name: Some("call-start-symbolic"),
                        },
                        append = &gtk::Label {
                            set_label: &gettext("Return to call"),
                        },
                    },
                    #[watch]
                    set_visible: model.state.live_call,
                    connect_clicked[sender] => move |_| sender.output(SidebarOutput::ShowLiveCall).unwrap(),
                },
                append = &gtk::Box {
                    add_css_class: "linked",
                    #[watch]
                    set_visible: !model.state.calls,
                    set_margin_start: 12,
                    set_margin_end: 12,
                    set_margin_bottom: 6,
                    #[name = "search_entry"]
                    append = &gtk::SearchEntry {
                        set_hexpand: true,
                        set_placeholder_text: Some(&gettext("Search chats")),
                        connect_search_changed[sender] => move |entry| {
                            sender.output(SidebarOutput::SearchChats(entry.text().to_string())).unwrap();
                        },
                    },
                    append = &gtk::MenuButton {
                        set_icon_name: "zaptide-filter-symbolic",
                        set_tooltip_text: Some(&gettext("Filters")),
                        update_property: &[gtk::accessible::Property::Label(&gettext("Filter chats"))],
                        #[watch]
                        set_class_active: ("zaptide-filters-active", model.state.filters_active()),
                        #[wrap(Some)]
                        set_popover = &gtk::Popover {
                            #[wrap(Some)]
                            set_child = &gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 12,
                                set_margin_top: 12,
                                set_margin_bottom: 12,
                                set_margin_start: 12,
                                set_margin_end: 12,
                                append = &gtk::Label {
                                    set_label: &gettext("Chat Type"),
                                    set_halign: gtk::Align::Start,
                                    add_css_class: "heading",
                                },
                                append = &adw::WrapBox {
                                    set_justify: adw::JustifyMode::Fill,
                                    set_justify_last_line: false,
                                    set_child_spacing: 6,
                                    set_line_spacing: 6,
                                    set_natural_line_length: 320,
                                    #[name = "all_filter"]
                                    append = &gtk::ToggleButton {
                                        #[wrap(Some)]
                                        set_child = &adw::ButtonContent {
                                            set_icon_name: "view-list-bullet-symbolic",
                                            set_label: &gettext("All"),
                                        },
                                        update_property: &[gtk::accessible::Property::Label(&gettext("All"))],
                                        #[watch]
                                        set_active: !model.state.filters.private_only && !model.state.filters.groups_only,
                                        #[watch]
                                        set_class_active: ("accent", !model.state.filters.private_only && !model.state.filters.groups_only),
                                        connect_toggled[sender] => move |button| if button.is_active() { sender.output(SidebarOutput::SetChatKindFilter(crate::native_chat_list::ChatKindFilter::All)).unwrap() },
                                    },
                                    append = &gtk::ToggleButton {
                                        #[wrap(Some)]
                                        set_child = &adw::ButtonContent {
                                            set_icon_name: "avatar-default-symbolic",
                                            set_label: &gettext("Private"),
                                        },
                                        update_property: &[gtk::accessible::Property::Label(&gettext("Private"))],
                                        set_group: Some(&all_filter),
                                        #[watch]
                                        set_active: model.state.filters.private_only,
                                        #[watch]
                                        set_class_active: ("accent", model.state.filters.private_only),
                                        connect_toggled[sender] => move |button| if button.is_active() { sender.output(SidebarOutput::SetChatKindFilter(crate::native_chat_list::ChatKindFilter::Private)).unwrap() },
                                    },
                                    append = &gtk::ToggleButton {
                                        #[wrap(Some)]
                                        set_child = &adw::ButtonContent {
                                            set_icon_name: "system-users-symbolic",
                                            set_label: &gettext("Groups"),
                                        },
                                        update_property: &[gtk::accessible::Property::Label(&gettext("Groups"))],
                                        set_group: Some(&all_filter),
                                        #[watch]
                                        set_active: model.state.filters.groups_only,
                                        #[watch]
                                        set_class_active: ("accent", model.state.filters.groups_only),
                                        connect_toggled[sender] => move |button| if button.is_active() { sender.output(SidebarOutput::SetChatKindFilter(crate::native_chat_list::ChatKindFilter::Groups)).unwrap() },
                                    },
                                },
                                append = &gtk::Label {
                                    set_label: &gettext("Show Only"),
                                    set_halign: gtk::Align::Start,
                                    add_css_class: "heading",
                                },
                                append = &adw::WrapBox {
                                    set_justify: adw::JustifyMode::Fill,
                                    set_justify_last_line: false,
                                    set_child_spacing: 6,
                                    set_line_spacing: 6,
                                    set_natural_line_length: 320,
                                    append = &gtk::ToggleButton {
                                        #[wrap(Some)]
                                        set_child = &adw::ButtonContent {
                                            set_icon_name: "mail-unread-symbolic",
                                            set_label: &gettext("Unread"),
                                        },
                                        update_property: &[gtk::accessible::Property::Label(&gettext("Unread"))],
                                        #[watch]
                                        set_active: model.state.filters.unread_only,
                                        #[watch]
                                        set_class_active: ("accent", model.state.filters.unread_only),
                                        connect_toggled[sender] => move |button| sender.output(SidebarOutput::SetUnreadFilter(button.is_active())).unwrap(),
                                    },
                                    append = &gtk::ToggleButton {
                                        #[wrap(Some)]
                                        set_child = &adw::ButtonContent {
                                            set_icon_name: "view-pin-symbolic",
                                            set_label: &gettext("Pinned"),
                                        },
                                        update_property: &[gtk::accessible::Property::Label(&gettext("Pinned"))],
                                        #[watch]
                                        set_active: model.state.filters.pinned_only,
                                        #[watch]
                                        set_class_active: ("accent", model.state.filters.pinned_only),
                                        connect_toggled[sender] => move |button| sender.output(SidebarOutput::SetPinnedFilter(button.is_active())).unwrap(),
                                    },
                                    append = &gtk::ToggleButton {
                                        #[wrap(Some)]
                                        set_child = &adw::ButtonContent {
                                            set_icon_name: "notifications-disabled-symbolic",
                                            set_label: &gettext("Muted"),
                                        },
                                        update_property: &[gtk::accessible::Property::Label(&gettext("Muted"))],
                                        #[watch]
                                        set_active: model.state.filters.muted == crate::native_chat_list::MutedFilter::Only,
                                        #[watch]
                                        set_class_active: ("accent", model.state.filters.muted == crate::native_chat_list::MutedFilter::Only),
                                        connect_toggled[sender] => move |button| sender.output(SidebarOutput::SetMutedFilter(button.is_active())).unwrap(),
                                    },
                                },
                            },
                        },
                    },
                },
                append = &gtk::Box {
                    set_spacing: 4,
                    set_margin_start: 12,
                    set_margin_end: 12,
                    set_margin_bottom: 6,
                    #[name = "chat_section"]
                    append = &adw::ToggleGroup {
                        set_hexpand: true,
                        set_homogeneous: true,
                        add_css_class: "flat",
                        add = adw::Toggle {
                            set_name: Some("chats"),
                            set_icon_name: Some("user-available-symbolic"),
                            set_tooltip: "Chats",
                        },
                        add = adw::Toggle {
                            set_name: Some("archived"),
                            set_label: Some(&gettext("Archived")),
                            set_tooltip: "Archived",
                            #[wrap(Some)]
                            set_child = &gtk::Box {
                                set_spacing: 6,
                                set_halign: gtk::Align::Center,
                                append = &gtk::Image {
                                    set_icon_name: Some("package-x-generic-symbolic"),
                                },
                                append = &gtk::Label {
                                    add_css_class: "zaptide-unread-pill",
                                    add_css_class: "muted",
                                    add_css_class: "compact",
                                    set_valign: gtk::Align::Center,
                                    #[watch]
                                    set_visible: model.state.archived_unread > 0,
                                    #[watch]
                                    set_label: &model.state.archived_unread.to_string(),
                                },
                            },
                        },
                        add = adw::Toggle {
                            set_name: Some("calls"),
                            set_label: Some(&gettext("Calls")),
                            set_tooltip: "Calls",
                            #[wrap(Some)]
                            set_child = &gtk::Box {
                                set_spacing: 6,
                                set_halign: gtk::Align::Center,
                                append = &gtk::Image {
                                    set_icon_name: Some("call-start-symbolic"),
                                },
                                append = &gtk::Label {
                                    add_css_class: "zaptide-unread-pill",
                                    add_css_class: "compact",
                                    set_valign: gtk::Align::Center,
                                    #[watch]
                                    set_visible: model.state.calls_unseen > 0,
                                    #[watch]
                                    set_label: &model.state.calls_unseen.to_string(),
                                },
                            },
                        },
                        // A click yields two outputs, so the state synced back in
                        // between is stale; if that sync counted as a click, the
                        // two stale values would overwrite each other forever.
                        #[watch]
                        #[block_signal(section_handler)]
                        set_active_name: Some(if model.state.calls { "calls" } else if model.state.archived { "archived" } else { "chats" }),
                        connect_active_name_notify[sender] => move |group| {
                            let name = group.active_name();
                            sender.output(SidebarOutput::ShowCalls(name.as_deref() == Some("calls"))).unwrap();
                            sender.output(SidebarOutput::SetArchivedFilter(name.as_deref() == Some("archived"))).unwrap();
                        } @section_handler,
                    },
                },
                append = &gtk::ScrolledWindow {
                    set_vexpand: true,
                    set_hscrollbar_policy: gtk::PolicyType::Never,
                    #[watch]
                    set_visible: !model.state.empty && !model.state.calls,
                    #[local_ref]
                    chat_coordinates -> super::sidebar_coordinates::SidebarCoordinates {},
                },
                append = &adw::StatusPage {
                    add_css_class: "compact",
                    set_vexpand: true,
                    set_icon_name: Some("system-search-symbolic"),
                    #[watch]
                    set_visible: model.state.empty && !model.state.calls,
                    #[watch]
                    set_title: model.state.empty_title,
                    #[watch]
                    set_description: Some(model.state.empty_description),
                },
                append = &gtk::ScrolledWindow {
                    set_vexpand: true,
                    set_hscrollbar_policy: gtk::PolicyType::Never,
                    #[watch]
                    set_visible: model.state.calls && !model.state.calls_empty,
                    #[local_ref]
                    calls_view -> gtk::ListBox {},
                },
                append = &adw::StatusPage {
                    add_css_class: "compact",
                    set_vexpand: true,
                    set_icon_name: Some("call-start-symbolic"),
                    set_title: &gettext("No Calls Yet"),
                    set_description: Some(&gettext("Voice and video calls appear here")),
                    #[watch]
                    set_visible: model.state.calls && model.state.calls_empty,
                },
            },
        }
    }

    fn init(
        init: Self::Init,
        _root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let mut model = Self {
            state: init.state,
            search_entry: None,
        };
        let chat_view = &init.chat_view;
        let chat_targets = init.chat_targets.clone();
        chat_view.add_css_class("navigation-sidebar");
        chat_view.add_css_class("zaptide-chat-list");
        chat_view.set_single_click_activate(true);
        let chat_sender = sender.clone();
        chat_view.connect_activate(move |_, position| {
            if let Some((account, chat)) = chat_targets.borrow().get(position) {
                chat_sender
                    .output(SidebarOutput::SelectChat(account, chat))
                    .unwrap();
            }
        });
        let menu_gesture = gtk::GestureClick::builder().button(3).build();
        let menu_sender = sender.clone();
        let menu_view = chat_view.downgrade();
        let menu_targets = init.chat_targets.clone();
        menu_gesture.connect_pressed(move |gesture, _, x, y| {
            let Some(list) = menu_view.upgrade() else {
                return;
            };
            let Some(target) = list
                .pick(x, y, gtk::PickFlags::DEFAULT)
                .and_then(|widget| chat_list_row(&list, widget))
                .and_then(|row| row_target(&row, &menu_targets.borrow()))
            else {
                return;
            };
            gesture.set_state(gtk::EventSequenceState::Claimed);
            menu_sender
                .output(SidebarOutput::ChatMenu { target, x, y })
                .unwrap();
        });
        chat_view.add_controller(menu_gesture);
        let menu_keys = gtk::EventControllerKey::new();
        let menu_sender = sender.clone();
        let menu_view = chat_view.downgrade();
        let menu_targets = init.chat_targets;
        menu_keys.connect_key_pressed(move |_, key, _, modifiers| {
            if !keyboard_menu_key(key, modifiers) {
                return gtk::glib::Propagation::Proceed;
            }
            let Some(list) = menu_view.upgrade() else {
                return gtk::glib::Propagation::Proceed;
            };
            let Some(row) = list
                .root()
                .and_then(|root| root.focus())
                .and_then(|focus| chat_list_row(&list, focus))
            else {
                return gtk::glib::Propagation::Proceed;
            };
            let (Some(target), Some(bounds)) = (
                row_target(&row, &menu_targets.borrow()),
                row.compute_bounds(&list),
            ) else {
                return gtk::glib::Propagation::Proceed;
            };
            menu_sender
                .output(SidebarOutput::ChatMenu {
                    target,
                    x: f64::from(bounds.x() + bounds.width() / 2.0),
                    y: f64::from(bounds.y() + bounds.height() / 2.0),
                })
                .unwrap();
            gtk::glib::Propagation::Stop
        });
        chat_view.add_controller(menu_keys);
        let chat_coordinates = super::sidebar_coordinates::SidebarCoordinates::new(chat_view);
        let calls_view = &init.calls_view;
        let widgets = view_output!();
        model.search_entry = Some(widgets.search_entry.clone());
        ComponentParts { model, widgets }
    }

    fn update(&mut self, input: Self::Input, _sender: ComponentSender<Self>) {
        match input {
            SidebarInput::Sync(state) => {
                // The entry owns its text. Writing the stored query back would
                // race the debounced search-changed and eat typed letters, so
                // only an explicit outside reset clears it.
                if let Some(entry) = &self.search_entry
                    && state.search_resets != self.state.search_resets
                {
                    entry.set_text("");
                }
                self.state = state;
            }
        }
    }
}

impl SidebarState {
    fn filters_active(&self) -> bool {
        let filters = self.filters;
        filters.unread_only
            || filters.pinned_only
            || filters.private_only
            || filters.groups_only
            || filters.muted != crate::native_chat_list::MutedFilter::default()
    }
}

impl Sidebar {
    pub(super) fn is_synced(&self, state: &SidebarState) -> bool {
        self.state == *state
    }
}
