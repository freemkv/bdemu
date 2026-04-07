[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue)](LICENSE)
[![Latest Release](https://img.shields.io/github/v/release/freemkv/bdemu?label=latest&color=brightgreen)](https://github.com/freemkv/bdemu/releases/latest)

# bdemu

Blu-ray drive emulator for development and testing. Intercepts Linux SG_IO ioctls via `LD_PRELOAD` to emulate a complete optical drive from captured SCSI response data. No real drive needed.

Part of the [freemkv](https://github.com/freemkv) project. **Linux only.**

## Download

**[Download latest release](https://github.com/freemkv/bdemu/releases/latest)**

Or build from source: `cargo build --release`

## Quick Start

```bash
# Emulate a drive and run freemkv against it
./bdemu run --profile profiles/hl-dt-st-bd-re-bu40n-1.03-nm00000 -- ./freemkv drive-info
```

## Commands

```
bdemu <command>

Emulation:
  run --profile <dir> [--disc <name>] -- <cmd>   Emulate a drive and run a command
  capture-disc <device> <dir> [--sectors N]      Capture disc from real hardware
  validate <profile_dir>                         Check profile completeness

Control (while emulator is running):
  status                                         Show emulator state
  eject                                          Eject the disc
  load <disc_name>                               Load a disc
  list-discs                                     List available discs
```

## Creating Profiles

### From real hardware

```bash
# Capture drive profile
freemkv drive-info --share profiles/my-drive/

# Capture a disc
bdemu capture-disc /dev/sg4 profiles/my-drive/discs/my-disc/ --sectors 50000
```

## Profile Structure

```
profiles/my-drive/
├── drive.toml           # Drive metadata
├── inquiry.bin          # INQUIRY response (96 bytes)
├── gc_*.bin             # GET_CONFIG features
├── rpc_state.bin        # REPORT KEY RPC state
├── mode_2a.bin          # MODE SENSE page 2A
└── discs/
    └── my-disc/
        ├── toc.bin      # READ TOC response
        ├── capacity.bin # READ CAPACITY response
        ├── disc_info.bin
        ├── ds_00.bin    # READ DISC STRUCTURE
        └── sectors.bin  # Sector dump (2048 bytes/sector)
```

## Environment Variables

| Variable | Description |
|----------|-------------|
| `BDEMU_PROFILE` | Path to drive profile directory |
| `BDEMU_DISC` | Disc subdirectory name |
| `BDEMU_QUIET` | Suppress SCSI command logging |

## License

AGPL-3.0-only
