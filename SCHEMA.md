# bdemu / freemkv-info Drive Profile Schema

## Overview

One JSON schema, two uses:
- `freemkv-info --share` produces it from real hardware
- `bdemu` consumes it to emulate a drive

All byte arrays are hex-encoded strings. All fields from real SCSI responses.

## Schema

```json
{
    "schema_version": 1,
    "source": "freemkv-info 0.1.0",
    "timestamp": "2026-04-06T01:00:00Z",

    "drive": {
        "manufacturer": "PIONEER",
        "product": "BD-RW BDR-S09",
        "revision": "1.34",
        "serial": "OEDL016822WL",
        "firmware_date": "2016-04-25"
    },

    "inquiry": {
        "raw": "058000325B000000...(96 bytes hex)",
        "peripheral_type": 5,
        "rmb": true,
        "additional_length": 91,
        "vendor": "PIONEER ",
        "product": "BD-RW   BDR-S09 ",
        "revision": "1.34",
        "vendor_specific": "2031362F30342F323520205049..."
    },

    "get_config": {
        "current_profile": "0x0043",
        "features": {
            "0x0000": { "raw": "..." },
            "0x0003": { "raw": "..." },
            "0x001D": { "raw": "..." },
            "0x001E": { "raw": "..." },
            "0x001F": { "raw": "..." },
            "0x0040": { "raw": "..." },
            "0x0041": { "raw": "..." },
            "0x0102": { "raw": "..." },
            "0x0107": { "raw": "..." },
            "0x0108": {
                "raw": "0108030C4F45444C303136383232574C",
                "serial": "OEDL016822WL"
            },
            "0x010C": {
                "raw": "010C03103230313630343235303030300000000",
                "firmware_date": "201604250000"
            },
            "0x010D": { "raw": "..." }
        }
    },

    "mode_sense": {
        "page_2a": {
            "raw": "...(capabilities page hex)"
        }
    },

    "report_key": {
        "rpc_state": {
            "raw": "0006000025FF0100"
        }
    },

    "read_buffer": {
        "0xF1": {
            "raw": "4F45444C303136383232574C20202020534154203836303049443433...",
            "serial": "OEDL016822WL",
            "interface": "SAT ",
            "chip_id": "8600",
            "fw_type": "ID43"
        },
        "0xB0": {
            "raw": "..."
        }
    },

    "read_disc_structure": {},
    "read_capacity": {},
    "read_toc": {}
}
```

## Field Notes

- `inquiry.raw`: Full 96-byte INQUIRY response, hex
- `get_config.features`: Each feature keyed by hex feature code. `raw` = full feature descriptor including header (feature_code + version/persistent/current + additional_length + data)
- `get_config.features.0x010C`: MMC-6 Firmware Information. `firmware_date` = CCYYMMDDHHMI (12 ASCII chars)
- `read_buffer`: Keyed by hex buffer ID. Only for mode 2 (vendor data)
- All decoded fields (serial, vendor, etc.) are convenience — `raw` is authoritative

## Producing (freemkv-info)

```
freemkv-info --share > my-drive.json
```

Sends standard SCSI commands to the drive, captures responses, outputs JSON.

## Consuming (bdemu)

```
bdemu my-drive.json -- makemkvcon --robot -r info disc:0
```

Intercepts SG_IO, returns responses from the JSON profile.
