# Changelog

## 0.13.40 (2026-04-28)

### Sync release — picks up libfreemkv 0.13.39

- Picks up ECC-block sweep, mapfile-based recovery, transport-failure abort.
- No bdemu functional changes.

## 0.13.26 (2026-04-28)

### Sync release — picks up libfreemkv 0.13.32 (hysteresis fix, SgIoTransport recovery)

No bdemu functional changes.

## 0.13.24 (2026-04-27)

### Sync release — picks up libfreemkv 0.13.24 MapStats split + fmt cleanup

No bdemu functional changes; consumes libfreemkv 0.13.24 for ecosystem
version sync.

## 0.13.23 (2026-04-27)

### Sync release — picks up libfreemkv 0.13.23 SCSI sense plumbing

No bdemu functional changes; consumes libfreemkv 0.13.23 for ecosystem
version sync.

## 0.13.22 (2026-04-26)

### Sync release — picks up libfreemkv 0.13.22 hysteresis recovery

No bdemu functional changes; consumes libfreemkv 0.13.22 for ecosystem
version sync.

## 0.13.21 (2026-04-26)

### Sync release — picks up libfreemkv 0.13.21 bisect-on-fail

bdemu surface unchanged. The libfreemkv bump fixes the BU40N
multi-sector read failure that was poisoning `capture-disc` on
damaged discs.

## 0.13.20 (2026-04-26)

### Sync release — no functional changes

Bumped to satisfy the unified-versioning rule. Actual changes are in
libfreemkv (SCSI sync rewrite + API cleanup) and autorip (UI / ETA
fixes). bdemu doesn't touch the SCSI transport directly, so this is a
transparent dep bump. The held 0.13.19 dev bundle (never released)
is folded into this release.

## 0.13.19 (2026-04-26 — held, never released)

Held in development; folded into 0.13.20.

## 0.13.18 (2026-04-26)

### Sync release — no functional changes

Bumped to satisfy the unified-versioning rule. Actual fix is in autorip
(`web.rs` two-bar UI). bdemu doesn't render any progress UI, so this
is a transparent dep bump.

## 0.13.17 (2026-04-26)

### Sync release — no functional changes

Bumped to satisfy the unified-versioning rule. Actual fix is in autorip
(hot-plug rescan).

## 0.13.16 (2026-04-26)

### Sync release — consume libfreemkv 0.13.16

No functional changes. Picks up the new `Progress` trait + `PassProgress`
struct architecture (single progress signal type replacing the leaky
positional callbacks).

## 0.13.15 (2026-04-26)

### Sync release — consume libfreemkv 0.13.15

bdemu doesn't use Disc::copy / Disc::patch directly, so the
`on_progress` signature change and the new `PatchOptions::reverse` /
`wedged_threshold` fields are transparent. No functional changes.

## 0.13.14 (2026-04-25)

### Sync release — no functional changes

Bumped to satisfy the unified-versioning rule. Actual fix is in autorip
(tracing-subscriber filter for the new `freemkv::scsi`/`freemkv::disc`
targets).

## 0.13.13 (2026-04-25)

### Version sync — consume libfreemkv 0.13.13

No functional changes. Picks up the new `tracing` instrumentation in
`SgIoTransport::execute` (Linux) + `Disc::copy` for in-flight rip-pipeline
diagnosis.

## 0.13.12 (2026-04-25)

### Version sync — consume libfreemkv 0.13.12

No functional changes. Picks up Fix 1 (stall-guard deletion), Fix 2
(async SCSI recovery on Linux + cross-platform try_recover on Windows +
macOS), Fix 4 (`PatchResult` instrumentation), and the
`PatchOptions::full_recovery` honor.

## 0.13.11 (2026-04-25)

### Version sync — consume libfreemkv 0.13.11

No functional changes.

## 0.13.10 (2026-04-25)

### Version sync — consume libfreemkv 0.13.10

No functional changes.

## 0.13.9 (2026-04-25)

### Version sync — consume libfreemkv 0.13.9

No functional changes.

## 0.13.8 (2026-04-25)

### Version sync — consume libfreemkv 0.13.8

Version sync only — no functional changes.

## 0.13.7 (2026-04-25)

### Version sync — consume libfreemkv 0.13.7

Version sync only — no functional changes. Pulls in libfreemkv 0.13.7
(no API change vs 0.13.6); the actual functional fix in this release
is autorip-side.

## 0.13.6 (2026-04-25)

### Version sync — consume libfreemkv 0.13.6
No functional changes. libfreemkv 0.13.6 strips the inline
retry/reset loop from `Drive::read` and emits `EventKind::BytesRead`
from `DiscStream`; neither surface is used by bdemu. Cargo.toml dep
pin `0.13.5` → `0.13.6`.

## 0.13.5 (2026-04-25)

### Version sync — consume libfreemkv 0.13.5
No functional changes. Ecosystem sync. Cargo.toml dep pin
`0.13.4` → `0.13.5`.

## 0.13.4 (2026-04-25)

