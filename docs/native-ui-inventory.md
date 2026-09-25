# Native UI Inventory

## Reference Packet

Frozen 2026-09-22. These projects are behavioral references only. ZapTide does not copy code,
templates, icons, strings, screenshots, assets, or visual identity from them. Any future source
reuse requires a separate license review and an explicit inventory entry.

| Reference | Frozen revision | License status | Approved scope | Excluded scope |
| --- | --- | --- | --- | --- |
| Fractal | `6c1adadb5d2bc47ceb1183be90e53e2eb8ba9928` | GPL-3.0-or-later | Adaptive chat navigation, list density, message grouping, accessible async states | Matrix behavior, code, branding, assets |
| Karere | `f9976c0d04d967dd1cb2e6923c07ebfd6de77da0` | GPL-3.0 | WhatsApp-oriented application actions, desktop integration, native shell | CEF/WebView architecture, code, branding, assets |
| Yandex Messenger Native | `f0a237c6016088a8e3816dde2e56c06e4a47c5eb` | MIT | Rust GTK component boundaries and messenger state composition | Code, branding, assets |
| Mutiny | `4bba492d4788cc51f77a2a3c38cc5a73434f2adc` | Not approved for source reuse | Conversation and community layout reconnaissance | All source/assets pending license verification |
| TorX GTK4 | `6c8c899ad229d84476b1fed7078f952ccbaef384` | GPL-3.0 with additional exceptions | Native chat-widget reconnaissance | All source/assets; title/logo obligations prohibit reuse |
| gtk-llm-chat | `3bd35a672e67614aa93aaa8867645064fc889ac4` | GPL-3.0 | Composer, streaming, empty, and error-state reconnaissance | Code, branding, assets |
| Bavarder | `18da53ea53a08c97429b8d67f3a43a217a3788e2` | Not approved for source reuse | Conversation-state reconnaissance | All source/assets pending license verification |

Selected public reference screenshots are evidence of layout and interaction patterns, never shipped
assets or pixel-copy targets:

| Reference state | Frozen source | SHA-256 | Use |
| --- | --- | --- | --- |
| Fractal main conversation | `screenshots/main.png` at `6c1adadb5d2bc47ceb1183be90e53e2eb8ba9928` | `20c77399f84f368ee7ff503cb3ef7197329d6737e8ada2b3601024777d302071` | Message grouping, list density, restrained shell |
| Fractal adaptive navigation | `screenshots/adaptive.png` at `6c1adadb5d2bc47ceb1183be90e53e2eb8ba9928` | `24456e9b10aa892f4f891fc7420671e580b1725471fdca655fd9366b5cbceb21` | Narrow/wide navigation behavior |
| Karere main window | `data/screenshots/main-window.png` at `f9976c0d04d967dd1cb2e6923c07ebfd6de77da0` | `38417381de3785954e4151d8270c6484d6981107290ba05c829f81d8efd503c3` | GNOME messaging shell and actions |
| Karere preferences | `data/screenshots/preferences.png` at `f9976c0d04d967dd1cb2e6923c07ebfd6de77da0` | `d9b4bf128185366a516163620a47359c3a6df47a6e0ebe8bbf3235c77f1cd4f9` | Native settings grouping |

Review cues are intentionally descriptive rather than pixel targets:

- restrained libadwaita chrome, no decorative dashboard shell;
- dense but scannable chat rows with strong unread/selection distinction;
- conversation groups that preserve sender, time, delivery, reply, and attachment context;
- adaptive split navigation that preserves chat, draft, and scroll state;
- native loading, offline, permission, empty, and recoverable-error states;
- keyboard-first focus order and semantic accessibility over custom canvas behavior.

Before visual criticism begins, pair these reference records with ZapTide synthetic screenshots at
720x480 and wide desktop widths. No external image is a shipped asset or pixel-copy target.

## Inventory Rules

- Each `UI-*` ID represents one user-visible behavior, not one egui function.
- Every entry must name native owner, GTK surface, domain action/event boundary, and deterministic
  test. Manual-only cases identify their exact matrix row.
- Existing behavior remains until native parity evidence accepts its ID. No dual production UI.
- `Removed` requires a plan revision approved before implementation. It is not a migration outcome.

## Behavior Inventory

