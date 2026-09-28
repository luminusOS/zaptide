---
title: Getting Started
description: Install ZapTide, link your phone, and load chat history.
nav_order: 2
---

## Install

ZapTide has no stable packages yet. Build the transition baseline from source.

Or build from source with a recent stable [Rust](https://rustup.rs):

```sh
git clone https://github.com/luminusOS/zaptide zaptide
cd zaptide
cargo install --path .
zaptide
```

On Linux, the build needs the GTK4 and libadwaita development libraries, ALSA, and CMake.
libopus and the H.264 decoder build from source. On Arch Linux:

```sh
sudo pacman -S --needed gtk4 libadwaita alsa-lib gstreamer gst-plugins-base gst-plugins-good cmake
```

A desktop entry ships in `packaging/applications/dev.luminusos.ZapTide.desktop`.

## Link with your phone

ZapTide links as a companion device, like WhatsApp Web. Start it and either:

- scan the QR code with your phone (WhatsApp, **Settings**, **Linked
  devices**, **Link a device**), or
- click **Link with phone number** and enter the eight-character code on your
  phone.

The link survives restarts. Your phone does not need to stay on the same
network or be online to read messages already stored in ZapTide.

## Message history

After linking, the phone sends recent history. The chat list appears within
seconds, and messages can take a few minutes to finish loading. ZapTide stores
new messages in its own archive. When you scroll past the stored history,
ZapTide asks your phone for older messages. The phone must be online.

## Try it in your own chat

Use WhatsApp's **Message yourself** chat to try messages, reactions, edits,
voice messages, and attachments privately.
