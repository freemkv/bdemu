# bdemu

4K UHD / Blu-ray drive emulator for development and testing. Emulates 4K UHD / BD / DVD / CD optical drives using captured hardware profiles.

Part of the [freemkv](https://github.com/freemkv) project.

## Features

- **LD_PRELOAD SCSI interceptor** — emulates a drive without real hardware
- **Profile-based** — each drive is a directory of captured SCSI response data
- **MMC-6 compliant** — 18 SCSI command handlers following the standard
- **Disc profiles** — emulate inserted discs with real TOC, capacity, and sector data
- **Capture tools** — `bdemu capture-disc` grabs disc data from real hardware
- **Profile validation** — `bdemu validate` checks profile completeness

## Quick Start

```bash
# Build
cargo build --release

# Emulate a drive (LD_PRELOAD)
BDEMU_PROFILE=profiles/bu40n \
BDEMU_DISC=test_disc \
LD_PRELOAD=target/release/libbdemu.so \
  your-app-here

# Capture a disc profile from real hardware
./target/release/bdemu capture-disc /dev/sg4 profiles/my-drive/discs/my-disc/

# Validate a profile
./target/release/bdemu validate profiles/bu40n/
```

## Profile Structure

```
profiles/bu40n/
├── drive.toml           # Drive metadata and file references
├── inquiry.bin          # INQUIRY response (96 bytes)
├── gc_0000.bin          # GET_CONFIG 0x0000 Profile List
├── gc_0108.bin          # GET_CONFIG 0x0108 Serial Number
├── gc_010c.bin          # GET_CONFIG 0x010C Firmware Information
├── gc_*.bin             # Other GET_CONFIG features
├── rpc_state.bin        # REPORT KEY RPC state
├── mode_2a.bin          # MODE SENSE page 2A capabilities
├── rb_f1.bin            # READ_BUFFER 0xF1 (Pioneer drives)
└── discs/
    └── my_disc/
        ├── toc.bin      # READ TOC response
        ├── capacity.bin # READ CAPACITY response
        ├── disc_info.bin
        ├── ds_00.bin    # READ DISC STRUCTURE format 0 (PFI)
        └── sectors.bin  # Sector dump (2048 bytes per sector)
```

## Creating Profiles

### Drive profile

Use [freemkv-info](https://github.com/freemkv/freemkv-info) to capture a drive profile from real hardware:

```bash
freemkv-info --share profiles/my-drive/
```

### Disc profile

```bash
bdemu capture-disc /dev/sg4 profiles/my-drive/discs/my-disc/ --sectors 10000
```

## SCSI Commands

| Opcode | Command | Notes |
|--------|---------|-------|
| 0x00 | TEST UNIT READY | Disc-aware |
| 0x03 | REQUEST SENSE | Sense history |
| 0x12 | INQUIRY | Standard + VPD pages |
| 0x1B | START STOP UNIT | Eject/load |
| 0x1E | PREVENT ALLOW MEDIUM REMOVAL | |
| 0x25 | READ CAPACITY | From disc profile |
| 0x28 | READ(10) | LBA-addressable from sector dump |
| 0x3B | WRITE BUFFER | Logged |
| 0x3C | READ BUFFER | Modes 2, 3, 6 |
| 0x43 | READ TOC | From disc profile |
| 0x46 | GET CONFIGURATION | RT=0, 1, 2 |
| 0x4A | GET EVENT STATUS | Media polling |
| 0x51 | READ DISC INFO | From disc profile |
| 0x5A | MODE SENSE(10) | Page 2A |
| 0xA3 | SEND KEY | Logged |
| 0xA4 | REPORT KEY | RPC state |
| 0xA8 | READ(12) | LBA-addressable |
| 0xAD | READ DISC STRUCTURE | Per-format from disc profile |
| 0xBB | SET CD SPEED | Logged |

## Environment Variables

| Variable | Description |
|----------|-------------|
| `BDEMU_PROFILE` | Path to drive profile directory |
| `BDEMU_DISC` | Disc subdirectory name (under `discs/`) |
| `BDEMU_QUIET` | Set to suppress SCSI command logging |

## License

AGPL-3.0
