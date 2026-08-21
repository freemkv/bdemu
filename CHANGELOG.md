# Changelog

## [1.6.7] — 2026-08-21

### Changed

- Version aligned to 1.6.7 for the unified release. No functional changes to
  this crate; the release was driven by autorip (per-webhook event selection,
  a progress bar per moved artifact, and move-queue / webhook-error fixes —
  see the autorip 1.6.7 notes).

## [1.6.6] — 2026-08-20

### Changed

- Version aligned to 1.6.6 for the unified release. No functional changes
  to this crate; the release was driven by autorip (webhooks may now target
  private/LAN addresses — see the autorip 1.6.6 notes).

## [1.6.5] — 2026-08-20

### Security

- **A profile could read files outside its own directory.** The blob
  filenames in `drive.toml`'s `[files]`, `[features]` and `[read_buffer]`
  sections were joined onto the profile directory with no containment, so a
  shared profile with `inquiry = "../../../../etc/shadow"` made bdemu read
  that file and serve it back as the emulated INQUIRY / feature / READ
  BUFFER response. Round-1 hardening covered the disc *directory* name but
  left these blob *names* raw; any name that escapes the profile directory
  is now refused (logged and treated as absent), for `inquiry`,
  `rpc_state`, `mode_2a` and every feature and read-buffer value. Profiles
  are third-party artifacts shared through the GitHub-issue workflow, so
  their filenames are untrusted.

### Fixed

- **A corrupt captured sector map no longer serves the wrong sector's bytes
  as success.** A `sectors.bin` with overlapping BDSM ranges made the
  binary-search lookup return an arbitrary match, handing the host one
  sector's data in answer to a request for another — at GOOD status.
  Overlapping ranges are now rejected, and a read near the top of the
  address space (an LBA close to `u32::MAX`) no longer wraps around to
  serve low sectors as GOOD — addresses are computed in 64-bit and anything
  past `u32::MAX` is a miss.

- **A read the fixture cannot satisfy is no longer reported as success.**
  READ(10)/READ(12) returned GOOD status with a zero-filled buffer whenever the
  requested LBA was outside the captured sector map, past the end of a legacy
  flat dump, or backed by a `sectors.bin` that was missing, empty or unreadable —
  with a log line identical to a real hit. A caller could not tell emulated disc
  content from nothing at all. Uncaptured sectors now fail with CHECK CONDITION /
  MEDIUM ERROR / UNRECOVERED READ ERROR (0x03/0x11/0x00). Sectors that ARE inside
  a captured BDSM range are still served verbatim, including genuinely zero ones.
- **Profile blobs that fail to load are logged.** Every read error except an
  oversized file used to collapse to an empty blob with no diagnostic; only a
  genuinely absent (optional) file is silent now.
- **The control socket is no longer stolen.** A second `bdemu run` unlinked the
  first instance's socket and quietly took over every `load`/`eject`/`status`.
  bdemu now refuses to bind over a live socket (stale sockets are still
  reclaimed), and `BDEMU_INSTANCE=<id>` gives concurrent emulators their own
  sockets.
- **`bdemu validate` checks blob sizes, not mere existence.** A zero-byte
  `sectors.bin` from an interrupted capture used to validate clean and exit 0.
- **Terminal-escape injection from third-party profiles.** `bdemu validate` and
  `list-discs` printed profile-derived strings (INQUIRY vendor/product, serial,
  firmware date, disc directory names) unfiltered; control characters are now
  replaced before display.
- **READ BUFFER answers unimplemented modes honestly.** Mode 0 and other
  unimplemented modes returned an empty GOOD response; they now return ILLEGAL
  REQUEST / INVALID FIELD IN CDB. The header comment claiming mode 0 was
  implemented is corrected.
- **Docs:** the README said `capture-disc` ejects automatically in three places;
  eject has been opt-in behind `--eject` (now documented) since the flag was
  added.

## [1.6.4] — 2026-08-15

### Changed