| ID | Existing behavior | Native owner | GTK/libadwaita surface | Required parity evidence | Status |
| --- | --- | --- | --- | --- | --- |
| UI-001 | Application activation, single instance, visible/hidden lifecycle | `application` | `adw::ApplicationWindow`, GActions | Planned `tests/lifecycle.rs` | Inventory complete |
| UI-002 | Link, QR, reconnect, offline, and fatal states | `components/login` | `adw::StatusPage`, QR image, actions | component test plus Orca login flow | Inventory complete |
| UI-003 | Chat list, pinned/archive/unread/mute state, filters, and search | `components/chat_list` | `gtk::ListView`, `FilterListModel`, `SearchEntry` | Planned projection/list test | Inventory complete |
| UI-004 | Conversation history, day/unread separators, replies, reactions, and delivery | `components/conversation` | `gtk::ListView`, `ScrolledWindow` | Planned anchor and row-reuse test | Inventory complete |
| UI-005 | Composer, drafts, editing/replying, IME, emoji, and send shortcuts | `components/composer` | native multiline text input and actions | input/component test plus IBus matrix | Inventory complete |
| UI-006 | Attachments, media, voice, polls, contacts, locations, and fallback rows | `components/media` | native rows, dialogs, and portal actions | synthetic component tests | Inventory complete |
| UI-007 | Preferences, theme, shortcuts, about, and destructive confirmations | `components/preferences` and `dialogs` | `AdwPreferencesDialog`, `AdwAboutDialog`, dialogs | action and accessibility test | Inventory complete |
| UI-008 | Notifications, links, files, clipboard, drag/drop, and background mode | `services` | GIO, portals, GNotification | Planned service/lifecycle test | Inventory complete |

The discovery pass must expand these eight top-level IDs into stable leaf IDs before a native view is
implemented or an egui view is removed.

## Discovered Leaf IDs

| ID | Parent | Behavior | Native target | Existing evidence | Status |
| --- | --- | --- | --- | --- | --- |
| UI-001.01 | UI-001 | Close hides only when configured background path exists | Root lifecycle state holds GApplication only while a verified tray, notification, or second-launch presentation path exists | Existing `App::hides_to_tray`; planned lifecycle test | Pending native design |
| UI-001.02 | UI-001 | Recreate/present window after tray, notification, second launch, or control command | Application activation action presents one retained `adw::ApplicationWindow`; no UI loop recreates backend state | Existing `main.rs` loop and single-instance tests; planned lifecycle test | Pending native design |
| UI-001.03 | UI-001 | Pump backend, linking, archive, tray, notifications, and control channel while hidden | Coalesced UI-neutral notifier drains bounded events under held application lifecycle; hidden mode has no GTK render loop, only owned wakeups | Planned hidden-held/lost-wakeup lifecycle test | Pending native design |
| UI-001.08 | UI-001 | Persist geometry, minimum size, and restoration failures across visibility transitions | Window-state service owns validated geometry persistence; a bad restore falls back to visible platform defaults without mutating backend state | Planned geometry restore/invalid-state test | Pending native design |
| UI-002.01 | UI-002 | QR linking image and pairing-code alternative | Relm4 link projection and native QR texture | `application::linking_qr_becomes_a_decodable_native_image`; Xvfb reaches unlinked state | Implemented, native-shell |
| UI-002.02 | UI-002 | Busy, reconnect, logged-out, offline, and fatal linking states | Relm4 link status and reconnect action | Native LinkStatus projection tests | Implemented, native-shell |
| UI-003.01 | UI-003 | Resizable sidebar width and adaptive sidebar visibility | `adw::OverlaySplitView` collapses at narrow widths; `Chats` action reopens navigation | Native synthetic AT-SPI flow at 720x480/GDK scale 1 and 2; screenshot/product visual review pending | Implemented, native-shell |
| UI-003.02 | UI-003 | All, unread, private, and group filtering | Relm4 filters backed by `ChatListProjection` | `native_chat_list` filter tests | Implemented, native-shell |
| UI-003.03 | UI-003 | Archived, locked, pinned, muted, unread, avatar, and search result states | Incremental GTK list model and ID-stable projection | `native_chat_list` and `application` projection tests | Implemented, native-shell |
| UI-004.01 | UI-004 | Empty conversation and no-chat states | Relm4 status and loading presentation | Native application projection tests | Implemented, native-shell |
| UI-004.02 | UI-004 | History rows, selection, reply, reactions, quote navigation, selected-row actions, and read state | Virtualized GTK message list and selected-row action panel | `native_transcript`, `application`, backend quote tests | Implemented, native-shell |
| UI-004.03 | UI-004 | Older-history fetch and prepend-anchor preservation at bounded window | GTK list controller with `LoadChat`/`FetchOlder` and stable message ID anchor | `older_page_window_keeps_new_rows_and_existing_anchor`; archive paging tests | Implemented, native-shell |
| UI-005.01 | UI-005 | Per-chat multiline drafts and focus restoration | `NativeComposerState` and GTK text buffer | `native_composer` multiline/draft tests | Implemented, native-shell |
| UI-005.02 | UI-005 | Text, IME-safe explicit submit, emoji chooser, participant picker, and send shortcuts | Native multiline input and picker dialogs | `native_composer`, mention-token, and shortcut tests | Implemented, native-shell; picker instead of autocomplete popup |
| UI-005.03 | UI-005 | Reply, edit, portal attachment picker, save, clipboard text/image, external URI, and file drop | Composer, GIO portal service, text view drop target | Portal validation/cancellation and application tests | Implemented, native-shell |
| UI-006.01 | UI-006 | Image, video, document, sticker, GIF, contact, location, poll, and unsupported rows | Native GTK media widget factories with bounded async image previews and explicit video fallbacks | `native_media`, `native_media_widgets` tests | Implemented, native-shell |
| UI-006.02 | UI-006 | Voice playback, seek, record, waveform, and send preview | `services::media::MediaService` and native voice row | Synthetic audio lifecycle, cancellation, permission, and codec tests | Implemented; live hardware validation pending |
| UI-006.03 | UI-006 | Poll creation, validation, voting, and recovery | Native create/vote controls plus existing recovery engine | `native_actions`, poll validation/recovery tests | Implemented, native-shell |
| UI-007.01 | UI-007 | Appearance, custom theme, zoom, and themes-folder controls | Adw preferences palette selector, GTK CSS palette adapter, and GIO themes-folder action | `native_theme` CSS/contrast projection, `theme::custom` local palette/security tests, settings persistence and portal tests | Implemented, native-shell; visual GNOME styling QA pending |
| UI-007.02 | UI-007 | Shortcuts, about, unlink confirmation, phone pairing, new contact, chat info, and forward | Native shortcuts/about, pairing/contact forms, unlink confirmation, chat-info dialog, and searchable forward destination picker | `application` phone validation and forwarding filter tests; manual dialog/focus matrix pending | Implemented, native-shell; runtime dialog acceptance pending |
| UI-007.03 | UI-007 | Update download, verify, cancel, restart, and release-notes state | Native update alert dialog and verified backend download/install lifecycle with cooperative cancellation | `updates` integrity/cancellation/install tests and native update event handling | Implemented, native-shell; live release/download/install acceptance pending |
| UI-008.01 | UI-008 | Keyboard focus visibility, toasts, and top-level banners | GTK focus, `AdwToastOverlay`, native status/error feedback | `tests/native_accessibility.rs`, synthetic AT-SPI smoke | Implemented; Orca speech/manual styling checks pending |
| UI-008.02 | UI-008 | Clipboard, paste, dropped files, and external desktop handlers | GTK/GIO/portal service | Portal safety, bounded image, URI, and drop tests | Implemented, native-shell |
| UI-008.03 | UI-008 | Title-bar drag and native-window gesture behavior | `adw::HeaderBar` plus GTK-native window behavior | Xvfb startup smoke; desktop drag matrix not run | Implemented; Wayland/X11 manual drag smoke pending |

