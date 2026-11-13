# Using ZapTide

See the [README](../README.md#install) for installation.

## Accounts and updates

Link from WhatsApp's **Linked devices**, using a QR code or your phone number.
The avatar adds or switches accounts. **Remove Account…** unlinks that account
and deletes its local data; other accounts are unaffected.

To update a Flatpak bundle, download the new version and replace the filename:

```sh
flatpak install --user --reinstall ./zaptide-vX.Y.Z-x86_64.flatpak
```

Use `--system` instead of `--user` for a system installation. AppImages support
[AppImageUpdate](https://github.com/AppImageCommunity/AppImageUpdate), which
downloads changed blocks. Replace pre-0.7.2 AppImages manually once because
they lack update information.

After updating, quit with Ctrl+Q or the menu and reopen. Closing the window
with Ctrl+W can leave the old version running. ZapTide has no built-in updater.

## Messages

Enter sends and Shift+Enter adds a line; this is configurable. Escape dismisses
selection, edits or replies before closing the chat. **Close Chat** returns
home, keeps drafts and marks nothing as read. Choosing an emoji returns focus
to the composer.

Names take priority over numbers: saved contact name, then WhatsApp name, then
phone number. This applies to messages, sidebar previews and notifications.
Mentions use bold text and the theme's accent color.

The composer and bubbles support WhatsApp formatting:

````text
*bold* _italic_ ~strikethrough~
`inline code`
```monospace```
- bulleted item
1. numbered item
> quoted text
````

The composer keeps delimiters visible and sends the original text. List items
keep their indentation when wrapped, and links remain selectable and clickable.
Bubbles fit their text up to the conversation's width limit; sender names,
quotes and attachments may need more space.

Formatting has not met the required work and whole-process memory bounds for
large, densely formatted messages and is not release-ready. The private GTK
and Pango builds used to investigate this are not supported dependencies.

## Notifications

Under **Preferences → Notifications**, choose whether to play a sound and the
minimum interval between sounds. The default is 30 seconds; choices range from
no interval to two minutes, shared across chats and accounts. The desktop
controls playback, including Do Not Disturb and its own sound settings.

Flatpak uses version 2 of the notification portal for sound. Other builds use
the desktop notification service when it supports sound. If neither supports
it, or the app's sound is off, notifications still arrive and the desktop may
play its own sound. [Sound attribution](../assets/sounds/README.md).

ZapTide goes offline soon after you leave it or stop using it, so WhatsApp can
notify your phone. The delay is configurable under Notifications.

## Media and downloads

Photos and playable videos open in the main panel. Back or Escape returns to
the chat. Photos support zoom and album navigation; videos have playback,
volume and fullscreen controls. **View Caption** opens a long caption without
shrinking the media. **Open With Another App** remains available. Right-click
a downloaded photo and choose **Copy Image** to copy it to the clipboard.

GIFs play silently while visible. Hiding them releases playback resources;
showing them again restarts playback. Lottie stickers play only when they use
the supported animation subset, including positions animated on separate axes.
Unsupported animations show a placeholder.

In the sticker picker, **Unstar** removes matching message stars from unlocked
chats without deleting saved files. **Remove** deletes matching copies from the
account's stickers folder without changing message stars.

Audio, PDF, EPUB, RTF, office documents, plain text, Markdown, CSV and common
archives open in their default app. Other documents, or files whose contents
do not match their names, ask for confirmation. These include scripts,
launchers, installers, packages, web pages, SVG, PostScript and playlists.
Flatpak then lets you choose the app. Very long filenames are shortened in
the download folder.

**Preferences → Storage → Delete old downloads** removes cached attachments
after the period you choose, from 30 days to one year. It runs after startup
and every few hours. Messages, saved stickers and files outside the media
cache stay. Viewing or playing a file protects it from cleanup. Removed media
can only download again while WhatsApp still has it.

Voice-note playback at 1.5x or 2x needs GStreamer's `scaletempo` plugin from
gst-plugins-good. Without it, playback works at 1x and other speeds show an error.

## Calls

> [!WARNING]
> No voice or video call against a phone has been verified. Audio quality and
> video interoperability remain untested, as do desktop ring playback and
> notification-button interaction. Treat calls as experimental.

Use the phone or camera button in a direct chat. Incoming calls appear in the
main window and offer Answer and Decline in a desktop notification, including
for inactive accounts. The call screen has mute and camera controls. You can
browse chats and choose **Return to call** in the sidebar. The Calls tab and
chat events show call history, including calls synced from your phone.

Only one call can be active. A second offers **Decline** or **End and answer**;
ignoring it leaves a missed call. There is no hold or busy reply. Only one
waiting call is announced; a third just rings. Group calls do not ring, and
quitting ends the call.

Calls use WhatsApp relay servers over UDP and the default microphone and
speaker. Video needs a camera, an H.264 encoder (`openh264` or `x264`) and a
decoder (`libav` or `openh264`). Without a camera the call can continue without
your picture. Outside a sandbox, ZapTide tries direct camera access, then the
camera portal if it finds none.

Flatpak uses the camera portal without direct device access. Refusing access
or leaving the prompt unanswered for one minute stops a video call you place;
an answered call continues without your picture. The portal route was tested
outside a sandbox, not inside a built Flatpak. The GNOME runtime's
`codecs-extra` extension supplies x264 when the remote offers it; the missing
extension case has not been tested. AppImage includes a decoder, but its
encoder and camera plugins have not been checked; voice calls are its
supported call path, subject to the testing warning above.

Ringing uses the desktop sound theme or a generated fallback tone. Incoming
ringing respects the app's notification setting and desktop Do Not Disturb.
The sandbox may not be able to read Do Not Disturb and will ring in that case.
The outgoing tone is unaffected. Answering or declining stops the incoming ring.

## Data and backups

Messages in `archive.db` and linked-device credentials in `session.db` are
encrypted. Both depend on the archive's keyring credential. Downloads, saved
stickers and settings are unencrypted; use full-disk encryption for those files
and their backups.

Outside Flatpak, the credential lives in a desktop Secret Service such as
GNOME Keyring or KWallet. Inside Flatpak it normally lives in
`~/.var/app/dev.luminusos.ZapTide/data/keyrings/default.keyring`, encrypted with
a per-app secret stored in the desktop keyring by the Secret portal. Back up
both the file and the desktop keyring. Without a Secret portal backend,
Flatpak uses the desktop Secret Service directly.

Unlock a locked keyring or restore a missing credential. Do not replace the
key or delete the archive to work around an error. If the credential is
permanently lost, removing and relinking the account deletes the unreadable
files; only history sent by the phone during linking can return. Older builds
cannot read encrypted `session.db`, so downgrading requires relinking.

ZapTide keeps its data and credentials separate from ZapFast, FastsApp and
FastWhatsApp. Link it separately. Native and Flatpak runs also use separate
data directories and may need separate linking.

Link previews contact websites directly while composing, without proxies or
redirects. Private and reserved destinations are blocked. Disable previews
per account or remove an individual preview.

If phone settings cannot be read, stored chats remain available with a notice.
Chats locked on the phone stay hidden, including their calls and starred
messages. Manage locks on your phone; ZapTide has no lock code or bypass.

## Other limitations

Status posts, communities and group administration are not supported.
Newsletter channels are read-only. Disappearing messages remain in the local
archive after expiring on your phone. Keyboard shortcuts and screen-reader
labels are available, but accessibility is incomplete.
