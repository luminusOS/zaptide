use gtk::prelude::*;
use relm4::prelude::*;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct SidebarState {
    /// Changes when search is cleared from outside the entry.
    pub(super) search_resets: u64,
    pub(super) filters: crate::native_chat_list::ChatListFilters,
    pub(super) archived: bool,
    pub(super) archived_unread: usize,
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
    SelectChat(crate::account::AccountId, String),
    NewChat,
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
                    set_tooltip_text: Some("Updating messages"),
                    #[watch]
                    set_visible: model.state.refreshing,
                },
                pack_end = &gtk::MenuButton {
                    set_icon_name: "open-menu-symbolic",
                    set_tooltip_text: Some("Main menu"),
                    update_property: &[gtk::accessible::Property::Label("Main menu")],
                    set_primary: true,
                    set_menu_model: Some(&init.menu),
                },
                pack_end = &gtk::Button {
                    set_icon_name: "chat-message-new-symbolic",
                    set_tooltip_text: Some("New chat"),
                    update_property: &[gtk::accessible::Property::Label("New chat")],
                    connect_clicked[sender] => move |_| sender.output(SidebarOutput::NewChat).unwrap(),
                },
            },
            #[wrap(Some)]
            set_content = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                append = &gtk::Box {
                    add_css_class: "linked",
                    set_margin_start: 12,
                    set_margin_end: 12,
                    set_margin_bottom: 6,
                    #[name = "search_entry"]
                    append = &gtk::SearchEntry {
                        set_hexpand: true,
                        set_placeholder_text: Some("Search chats"),
                        connect_search_changed[sender] => move |entry| {
                            sender.output(SidebarOutput::SearchChats(entry.text().to_string())).unwrap();
                        },
                    },
                    append = &gtk::MenuButton {
                        set_icon_name: "zaptide-filter-symbolic",
                        set_tooltip_text: Some("Filters"),
                        update_property: &[gtk::accessible::Property::Label("Filter chats")],
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
                                    set_label: "Chat Type",
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
                                            set_label: "All",
                                        },
                                        update_property: &[gtk::accessible::Property::Label("All")],
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
                                            set_label: "Private",
                                        },
                                        update_property: &[gtk::accessible::Property::Label("Private")],
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
                                            set_label: "Groups",
                                        },
                                        update_property: &[gtk::accessible::Property::Label("Groups")],
                                        set_group: Some(&all_filter),
                                        #[watch]
                                        set_active: model.state.filters.groups_only,
                                        #[watch]
                                        set_class_active: ("accent", model.state.filters.groups_only),
                                        connect_toggled[sender] => move |button| if button.is_active() { sender.output(SidebarOutput::SetChatKindFilter(crate::native_chat_list::ChatKindFilter::Groups)).unwrap() },
                                    },
                                },
                                append = &gtk::Label {
                                    set_label: "Show Only",
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
                                            set_label: "Unread",
                                        },
                                        update_property: &[gtk::accessible::Property::Label("Unread")],
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
                                            set_label: "Pinned",
                                        },
                                        update_property: &[gtk::accessible::Property::Label("Pinned")],
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
                                            set_label: "Muted",
                                        },
                                        update_property: &[gtk::accessible::Property::Label("Muted")],
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
                            set_label: Some("Archived"),
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
                        #[watch]
                        set_active_name: Some(if model.state.archived { "archived" } else { "chats" }),
                        connect_active_name_notify[sender] => move |group| {
                            sender.output(SidebarOutput::SetArchivedFilter(group.active_name().as_deref() == Some("archived"))).unwrap();
                        },
                    },
                },
                append = &gtk::ScrolledWindow {
                    set_vexpand: true,
                    set_hscrollbar_policy: gtk::PolicyType::Never,
                    #[watch]
                    set_visible: !model.state.empty,
                    #[local_ref]
                    chat_view -> gtk::ListView {
                        add_css_class: "navigation-sidebar",
                        add_css_class: "zaptide-chat-list",
                        set_single_click_activate: true,
                        connect_activate[sender, chat_targets] => move |_, position| {
                            if let Some((account, chat)) = chat_targets.borrow().get(position) {
                                sender.output(SidebarOutput::SelectChat(account, chat)).unwrap();
                            }
                        },
                    },
                },
                append = &adw::StatusPage {
                    add_css_class: "compact",
                    set_vexpand: true,
                    set_icon_name: Some("system-search-symbolic"),
                    #[watch]
                    set_visible: model.state.empty,
                    #[watch]
                    set_title: model.state.empty_title,
                    #[watch]
                    set_description: Some(model.state.empty_description),
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
        let chat_targets = init.chat_targets;
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