## Discovery Closure

`Action` declaration source is `src/model.rs:567-771`; its sole state-transition consumer is
`App::apply` at `src/app.rs:1924-2624`. UI producers are constrained to `src/ui/`, except desktop
activation producers (`src/main.rs`, `src/tray.rs`, `src/notify.rs`, and `src/single_instance.rs`).
The current worker is a legacy exception: `src/backend/worker.rs:2646-2655,2695-2709` invokes
blocking `rfd::FileDialog` for attachment/sticker selection. Native migration moves invocation to
the GTK main-context portal service; accepted and cancelled responses return to the worker only as
typed requests/events with request IDs.
The Native Contract Matrix maps every Action variant to a leaf; non-Action input, lifecycle, and
row-factory behaviors are mapped by their owning source module in their leaf descriptions. No
`Pending mapping` action group remains. Settings separation is fixed by UI-007.06: persisted typed
settings are domain state, stylesheet/layout application is GTK presentation state, and external
open, portal, notification, and window activation policy are platform services.

## Action Coverage Queue

`model::Action` is the current completeness denominator for user-initiated behavior. These groups
cover every variant at `src/model.rs:567-771`; the Native Contract Matrix maps each group to stable
leaves, a native owner, and required test evidence.

| Queue ID | Action variants | Native owner | Status |
| --- | --- | --- | --- |
| ACT-01 | `Open`, `OpenChat`, `StartChat`, `OpenMessage`, `CloseChat`, `KeepUnread`, `Search`, `SetChatFilter`, `ToggleSidebar`, `ScrollTo`, `ScrollToBottom` | application, chat list, conversation | UI-001.06, UI-003.04-.05, UI-004.04 |
| ACT-02 | `SendText`, `Composing`, `MarkRead`, `LoadOlder`, `FetchOlder`, `SendPending`, `RemovePending`, `ClearPending` | conversation and composer | UI-004.04-.05, UI-005.04-.05 |
| ACT-03 | `CreatePoll`, `RefreshPoll`, `VotePoll` | poll dialog and message row | UI-006.06 |
| ACT-04 | `Download`, `OpenFile`, `OpenFolder`, `OpenUrl`, `CopyText`, `Attach`, `SendFiles`, `PasteImage` | media and portal service | UI-005.05, UI-006.07, UI-008.04-.05 |
| ACT-05 | `PlayVoice`, `SeekVoice`, `CycleVoiceSpeed`, `StartRecording`, `CancelRecording`, `SendRecording` | voice row and media service | UI-006.04-.05 |
| ACT-06 | `Reply`, `CancelReply`, `Forward`, `Edit`, `CancelEdit`, `DeleteForEveryone`, `DeleteForMe`, `React`, `SetArchived`, `SetPinned`, `SetMuted`, `SetLocked` | message row, chat list, dialogs | UI-003.06, UI-004.06-.07 |
| ACT-07 | `TogglePicker`, `ClosePicker`, `OpenReactionPicker`, `InsertEmoji`, `InsertEmojiCompletion`, `CloseEmojiSuggestions`, `InsertMention`, `CloseMentions`, `SendSticker`, `SaveSticker`, `ForgetSticker`, `ImportStickerUrl`, `PickStickerArchive`, `DeleteStickerPack`, `SearchGifs`, `SendGif` | composer picker and media service | UI-005.06-.10 |
| ACT-08 | `EditContact`, `SaveContact`, `NewContact`, `PairWithPhone`, `Unlink`, `Reconnect` | login, contacts, dialogs | UI-002.03, UI-007.04 |
| ACT-09 | `ShowDialog`, `CloseDialog`, `FocusSearch`, `FocusComposer`, `HideShortcutHints`, `DismissChatLockHint`, `ShowUpdate`, `CloseUpdate`, `DownloadUpdate`, `InstallUpdate` | root actions, dialogs, update dialog | UI-003.05, UI-004.07, UI-005.04, UI-007.05-.07 |
| ACT-10 | `SetTheme`, `SetCustomTheme`, `ReloadThemes`, `OpenThemesFolder`, `SettingsChanged`, `ZoomBy`, `ResetZoom` | preferences and settings service | UI-007.06 |
| ACT-11 | `Quit`, `ShowWindow`, `HideWindow`, `CloseWindow` | application lifecycle | UI-001.04-.05 |

