---
title: Settings & Files
description: Settings and paths for the archive, configuration, caches, and logs.
nav_order: 0
---

## File locations

ZapTide follows each platform's conventions. On Linux:

| What | Where | Safe to delete? |
| --- | --- | --- |
| Settings | `~/.config/zaptide/settings.json` | Yes, you lose preferences |
| Message archive | `~/.local/state/zaptide/archive.db` | Yes; only history available from WhatsApp can be restored |
| Session keys | `~/.local/state/zaptide/session.db` | Yes; you must link again |
| Attachments | `~/.cache/zaptide/media/` | Yes; available files download again when viewed |
| Profile pictures | `~/.cache/zaptide/avatars/` | Always |
| Stickers | `~/.cache/zaptide/stickers/` | Always |
| GIF search stills | `~/.cache/zaptide/gifs/` | Always |
| Last run's log | `~/.local/state/zaptide/zaptide.log` | Always |
| Crash log | `~/.local/state/zaptide/panic.log` | Always |

Back up the archive if you need its history. WhatsApp sends only recent
history to a new device, although ZapTide can request some older messages from
the phone. Clearing the media cache makes ZapTide download attachments again.
Expired attachments may still be available through the phone.

On macOS, settings, state, and logs are under
`~/Library/Application Support/dev.luminusos.zaptide`; caches are under
`~/Library/Caches/dev.luminusos.zaptide`. On Windows, settings are under
`%APPDATA%\luminusos\zaptide\config`, state and logs under
`%LOCALAPPDATA%\luminusos\zaptide\data`, and caches under
`%LOCALAPPDATA%\luminusos\zaptide\cache`.

ZapTide never opens, moves, copies, or deletes ZapFast, FastsApp, or
FastWhatsApp data automatically. Link it as a separate companion device.

## Settings

Changes on the Settings page are saved to `settings.json` immediately:

- **Theme**: light, dark, or follow the system.
- **Enter sends**: swap Enter and Shift+Enter.
- **Download attachments automatically**: download files up to 64 MB when
  they enter view, or only when clicked.
- **Show sender pictures**: avatars next to group messages.
- **Names from your address book**: use contact names everywhere. When off,
  prefer public profile names.
- **Send read receipts**: the blue ticks others see.
- **Keep running in the background**: keep ZapTide running after its window
  closes.
- **Notifications**: use desktop notifications with the chat picture.
- **Show previews**: include the sender and message text in notifications.

## The log

Each run replaces `zaptide.log` and records warnings and errors. Include the
end of this file when reporting an issue.
