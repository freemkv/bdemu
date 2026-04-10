# bdemu — Feature List

## v0.5.0 (current)

### Done
- [x] LD_PRELOAD SCSI interceptor (Rust cdylib)
- [x] 18 MMC-6/SPC-4 SCSI command handlers
- [x] Drive profiles: TOML + .bin directory format
- [x] Disc profiles: TOC, capacity, disc_info, disc structures, sector dump
- [x] LBA-addressable sector reads from disc dump
- [x] Per-format disc structure serving (ds_00.bin, ds_01.bin, etc.)
- [x] UNIT_ATTENTION on media change
- [x] REQUEST_SENSE with sense history
- [x] INQUIRY VPD pages (0x00, 0x80)
- [x] GET_CONFIGURATION all request types (rt=0, 1, 2)
- [x] GET_EVENT_STATUS_NOTIFICATION (media polling)
- [x] Profiles: BU40N (real hardware), Pioneer BDR-S09 (ImgBurn)
- [x] `bdemu capture-disc` — capture disc from real hardware
- [x] `bdemu validate` — check profile completeness
- [x] Disc state: no disc / disc loaded via BDEMU_DISC
- [x] libfreemkv integration for correct unlock responses
- [x] Control socket: insert/eject disc at runtime
- [x] BDSM sparse sector format
- [x] Smart capture via libfreemkv UDF range discovery
- [x] Auto-eject after capture
- [x] Auto-rename output to slugified volume ID

### Planned
- [ ] AACS key exchange (SEND_KEY/REPORT_KEY handshake)
- [ ] `bdemu capture-drive` — capture drive profile (currently in freemkv drive-info)
- [ ] Multiple simultaneous drives
- [ ] Disc image (.iso) support for sector reads
- [ ] Full AACS volume ID emulation

### Future
- [ ] GUI frontend for disc management
- [ ] Network-shared profiles
- [ ] Automated test suite: identity → unlock → scan → rip
- [ ] Profile format versioning and migration
- [ ] Pioneer platform unlock support
- [ ] HL-DT-ST Renesas platform support