`Page` has two entries (`Chats`, `Settings`), `Dialog` has eight (`Shortcuts`, `About`,
`ConfirmUnlink`, `PairWithPhone`, `NewContact`, `ChatInfo`, `Forward`, `CreatePoll`), and
`PickerTab` has three (`Emoji`, `Gifs`, `Stickers`). They are included in ACT-01, ACT-07, and
ACT-09, but each must retain its own leaf-level evidence.

## Native Contract Matrix

This matrix closes the current action denominator. A row may cover multiple action variants only
when one native component owns one atomic user-visible behavior. Every planned test uses synthetic
models and a fake backend or portal; no test requires a real account or archive.

| Leaf ID | Current action variants | Native owner and contract | Required evidence |
| --- | --- | --- | --- |
| UI-001.04 | `Quit`, `ShowWindow`, `HideWindow`, `CloseWindow` | Root Relm4 component maps actions to explicit lifecycle transitions and GApplication holds; no component calls GTK close directly. | Planned `tests/lifecycle.rs`: repeated close, hide, re-present, quit, late event |
| UI-001.05 | Tray menu, notification activation, second launch, control activation, and hidden backend pumping | Root application accepts all presentation requests through one GAction/message path, balances hold/release, and drains backend events while no window is visible. | Planned `tests/lifecycle.rs`: all activation sources, hidden event burst, worker failure |
| UI-001.06 | `Open(Page)`, `ToggleSidebar` | Window/navigation component preserves selection, draft, and scroll state through `Chats` and `Settings` navigation. | Planned navigation projection test at 720x480 and wide layout |
| UI-001.07 | Visible and hidden scheduling: audio progress, recording meter, toast expiry, animation, and backend wakeup | Each service owns one cancellable GLib source or event notifier. Visible window uses frame-clock/timeout work only while observed; hidden mode retains backend/tray/notification wakeups, cancels UI animation/scroll sources, and never busy-loops. | Planned visible/hidden source ownership, cancellation, and lost-wakeup tests |
| UI-001.08 | Window geometry, minimum size, recreation, and failed restoration | Window-state service loads validated geometry before presentation, applies platform minimums, persists after stable configure events, and falls back safely on invalid or unavailable monitor state. | Planned geometry/minimum/recreate/failure test |
| UI-002.03 | `PairWithPhone`, `Reconnect`, `Unlink` | Login component validates display input, then emits typed backend requests. QR/pairing values never enter logs or accessibility text. | Login reducer test; Orca linking matrix |
| UI-003.04 | `OpenChat`, `StartChat`, `CloseChat`, `KeepUnread` | Chat-list/conversation coordinator selects by stable chat ID, persists/restores drafts, and keeps unread-filter row stable until selection changes. | `tests/model_projection.rs`: selection, drafts, unread filter |
| UI-003.05 | `SetChatFilter`, `Search`, `FocusSearch` | Chat-list owns `FilterListModel`/search projection; search result updates are asynchronous backend input, never widget-owned data. | Filter and search projection tests |
| UI-003.06 | `SetArchived`, `SetPinned`, `SetMuted`, `SetLocked` | Chat-row menu performs optimistic projection by chat ID and emits backend command; locking/archiving active chat routes through UI-003.04 close semantics. | Chat mutation and locked-chat regression tests |
| UI-004.04 | `OpenMessage`, `ScrollTo`, `ScrollToBottom`, `LoadOlder`, `FetchOlder` | Conversation controller owns stable message-ID anchoring and bounded history requests; GTK scroll adjustment is updated only after model insertion. | Prepend-anchor, search-target, and pagination tests |
| UI-004.05 | `MarkRead` | Conversation visibility controller sends read request once per stable chat/message position. | Planned read-idempotence test |
| UI-006.07 | `Download` | Media-transfer controller projects pending, success, failure, retry, and safe-file state by stable message ID; transfer work remains backend-owned and stale-account completions cannot write caches. | `native_attachments` state projection, application completion, and stale-session cache-write tests; explicit in-progress media cancel and runtime safe-file lifecycle remain |
| UI-004.06 | `Reply`, `CancelReply`, `Edit`, `CancelEdit`, `DeleteForEveryone`, `DeleteForMe`, `Forward`, `React` | Message-row menu and composer share typed message intent; optimistic mutation is isolated from list factory binding. | Context-menu action and optimistic-update tests |
| UI-004.07 | `ShowDialog(Forward)`, `ShowDialog(ChatInfo)`, `CloseDialog` | Native dialog coordinator offers searchable, stable-ID forward destinations and selected-chat info. | `application::forward_chat_picker_filters_locked_and_broadcast_destinations`; dialog focus/runtime matrix pending |
| UI-004.08 | Transcript text selection and annotated cross-message copy | Conversation selection controller owns selectable text, stable cross-row selection, edge-scroll, cancellation, and annotated clipboard payloads. It retains `[time, date] Name:` headers per selected message and restores emoji before copy; selection survives list rebinding only when all selected message IDs remain present. Consecutive messages group sender/time metadata while preserving full accessible labels. | `native_transcript` selection/copy/rebind tests; `application::consecutive_messages_group_sender_and_timestamp_metadata`; native GTK edge-scroll/cancel smoke remains |
| UI-004.09 | Linux trackpad scrolling, axis lock, scaling, glide decay, and repaint scheduling | Conversation scroll controller uses GTK event controllers and `ScrolledWindow` kinetic scrolling. Any parity adjustment is stateful per gesture, cancelled on direction/input change, and schedules no repaint outside active movement. | Native GTK kinetic scrolling owns platform behavior; Wayland/libinput gesture matrix remains unrun |
| UI-005.04 | `SendText`, `Composing`, `FocusComposer` | Composer owns native text/IME state and emits text/typing intents; domain state retains drafts, reply/edit metadata, and mention tokens. | Composer reducer, IBus/compose manual matrix |
| UI-005.05 | `Attach`, `SendFiles`, `PasteImage`, `SendPending`, `RemovePending`, `ClearPending` | Composer requests files through portal service, stages only validated synthetic metadata, and sends caption/attachments atomically. | Portal-response cancellation and pending-attachment tests |
| UI-005.06 | `TogglePicker(Emoji)`, `InsertEmoji`, `InsertEmojiCompletion`, `CloseEmojiSuggestions`, `InsertMention`, `CloseMentions` | Composer completion controller replaces text by verified character boundaries and preserves logical cursor/focus. | Emoji/mention completion tests, RTL and compose matrix |
| UI-005.07 | `TogglePicker(Gifs)`, `SearchGifs`, `SendGif` | GIF picker uses cancellable backend search keyed to active request; stale results cannot replace newer query/chat state. | Search request-correlation test |
| UI-005.08 | `TogglePicker(Stickers)`, `SendSticker`, `SaveSticker`, `ForgetSticker`, `ImportStickerUrl`, `PickStickerArchive`, `DeleteStickerPack` | Sticker picker emits typed backend/portal intents; archive selection stays in portal service and never blocks GTK. | Sticker request/cancellation tests |
| UI-005.11 | Attachment and sticker archive picker invocation, cancellation, window destruction, and stale portal results | GTK main context owns `FileDialog`/portal requests with weak window ownership and request IDs. Backend receives only accepted file handles after response; cancellation, destroyed windows, and stale responses are ignored without blocking worker tasks. | `native_portals` request cancellation/late completion tests; GTK window-destruction and sticker-picker matrix remain |
| UI-005.09 | `OpenReactionPicker` | Reaction popover anchor object belongs to row `(chat_id, message_id)`, closes when row/chat disappears, restores focus, and never carries a pointer rectangle across model updates. | Planned reaction popover lifecycle and keyboard test |
| UI-005.10 | `ClosePicker` | Active composer auxiliary surface closes consistently on toggle, Escape, outside click, and send; emoji/GIF/sticker query and completion state reset, while composer focus is restored when closure was keyboard-initiated. | Planned picker close/reset/focus test |
| UI-006.04 | `PlayVoice`, `SeekVoice`, `CycleVoiceSpeed` | Voice row requests playback through media service; playback state is observed asynchronously and list binding remains cheap. | Playback state/restart/cancellation tests |
| UI-006.05 | `StartRecording`, `CancelRecording`, `SendRecording` | Composer/media service owns recorder lifecycle, permission denial, cancellation, and late completion. | Recorder permission/cancellation test |
| UI-006.06 | `CreatePoll`, `RefreshPoll`, `VotePoll`, `ShowDialog(CreatePoll)` | Native poll dialog validates against toolkit-neutral `PollDraft`; poll row sends bounded commands and projects refresh/vote state. | Poll validation/vote/recovery tests |
| UI-007.04 | `EditContact`, `SaveContact`, `NewContact`, `ShowDialog(NewContact)` | Contacts dialog validates input and emits backend request; contact and chat selection updates by stable ID. | `application::pairing_and_contact_phone_numbers_normalize_to_international_digits`; backend contact-result flow; request correlation and runtime acceptance pending |
| UI-007.05 | `ShowDialog(Shortcuts)`, `ShowDialog(About)`, `ShowDialog(ConfirmUnlink)`, `ShowDialog(PairWithPhone)` | Root actions present native libadwaita dialogs with destructive action defaulting to Cancel. | Native dialog construction; Orca spoken-output and manual keyboard/focus matrix pending |
| UI-007.06 | `SetTheme`, `SetCustomTheme`, `ReloadThemes`, `OpenThemesFolder`, `SettingsChanged`, `ZoomBy`, `ResetZoom`, `HideShortcutHints`, `DismissChatLockHint` | Preferences/settings service separates typed persisted settings from GTK style state; `Catalog` discovers validated local/bundled palettes and GTK CSS adapter applies colors while retaining libadwaita layout. Themes folder opens through cancellable GIO. | Settings migration, `native_theme` CSS projection, `theme::custom` catalog/security tests, absolute-local-folder URI test |
| UI-007.07 | `ShowUpdate`, `CloseUpdate`, `DownloadUpdate`, `CancelUpdate`, `InstallUpdate` | Native update dialog observes toolkit-neutral update state, exposes cancellation, and emits verified restart request through backend lifecycle coordinator. | Update integrity/cancellation/install tests; current/available/error completion reaches both shells; live release/download acceptance pending |
| UI-008.04 | `OpenFile`, `OpenFolder`, `OpenUrl` | Portal/GIO service validates paths and URI schemes; external operations require user action and are cancellable. | Planned hostile path/URI and portal-denial tests |
| UI-008.05 | `CopyText`, text paste, image paste | GTK clipboard service owns text/image transfers, completion, cancellation, and composer focus; image paste stages a captionable attachment. | Planned clipboard text/image and late-completion tests |
| UI-008.06 | File drag/drop | GTK drop target validates files before staging; drop cancellation and failed portal results leave composer/model consistent. | Planned synthetic drop and cancellation test |
| UI-008.07 | Keyboard focus, focus restoration, toasts, banners, and theme announcements | GTK accessibility/focus controller plus `AdwToastOverlay` and banner state expose native roles, focus order, and non-content error notices. | `tests/native_accessibility.rs`; synthetic AT-SPI at GDK scale 1/2 confirms search/composer focus; spoken Orca output and styling matrix remain |

