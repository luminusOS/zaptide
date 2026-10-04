<div align="center">

<img src="packaging/icons/zaptide.svg" alt="ZapTide logo" width="160" height="160" />

# ZapTide

A native WhatsApp companion for Linux, built with Rust, GTK4 and libadwaita.

[LuminusOS](https://luminusos.org) · [Report a bug](https://github.com/luminusOS/zaptide/issues)

</div>

ZapTide links to your phone through [whatsapp-rust](https://github.com/oxidezap/whatsapp-rust).
It uses a native desktop interface instead of a browser engine. Messages are
stored locally in an encrypted archive.

![ZapTide showing a chat](packaging/screenshots/screenshot.png)

## Features

- Chat, search and send text, replies, reactions, polls, voice notes, stickers
  and attachments. Older messages load from the archive or your phone.
- Contact messages show names and phone numbers with actions to copy a number,
  add the contact to your contacts app, or message it on WhatsApp.
  Use **Attach → Contact** to share a saved WhatsApp contact or choose a `.vcf`
  file through the system file chooser. Preview the contact before sending.
- Photos sent together appear as one album in a single bubble, and files can be
  dropped anywhere in a conversation to attach them.
- The sticker picker shows your recently used stickers from your phone and
  stickers you sent or saved locally. Stickers received from other people stay
  in chats and do not appear in the picker automatically.
- Read and answer quick-reply buttons and single-select lists when a sender
  delivers them. Mentioned contacts show their saved names in messages and
  copied transcripts; unknown contacts can show their WhatsApp name on hover.
- Get desktop notifications, see typing and delivery status, and keep chat
  read state, mute settings and locks in sync with your phone. Chats already
  stored on this computer appear even when that settings sync cannot be read
  from your phone, and a notice says so; locked chats stay locked until the
  phone's settings arrive.
- When reading older messages, new arrivals keep your position and provide a
  button to return to recent messages.
- Use light, dark or custom themes. Navigate with keyboard shortcuts and use
  screen-reader labels; accessibility support is still incomplete.

Videos and documents open in your default desktop apps. Calls, status posts,
communities and group administration are not supported. Newsletter channels
are read-only. Messages with disappearing timers remain in the local archive
after they expire on your phone.

## Install

Download a Flatpak bundle or AppImage for x86_64 or aarch64 from
[GitHub Releases](https://github.com/luminusOS/zaptide/releases).

With a Flathub remote configured, install the downloaded `.flatpak` bundle:

```sh
flatpak install --user ./zaptide-vX.Y.Z-x86_64.flatpak
```

For an ARM64 system, use the `aarch64` bundle instead.

Save the downloaded AppImage as `zaptide.AppImage`, make it executable and run it:

```sh
chmod +x ./zaptide.AppImage
./zaptide.AppImage
```

On first launch, use WhatsApp's **Linked devices** menu to scan the QR code or
link with your phone number. Recent history may take a few minutes to arrive.

## Your data

The message archive is encrypted with a key held by your OS keyring. Keep both
the archive and the keyring credential when backing up or moving a profile:
`archive.db` alone cannot be decrypted. If the keyring is locked, unlock it and
retry. If the key is missing, restore the original credential store rather
than replacing the key or deleting the archive.

Device credentials, downloaded files, saved stickers and settings are not
encrypted by ZapTide. Use full-disk encryption if you need to protect those
files and backups. ZapTide keeps its data separate from ZapFast, FastsApp and
FastWhatsApp; link it as a separate companion device.

## Development

See [PACKAGING.md](PACKAGING.md) for build dependencies and Flatpak packaging, and
[AGENTS.md](AGENTS.md) for architecture and contribution rules. ZapTide is a
[ZapFast](https://github.com/crmne/zapfast) fork with a native GTK interface.

ZapTide is unofficial and is not affiliated with WhatsApp or Meta. Using an
unofficial client may violate WhatsApp's terms and put your account at risk.

Licensed under [MIT](LICENSE).
