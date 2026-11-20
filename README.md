[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![CI](https://github.com/freemkv/bdemu/actions/workflows/ci.yml/badge.svg)](https://github.com/freemkv/bdemu/actions/workflows/ci.yml)
[![Latest Release](https://img.shields.io/github/v/release/freemkv/bdemu?label=latest&color=brightgreen)](https://github.com/freemkv/bdemu/releases/latest)
[![codecov](https://codecov.io/gh/freemkv/bdemu/branch/dev/graph/badge.svg)](https://codecov.io/gh/freemkv/bdemu)

# bdemu

Blu-ray drive emulator for development and testing. Intercepts Linux SG_IO ioctls via `LD_PRELOAD` to emulate a complete optical drive from captured SCSI response data. No real drive needed.

Part of the [freemkv](https://github.com/freemkv) project. **Linux only.**

## Download

**[Download latest release](https://github.com/freemkv/bdemu/releases/latest)**

The release is a tarball (`bdemu-x86_64-linux.tar.gz`) with the `bdemu` binary,
`libbdemu.so` and the sample `profiles/`.

Or build from source with `cargo build --release` (see `rust-version` in
`Cargo.toml` for the minimum Rust). The build produces both `bdemu` and
`libbdemu.so`. `bdemu run` looks for `libbdemu.so` next to the `bdemu` binary,
so keep them together.

## Quick Start

```bash
# Capture a disc (auto-names; pass --eject to eject the tray afterwards)
bdemu capture-disc /dev/sr0 ./testbed/disc

# Emulate a drive and scan the captured disc
bdemu run --profile profiles/hl-dt-st-bd-re-bu40n-1.03-nm00000 --disc my_movie -- freemkv info disc://
```

## Commands

```
bdemu 1.8.10

Commands:
  run --profile <dir> [--disc <name>] -- <cmd>   Emulate drive, run command
  capture-disc <device> <output_dir> [--eject]   Smart capture from hardware
  validate <profile_dir>                         Check profile completeness

Control (while emulator is running):
  status                                         Show emulator state
  eject                                          Eject the disc
  load <disc_name>                               Load a disc
  list-discs                                     List available discs

Examples:
  bdemu capture-disc /dev/sr0 ./testbed/disc     Capture + auto-name (add --eject to eject)
  bdemu run -p profiles/bu40n -d sample -- ./freemkv info
  bdemu validate profiles/bu40n/
```

`-p` and `-d` are short forms of `--profile` and `--disc`.

## Smart Capture

`capture-disc` uses libfreemkv to parse the disc's UDF filesystem and capture only the sectors needed for emulation. A capture is a few MB instead of tens of GB.

After a clean capture, the output directory is renamed to a slug of the disc's volume ID (e.g. `disc` becomes `sample_film`). If the name already exists, a number is appended (`sample_film_2`). A partial capture (read errors) keeps the bytes it wrote but is not renamed.

The tray is **not** ejected by default. Pass `--eject` to `capture-disc` if you
want the disc ejected once the capture finishes. Eject is irreversible on
slot-loading drives (you have to physically reload the disc), so it is opt-in.

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
+-- rb_*.bin             # READ BUFFER responses (some profiles)
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
| `BDEMU_INSTANCE` | Instance id for the control socket (`$XDG_RUNTIME_DIR/bdemu-<id>.sock`). Unset uses `bdemu.sock`. Set it on both the `bdemu run` side and the `status`/`load`/`eject` side when running two emulators at once, otherwise the second refuses to start rather than stealing the first's socket. Ids may contain ASCII letters, digits, `-` and `_`. |

## License

MIT