Direct egui/eframe dependencies found in this contract are explicit migration targets: raw
drag/drop/paste input, text cursor state, focus requests/visibility, custom scroll anchoring,
title-bar drag, context URL and clipboard output, viewport close/focus commands, cursor/selection
anchors, theme and zoom application, and post-render action draining. None may survive behind a
compatibility wrapper.

## App State Ownership

Source: `src/app.rs:107-278`. This is a complete field classification for the current `App`, not a
proposal to retain its shape. Task 3 extracts only the first three categories behind typed boundaries;
the final category is deleted rather than reimplemented as an adapter.

| Category | Fields | Native disposition |
| --- | --- | --- |
| Toolkit-neutral domain and backend projection | `link`, `syncing`, `sync_percent`, `me`, `me_name`, `me_about`, `chats`, `contacts`, `conversations`, `open_chat`, `drafts`, `draft_mentions`, `composer`, `composer_mentions`, `reply_to`, `editing`, `composing`, `last_keystroke`, `search`, `search_hits`, `locked_folder`, `chat_lock_check`, `typing`, `presence`, `account_receipts_off`, `pending`, `played_told`, `gif_query`, `gif_results`, `gif_pending`, `gif_error`, `stickers`, `stickers_saved`, `sticker_packs`, `stickers_pending`, `sticker_import_pending`, `sticker_link`, `poll_draft`, `poll_creating`, `poll_voting`, `contact_edit`, `new_contact_phone`, `new_contact_name`, `new_contact_last`, `new_contact_pending`, `pair_phone`, `chat_filter`, `unread_kept`, `update`, `update_download`, `update_support`, `update_inspecting`, `update_arguments` | Keep data types or project them into typed application/component models. Split `Pending::Picture` texture cache from attachment metadata; retain no widget or GPU handle. |
| Native presentation state | `palette`, `custom_themes`, `applied_dark`, `zoom_applied`, `scroll_chat_into_view`, `emoji_start`, `emoji_selected`, `mention_start`, `mention_selected`, `avatars`, `avatar_requests`, `avatars_full`, `avatar_full_requests`, `dropping`, `picker`, `picker_search`, `picker_focus`, `reaction_target`, `open_message_menu`, `emoji_jump`, `page`, `dialog`, `forward_search`, `sidebar_visible`, `show_archived`, `toasts`, `actions`, `last_update_check`, `show_update`, `scroll_to_bottom`, `at_bottom`, `scroll_anchor`, `focus_composer`, `focus_search`, `window_focused` | Relm4 root and bounded feature components own these as native presentation models, keyed by stable domain IDs. Theme settings remain typed data; GTK style objects stay within presentation. |
| Platform service and lifecycle | `dirs`, `settings`, `settings_dirty`, `last_settings_save`, `backend`, `player`, `recording`, `quit_requested`, `tray`, `window_hidden`, `hide_intent`, `wants_show`, `control_commands`, `notification_opens`, `notifications` | Move behind application, settings, media, portal, notification, and lifecycle services. `Backend`, archive, protocol, and media workers retain no GTK object. |
| Egui-only | `picker_anchor`, `reaction_anchor`, `copy_rows`, `selection_view`, `scroll_lock`, `scroll_from_trackpad`, `scroll_history`, `scroll_accum`, `glide`, `scroll_last_event`, `waker` | Replace with native widget-local anchors, GTK selection/clipboard, `ScrolledWindow` gesture behavior, GLib source ownership, and a UI-neutral notifier. Do not preserve names or create compatibility wrappers. |

