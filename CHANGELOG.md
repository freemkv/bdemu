# Changelog

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
