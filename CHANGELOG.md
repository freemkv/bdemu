# Changelog

## 0.3.0 (2026-04-07)

### Capture

- **Auto-eject**: disc tray opens automatically after capture completes
- **Auto-rename**: output directory renamed to slugified UDF volume ID (e.g. `disc` → `dune__part_two`)
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
