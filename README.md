[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue)](LICENSE)
[![Latest Release](https://img.shields.io/github/v/release/freemkv/bdemu?label=latest&color=brightgreen)](https://github.com/freemkv/bdemu/releases/latest)

# bdemu

Blu-ray drive emulator for development and testing. Intercepts Linux `SG_IO` ioctls via `LD_PRELOAD` to emulate a complete optical drive from captured SCSI response data. No real drive needed.

Uses [libfreemkv](https://github.com/freemkv/libfreemkv) for unlock signature lookup. Part of the [freemkv](https://github.com/freemkv) project. **Linux only.**

## Download

**Latest: v0.2.4 (2026-04-06)**

| Platform | | |
|----------|-|---|
| Linux (Intel/AMD) | [**Download**](https://github.com/freemkv/bdemu/releases/download/v0.2.4/bdemu-v0.2.4-x86_64-linux.tar.gz) | Includes bdemu, libbdemu.so, profiles |

[Older versions](https://github.com/freemkv/bdemu/releases) · Build from source: `cargo build --release`

## Quick Start

```bash
wget -qO- https://github.com/freemkv/bdemu/releases/download/v0.2.4/bdemu-v0.2.4-x86_64-linux.tar.gz | tar xz
cd bdemu-v0.2.4

# Emulate a BU40N drive and run freemkv against it
./bdemu run --profile profiles/hl-dt-st-bd-re-bu40n-1.03-nm00000 -- ./freemkv info
```

```
bdemu: loaded 'BD-RE BU40N' (14 features, 1 read_bufs, disc=no)
bdemu: control socket at /tmp/bdemu.sock

freemkv 0.1.5

Drive Information
  Device:              /dev/sg4
  Manufacturer:        HL-DT-ST
  Product:             BD-RE BU40N
  ...
```

## Control (while running)

In another terminal, interact with the running emulator:

```bash
./bdemu status             # show emulator state
./bdemu eject              # eject the disc
./bdemu load sample        # load a disc profile
./bdemu list-discs         # list available discs
```

## All Options

```
bdemu 0.2.4

Commands:
  run --profile <dir> [--disc <name>] -- <cmd>   Emulate a drive and run a command
  capture-disc <device> <dir> [--sectors N]      Capture disc from real hardware
  validate <profile_dir>                         Check profile completeness

Control (while emulator is running):
  status                                         Show emulator state
  eject                                          Eject the disc
  load <disc_name>                               Load a disc
  list-discs                                     List available discs

Examples:
  bdemu run --profile profiles/bu40n -- ./freemkv info
  bdemu run --profile profiles/bu40n --disc sample -- ./freemkv rip
  bdemu eject                                    # while running
  bdemu load sample2                             # swap disc
  bdemu capture-disc /dev/sg4 profiles/my-drive/discs/my-disc/
```

## Creating Profiles

### Drive profile (from real hardware)

```bash
freemkv info --share profiles/my-drive/
```

### Disc profile (from real hardware)

```bash
bdemu capture-disc /dev/sg4 profiles/my-drive/discs/my-disc/
```

### Minimal test disc

```bash
mkdir -p profiles/my-drive/discs/test/
dd if=/dev/zero of=profiles/my-drive/discs/test/sectors.bin bs=2048 count=100
```

## Profile Structure

```
profiles/bu40n/
├── drive.toml           # Drive metadata
├── inquiry.bin          # INQUIRY response (96 bytes)
├── gc_*.bin             # GET_CONFIG features
├── rpc_state.bin        # REPORT KEY RPC state
├── mode_2a.bin          # MODE SENSE page 2A
└── discs/
    └── sample/
        ├── toc.bin      # READ TOC response
        ├── capacity.bin # READ CAPACITY response
        ├── disc_info.bin
        ├── ds_00.bin    # READ DISC STRUCTURE
        └── sectors.bin  # Sector dump (2048 bytes/sector)
```

## SCSI Commands

18 MMC-6 commands emulated:

| Opcode | Command | Source |
|--------|---------|--------|
| 0x00 | TEST UNIT READY | Disc presence |
| 0x03 | REQUEST SENSE | Sense history |
| 0x12 | INQUIRY | inquiry.bin |
| 0x25 | READ CAPACITY | capacity.bin |
| 0x28 | READ(10) | sectors.bin |
| 0x3C | READ BUFFER | libfreemkv signatures |
| 0x43 | READ TOC | toc.bin |
| 0x46 | GET CONFIGURATION | gc_*.bin |
| 0x4A | GET EVENT STATUS | Media polling |
| 0xA4 | REPORT KEY | rpc_state.bin |
| 0xA8 | READ(12) | sectors.bin |
| 0xAD | READ DISC STRUCTURE | ds_*.bin |

## Environment Variables

| Variable | Description |
|----------|-------------|
| `BDEMU_PROFILE` | Path to drive profile directory |
| `BDEMU_DISC` | Disc subdirectory name |
| `BDEMU_QUIET` | Suppress SCSI command logging |

## License

AGPL-3.0-only
