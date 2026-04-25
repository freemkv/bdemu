# Changelog

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
