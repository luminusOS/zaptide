# ZapTide agent guide

## Scope

ZapTide is a Linux-only WhatsApp client forked from ZapFast, built with Rust,
GTK4, libadwaita, Relm4 and whatsapp-rust. Keep it a small native client: no
browser engine, telemetry, hosted backend or second account system. Do not
reimplement protocol features or claim support from a protobuf field alone.
Keep changes within the requested task and preserve unrelated behaviour.

## Git policy

Create commits, amend them, push, open PRs, tag or publish releases only when
the user explicitly requests the corresponding operation. Otherwise leave
changes local. Never discard someone else's work.

When requested, use English Conventional Commits with an imperative subject
under 72 characters. Explain the problem and rationale in the body, wrapped
at about 72 characters. Preserve the configured author; add no AI attribution.
PR titles use the same convention. Descriptions use concise English prose
without hard-wrapped paragraphs, decorative headings or checklists unless
the user or a mandatory repository template requires them.

## Privacy

- Never read chat rows, messages, contacts or other personal content from
  `archive.db` or exported logs, even read-only. Schema and row counts are fine.
  Give the user a query to run when a diagnosis needs their data.
- Use synthetic fixtures and mock credentials. Never capture real chats or
  log message contents, phone numbers, keys or QR payloads at a shipping level.
  Never log `CallError`, which contains relay addresses and JIDs.
- Keep ZapTide's XDG paths and keyring service separate from ZapFast, FastsApp
  and FastWhatsApp. Importing needs explicit consent and a separately reviewed
  migration plan; never adopt their data automatically.
- A missing or locked encryption key stops linking. Never create a disposable
  archive or replace a key to hide the failure.

## Commands and validation

Use the pinned toolchain in `rust-toolchain.toml`. Native dependencies and
container requirements are in [PACKAGING.md](PACKAGING.md). Run commands in a
Toolbox with those dependencies when the host lacks them.

```sh
cargo build --locked
just run
just pot
```

`just run` compiles translations before launching. `just pot` refreshes and
merges translation strings; use it when translatable strings change.

For Rust code or build/dependency changes, run all checks before finishing:

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps
git diff --check
```

Add focused tests for changed behaviour. Run affected display-dependent tests
explicitly with synthetic data, using Xvfb or a graphical session. Check
keyboard interaction, layout and accessibility where relevant. Report what
ran, what only compiled and what remains untested. Never weaken lints,
suppress errors, skip or delete tests merely to get green checks.

For documentation-only changes, check links, paths, commands against their
definitions and `git diff --check`; do not run Cargo builds or tests unless
the documentation change needs them. Workflow and packaging changes need
their own relevant checks, not an automatic claim that Cargo covers them.

## Architecture

Read [ARCHITECTURE.md](ARCHITECTURE.md) for layout and thread ownership. Before
changing a subsystem, read its section in
[architecture contracts](docs/architecture-contracts.md); those constraints
remain mandatory even though they are outside this file.

- Widgets send `Input`; Relm4 `update` owns application state. Signal handlers
  may change only their widget's value, not application state.
- UI and worker exchange only `Command` and `Event`; each event wakes the UI
  through `Waker`. Views use model types, never protobufs. Use
  `Worker::canonical` for incoming JIDs.
- Preserve archive transactions, generation guards, callback ownership and
  cleanup ordering. Shared helpers must not merge separate workload limits.
- The window owns presence. Keep `PresencePolicy::Manual`; Automatic breaks
  phone notifications by announcing availability on reconnect.

## Coding and review

Follow surrounding style and naming. Prefer explicit ownership and small
domain-specific functions over speculative abstractions or forwarding
wrappers. Do not compress control flow to shorten a file. Comments explain
non-obvious constraints or decisions, not visible operations; document public
interfaces and remove obsolete comments.

Use libadwaita classes and symbolic icons before custom CSS. Structural CSS
belongs in `data/style.css`, not generated Rust strings. Use gettext for UI
strings. Do not add compatibility paths for unsupported platforms or fake
behaviour behind a label. Review the diff for correctness, privacy, resource
lifetime and unnecessary complexity.

## Documentation policy

The README is a short entry point: purpose, representative features,
installation, essential warnings and links. It is not a changelog, test log,
architecture notebook or catalogue of every fix.

Update it only when that entry-point information becomes wrong or incomplete.
A code change or commit does not itself require a README change. Edit an
existing statement instead of appending another bullet for the same feature.
Put user instructions in `docs/user-guide.md`, architecture and invariants in
`ARCHITECTURE.md` and `docs/architecture-contracts.md`, development guidance in
`CONTRIBUTING.md`, and packaging/release procedures in `PACKAGING.md`.

Write direct, factual prose. Remove generated filler, repeated explanations,
decorative formatting and unsupported claims; use the humanizer workflow when
available. Do not use em dashes in user-facing writing or agent responses.
Preserve security warnings, licence notices, commands and known limitations
when shortening documentation. Do not create a new document for each fix.

## Releases

Releases require an explicit request and follow
[the release procedure](PACKAGING.md#releases). Ordinary fixes accumulate on
`main`; a regression in a just-released version is the exception. Never leave
generated release notes in place or treat a pushed tag as a finished release.
