<div align="center">

<img src="data/icons/zaptide.svg" alt="ZapTide logo" width="128" height="128" />

# ZapTide

A native WhatsApp companion for Linux, built with Rust, GTK4 and libadwaita.

[Download](https://github.com/luminusOS/zaptide/releases/latest) · [User guide](docs/user-guide.md) · [Report a bug](https://github.com/luminusOS/zaptide/issues)

</div>

ZapTide links to your phone through [whatsapp-rust](https://github.com/oxidezap/whatsapp-rust),
without a browser engine. Messages stay in a local, encrypted archive.

![ZapTide showing a chat](packaging/screenshots/screenshot.png)

> [!WARNING]
> ZapTide is unofficial and is not affiliated with WhatsApp or Meta. Using an
> unofficial client may violate WhatsApp's terms and put your account at risk.

## Features

- Messages, voice notes, polls, stickers and attachments.
- Multiple accounts with separate history and settings.
- Chat search, starred messages and link previews.
- Desktop notifications, synced read status and call history.
- Light, dark and custom themes.

Voice and video calls are experimental. Calls against a phone have not been
verified; see [call limitations](docs/user-guide.md#calls) before relying on them.
Group calls, status posts, communities and group administration are not supported.

## Install

Download a [Flatpak or AppImage](https://github.com/luminusOS/zaptide/releases/latest)
for x86_64 or aarch64 (ARM64). Native distribution packages are not available.

For Flatpak, configure [Flathub](https://flathub.org/setup), then install the
downloaded bundle (replace the filename):

```sh
flatpak install --user ./zaptide-vX.Y.Z-x86_64.flatpak
flatpak run dev.luminusos.ZapTide
```

For AppImage, save the download as `zaptide.AppImage`:

```sh
chmod +x ./zaptide.AppImage
./zaptide.AppImage
```

Open WhatsApp on your phone and scan the QR code from **Linked devices**, or
link with your phone number. History may take a few minutes to arrive.

> [!IMPORTANT]
> Backups need the archive and its keyring credential. `archive.db` alone cannot
> be decrypted. Downloads and settings are not encrypted. Read
> [data and backups](docs/user-guide.md#data-and-backups) before moving or deleting files.
> Disappearing messages remain in the local archive after they expire on your phone.

## Documentation

Read the [user guide](docs/user-guide.md) for updates, preferences, media and
known limitations. For source builds, dependencies and development checks, see
[CONTRIBUTING.md](CONTRIBUTING.md) and [PACKAGING.md](PACKAGING.md).
[ARCHITECTURE.md](ARCHITECTURE.md) describes the code; [AGENTS.md](AGENTS.md)
sets the rules for coding agents. Report vulnerabilities through
[SECURITY.md](SECURITY.md).

ZapTide is a [LuminusOS](https://luminusos.org) hard fork of
[ZapFast](https://github.com/crmne/zapfast), distributed under
[GPL-3.0-or-later](LICENSE). Original code © Carmine Paolino remains covered
by its [MIT notice](LICENSE-MIT).
