[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Latest Release](https://img.shields.io/github/v/release/freemkv/bdemu?label=latest&color=brightgreen)](https://github.com/freemkv/bdemu/releases/latest)

# bdemu

Blu-ray drive emulator for development and testing. Intercepts Linux SG_IO ioctls via `LD_PRELOAD` to emulate a complete optical drive from captured SCSI response data. No real drive needed.

Part of the [freemkv](https://github.com/freemkv) project. **Linux only.**

## Download

**[Download latest release](https://github.com/freemkv/bdemu/releases/latest)**

Or build from source: `cargo build --release`

## Quick Start

```bash
# Capture a disc (auto-names, auto-ejects)
bdemu capture-disc /dev/sr0 ./testbed/disc

# Emulate a drive and scan the captured disc
bdemu run --profile profiles/bu40n --disc my_movie -- freemkv info disc://
```

## Commands

```
bdemu 1.3.0-rc.5.3-rc.5.2-rc.5.1-rc.5-rc.4.3-rc.4.2-rc.4.1-rc.4-rc.3-rc.2-rc.1-rc.1

Commands:
  run --profile <dir> [--disc <name>] -- <cmd>   Emulate drive, run command
  capture-disc <device> <output_dir>             Smart capture from hardware
  validate <profile_dir>                         Check profile completeness

Control (while emulator is running):
  status                                         Show emulator state
  eject                                          Eject the disc
  load <disc_name>                               Load a disc
  list-discs                                     List available discs

Examples:
  bdemu capture-disc /dev/sr0 ./testbed/disc     Capture, auto-names, ejects
  bdemu run -p profiles/bu40n -d sample -- freemkv info disc://
  bdemu validate profiles/bu40n/
```

## Smart Capture

`capture-disc` uses libfreemkv to parse the disc's UDF filesystem and capture only the sectors needed for emulation. A typical capture is 15-80 MB instead of 25-90 GB.

After capture, the output directory is automatically renamed to the disc's volume ID (e.g. `disc` becomes `sample_film`). If the name already exists, a number is appended (`sample_film_2`).

The disc tray ejects automatically when capture completes.

## Creating Profiles

### From real hardware

```bash
# Capture drive identity (one-time per drive)
freemkv info disc:// --share

# Capture discs (repeat for each disc)
bdemu capture-disc /dev/sr0 profiles/my-drive/discs/disc
```

## Profile Structure

```
profiles/my-drive/
+-- drive.toml           # Drive metadata
+-- inquiry.bin          # INQUIRY response (96 bytes)
+-- gc_*.bin             # GET_CONFIG features
+-- rpc_state.bin        # REPORT KEY RPC state
+-- mode_2a.bin          # MODE SENSE page 2A
+-- discs/
    +-- my-disc/
        +-- toc.bin      # READ TOC response
        +-- capacity.bin # READ CAPACITY response
        +-- disc_info.bin
        +-- ds_00.bin    # READ DISC STRUCTURE
        +-- sectors.bin  # BDSM sparse sector map
```

## Environment Variables

| Variable | Description |
|----------|-------------|
| `BDEMU_PROFILE` | Path to drive profile directory |
| `BDEMU_DISC` | Disc subdirectory name |
| `BDEMU_QUIET` | Suppress SCSI command logging |

## License

MIT
