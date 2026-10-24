<div align="center">

<img src="data/icons/zaptide.svg" alt="ZapTide logo" width="128" height="128" />

# ZapTide

A native WhatsApp companion for Linux, built with Rust, GTK4 and libadwaita.

[Download](https://github.com/luminusOS/zaptide/releases/latest) · [Features](#features) · [Install](#install) · [Report a bug](https://github.com/luminusOS/zaptide/issues)

</div>

ZapTide links to your phone through
[whatsapp-rust](https://github.com/oxidezap/whatsapp-rust). It uses a native
desktop interface without a browser engine and stores messages in a local,
encrypted archive.

![ZapTide showing a chat](packaging/screenshots/screenshot.png)

> [!WARNING]
> ZapTide is unofficial and is not affiliated with WhatsApp or Meta. Using an
> unofficial client may violate WhatsApp's terms and put your account at risk.

## Features

- Send text, replies, reactions, polls, voice notes, stickers and attachments.
  Drop files into a conversation; photos sent together appear as an album.
- Search chats and load older messages from the archive or your phone.
- Run several accounts at once, each with its own messages and settings.
  Notifications identify the receiving account and open the right conversation.
- Preview links in the composer. The per-account switch in the Profile dialog
  stays in sync with the phone.
- Star messages and browse them in each conversation's Contact Info or Group
  Info. Choosing a starred message scrolls to it; stars sync with your phone.
- Share saved WhatsApp contacts or preview and send a `.vcf` file through
  **Attach > Contact**. Received contacts have actions to copy their number,
  add them to your contacts app or start a chat.
- Get desktop notifications and typing and delivery status. Read state, mute
  settings and chat locks stay in sync with the phone.
- Choose a light, dark or custom theme. The tray icon follows the desktop's
  light/dark preference independently of the app's appearance.

## Install

[Download the latest release](https://github.com/luminusOS/zaptide/releases/latest).
Flatpak bundles and AppImages are available for **x86_64** and **aarch64 (ARM64)**.
There are no native distribution packages yet.

### Flatpak

With a [Flathub remote configured](https://flathub.org/setup), install the
downloaded bundle. Replace the filename below with the one you downloaded:

```sh
flatpak install --user ./zaptide-vX.Y.Z-x86_64.flatpak
flatpak run dev.luminusos.ZapTide
```

To replace it with a newer downloaded bundle, use `--reinstall`:

```sh
flatpak install --user --reinstall ./zaptide-vX.Y.Z-x86_64.flatpak
```

Use `--system` instead of `--user` if that is where you installed ZapTide.

### AppImage

Save the downloaded AppImage as `zaptide.AppImage`, make it executable and run it:

```sh
chmod +x ./zaptide.AppImage
./zaptide.AppImage
```

Use [AppImageUpdate](https://github.com/AppImageCommunity/AppImageUpdate) to
update in place by downloading only the changed blocks. AppImages from before
0.7.2 need to be replaced manually once because they lack update information.

After updating either format, quit ZapTide from its menu and reopen it. Closing
the window can leave the old version running in the background.

## Link your account

1. Open WhatsApp's **Linked devices** menu on your phone.
2. Scan ZapTide's QR code, or choose to link with your phone number.
3. Allow a few minutes for recent history to arrive.

Add or switch accounts from the avatar above the chat list. **Remove Account…**
unlinks the selected account and deletes only its local data.

## Using ZapTide

- **Close a conversation:** Escape first dismisses message selection, an edit or
  a reply, then returns to the empty home screen. **Close Chat** in the chat menu
  also returns home. Your draft stays with the chat; closing marks nothing as read.
- **Typing:** choosing an emoji returns focus to the message box. Enter sends by
  default and Shift+Enter adds a line; this is configurable.
- **Stickers:** the picker includes recently used stickers from your phone and
  stickers you sent or saved locally. Received stickers stay in their chats
  unless saved; they are not automatically added to the picker.
- **Interactive messages:** answer quick-reply buttons and single-select lists.
  Mentions use saved names in messages and copied transcripts.
- **Tray support:** GNOME needs a StatusNotifierItem extension such as
  AppIndicator to show the tray icon.

## Your data

> [!IMPORTANT]
> The archive is encrypted with a key in your OS keyring. Back up both:
> `archive.db` alone cannot be decrypted. Unlock a locked keyring and retry;
> if the key is missing, restore the original credential store instead of
> replacing the key or deleting the archive.

Device credentials, downloaded files, saved stickers and settings are not
encrypted by ZapTide. Use full-disk encryption to protect those files and backups.
ZapTide keeps separate data folders and keyring entries from ZapFast, FastsApp
and FastWhatsApp; link it as a separate companion device.

Link previews contact the website directly while you compose, without a proxy
or redirects. Private and reserved network destinations are blocked. Disable
previews per account or remove an individual preview before sending.

If phone settings cannot be read, stored chats remain available with a notice.
Locked chats stay locked until the phone's settings arrive.

## Limitations

- Calls, status posts, communities and group administration are not supported.
  Newsletter channels are read-only.
- Videos and documents open in your default desktop apps.
- Messages with disappearing timers remain in the local archive after they
  expire on your phone.
- Starred messages are listed per chat, and locked chats list none.
- Keyboard shortcuts and screen-reader labels are available, but accessibility
  support is still incomplete.

## Build from source

Install Rust through rustup and the native dependencies listed in
[PACKAGING.md](PACKAGING.md). The repository pins its Rust toolchain and requires
GTK 4.14+ and libadwaita 1.10+; CI builds on Fedora 45.

```sh
git clone https://github.com/luminusOS/zaptide.git
cd zaptide
cargo build --locked
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for local checks and running the app with
translations, and [ARCHITECTURE.md](ARCHITECTURE.md) for the code layout.
Report vulnerabilities privately through [SECURITY.md](SECURITY.md).

## About the project

ZapTide is a [LuminusOS](https://luminusos.org) project and a hard fork of
[ZapFast](https://github.com/crmne/zapfast) by Carmine Paolino. It has since
diverged, with a GTK4/libadwaita interface built with Relm4 and support for
multiple accounts. The original code is © Carmine Paolino; see [LICENSE](LICENSE).