`Pending::Picture.texture` (`src/app.rs:282-289`) is egui-only despite `pending` being domain
projection. `copy_rows` carries transcript data but is egui lifecycle-coupled because rows are
registered per frame; the native transcript selection controller must own its replacement.

## Dependency Spike

The isolated `native-spike` feature compiles the approved stack without exposing a second UI:

```toml
gtk4 = { version = "0.11", features = ["v4_14"], optional = true }
libadwaita = { version = "0.9", features = ["v1_5"], optional = true }
relm4 = { version = "0.11", default-features = false, features = ["macros", "libadwaita", "gnome_46"], optional = true }
```

- `cargo check --locked --features native-spike` passed on 2026-09-22.
- Lockfile resolved `gtk4 0.11.5`, `libadwaita 0.9.2`, and `relm4 0.11.0`.
- `cargo tree --locked --features native-spike -d` found one GTK4/GLib/libadwaita/Relm4 runtime
  stack. It also found old/new `calloop`, `smithay-client-toolkit`, and related Wayland duplicates:
  these are carried by active eframe/egui and must disappear at Task 7, not be papered over now.
- `system-deps` major versions 7 and 9 are build-time dependencies of libadwaita and GTK bindings;
  they are not duplicate runtime widget stacks.

No GTK type is reachable from backend, archive, protocol, or domain code. The feature exists only
for Task 2 prototypes and is replaced by normal required native dependencies when Task 3 replaces
the eframe shell.

