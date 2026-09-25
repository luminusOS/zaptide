# ZapTide agent guide

ZapTide is a Linux-first native WhatsApp client forked from ZapFast: Rust, GTK4/libadwaita through Relm4, and the
[whatsapp-rust](https://github.com/oxidezap/whatsapp-rust) library for the
protocol. These notes are for coding agents and new contributors.

## Product boundaries

- Keep it a small native client. No browser engine, no telemetry, no
  hosted backend, no second account system.
- The protocol comes from whatsapp-rust. Do not reimplement pieces of it
  here, and do not advertise a capability merely because a protobuf field
  for it exists.
- Do not broaden a task into adjacent features or a general refactor.
  Preserve existing user behaviour unless the task changes it.

## Privacy

- The user's archive is personal data. Do not read chat rows, message
  bodies, contacts, or other user content out of `archive.db` or any
  exported log, not even read-only. Schema, column existence, and row
  counts are fine; message contents are not.
- When a bug report or feature needs the user's data, hand the user the
  query or command to run and let them report the result back.
- Never log message contents, phone numbers, keys, or QR payloads at a
  level that ships (see the definition of done); treat existing
  captures of them the same way.

## Architecture

- `src/application.rs` is the Relm4 root component: the link page, the chat
  sidebar, and the conversation. Widgets send `Input`s; `update` applies them.
  Never mutate application state from a signal handler beyond the widget's own
  value (composer text, search text, toggles).
- `src/backend.rs` is the interface's handle to a tokio runtime on its own
  thread; `src/backend/worker.rs` runs there. It owns the whatsapp-rust
  `Bot`, the message archive, downloads, and profile pictures. The two
  sides talk only through `Command` (interface to runtime) and `Event`
  (runtime to interface); every event wakes the window through `Waker`.
- `src/archive.rs` is the SQLite store of chats, messages, contacts, and
  privacy-id mappings. WhatsApp replays history once, at link time, so the
  archive is the only copy. It keeps each message's raw protobuf because
  the keys to fetch an attachment live in it. `src/archive/encryption.rs` opens
  the archive with SQLCipher and a random key stored in the OS keyring. Plaintext
  migration checkpoints the old WAL and verifies an encrypted staging file before
  atomic replacement. A locked or missing key stops linking; never fall back to
  a disposable archive. Tests use fixtures and mock credentials only.
- `src/model.rs` holds the app's own types. Views never touch a protobuf;
  the worker translates in `classify()` and `parse_conversation()`.
- Poll creation, voting, and decryption use whatsapp-rust's `Client::polls()`.
  `backend/worker/polls.rs` retains the original creator identity and key in the
  encrypted archive; `archive/polls.rs` keeps each voter's latest timestamp and
  message id, including encrypted updates whose parent has not arrived yet.
  History replay must not undo a newer vote or withdrawal. Decryption runs in
  batches of eight, with failures retried after reconnecting. The interface only
  receives option counts and its own selection, never keys or protobufs. Visible
  polls request phone history automatically, anchored after the creation message
  so the response includes its vote snapshot. `poll_history.rs` serializes these
  requests and retries from 30 seconds to 15 minutes without an interface timer.
  History request timestamps are Unix seconds: the library argument and wire
  field misleadingly end in `Ms`. Do not multiply archive timestamps by 1,000.
  A repeated poll question with no usable vote snapshot cannot finish recovery.
- Chat ids are canonical strings: a chat behind a privacy id (`@lid`) is
  filed under its phone number once the mapping is known. Use
  `Worker::canonical` for anything that arrives as a `Jid`.
- `src/updates/` downloads verified GitHub releases and hands installation to a
  helper after an explicit restart action. Keep package-manager detection, asset
  checksums, startup acknowledgement and rollback intact. Portable releases carry
  `packaging/zaptide-portable.txt`; the Windows installer has its own marker.
- `src/theme/custom.rs` scans local JSON palettes off the UI thread, caching the
  last usable choice in settings, with shared Spotifast palettes embedded as
  defaults. On Linux filesystem notifications reload the catalog and the active
  Omarchy palette without a repaint timer; following Omarchy does not require
  packaged assets. Native packages ship optional hooks and templates, preserving
  existing per-user files. `reload-themes` uses the single-instance channel
  without opening a window.
- `src/native_theme.rs` turns a palette into GTK CSS; structural styling lives
  beside `apply_theme` in `src/application.rs`. Use libadwaita style classes and
  symbolic icons from the icon theme before adding custom CSS.
- Message rows and media widgets live in `src/native_media_widgets.rs`; text
  selection and copy across messages in `src/native_transcript.rs`.
- Group names and members come from `groups().get_metadata`, asked one
  turn at a time (two per 5 s tick, `pump_group_info`): dozens of unnamed
  groups arrive with history sync and a burst of queries hits the
  server's rate limit, which once left groups called "Group" forever.
  Failures back off (30 s doubling, seven tries); item-not-found,
  forbidden and not-authorized are final and stop the asking.
- A download that answers 403/404/410 goes through
  `client.media_reupload().request(..)` (a server-error receipt; WhatsApp
  has the phone re-upload and answers with a fresh `direct_path`) and is
  fetched once more before the bubble reports "No longer on WhatsApp's
  servers". Download failures never toast; they live in the bubble as
  "... · click to retry". Copied text is refined by
  `transcript::refine`: emoji placeholders map back through each row's
  `placements`.
- History sync can bring a chat with a name and no messages at all; a
  history request for such a chat is anchored at the present with an
  empty message id (`worker::fetch_older`), and the app asks the phone
  as soon as such a chat loads or opens, instead of never.
- `src/voice.rs` is the codec for voice messages: OGG/Opus in and out
  (the `ogg` crate for the container, `opus` with libopus bundled and
  built by cmake for the codec, so cmake is a build dependency), plus
  the 64-bar waveform WhatsApp draws and a mono/48 kHz resampler.
  `src/audio.rs` is the sound: `Player` plays one clip at a time through
  rodio (OGG/Opus through `voice`, MP3/M4A/WAV through rodio's decoders,
  decoded on a thread, the device opened on demand and released when the
  clip ends) and `Recorder` reads the default microphone through rodio's
  `Microphone` on a thread, keeping a loudness per 50 ms for the live bars.
  Linux needs ALSA headers to build (`libasound2-dev` on Debian,
  `alsa-lib` on Arch). `Action::PlayVoice/SeekVoice` drive the player from
  the bubble; `StartRecording/CancelRecording/SendRecording` the
  microphone from the composer (the send button is a microphone when there
  is nothing to send); `Command::SendVoice` normalizes
  (`voice::normalize`, quiet takes up to just under full scale, gain
  capped), encodes and sends push-to-talk with the waveform and the reply
  quote if one was open; `Command::MarkPlayed` sends the played receipt
  once per incoming voice message.
- `src/paths.rs` uses a ZapTide-only XDG namespace. Never adopt or open ZapFast,
  FastsApp, or FastWhatsApp data automatically. The keyring service must remain
  separate too. Any importer requires explicit consent and a
  separately reviewed migration plan.
- Quitting shuts the backend down on a thread and then calls
  `relm4::main_application().quit()`. GApplication provides single-instance
  activation. `src/native_notifications.rs` sends GNotifications for live
  messages when the reader is away from that chat.
- Group delivery uses `archive::receipts`: save the recipients when filing an
  outgoing message, record each person's receipt, then take the least advanced
  recipient. Never promote a group from one reader, apply a receipt to earlier
  messages, or infer a historical audience from current membership. History
  trusts the phone's aggregate status, not a partial `user_receipt` list.
- Private read-state writes all use the `regular_low` app-state collection.
  `backend::read_sync` permits one at a time and backs off the whole queue after
  failure; per-chat retry queues would repeatedly rebuild the same failed
  collection. Pending positions stay in the archive until acknowledged. Snapshot
  recovery and no-progress conflict detection belong to whatsapp-rust.
- The name and icon under the phone's Linked devices come from
  `DevicePropsOverride` in `start_bot` (`os` is the name shown, the
  platform type picks the icon); WhatsApp reads them at pairing only, so a
  change shows after unlinking and linking again.
- Older history comes from the phone on demand (`Command::FetchOlder` →
  `Client::fetch_message_history` → a `HistorySync` chunk with
  `sync_type == ON_DEMAND`); the archive is paged first, the phone only
  when it is exhausted.
- Platform-specific code belongs behind `cfg` blocks; a change for one
  platform must keep the other two compiling.
- `Popup::context_menu` opens on the *response's* right-click, which those
  inner widgets take for themselves; the bubble reads the right-click from
  the input over its own rect and opens `Popup::menu` itself, so the menu
  comes up anywhere on the message.

## Releasing

Never use em dashes in user-facing writing, including release titles, release
notes, and agent responses. Use commas, colons, parentheses, or full stops.

Before writing release notes, read the previous two stable releases of
`../spotifast` and match their style: a short plain-language summary, `New`
and `Fixed` sections with bold user-facing results, a `Thanks` section, and
a full-changelog link. Credit who did what on the relevant item, with issue
or PR numbers, and acknowledge reporters separately from implementers.
Include screenshots or short videos of the main features, especially Omarchy
theme integration when relevant. Capture only synthetic offline demo content,
never real chats. Verify every media link and do not leave generated notes
in place. Describe known limitations honestly.

Do not cut a release for every fix. Work accumulates on `main` until
there is something substantial to announce: a feature, or a batch of
fixes worth a changelog entry. Five patch releases in a day is what this
rule exists to prevent. The exception is a regression in something just
released, which goes out as soon as it is fixed.

A release is not finished when the tag is pushed. Do these in order:

1. Bump `version` in `Cargo.toml` and update `Cargo.lock` with a build. Run
   the full checks, commit, and push before tagging so the binaries report
   the right version.
2. Tag `vX.Y.Z` and push the tag. Wait for every platform build, artifact,
   and `checksums.txt`.
3. Replace the generated GitHub notes with written release notes. Start with
   a short summary, group user-visible changes under headings such as `New`
   and `Fixed`, credit contributors and reporters where it helps, and end
   with a full-changelog link comparing the previous tag. Write about what
   changed for the user, not the commit history.
4. After the release files exist, update both `zaptide_version` in
   `docs/_config.yml` and the version menu in `docs/_data/versions.yml`.
   The menu lists only the current version, which points to `/download/`,
   and the Changelog link; do not add older versions to it. Never point the
   download page at files that do not exist yet. Set `release_asset_prefix` to
   `zaptide` and `release_app_name` to `ZapTide` only once those assets exist.
5. Update the AUR packages from the templates in `packaging/arch/`. The shared
   packaging workflow generates versions, hashes and `.SRCINFO` after the
   release exists, and publishes when `PUBLISH_AUR` and the required secrets
   are configured. Otherwise use `native-packages` to build, stage,
   review and publish the generated recipes; see `PACKAGING.md`. Validate
   native builds with `makepkg -f`. A recipe-only `zaptide-git` change does
    not require an application release.

## Platform dependencies

- `gtk::EmojiChooser` renders emoji through the system's color emoji font.
  On Fedora, install `google-noto-emoji-color-fonts`; on Arch,
  `noto-fonts-emoji`; on Debian/Ubuntu, `fonts-noto-color-emoji`. Without
  the font, emoji fall back to monochrome glyphs or render as boxes.

## Definition of done

- Add focused tests for changed behaviour. The `demo` feature carries sample
  data and a headless layout test of every screen (`src/demo.rs`); extend
  the sample when a new kind of content or state is added, and use
  `--demo-shot` to look at the result.
- Update the README when user-visible behaviour, settings, files, or network
  access changes.
- Run the full checks before finishing:

  ```sh
  cargo fmt --all --check
  cargo clippy --locked --all-targets -- -D warnings
  cargo clippy --locked --all-targets --all-features -- -D warnings
  cargo test --locked --all-targets
  cargo test --locked --all-targets --all-features
  RUSTDOCFLAGS='-D warnings' cargo doc --locked --all-features --no-deps
  ```

  Do not weaken a lint, delete a test, or add an `allow` merely to make
  them pass without explaining why the rule does not apply.
- Report platform coverage honestly: say what was run and what was only
  compiled.
- Never log message contents, phone numbers, keys, or QR payloads at a
  level that ships. The log file is meant to be attached to bug reports.
