# Changelog

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
