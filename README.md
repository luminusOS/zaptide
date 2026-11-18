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

Install the Flatpak from the Luminus repository, which keeps ZapTide updated
with the rest of your Flatpak apps. Runtimes come from Flathub, which Flatpak
adds if it is missing:

```sh
flatpak install --user --from https://flatpak.luminusos.org/stable/dev.luminusos.ZapTide.flatpakref
flatpak run dev.luminusos.ZapTide
```

Release candidates use the testing channel:
`https://flatpak.luminusos.org/testing/dev.luminusos.ZapTide.flatpakref`.

If you installed a Flatpak bundle from GitHub, move it to the repository once.
Your chats, keys and settings stay:

```sh
flatpak remote-add --user --if-not-exists luminus https://flatpak.luminusos.org/stable/luminus-stable.flatpakrepo
flatpak install --user --reinstall luminus dev.luminusos.ZapTide//stable
```

> [!NOTE]
> ZapTide's Flatpak ID is changing to `org.luminusos.ZapTide`. Repository
> installs move to it automatically. Open ZapTide 0.9.0 or later once before
> that update so your archive keys move with it.

Flatpak bundles and AppImages for x86_64 or aarch64 (ARM64) are also on the
[releases page](https://github.com/luminusOS/zaptide/releases/latest). Native
distribution packages are not available.

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
