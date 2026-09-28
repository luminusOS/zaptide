---
title: Download
description: Get ZapTide for Linux, with install instructions.
nav_order: 1
---

ZapTide has no stable release yet. Existing ZapFast and FastsApp packages are
different applications and do not share ZapTide's data.

Build the transition baseline from source:

```sh
git clone https://github.com/luminusOS/zaptide
cd zaptide
cargo build --release --locked
```

ZapTide needs ALSA, libxkbcommon, Wayland, OpenGL development files, CMake, and
a C/C++ toolchain. See [Getting Started]({{ site.baseurl }}/getting-started/) for distribution
commands. Release packages will appear on the
[GitHub releases page](https://github.com/luminusOS/zaptide/releases) after the
GTK/libadwaita migration passes its release gates.
