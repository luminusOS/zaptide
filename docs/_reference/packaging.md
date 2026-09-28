---
title: Release packaging
description: Shared packaging automation and application-specific release definitions.
nav_order: 20
---

ZapTide keeps release asset definitions and nFPM configuration in
`native-packages.yaml` and `packaging/`. Common automation comes from the pinned
[native-packages](https://github.com/crmne/native-packages) gem, installed with `gem install native-packages --version 0.5.1`.

Stable releases build the Linux artifacts first.
The shared packaging workflow then verifies published checksums and attaches
Linux DEB/RPM packages. PRs only validate packaging.

Native bundle contents remain application-specific.
See the repository's [maintainer packaging guide](https://github.com/luminusOS/zaptide/blob/main/PACKAGING.md)
for commands and the shared tool's [platform coverage](https://github.com/crmne/native-packages/blob/main/docs/platforms.md)
for the boundaries.
