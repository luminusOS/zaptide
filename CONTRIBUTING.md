# Contributing to ZapTide

Contributions can be bug reports, translations, documentation or code. Read
[ARCHITECTURE.md](ARCHITECTURE.md) for the code layout and
[PACKAGING.md](PACKAGING.md) for dependencies. Coding agents must also follow
[AGENTS.md](AGENTS.md) and the relevant
[architecture contracts](docs/architecture-contracts.md).

## Discuss the change

Search existing issues and pull requests before opening another one. Use the
issue forms for reproducible bugs and feature proposals. Discuss significant
features, architecture changes, dependencies, and account-data migrations before
implementing them. Keep each PR focused on one independently reviewable change.

For vulnerabilities, follow [SECURITY.md](SECURITY.md), not public issues.
Follow our [Code of Conduct](CODE_OF_CONDUCT.md) in all project spaces.

## Protect personal data

- Use synthetic fixtures, a separate development profile, and test accounts.
- Never submit archives, session databases, QR codes, pairing codes, tokens,
  keyring credentials, phone numbers, or real message contents.
- Inspect logs and screenshots before sharing them. Prefer a minimal synthetic
  example; do not attach an entire log or database for convenience.
- Do not change or delete an existing user profile to reproduce a bug.

## Set up development

The Rust toolchain is pinned in `rust-toolchain.toml`. Install Rust through rustup
and the native dependencies listed in [PACKAGING.md](PACKAGING.md). CI uses Fedora
45 because the app requires libadwaita 1.10. Do not lower platform requirements or
update dependencies merely to make a local environment compile.

For a Fedora 45 development environment, the CI dependency set is:

```sh
sudo dnf install gcc gcc-c++ git gtk4-devel libadwaita-devel \
  gstreamer1-devel gstreamer1-plugins-base-devel cmake libseccomp-devel fontconfig-devel perl make tar zstd
cargo build --locked
```

Runtime image loading also needs `glycin-loaders` and `bubblewrap`; install emoji
fonts such as `google-noto-emoji-color-fonts` for emoji UI testing. Flatpak builds
and runtime dependencies are documented in `PACKAGING.md`.

Clone and build with the pinned toolchain:

```sh
git clone https://github.com/luminusOS/zaptide.git
cd zaptide
cargo build --locked
```

Quit any running ZapTide before `cargo run --locked`. Use `just run` to compile
translations before launching. Native runs use the desktop Secret Service and
separate data directories from Flatpak, so they may need separate linking.

## Work on a branch

Fork the repository if you do not have write access. Contributors with write
access follow the same PR workflow; do not push directly to `main`.

```sh
git fetch origin
git switch -c fix/short-description origin/main
```

Use names such as `fix/emoji-focus`, `feat/example`, or `docs/example`. Avoid
unrelated formatting, generated files, dependency upgrades, or release changes.
Never commit credentials or local application data. Commit the lockfile when a
dependency change requires it; do not hand-edit resolved dependency entries.

## Verify your change

For Rust code or build/dependency changes, run these checks before review:

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps
git diff --check
```

Documentation-only changes need link and path checks, commands compared with
their definitions, and `git diff --check`, not a Cargo build. Use the relevant
validators for workflows and packaging; Cargo alone does not cover them.

When a pull request touches `Cargo.toml`, `Cargo.lock` or `deny.toml`, the
Dependencies workflow runs
[cargo-deny](https://github.com/EmbarkStudios/cargo-deny) against the RustSec
database. It also runs when those files change on `main`, and weekly. To run it
locally:

```sh
cargo deny check --warn unmaintained advisories bans sources
```

Add a focused regression test for a bug fix and confirm it fails before the fix.
Do not skip tests, weaken lints, or suppress errors just to make checks pass.
Display-dependent tests are ignored by the default suite; run affected ones
explicitly on a graphical session, with in-memory GSettings when applicable:

```sh
GSETTINGS_BACKEND=memory cargo test --locked --lib TEST_NAME -- --ignored
```

For UI changes, check keyboard focus, narrow windows, light/dark appearance, and
accessibility where relevant. Report exactly what was tested and what was not.
Do not equate compilation with visual verification. Use synthetic screenshots.

## Commits and pull requests

- Use English Conventional Commit subjects, for example
  `fix(composer): restore focus after choosing an emoji`.
- Use imperative wording, keep the subject under 72 characters, and explain the
  problem and rationale in the body when the change is not self-evident.
- Keep commits atomic. Write comments for non-obvious constraints and decisions,
  not for statements the code already expresses. Document public interfaces.
- Use a Conventional Commit title for the PR as well. Open it against `main`,
  follow the template unless the maintainer requests another format, and link the issue. Use
  `Closes #NUMBER` only when merging the PR should resolve that issue.
- Include commands/results, limitations, and synthetic UI evidence when useful.
  Update the relevant user guide or translated strings when behaviour changes.
  The README is an entry point, not a record of each fix; edit it only when its
  overview, installation steps or essential warnings need to change.
- Respond to review and re-run affected checks after changes. Prefer a draft PR
  while work or validation is incomplete.

## Merge policy

`main` requires a PR, the GitHub Actions checks `fmt` and `check`, an up-to-date
branch, and resolved review conversations. Force-pushes and deletion of `main`
are blocked. There is no configured administrator bypass. Mandatory approvals
are zero to support a single maintainer; this is not a reason to skip review.
Changes are squash-merged, and merged topic branches are deleted automatically.

The versioned rule definition and administrator instructions are in
[.github/MAINTAINERS.md](.github/MAINTAINERS.md). A file alone does not protect a
branch: enforcement must also be enabled in GitHub's repository settings.

## Releases

Maintainers cut releases separately from ordinary fixes. Follow
[the release procedure](PACKAGING.md#releases); do not bump versions, tag or
publish packages in an unrelated PR.

## License

ZapTide is free software: you can redistribute it and/or modify it under the
terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version. It is distributed without any warranty. See [LICENSE](LICENSE) for
the full terms. Contributions use `GPL-3.0-or-later`; there is no CLA or DCO check.

The inherited ZapFast code retains its MIT notice and copyright in
[LICENSE-MIT](LICENSE-MIT). Earlier MIT releases remain MIT. Dependencies,
vendored code and assets retain their own licences, including the
[CC0 notification sound](assets/sounds/README.md). AppStream metadata remains
`CC0-1.0`.