### Version sync — consume libfreemkv 0.13.4
No functional changes. libfreemkv 0.13.4's scope (drive_has_disc
recovery rollback + sysfs identity fallback) doesn't touch any
surface bdemu uses. Cargo.toml dep pin `0.13.3` → `0.13.4`.

## 0.13.3 (2026-04-24)

### Version sync — consume libfreemkv 0.13.3
No functional changes. libfreemkv 0.13.3 fixes the wedge-signature
predicate in `drive_has_disc` recovery; bdemu doesn't call that path.
Cargo.toml dep pin `0.13.2` → `0.13.3`.

## 0.13.2 (2026-04-24)

### Version sync — consume libfreemkv 0.13.2
No functional changes. libfreemkv 0.13.2's discovery API
(`list_drives` / `drive_has_disc`) and visibility tightening don't
touch any surface bdemu uses. Cargo.toml dep pin `0.13` → `0.13.2`.

## 0.13.0 (2026-04-24)

### Version sync — consume libfreemkv 0.13.0
No functional changes. libfreemkv 0.13.0's API hygiene pass (English
elimination, label-purpose enum, dead-code sweep) doesn't touch any
surface bdemu uses. `Cargo.toml` dep pin `0.12` → `0.13`.

## 0.12.0 (2026-04-24)

### Rust 2024 edition migration
- Bumped `edition = "2024"`.
- `#[no_mangle]` → `#[unsafe(no_mangle)]` per 2024 unsafe-attribute rules.
- `unsafe_op_in_unsafe_fn`: explicit `unsafe { … }` blocks inside the `pub unsafe extern "C" fn ioctl` body.
- Consumes libfreemkv 0.12.0. No behavior change.

## 0.11.22 (2026-04-24)

### Version sync — no functional changes
Part of the 0.11.22 ecosystem release. Consumes libfreemkv 0.11.22.

## 0.11.21 (2026-04-24)

### Version sync
- No functional changes. Part of the 0.11.21 ecosystem sync.
- Consumes libfreemkv 0.11.21.

### License SPDX normalization
- `Cargo.toml` license field: `AGPL-3.0` → `AGPL-3.0-only`.

## 0.11.16 (2026-04-21)

### Version sync
- Unified version with libfreemkv 0.11.16.

## 0.11.15 (2026-04-21)

### Version sync + fix
- Unified version with libfreemkv 0.11.15. Fix read() 4th arg.

## 0.11.13 (2026-04-21)

### Version sync
- Unified version with libfreemkv 0.11.13.

## 0.11.12 (2026-04-21)

### Version sync
- Unified version with libfreemkv 0.11.12.

## 0.11.11 (2026-04-20)

### Version sync
- Unified version with libfreemkv 0.11.11.

## 0.11.10 (2026-04-20)

### Version sync
- Unified version with libfreemkv 0.11.10.

## 0.11.9 (2026-04-20)

### Version sync
- Unified version with libfreemkv 0.11.9.

## 0.11.8 (2026-04-20)

### Version sync
- Unified version with libfreemkv 0.11.8.

## 0.11.7 (2026-04-19)

### Version sync
- Unified version with libfreemkv 0.11.7.

## 0.11.6 (2026-04-18)

### Version sync
- Unified version with libfreemkv 0.11.6.

## 0.11.5 (2026-04-18)

### Version sync
- Unified version with libfreemkv 0.11.5.

## 0.11.3 (2026-04-18)

### Unified versioning
- All freemkv repos now share the same version number.
- Updated libfreemkv dependency to 0.11.

## 0.9.0 (2026-04-15)

### API update
- **libfreemkv 0.9** — updated for Drive object API, `drive.read()` single method
- **Rust 1.86 MSRV** pinned

## 0.8.0 (2026-04-11)

### libfreemkv API migration
- Updated for libfreemkv Drive object, typed StreamUrl, public re-exports

## 0.5.0 (2026-04-09)

### Emulator

- **libfreemkv unlock**: Drive signature lookup via bundled profile database (replaces hardcoded "MMkv")
- **Control socket**: Runtime disc swapping via `/tmp/bdemu.sock` (load, eject, list-discs, status)
- **UNIT_ATTENTION**: Proper media-changed signaling on disc swap
- **Thread safety**: Replaced `static mut` globals with atomics and mutexes

### CLI

- Added `run` subcommand for LD_PRELOAD launching
- Added `status`, `eject`, `load`, `list-discs` control commands

## 0.3.0 (2026-04-07)

### Capture

- **Auto-eject**: disc tray opens automatically after capture completes
- **Auto-rename**: output directory renamed to slugified UDF volume ID (e.g. `disc` -> `dune__part_two`)
- **Collision handling**: appends `_2`, `_3` etc. if name already exists
- **Fixed sector ranges**: uses updated libfreemkv that captures all JAR/metadata files

### CLI

- Updated help text and examples
- Removed `--sectors N` flag (was never implemented)

## 0.2.4

- BDSM sparse sector format
- Smart capture via libfreemkv UDF range discovery

## 0.2.0

- Initial public release
- LD_PRELOAD SCSI interceptor
- Directory-based drive profiles
- Disc capture from real hardware
- Runtime disc swapping via Unix socket
