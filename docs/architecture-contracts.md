# Architecture contracts

Read the relevant section before changing a subsystem. These are required
constraints, not a history of fixes. See [ARCHITECTURE.md](../ARCHITECTURE.md)
for layout and [AGENTS.md](../AGENTS.md) for privacy and workflow rules.

## Worker and lifecycle

Operation implementations belong in `src/backend/worker/`; shared state,
batch invariants and orchestration stay in the parent. Startup connects before
staging cleanup or archive re-derivation. Maintenance yields between 256-row
steps, interleaving incoming events. Archive batches emit only after commit
and restore protected in-memory state on rollback.

Account-bound results use `Command::Completed(Completion { generation, done })`.
Keep the central session guard separate from avatar, request, attempt, call
and privacy generations. A completed media sweep releases its running slot
before stale-result rejection, including after an account change.

Stale results and failed enqueue discard only owned staging paths, never
destination files, and notify waiting outbound-batch and group-recipient
callbacks once with `false`. Owned deferred-download drop uses the same path.
Successful enqueue followed by receiver shutdown is not covered by that
cleanup; never add cleanup `Drop` to cloneable completion payloads.

`application::OpenChatState` centralizes reset state, but callers retain
cleanup order. Leaving an account preserves starred messages; complete chat
reset clears them. Invalidate request tokens and preserve callback ownership.
Resource permits in `src/shared_resource.rs` remain separate by workload.

The window owns presence through `Command::SetOnline` and
`application/presence.rs`; the worker announces it in `worker/presence.rs`.
Keep `PresencePolicy::Manual`. Automatic announces availability on reconnect
and prevents the phone from notifying while the linked device is online.

Quitting shuts the backend down on a thread before calling
`relm4::main_application().quit()`. GApplication provides single-instance
activation. Notification presentation belongs in `src/native_notifications.rs`.
Updates belong to the package manager or Flatpak remote, never an app updater.

## Archive and credentials

The archive stores messages, chats, contacts and privacy-ID mappings. History
replays at linking, so the local archive is the only retained copy. Preserve
raw protobufs: they carry attachment-fetch keys.

`archive/encryption.rs` opens SQLCipher with a random key managed through oo7
in `archive/secrets.rs`. Outside Flatpak, find credentials in any Secret
Service collection and create no duplicate. Inside Flatpak, use the Secret
portal's file keyring. Copy a missing key once from the legacy Secret Service
item, then read it back from disk before use. Never write or delete that
legacy item. Packaging documents the no-portal fallback and migration gate.

Keyring work runs in order on one thread. GTK waits only for the one-time key
copy during legacy-layout migration, for at most two minutes. Plaintext
migration checkpoints the old WAL and verifies encrypted staging before
atomic replacement. Missing or locked keys stop linking; no disposable archive.
Tests use fixtures and mock credentials only.

`backend/worker/device_store.rs` encrypts `session.db` with an HKDF key derived
from the folder's archive key, not a second keyring item. Apply the key through
a connection hook to every pooled connection. Create a new key only when
neither database in the folder is encrypted.

## History, identity and receipts

Canonicalize incoming JIDs with `Worker::canonical`. File privacy-ID chats
under their phone-number ID once the mapping is known. Page the archive first;
only request phone history after it is exhausted. `Command::FetchOlder` uses
`Client::fetch_message_history`, answered by `HistorySync` with
`sync_type == ON_DEMAND`. A named chat with no messages requests history on
load or open, anchored at the present with an empty message ID.

Group names and members use `groups().get_metadata` through `pump_group_info`,
one request at a time, two per five-second tick. Retry failures from 30 seconds
with doubling backoff, at most seven tries. Item-not-found, forbidden and
not-authorized are final. Do not turn history sync into a metadata-query burst.

Group delivery saves recipients when filing each outgoing message, records
individual receipts and takes the least advanced recipient. Never promote a
group from one reader, apply a receipt to earlier messages or infer historical
recipients from current members. History trusts the phone's aggregate status,
not a partial `user_receipt` list.

Private read-state writes use `regular_low`. `backend::read_sync` serializes
them and backs off the whole queue on failure; do not add per-chat retry
queues. Pending positions remain archived until acknowledged. Snapshot
recovery and no-progress conflict detection belong to whatsapp-rust.

The linked-device name and icon come from `DevicePropsOverride` in `start_bot`:
`os` names the device and platform type picks its icon. Changes take effect
only after unlinking and pairing again.

## Sidebar scrolling