`examples/native_spike.rs` is the first structural prototype. It uses a Relm4 root component,
`adw::OverlaySplitView`, two `TypedListView` virtualized lists, a native multiline `TextView`,
10,000 synthetic chats, and a synthetic 10,000-event burst. The burst drains one queued Relm4 input
at a time in batches of at most 256, schedules each continuation through a GLib idle source, and
retains at most 2,000 active conversation rows. It has no backend, archive, account, production
entry point, or screenshot output. Runtime performance,
720x480 adaptation, scroll anchoring, accessibility, and manual GNOME validation remain open Task
2 evidence, not implied by compilation.

`cargo test --locked --example native-spike --features native-spike` passes the deterministic burst
bound: every drain has 1 through 256 events, 10,000 events require 40 batches, and retained model
entries never exceed 2,000.

Set `ZAPTIDE_NATIVE_SPIKE_BURST=1` only for measurement runs to inject the synthetic burst after
the Relm4 root initializes. It is opt-in and unavailable from ZapTide's production binary. This
produced `/tmp/opencode/native-spike-burst.syscap` (43,755,640 bytes) during a 15-second Sysprof
capture on 2026-09-22.

### Pending Manual Evidence

Fedora 45 tooling is installed: `xvfb-run`, Weston, Sysprof, and Orca 51.rc (AT-SPI2 2.61.90 on
GNOME Wayland). The prototype launched under both Xvfb and the current GNOME session using only
synthetic data. A 15-second startup/idle capture was written to
`/tmp/opencode/native-spike-idle.syscap` (12,388,208 bytes) with
`sysprof-cli --no-sysprofd --no-perf --gtk --speedtrack`. It is smoke evidence only: the privileged
Sysprof action is not registered in the host PolicyKit daemon. `sysprof-cli` and its policy file
are installed in the Toolbox, but `flatpak-spawn --host sysprof-cli --version` shows the host lacks
the binary. Unprivileged perf sampling is denied, so the capture cannot accept CPU, wakeup,
p95-frame, or latency budgets. Orca version was verified but no spoken semantic flow was executed.
The same unprivileged capture mode was used for the opt-in synthetic burst; it proves reproducible
invocation, not the performance budget.

