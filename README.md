<div align="center">

<img src="data/icons/zaptide.svg" alt="ZapTide logo" width="128" height="128" />

# ZapTide

A native WhatsApp companion for Linux, built with Rust, GTK4 and libadwaita.

[Download](https://github.com/luminusOS/zaptide/releases/latest) · [Features](#features) · [Install](#install) · [Report a bug](https://github.com/luminusOS/zaptide/issues)

</div>

ZapTide links to your phone through [whatsapp-rust](https://github.com/oxidezap/whatsapp-rust),
without a browser engine. Messages stay in a local, encrypted archive.

![ZapTide showing a chat](packaging/screenshots/screenshot.png)

> [!WARNING]
> ZapTide is unofficial and is not affiliated with WhatsApp or Meta. Using an
> unofficial client may violate WhatsApp's terms and put your account at risk.

## Features

- Text, voice notes, polls, stickers and attachments.
- Multiple accounts with separate history and settings.
- Chat search, starred messages and link previews.
- Desktop notifications and synced read status.
- Light, dark and custom themes.

## Install

[Download a Flatpak or AppImage](https://github.com/luminusOS/zaptide/releases/latest)
for x86_64 or aarch64 (ARM64). Native distribution packages are not available yet.

### Flatpak

Configure [Flathub](https://flathub.org/setup), then install the bundle (replace the filename):

```sh
flatpak install --user ./zaptide-vX.Y.Z-x86_64.flatpak
flatpak run dev.luminusos.ZapTide
```

For a newer bundle, use `--reinstall`:

```sh
flatpak install --user --reinstall ./zaptide-vX.Y.Z-x86_64.flatpak
```

For a system installation, replace `--user` with `--system`.

### AppImage

Save the download as `zaptide.AppImage` and run it:

```sh
chmod +x ./zaptide.AppImage
./zaptide.AppImage
```

Update in place with [AppImageUpdate](https://github.com/AppImageCommunity/AppImageUpdate),
which downloads only changed blocks. Replace pre-0.7.2 AppImages manually once;
they lack update information.

After updating, quit from the menu and reopen. Closing the window can leave the
old version running.

## First steps

Scan the QR code from WhatsApp's **Linked devices**, or link with your phone number.
History may take a few minutes. The avatar adds or switches accounts;
**Remove Account…** unlinks one and deletes only its local data.

Enter sends; Shift+Enter adds a line (configurable). Emoji selection returns focus
to the input. Escape dismisses selection, edits or replies before closing the chat.
**Close Chat** also returns home; both keep drafts and mark nothing as read.

## Your data

> [!IMPORTANT]
> Back up the archive with its OS keyring credential: `archive.db` alone cannot
> be decrypted. Unlock a locked keyring; restore a missing credential instead of
> replacing the key or deleting the archive.

Device credentials, downloads, saved stickers and settings are unencrypted; use
full-disk encryption for them and backups. Data and keyring entries are separate
from ZapFast, FastsApp and FastWhatsApp. Link ZapTide separately.

Previews contact websites directly while composing, without proxies or redirects.
Private/reserved destinations are blocked. Disable previews per account or remove one.

Stored chats remain available with a notice if phone settings cannot be read;
locked chats stay locked until those settings arrive.

## Limitations

- Calls, status posts, communities and group administration are not supported.
  Newsletter channels are read-only.
- Messages with disappearing timers remain in the local archive after they
  expire on your phone.
- Keyboard shortcuts and screen-reader labels are available; accessibility is incomplete.

## Build from source

Install Rust through rustup and the dependencies in [PACKAGING.md](PACKAGING.md).
The toolchain is pinned. GTK 4.14+ and libadwaita 1.10+ are required; CI uses Fedora 45.

```sh
git clone https://github.com/luminusOS/zaptide.git
cd zaptide
cargo build --locked
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for checks and running with translations,
and [ARCHITECTURE.md](ARCHITECTURE.md) for the code layout.
Report vulnerabilities privately through [SECURITY.md](SECURITY.md).

## About the project

ZapTide is a [LuminusOS](https://luminusos.org) hard fork of
[ZapFast](https://github.com/crmne/zapfast), with a Relm4 interface and multiple accounts.
Original code © Carmine Paolino; see [LICENSE](LICENSE).