- **No functional change.** This crate ships alongside the rest of freemkv at a
  matching version; its behaviour is untouched.

## [1.6.3] — 2026-08-10

### Changed

- **No functional change.** This crate ships alongside the rest of freemkv at a
  matching version. Its build and release checks were updated; drive emulation
  is untouched.

## [1.6.2] — 2026-08-08

Version sync with the workspace. No functional change in this crate.

## [1.6.1] — 2026-08-07

Version sync with the workspace. No functional change in this crate.

## [1.6.0] — 2026-08-03

Version sync with the workspace. No functional change.

## [1.5.2] — 2026-07-22

Version sync with the workspace. No functional change.

## [1.4.5] — 2026-07-18

Version sync with the workspace; inherits libfreemkv 1.4.5.

## [1.4.4] — 2026-07-17

Version sync with the workspace; inherits libfreemkv 1.4.4.

## [1.4.3] — 2026-07-17

Version sync with the workspace; inherits libfreemkv 1.4.3.

## [1.4.2] — 2026-07-15

Version sync with the workspace; inherits libfreemkv 1.4.2.

## [1.4.1] — 2026-07-14

Version sync with the workspace; inherits libfreemkv 1.4.1.

## [1.4.0] — 2026-07-13

Version sync with the workspace; inherits libfreemkv 1.4.0.

## [1.3.2] — 2026-07-10

Version sync with the workspace; inherits libfreemkv 1.3.2.

## [1.3.1] — 2026-07-10

### Licensing

- **Relicensed to the MIT License, from 1.3.1 onwards** (releases up to and
  including 1.3.0 remain under AGPL-3.0).

Version sync with the workspace; inherits libfreemkv 1.3.1.

## [1.3.0] — 2026-07-08

Version sync with the rest of the workspace; inherits libfreemkv 1.3.0.

### Changed

- Wires the drive's `product_id` into the emulated `DriveId` (new field on
  `freemkv_unlock::DriveId`).

## [1.2.0] — 2026-06-29

Version sync with the rest of the workspace. No functional changes to the
emulator; inherits libfreemkv 1.2.0.

## [1.0.0-rc.2]

Version sync with the rest of the workspace. No functional changes to the
emulator; inherits libfreemkv 1.0.0-rc.2 as a dependency.

## [1.0.0-rc.1]

Version sync with the rest of the workspace. No functional changes to the
emulator; inherits libfreemkv 1.0.0-rc.1 as a dependency.

## 0.31.0 (2026-06-08)

Hardening release for the control channel, capture path, and SCSI handling.

### Fixed

- Control socket: a per-command client read timeout so loading a multi-GB disc
  image no longer trips a false timeout, while quick commands keep a short
  timeout; the listener now serves connections concurrently and caps the
  request line length.
- Capture: signal-killed children report `128 + signum`; a reused read buffer
  is re-zeroed on a short transfer; an oversized `sectors.bin` is rejected
  before a single huge allocation.
- Argument parsing rejects a missing or flag-shaped value for `--profile` /
  `--disc`.

### Changed

- Release profile now builds with thin LTO and a single codegen unit.

## Pre-1.0 development

Versions 0.x were the development series leading up to 1.0. bdemu is the
Blu-ray disc emulator used for testing; most 0.x releases were version-sync
bumps tracking libfreemkv. The functional highlights, condensed:

- **Disc emulation via LD_PRELOAD.** A SCSI interceptor presents a captured
  disc image to the rip path, with directory-based drive profiles and runtime
  disc swapping over a Unix control socket (load, eject, list-discs, status),
  including proper media-changed (UNIT_ATTENTION) signaling.
- **Capture from real hardware.** Smart capture using libfreemkv's UDF range
  discovery, a sparse sector format, auto-eject and auto-rename of the output to
  the disc's volume ID, and collision handling.
- **Platform.** Rust 2024 edition, Rust 1.86 MSRV, thread-safe globals.
  Tracks libfreemkv across the workspace's unified versioning.