After `cargo build --locked --example native-spike --features native-spike`, the built example ran
under `xvfb-run` at 720x480 with `GDK_SCALE=1` and `GDK_SCALE=2` for 12 seconds each without startup
errors. This is adaptive-startup smoke only; it does not prove clipping, navigation semantics, or
visual quality.

Run the prototype in a GNOME 46+ Wayland session at scale factors 1 and 2, including 720x480 and
wide layouts. Record privileged Sysprof traces and process samples for five runs of visible idle,
hidden-held idle, and fast scrolling; then inspect row realization, frame deadline, CPU/wakeups,
RSS growth, and backend-event latency against Task 2 limits. Execute keyboard, IBus, compose, RTL,
high-contrast, and Orca flows with synthetic data only.

Product direction on 2026-09-22 defers Sysprof work. This is not performance acceptance or a
release-gate waiver; it permits continuing discovery and prototype work while privileged traces
remain pending.

## Gauntlet Record

Three independent inventory reviews found and drove fixes for action coverage, hidden lifecycle,
media transfer, portal ownership, selection, touchpad input, scheduling, and geometry. A fourth
acceptance review could not run because the delegated-review quota was exhausted, rather than from
a technical failure. A later independent re-review returned `PASS` after annotated transcript-copy
and legacy worker picker boundaries were corrected. Inventory and dependency/prototype design may
now advance; native-shell migration still may not remove an egui surface until Task 2 manual
performance, accessibility, adaptive-layout, and scroll evidence passes. Evidence for the review is
this frozen inventory, `Cargo.lock`, `cargo check --locked --features native-spike`, the burst test,
and the duplicate-tree report above.
