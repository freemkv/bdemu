[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue)](LICENSE)

# bdemu

Blu-ray drive emulator for development and testing. Intercepts Linux `SG_IO` ioctls via `LD_PRELOAD` to emulate a complete optical drive from captured SCSI response data. No real drive needed.

Uses [libfreemkv](https://github.com/freemkv/libfreemkv) for unlock signature lookup — the emulated drive responds with correct signatures from the bundled profile database.

Part of the [freemkv](https://github.com/freemkv) project.

## Quick Start

```bash
# Build
cargo build --release

# Run any application against an emulated drive
BDEMU_PROFILE=profiles/bu40n \
BDEMU_DISC=sample \
LD_PRELOAD=target/release/libbdemu.so \
  freemkv info
```

Output:

```
[bdemu] Loaded drive profile: profiles/bu40n
[bdemu] Loaded disc: sample (326481 sectors)
[bdemu] Intercepting SG_IO on fd=3

Drive Information
  Vendor:              HL-DT-ST
  Product:             BD-RE BU40N
  Revision:            1.03
  Firmware type:       NM00000
  Firmware date:       2018-10-24

Profile Match
  Chipset:             MediaTek MT1959
  Status:              Matched (ready to unlock)
```

## Creating a Drive Profile

### Option 1: From `freemkv info --share`

The easiest way. Captures all the SCSI responses bdemu needs:

```bash
sudo freemkv info --share profiles/my-drive/
```

This creates the profile directory with `.bin` files for INQUIRY, GET CONFIGURATION, MODE SENSE, etc.

### Option 2: Manual capture

If you need to capture specific responses:

```bash
# Standard responses are captured via freemkv
sudo freemkv info --share profiles/my-drive/

# Disc data is captured separately
bdemu capture-disc /dev/sr0 profiles/my-drive/discs/my-disc/ --sectors 10000
```

## Creating a Disc Profile

bdemu serves real sector data from captured disc dumps. To capture a disc:

```bash
# Capture TOC, capacity, disc info, structure, and sector data
bdemu capture-disc /dev/sr0 profiles/my-drive/discs/my-disc/

# Limit the number of sectors (useful for testing)
bdemu capture-disc /dev/sr0 profiles/my-drive/discs/my-disc/ --sectors 1000
```

### Minimal test disc

For basic testing, you only need a few files:

```bash
mkdir -p profiles/my-drive/discs/test/

# Create a minimal disc with 100 sectors of zeros
dd if=/dev/zero of=profiles/my-drive/discs/test/sectors.bin bs=2048 count=100
```

bdemu will generate synthetic TOC and capacity responses if the `.bin` files are missing.

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
└── discs/
    └── sample/
        ├── toc.bin      # READ TOC response
        ├── capacity.bin # READ CAPACITY response
        ├── disc_info.bin
        ├── ds_00.bin    # READ DISC STRUCTURE format 0 (PFI)
        └── sectors.bin  # Sector dump (2048 bytes/sector)
```

## Emulated SCSI Commands

18 MMC-6 commands are handled:

| Opcode | Command | Source |
|--------|---------|--------|
| 0x00 | TEST UNIT READY | Disc presence |
| 0x03 | REQUEST SENSE | Sense history |
| 0x12 | INQUIRY | inquiry.bin |
| 0x1B | START STOP UNIT | Eject/load |
| 0x1E | PREVENT ALLOW MEDIUM REMOVAL | |
| 0x25 | READ CAPACITY | capacity.bin |
| 0x28 | READ(10) | sectors.bin |
| 0x3C | READ BUFFER | libfreemkv signatures |
| 0x43 | READ TOC | toc.bin |
| 0x46 | GET CONFIGURATION | gc_*.bin |
| 0x4A | GET EVENT STATUS | Media polling |
| 0x51 | READ DISC INFO | disc_info.bin |
| 0x5A | MODE SENSE(10) | mode_2a.bin |
| 0xA4 | REPORT KEY | rpc_state.bin |
| 0xA8 | READ(12) | sectors.bin |
| 0xAD | READ DISC STRUCTURE | ds_*.bin |

Unsupported commands return CHECK CONDITION with appropriate sense data.

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `BDEMU_PROFILE` | Path to drive profile directory | Required |
| `BDEMU_DISC` | Disc subdirectory name (under `discs/`) | No disc loaded |
| `BDEMU_QUIET` | Suppress SCSI command logging | Logging enabled |
| `BDEMU_LOG` | Log file path | stderr |

## Validating a Profile

```bash
bdemu validate profiles/bu40n/

Profile: profiles/bu40n/
  inquiry.bin:    OK (96 bytes)
  gc_0000.bin:    OK (168 bytes)
  gc_010c.bin:    OK (28 bytes)
  mode_2a.bin:    OK (32 bytes)
  rpc_state.bin:  OK (8 bytes)
  drive.toml:     OK
  Disc 'sample':  OK (326481 sectors)
```

## Platform

Linux only — requires `LD_PRELOAD` and `SG_IO` ioctl interception. Not applicable to macOS or Windows.

## License

AGPL-3.0-only