The scrolled window owns the proxy adjustment; the list owns the real
adjustment. Outside a pending restoration, keep `real = proxy + origin` and
translate adjustment limits by the same origin. Ordinary layout must not
change the origin or replay input.

Projection updates commit the model immediately and preserve a surviving
visible chat by identity and offset before its first snapshot. Pending input
must be applied once, including input received before the model update.
Exhausting materialization attempts is cancellation, not a successful restore.

Cancel pending restoration coherently on navigation, unmap or adjustment
replacement. Reentrant signals and old callbacks must not modify a newer
transaction or publish to a replaced adjustment. Release Rust borrows before
calling GTK setters that can emit signals. Selection and application state
remain owned by Relm4; the bridge owns only widget coordinates and geometry.

## Polls

Use whatsapp-rust's `Client::polls()` for creation, voting and decryption.
`backend/worker/polls.rs` retains the creator identity and key in the encrypted
archive. `archive/polls.rs` keeps each voter's latest timestamp and message ID,
including encrypted updates received before their parent. History must not
undo a newer vote or withdrawal.

Decrypt in batches of eight and retry failures after reconnecting. The UI
receives option counts and its own selection, never keys or protobufs.
Visible polls request phone history after the creation message to include
the vote snapshot. `poll_history.rs` serializes requests, retrying from 30
seconds to 15 minutes without a UI timer. Request timestamps are Unix seconds
despite the library and wire field names ending in `Ms`; never multiply by
1,000. A repeated question without a usable vote snapshot cannot end recovery.

## Downloads and message presentation

Downloads stream to same-directory staging files without clobbering existing
files. Use fallible OS entropy for staging names; failure is a local download
error, not a reason to invent a fallback name. HTTP 403/404/410 uses
`client.media_reupload().request(..)` for a fresh `direct_path`, then one fetch
retry before reporting that WhatsApp no longer has the file. Do not retry
local staging failures or add download caps through this helper. Failures
stay in the bubble with click-to-retry, never a toast.

With retention enabled, `backend/worker/retention.rs` schedules
`media_retention::sweep` on a blocking thread about 30 seconds after receiving
the setting and every six hours. Nothing is scheduled while off. Sweep only
archived paths directly in the account media cache, older than the retention
period by modification time and not held by a viewer or playing clip.
Exclude stickers, messages missing re-fetch keys, and pending or failed sends.
Clear the archived `media.path` after removal so the UI offers downloading again.

`transcript::refine` maps emoji placeholders back through row `placements`.
Keep copying consistent with displayed text. A bubble reads right-clicks over
its own rectangle and opens `Popup::menu`; `Popup::context_menu` on an inner
response misses clicks consumed by its child widgets.

## Voice notes

`src/voice.rs` handles OGG/Opus, the 64-bar waveform and mono/48 kHz resampling.
The `ogg` crate supplies the container; `opus` bundles libopus through cmake.
`src/services/media.rs` plays one clip at a time with GStreamer's `playbin`.
`scaletempo` preserves pitch at 1.5x and 2x; without it only 1x works and other
speeds report an error in the bubble.

Bounded waveform decoding runs on a worker thread in `src/audio.rs`, using
`opus` for Ogg Opus and `decodebin` into an appsink for other audio. Because
decoding starts without a click, reject adaptive, source, video, image and
subtitle elements: received media must never open another URL or file.

`Recorder` reads `autoaudiosrc` as mono 48 kHz through a threaded appsink,
rejecting silent fallback when no sound server or microphone exists. It keeps
loudness per 50 ms for live bars. `StartRecording`, `CancelRecording` and
`SendRecording` drive it from the composer. `Command::SendVoice` normalizes
quiet input with capped gain, encodes it and sends push-to-talk with waveform
and reply quote. `Command::MarkPlayed` sends one played receipt per incoming note.

## Calls

`backend/worker/calls.rs` owns one call and drives whatsapp-rust's `voip()`;
`model/call.rs` owns its state machine. File calls as `Content::Call` messages;
`archive/calls.rs` queries them for the Calls tab. Devices and H.264 pipelines
live in `src/calls/`.

Reject an unanswered offer without a handle through `voip().reject` only
when we decide to end it, never when the caller or another device ends it.
Close and hang up use one command because only the worker knows the phase.
Release media before waiting on network operations.

One `Ringer` in `calls/ringtone.rs` is driven only by `sync_ring`. Every end or
close path must pass through it. Call notification tokens are keyed by account
and call ID, separate from chat tokens. A second incoming call becomes
`waiting` and rings; there is no hold or busy reply in whatsapp-rust.
Never log `CallError`: it contains relay addresses and JIDs.
