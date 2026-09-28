# ZapTide

**WhatsApp, native and fast.** ZapTide is a Linux-first WhatsApp client written in Rust
with GTK4, libadwaita, and [Relm4](https://relm4.org). It uses
[whatsapp-rust](https://github.com/oxidezap/whatsapp-rust) for the WhatsApp Web
protocol. It links to your phone as a companion device and has no browser engine. ZapTide is a
[ZapFast](https://github.com/crmne/zapfast) fork with a native GTK4/libadwaita interface. In ZapFast's Linux test, it opened in
under a second and used about 150 MB of idle RAM, compared with 1.13 GB for
WhatsApp Web and its Chromium processes. [See the upstream measurements](https://zapfast.rocks/benchmarks/).

ZapTide retains ZapFast's direct WhatsApp protocol implementation.

![ZapTide showing a chat with a photo, a document, a voice message, a quoted reply, and a link](docs/screenshot.png)

Development happens at **[github.com/luminusOS/zaptide](https://github.com/luminusOS/zaptide)**.

![A group chat with sender names and pictures, a photo with reactions, a reply with a mention, and a poll](docs/screenshot-group.png)

![The linking screen with the QR code](docs/screenshot-link.png)

## What it does

- **Links to your phone.** Scan a QR code or link with your phone number.
  Recent history is copied to this computer after linking and stored here.
- **Chats.** See pinned, unread, muted, and archived chats, typing indicators,
  and message status. Search chats, saved messages, and contacts.
  Filter the list to private (one-to-one) or group chats, and to unread,
  pinned, or muted chats, with the pills under the search bar.
  Pinned chats stay in pin order (most recently pinned first), regardless of
  new messages. Chat and contact name searches ignore accents, so `Angel`
  finds `Ángel`.
  Typing indicators show other participants, excluding your own linked devices.
  Newsletter channels are read-only; publishing channel posts is not supported.
- **Read state across devices.** Reading a chat syncs its unread badge with
  your phone and other linked devices, including when read receipts are off.
  Replies from another device clear preceding unread messages. The read-receipt
  toggle also controls voice-message played receipts; account privacy is checked
  before sending receipts in direct chats. A hidden window does not read messages.
- **Conversations.** See replies, reactions, edits, deleted messages, read
  receipts, sender names, and group pictures. Older messages load as you
  scroll up, first from the local archive and then from your phone.
  Group messages show two gray checks after every recipient has received
  them, and blue checks after every recipient has read them. The recipient
  list and individual receipts are saved locally; later membership changes
  do not change that list. If the original recipients are unknown, ZapTide
  waits for the phone's aggregate status instead of guessing from one reader.
- **WhatsApp formatting.** Bold, italic, strikethrough, code, lists, quotes,
  mentions, and link previews are supported. Links are clickable. Hebrew and
  Arabic RTL paragraphs keep logical word order by reordering font runs; this
  is not a full Unicode Bidirectional Algorithm. ZapTide prefers an installed
  Noto Color Emoji and falls back to the bundled copy. Emoji-only messages
  are larger.
- **Screen-reader access.** AccessKit exposes the interface to desktop
  accessibility services. Custom buttons, chat rows, settings switches and
  message text include readable labels. Keyboard and screen-reader support is
  not complete.
  After Tab, the focused control is outlined and scrolled into view; using
  the mouse hides the outline again.
- **Safer desktop opening.** Links open only web pages or email addresses.
  Common documents and media open in their default apps; executable, script,
  and unrecognized attachment formats open their containing folder instead.
- **Send attachments with captions.** Paste a picture, drop files, or use the
  file picker. They stay in the composer until you send them or press Escape.
- **Mute chats** for eight hours, one week, or indefinitely. The setting also
  applies on your phone and to desktop notifications. Mute changes from your
  phone survive history arriving later, including during initial linking.
  Existing installations request one settings refresh after upgrading to
  recover previously lost mute settings and pin order, without relinking.
- **Audio in the conversation.** Play and seek received voice notes and audio
  files in their message bubbles, with a waveform and keyboard-accessible seek
  control. Voice playback cycles between 1x, 1.5x, and 2x without changing
  pitch; the last choice applies to later voice notes. On Linux, GStreamer
  handles playback, with a Rust fallback for accelerated playback if its
  `scaletempo` plugin is missing. Other platforms retain the Rust audio player. You can
  record, reply with, and send voice notes; recordings are normalized and
  OGG/Opus needs no external decoder.
- **Send messages.** Press Enter to send text and Shift+Enter for a new line.
  You can swap these keys in Settings. The composer is focused when you open
  or return to a conversation; invoking search keeps focus in search, and
  Escape clears search and returns to the composer; another Escape closes the
  chat and saves your text draft. Open menus, dialogs, and unfinished actions
  are dismissed first. Type `:name` to autocomplete
  an emoji without leaving the composer, or `@` in a group to mention a member.
  Reply, react with any emoji, edit, forward, delete, and check when a message was sent,
  delivered, or read.
- **Disappearing-message timers.** Outgoing messages use the chat's known
  timer, including replies, attachments, edits, and forwards. Forwarded copies
  use the destination chat's timer. Received messages remain in the local archive
  after they expire on the phone.
  A clock badge on chat avatars shows enabled timers and follows changes from
  the phone. Changing the default timer for new chats leaves existing chats alone.
- **View attachments.** ZapTide downloads files up to 64 MB automatically or
  on click. Photos, stickers, GIFs, voice messages, audio, locations, contacts,
  polls, and link previews appear in the chat. Videos and documents open in
  their default desktop apps. Profile pictures and downloaded images support
  filenames with spaces or non-ASCII characters.
  If an attachment has expired, ZapTide asks your
  phone to upload it again.
- **Polls.** Use the checklist button beside the paperclip to create a poll with
  2–12 answers. Turn off **Allow multiple answers** for a single-choice poll.
  Click an answer in a poll to vote; click a selected answer again to remove
  it. Results and your selection are retained in the encrypted archive, including
  votes received through phone history. Visible polls automatically request earlier
  votes from your phone. If it is offline, results are labelled incomplete and the
  request retries with backoff; no refresh button or relinking is needed.
  Voting needs the original poll's key;
  if that key is missing, the message explains that voting is available on your
  phone. Creating polls in disappearing-message chats is not yet supported by
  the protocol library's poll API, so ZapTide blocks it instead of ignoring the timer.
- **Emoji, GIF, and sticker picker.** Search emoji and GIFs, use recent emoji
  and stickers, and save stickers with a right-click. Emoji autocomplete and
  picker search select their first match; use the arrow keys and Enter to
  choose it. GIF search needs a free GIPHY API key unless the build includes
  one.
- **Sticker packs.** Import a pack from a `signal.art` link or `.wastickers`
  file. Animated packs remain animated. Packs are stored as WebP files on your
  computer.
- **Consistent names.** Use names from your address book or public WhatsApp
  profile names across chats, replies, mentions, and notifications.
- **Groups.** See members, sender names, and sender pictures. Announcement
  groups are read-only for non-admins.
- **Presence.** See online, last-seen, and typing status, and send your typing
  status.
- **Idle rendering.** History-sync progress updates when data arrives. Animated
  stickers and GIFs play only while their message or picker tile is visible.
- **Sync recovery.** A conflicting app-state collection is recovered through
  whatsapp-rust, including requesting a fresh snapshot from the paired phone
  when validation fails. Private read-state updates run one at a time. Failures
  pause the whole queue with backoff from 30 seconds to 15 minutes; pending reads
  remain saved and resume automatically. New messages can still arrive.
- **Desktop notifications.** Get notifications with the chat name, a message
  preview, and the chat picture when the chat is not on screen. A newer message
  replaces its chat's notification. Turn off **Show previews** in Preferences to
  hide the sender and text. Muted chats do not notify you, and archived
  chats stay quiet until you unarchive them. GNOME drops notifications from apps
  without an installed desktop file, so a `cargo run` build notifies only after
  `packaging/applications/dev.luminusos.ZapTide.desktop` is copied to
  `~/.local/share/applications/`. Clicking a notification opens the chat, and
  reading the chat here or on another device dismisses its outstanding
  notifications.
- **Themes.** Light, dark, follow the system, or a local JSON palette. Zoom with
  Ctrl+plus and Ctrl+minus.
- **Message bubbles.** Incoming messages align left, outgoing messages align
  right. Right-click a message or focus it and press Menu or Shift+F10 for
  reply, edit, react, forward, poll voting, and other available actions.
- **Copy text.** Select part of a message or copy across messages in
  WhatsApp's `[time, date] Name:` format. Contact names and numbers are also
  selectable.
- **Keyboard shortcuts.** `Ctrl+K` searches, `Alt+↑/↓` switches chats and
  keeps the active chat visible in the list, `Esc` cancels the current action,
  `Ctrl+L` focuses the message input, and `Ctrl+/` lists all shortcuts. The × at the left of the shortcut hints
  hides the bar; restore it with **Show shortcut hints** in Settings.
- **Local storage.** Messages, contacts and sticker metadata are stored in a
  SQLCipher-encrypted archive, unlocked automatically through your OS keyring.
  Existing plaintext archives are migrated on first use. Attachments remain
  ordinary files in the cache directory. Unlinking deletes both and removes this device from
  your phone.

## What it does not do yet

- Play ordinary videos in the app (they open in your player), or reply to
  a message with an attachment.
- Calls, status posts, communities, newsletters, and group administration.

## Installing

ZapTide has no stable release yet. Build it from source.

Native and Flatpak packaging will be published after the GTK/libadwaita migration passes its
release gates. Existing ZapFast packages are not ZapTide packages and do not share application data.

To build and install the current checkout as a Flatpak (requires `flatpak-builder`
and a Flathub remote):

```sh
flatpak-builder --user --install --force-clean --install-deps-from=flathub build-dir packaging/flatpak/dev.luminusos.ZapTide.yml
```

### Archive encryption

The archive key is a random 256-bit secret in Secret Service. ZapTide needs a
working Secret Service provider (for example GNOME Keyring or KeePassXC with Secret Service enabled).
If the keyring is locked or unavailable, unlock it and click Retry; ZapTide keeps
its archive intact and waits before connecting. It never saves a replacement
plaintext archive. Back up both the archive and its OS keyring key: copying only
`archive.db` to another computer is insufficient.

A missing key is different from a locked keyring. If ZapTide says the key is
missing, restore the original OS credential store or use the original profile
location. Do not delete the archive or create replacement credentials: neither
can decrypt the existing archive. For help, report the OS, app version, whether
the profile was moved/restored, and the error text with personal paths removed.
Never attach the archive, keys, or full logs from older releases.

Only `archive.db` and its SQLite journal/WAL are encrypted. Device credentials in
`session.db`, downloaded media, profile pictures, saved sticker files and settings
remain ordinary files. Use full-disk encryption for those files, swap, backups and
remnants of the old plaintext archive. Migration removes the original only after
verifying its encrypted copy; deletion cannot guarantee erasure from SSDs or
snapshots. Keyring unlocking also does not protect against software running as you
while your login is unlocked.

### From source

ZapTide needs Rust, a C/C++ toolchain, CMake and Perl (for bundled OpenSSL). `rust-toolchain.toml` pins the exact version. On Linux,
it also needs GUI development packages:

```sh
# Arch
sudo pacman -S libxkbcommon wayland mesa alsa-lib gstreamer gst-plugins-base gst-plugins-good cmake perl
```

Then:

```sh
cargo install --path .
zaptide
```

The desktop file and icon are in `packaging/`.

`whatsapp-rust` is pinned to a Git commit because version 0.7.0 on crates.io
enables a `simd` feature that needs nightly Rust. The pinned commit builds on
stable Rust and includes the upstream fixes for missing app-state snapshots and
conflicts that make no progress. ZapTide does not reset your session to recover
a collection.

## Using it

On first start, scan the QR code from WhatsApp under **Linked devices**,
**Link a device**. To link without the camera, click **Link with phone number
instead**, enter your number with its country code, then enter the shown code
on your phone.

WhatsApp then sends your recent history. This can take a few minutes. A banner
shows the progress. New messages arrive live, and your phone does not need to
stay on the same network.

Right-click a chat or message to open its menu. Double-click beside a message,
or on its edge, to reply to it (a double-click on its text still selects the
word). Open Settings from the gear or
with `Ctrl+,`. Use the pencil to message a new number or save a contact. You
can also open a group member's contact card. Saved names sync through WhatsApp
to your phone and linked devices.

### Locked chats

**Lock chat** in a chat's right-click menu moves the chat into a locked
folder: it disappears from the chat list, search, and the unread badge, and
its messages never raise a desktop notification. The lock state syncs
with your phone and other linked devices.

Set a **secret code for locked chats** in Settings, then type the code in the
search field: a "Locked chats" entry appears below the search. Click it to
open the folder; leaving it (back button, or changing the search) hides the
locked chats again until you retype the code.

On the first start after upgrading, chats wait for WhatsApp's lock-state
recovery before appearing. Failed recovery retries while keeping chats hidden.
The recovered state is saved in the encrypted archive for offline use.

Protocol logs omit private payloads and raw error details, including verbose
logging. Panic logs record the source location without the panic payload.
Pairing signature failures and rate limits retain a diagnostic category.

Offline previews for these states use `--demo --demo-page channel`,
`--demo --demo-page locked`, and `--demo --demo-page keyring`.

The protocol dependency includes the upstream WhatsApp Business pairing fix.
Device-store migration waits until an updated window is acknowledged, preserving
startup rollback; an unused legacy column is retained for 0.14 compatibility.

## Files

| What | Linux | Notes |
| --- | --- | --- |
| Settings | `~/.config/zaptide/settings.json` | JSON, safe to edit |
| Device keys | `~/.local/state/zaptide/session.db` | Owned by whatsapp-rust; deleting it unlinks |
| Messages | `~/.local/state/zaptide/archive.db` | SQLCipher-encrypted SQLite, unlocked by the OS keyring; raw messages retain attachment keys |
| Attachments, avatars | `~/.cache/zaptide/` | Safe to delete |
| Saved stickers and packs | `~/.local/state/zaptide/stickers/` | Plain WebP files; each pack is a folder |
| Log of the last run | `~/.local/state/zaptide/zaptide.log` | `--verbose` for more |

ZapTide has its own XDG directories, keyring service, process identity, and single-instance wire
protocol. It never opens, moves, copies, or deletes ZapFast, FastsApp, or FastWhatsApp data
automatically. Link ZapTide as a separate companion device.

ZapTide restricts its configuration, state, and cache directories to the
current user (`0700`), including existing installations. Startup stops if those
directories cannot be created or secured, before opening logs or databases.

### Local themes

**Settings → Appearance → Theme** uses the same picker as Spotifast, with
Follow system, Light, Dark, and its Catppuccin, Catppuccin Latte, Nord, Ristretto,
Tokyo Night, Rose Pine, Rose Pine Moon, and Rose Pine Dawn palettes.
Choose **Open themes folder** below the picker to add
JSON palettes beside `settings.json`. A local file with a bundled palette's name
overrides it. For example:

```json
{"base":"dark","colors":{"accent":"#89b4fa","bubble_out":"#293954"}}
```

Unspecified colors inherit the light or dark base. Spotifast palettes also work:
chat backgrounds, bubbles, and links derive from their interface colors when not
specified. Color names match `Palette`
in `src/theme.rs`; use `#RRGGBB` or `#RRGGBBAA`. The last accepted palette is cached
in settings, so a missing or damaged theme file does not reset your appearance.
Linux watches the themes folder for changes without periodic repaints. On other
platforms, use `zaptide reload-themes` after editing. The command also works while
the window is closed and never launches a stopped app.

**Follow system** uses the desktop's light/dark preference.

### Updating ZapTide

ZapTide updates through the package manager or Flatpak remote it was installed
from. It does not download or install releases itself.

## Developing

```sh
cargo run --features demo -- --demo            # sample chats, no connection
cargo run --features demo -- --demo-page login # or settings, pair, info, light, …
cargo run --features demo -- --demo-shot shot.png --demo-page chat,light
cargo run --features demo -- --demo-tour      # Space starts/replays a 41-second tour
cargo test --all-features                      # includes a headless layout of every screen
cargo clippy --all-targets --all-features -- -D warnings
```

The Task 4 native-shell synthetic harness requires `native-shell,demo` and the
explicit `ZAPTIDE_NATIVE_SYNTHETIC=1` opt-in. It uses a detached command recorder and
synthetic Link, Chats, and Messages events; no account, session, user archive,
or network connection is opened. Audit lines contain only command variant names
and counts. Run its Xvfb smoke test with:

```sh
scripts/native-synthetic-e2e.sh
```

The script drives the synthetic chat through AT-SPI with `pyatspi` when installed,
or `xdotool` otherwise. Requires GTK development dependencies, Xvfb, D-Bus, and
either `pyatspi` or `xdotool`.

To include a default GIPHY key for GIF search, set it at build time. A key in
Settings overrides it:

```sh
ZAPTIDE_GIPHY_KEY=your-key cargo build --release
```

`AGENTS.md` describes the architecture and the rules for changes.

### Recording a demo

The `demo` feature uses offline sample chats in a fresh temporary directory.
It does not open your linked account, read your message archive, connect to
WhatsApp, or register a tray icon. You can run it alongside your regular app.

```sh
cargo build --locked --features demo
./target/debug/zaptide --demo-tour --demo-size 1280x800
```

The **ZapTide Demo** window waits for **Space**. The 41-second tour starts with
search, switches chats with keyboard shortcuts, scrolls, right-clicks a message
and selects Reply, types quickly, completes emoji and mentions, searches the GIF
picker and sends a still sticker, opens group information and the shortcut list,
and changes themes through Settings. It uses the normal mouse and keyboard handlers;
a local responder handles outgoing messages with no WhatsApp connection.
The GIF-search thumbnails and still stickers are rendered from the bundled
Noto emoji font; demo GIF search uses these local fixtures. The tour makes no
sound and holds its final frame. Space rebuilds the sample and replays.
For an automatic start, add `--demo-tour-delay 5000` (milliseconds).
Use `--demo` instead of `--demo-tour` to explore the sample chats yourself.

To record the tour, start a screen recording of the demo window, then press
Space in ZapTide. Keep the window visible and stationary until the tour finishes.

To annotate the video with a visible pointer, click rings, and outlined shortcut
labels, add `--demo-tour-events tour.json` when launching the tour. After
recording, run:

```sh
python3 scripts/render-demo.py recording.mp4 tour.json launch.mp4 --start 0.8
```

Set `--start` to the recording time (in seconds) when you pressed Space. The
export trims the setup footage, adds a caption band below the app, and produces
a silent H.264 MP4. It requires `ffmpeg` with libass support and `ffprobe`.
These annotations are added during video export, not drawn by the app. The
trace contains only pointer coordinates and shortcut labels, not typed text.

## Disclaimer

ZapTide is an unofficial client and is not affiliated with WhatsApp or
Meta. Using an unofficial client may be against WhatsApp's terms of service
and could get an account suspended. Use it at your own risk.

## Packaging maintenance

Release packaging uses the [native-packages](https://rubygems.org/gems/native-packages) gem. `native-packages.yaml` declares the RPM package; installation assets live in `packaging/`; see [PACKAGING.md](PACKAGING.md) for local commands and CI behavior.

## License

MIT. Inter and Noto Color Emoji are under the SIL Open Font License; the icons
are from [Lucide](https://lucide.dev) (ISC).
