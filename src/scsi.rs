// bdemu — Blu-ray Drive Emulator
// AGPL-3.0 — freemkv project
//
// MMC-6 / SPC-4 compliant SCSI command handlers
// Reference: MMC-6 (mmc6r02g.pdf), SPC-4, SBC-3

use crate::profile::LoadedProfile;
use crate::sg::SgIoHdr;

static mut CALL_NUM: u32 = 0;
// Track last sense for REQUEST_SENSE
static mut LAST_SENSE: [u8; 3] = [0, 0, 0]; // sense_key, asc, ascq
static mut MEDIA_CHANGED: bool = false;

/// Called by the control socket to signal disc change.
pub unsafe fn set_media_changed(changed: bool) {
    MEDIA_CHANGED = changed;
}

fn call() -> u32 {
    unsafe { CALL_NUM += 1; CALL_NUM }
}

fn log(num: u32, msg: &str) {
    if std::env::var("BDEMU_QUIET").is_err() {
        eprintln!("  [{:3}] {}", num, msg);
    }
}

/// Look up the unlock signature for this emulated drive using libfreemkv.
/// Matches the drive's INQUIRY fields + firmware date against the bundled profile database.
fn lookup_unlock_signature(profile: &LoadedProfile) -> [u8; 4] {
    use libfreemkv::identity::DriveId;

    // Extract firmware date from GET_CONFIG 010C feature data
    let firmware_date = profile.find_feature(0x010C)
        .and_then(|data| {
            // Feature descriptor: [0-1] code, [2] version, [3] addl_len, [4+] data
            if data.len() > 4 {
                let date_bytes = &data[4..16.min(data.len())];
                Some(String::from_utf8_lossy(date_bytes).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default();

    // Build DriveId from the emulated drive's INQUIRY + firmware date
    let drive_id = DriveId::from_inquiry(&profile.inquiry, &firmware_date);

    // Search libfreemkv's bundled profiles
    if let Ok(profiles) = libfreemkv::profile::load_bundled() {
        if let Some(matched) = libfreemkv::profile::find_by_drive_id(&profiles, &drive_id) {
            if matched.drive_signature != [0; 4] {
                log(0, &format!("  Profile matched: {} {} {} (sig={:02x}{:02x}{:02x}{:02x})",
                    matched.vendor_id.trim(), matched.product_id.trim(),
                    matched.product_revision.trim(),
                    matched.drive_signature[0], matched.drive_signature[1],
                    matched.drive_signature[2], matched.drive_signature[3]));
                return matched.drive_signature;
            }
        }
        // No match — log clearly
        log(0, &format!("  No profile match for: {} (date={})", drive_id, firmware_date));
    }

    [0; 4]
}

fn save_sense(key: u8, asc: u8, ascq: u8) {
    unsafe { LAST_SENSE = [key, asc, ascq]; }
}

pub fn handle_scsi(hdr: &mut SgIoHdr, profile: &LoadedProfile) {
    let n = call();
    hdr.clear_status();

    // Check for UNIT_ATTENTION (media changed) — takes priority
    // Per SPC-4 §5.9.4, UNIT_ATTENTION reported on first command after change
    unsafe {
        if MEDIA_CHANGED && hdr.opcode() != 0x12 && hdr.opcode() != 0x03 {
            MEDIA_CHANGED = false;
            hdr.set_check_condition(0x06, 0x28, 0x00); // UNIT ATTENTION, MEDIUM MAY HAVE CHANGED
            save_sense(0x06, 0x28, 0x00);
            log(n, &format!("SCSI 0x{:02X} -> UNIT_ATTENTION (media changed)", hdr.opcode()));
            return;
        }
    }

    match hdr.opcode() {
        0x00 => cmd_test_unit_ready(hdr, profile, n),
        0x03 => cmd_request_sense(hdr, n),
        0x12 => cmd_inquiry(hdr, profile, n),
        0x1B => cmd_start_stop_unit(hdr, profile, n),
        0x1E => cmd_prevent_allow_removal(hdr, n),
        0x25 => cmd_read_capacity(hdr, profile, n),
        0x28 => cmd_read_10(hdr, profile, n),
        0x3B => cmd_write_buffer(hdr, n),
        0x3C => cmd_read_buffer(hdr, profile, n),
        0x43 => cmd_read_toc(hdr, profile, n),
        0x46 => cmd_get_configuration(hdr, profile, n),
        0x4A => cmd_get_event_status(hdr, profile, n),
        0x51 => cmd_read_disc_info(hdr, profile, n),
        0x5A => cmd_mode_sense(hdr, profile, n),
        0xA3 => cmd_send_key(hdr, n),
        0xA4 => cmd_report_key(hdr, profile, n),
        0xA8 => cmd_read_12(hdr, profile, n),
        0xAD => cmd_read_disc_structure(hdr, profile, n),
        0xBB => cmd_set_cd_speed(hdr, n),
        _ => {
            hdr.write_response(&[]);
            log(n, &format!("SCSI 0x{:02X} ({} bytes) [unhandled]", hdr.opcode(), hdr.dxfer_len));
        }
    }
}

// ============================================================================
// 0x00 — TEST UNIT READY (SPC-4 §6.33)
// ============================================================================
// Returns GOOD if medium present and ready, NOT READY otherwise.

fn cmd_test_unit_ready(hdr: &mut SgIoHdr, profile: &LoadedProfile, n: u32) {
    if !profile.has_disc() {
        // NOT READY — MEDIUM NOT PRESENT
        hdr.set_check_condition(0x02, 0x3A, 0x00);
        save_sense(0x02, 0x3A, 0x00);
        log(n, "TEST_UNIT_READY -> NOT READY (no medium)");
    } else {
        save_sense(0, 0, 0);
        log(n, "TEST_UNIT_READY -> GOOD");
    }
}

// ============================================================================
// 0x03 — REQUEST SENSE (SPC-4 §6.27)
// ============================================================================
// Returns the last sense data. Always succeeds.

fn cmd_request_sense(hdr: &mut SgIoHdr, n: u32) {
    let alloc = hdr.cdb(4) as usize;
    let mut sense = [0u8; 18];
    sense[0] = 0x70; // response code: current, fixed format
    unsafe {
        sense[2] = LAST_SENSE[0]; // sense key
        sense[7] = 10;             // additional sense length
        sense[12] = LAST_SENSE[1]; // ASC
        sense[13] = LAST_SENSE[2]; // ASCQ
    }
    let len = std::cmp::min(alloc, 18);
    hdr.write_response(&sense[..len]);
    log(n, &format!("REQUEST_SENSE ({} bytes)", alloc));
}

// ============================================================================
// 0x12 — INQUIRY (SPC-4 §6.4)
// ============================================================================
// Standard INQUIRY: return profile inquiry data.
// VPD INQUIRY (EVPD=1): return vital product data pages.

fn cmd_inquiry(hdr: &mut SgIoHdr, profile: &LoadedProfile, n: u32) {
    let evpd = hdr.cdb(1) & 0x01;
    let page_code = hdr.cdb(2);

    if evpd == 0 {
        // Standard INQUIRY
        hdr.write_response(&profile.inquiry);
        log(n, &format!("INQUIRY standard ({} bytes)", hdr.dxfer_len));
    } else {
        // VPD INQUIRY
        match page_code {
            // Page 0x00: Supported VPD Pages
            0x00 => {
                let resp = [
                    0x05,       // peripheral qualifier + device type (CD/DVD)
                    0x00,       // page code
                    0x00, 0x02, // page length = 2
                    0x00,       // supported: page 0x00
                    0x80,       // supported: page 0x80 (serial)
                ];
                hdr.write_response(&resp);
                log(n, "INQUIRY VPD page 0x00 (supported pages)");
            }
            // Page 0x80: Unit Serial Number
            0x80 => {
                // Extract serial from GET_CONFIG 0x0108 feature data
                let serial = profile.find_feature(0x0108)
                    .map(|f| if f.len() > 4 { &f[4..] } else { &[] as &[u8] })
                    .unwrap_or(&[]);
                let mut resp = vec![0x05, 0x80, 0x00, serial.len() as u8];
                resp.extend_from_slice(serial);
                hdr.write_response(&resp);
                log(n, &format!("INQUIRY VPD page 0x80 (serial, {} bytes)", serial.len()));
            }
            _ => {
                // Unsupported VPD page
                hdr.set_check_condition(0x05, 0x24, 0x00); // ILLEGAL REQUEST
                save_sense(0x05, 0x24, 0x00);
                log(n, &format!("INQUIRY VPD page 0x{:02X} -> ILLEGAL REQUEST", page_code));
            }
        }
    }
}

// ============================================================================
// 0x1B — START STOP UNIT (SPC-4 §6.30, MMC-6 §6.37)
// ============================================================================
// Bit 0 of CDB[4]: START (1=start, 0=stop)
// Bit 1 of CDB[4]: LOEJ (1=load/eject, 0=no)
// START=0 LOEJ=1 = eject disc
// START=1 LOEJ=1 = load disc

fn cmd_start_stop_unit(hdr: &mut SgIoHdr, _profile: &LoadedProfile, n: u32) {
    let start = hdr.cdb(4) & 0x01;
    let loej = (hdr.cdb(4) >> 1) & 0x01;

    if loej == 1 && start == 0 {
        log(n, "START_STOP_UNIT -> EJECT");
        // Could update disc state here
    } else if loej == 1 && start == 1 {
        log(n, "START_STOP_UNIT -> LOAD");
    } else if start == 1 {
        log(n, "START_STOP_UNIT -> START");
    } else {
        log(n, "START_STOP_UNIT -> STOP");
    }
}

// ============================================================================
// 0x1E — PREVENT ALLOW MEDIUM REMOVAL (SPC-4 §6.14)
// ============================================================================

fn cmd_prevent_allow_removal(hdr: &mut SgIoHdr, n: u32) {
    let prevent = hdr.cdb(4) & 0x03;
    log(n, &format!("PREVENT_ALLOW_REMOVAL prevent={}", prevent));
}

// ============================================================================
// 0x25 — READ CAPACITY (SBC-3 §5.16)
// ============================================================================
// Returns last LBA and block size.

fn cmd_read_capacity(hdr: &mut SgIoHdr, profile: &LoadedProfile, n: u32) {
    if let Some(disc) = &profile.disc {
        if !disc.capacity.is_empty() {
            hdr.write_response(&disc.capacity);
            log(n, &format!("READ_CAPACITY ({} bytes) from disc", hdr.dxfer_len));
            return;
        }
    }

    if !profile.has_disc() {
        hdr.set_check_condition(0x02, 0x3A, 0x00); // NOT READY
        save_sense(0x02, 0x3A, 0x00);
        log(n, "READ_CAPACITY -> NOT READY (no medium)");
        return;
    }

    // Default: ~25GB BD-SL
    let mut resp = [0u8; 8];
    let lba: u32 = 12219391;
    let blk: u32 = 2048;
    resp[0..4].copy_from_slice(&lba.to_be_bytes());
    resp[4..8].copy_from_slice(&blk.to_be_bytes());
    hdr.write_response(&resp);
    log(n, &format!("READ_CAPACITY ({} bytes) default", hdr.dxfer_len));
}

// ============================================================================
// 0x28 — READ(10) (SBC-3 §5.8)
// ============================================================================
// Transfer LBA sectors to host.

fn cmd_read_10(hdr: &mut SgIoHdr, profile: &LoadedProfile, n: u32) {
    let lba = u32::from_be_bytes([hdr.cdb(2), hdr.cdb(3), hdr.cdb(4), hdr.cdb(5)]);
    let count = u16::from_be_bytes([hdr.cdb(7), hdr.cdb(8)]);
    read_sectors(hdr, profile, lba, count as u32, n, "READ(10)");
}

// ============================================================================
// 0xA8 — READ(12) (SBC-3 §5.9)
// ============================================================================

fn cmd_read_12(hdr: &mut SgIoHdr, profile: &LoadedProfile, n: u32) {
    let lba = u32::from_be_bytes([hdr.cdb(2), hdr.cdb(3), hdr.cdb(4), hdr.cdb(5)]);
    let count = u32::from_be_bytes([hdr.cdb(6), hdr.cdb(7), hdr.cdb(8), hdr.cdb(9)]);
    read_sectors(hdr, profile, lba, count, n, "READ(12)");
}

fn read_sectors(hdr: &mut SgIoHdr, profile: &LoadedProfile, lba: u32, count: u32, n: u32, cmd: &str) {
    if !profile.has_disc() {
        hdr.set_check_condition(0x02, 0x3A, 0x00);
        save_sense(0x02, 0x3A, 0x00);
        log(n, &format!("{} lba={} count={} -> NOT READY", cmd, lba, count));
        return;
    }

    let sector_size = 2048usize;
    let total = (count as usize) * sector_size;
    let mut data = vec![0u8; total];

    if let Some(disc) = &profile.disc {
        if !disc.sector_map.is_empty() {
            // Sparse sector map: look up each requested sector
            for i in 0..count as usize {
                let target_lba = lba + i as u32;
                if let Some(offset) = lookup_sector(&disc.sector_map, &disc.sectors, target_lba) {
                    let dst = i * sector_size;
                    data[dst..dst + sector_size].copy_from_slice(
                        &disc.sectors[offset..offset + sector_size]
                    );
                }
                // Not in map = zeros (already initialized)
            }
        } else if !disc.sectors.is_empty() {
            // Legacy flat dump (LBA = byte offset / 2048)
            let max_sectors = disc.sectors.len() / sector_size;
            for i in 0..count as usize {
                let sector_lba = lba as usize + i;
                if sector_lba < max_sectors {
                    let src_start = sector_lba * sector_size;
                    data[i * sector_size..(i + 1) * sector_size]
                        .copy_from_slice(&disc.sectors[src_start..src_start + sector_size]);
                }
            }
        } else if !disc.sector_data.is_empty() {
            for i in 0..count as usize {
                let src_len = std::cmp::min(disc.sector_data.len(), sector_size);
                data[i * sector_size..i * sector_size + src_len]
                    .copy_from_slice(&disc.sector_data[..src_len]);
            }
        }
    }

    hdr.write_response(&data);
    log(n, &format!("{} lba={} count={} ({} bytes)", cmd, lba, count, hdr.dxfer_len));
}

/// Look up a sector in the sparse sector map.
/// Returns the byte offset into the sectors data, or None if not captured.
fn lookup_sector(map: &[(u32, u32, usize)], _data: &[u8], lba: u32) -> Option<usize> {
    for &(start, count, byte_offset) in map {
        if lba >= start && lba < start + count {
            return Some(byte_offset + (lba - start) as usize * 2048);
        }
    }
    None
}

// ============================================================================
// 0x3B — WRITE BUFFER (SPC-4 §6.38)
// ============================================================================

fn cmd_write_buffer(hdr: &mut SgIoHdr, n: u32) {
    let mode = hdr.cdb(1) & 0x1F;
    let buf_id = hdr.cdb(2);
    log(n, &format!("WRITE_BUFFER mode={} buf=0x{:02X} ({} bytes)", mode, buf_id, hdr.dxfer_len));
}

// ============================================================================
// 0x3C — READ BUFFER (SPC-4 §6.7)
// ============================================================================
// Mode 0: combined header + data
// Mode 2: data — vendor-specific buffer data
// Mode 3: descriptor — buffer capacity info

fn cmd_read_buffer(hdr: &mut SgIoHdr, profile: &LoadedProfile, n: u32) {
    let mode = hdr.cdb(1) & 0x1F;
    let buf_id = hdr.cdb(2);

    // Detect unlock CDB patterns:
    // MT1959-A: mode=1 buf=0x44 len=64
    // MT1959-B: mode=2 buf=0x77 len=64
    // Pioneer:  mode=2 buf varies
    let is_unlock = (mode == 1 && buf_id == 0x44)
                 || (mode == 2 && buf_id == 0x77);

    if is_unlock {
        // Look up drive signature from libfreemkv bundled profiles.
        // Match the emulated drive's INQUIRY fields against the profile database.
        let sig = lookup_unlock_signature(profile);
        let mut resp = vec![0u8; hdr.dxfer_len as usize];
        if resp.len() >= 16 {
            // Signature at [0:4] from profile database
            resp[0..4].copy_from_slice(&sig);
            // "MMkv" at [12:16] — universal verification
            resp[12] = b'M'; resp[13] = b'M'; resp[14] = b'k'; resp[15] = b'v';
        }
        hdr.write_response(&resp);
        log(n, &format!("READ_BUFFER mode={} buf=0x{:02X} -> UNLOCK (sig={:02x}{:02x}{:02x}{:02x})",
                         mode, buf_id, sig[0], sig[1], sig[2], sig[3]));
        return;
    }

    match mode {
        // Mode 2: Data — look up by buffer ID from profile
        2 => {
            if let Some(data) = profile.find_read_buf(buf_id) {
                hdr.write_response(data);
                log(n, &format!("READ_BUFFER mode=2 buf=0x{:02X} ({} bytes)", buf_id, hdr.dxfer_len));
            } else {
                hdr.set_check_condition(0x05, 0x24, 0x00); // ILLEGAL REQUEST
                save_sense(0x05, 0x24, 0x00);
                log(n, &format!("READ_BUFFER mode=2 buf=0x{:02X} -> ILLEGAL REQUEST", buf_id));
            }
        }
        // Mode 3: Descriptor — return buffer capacity
        3 => {
            let resp = [0u8; 4];
            hdr.write_response(&resp);
            log(n, &format!("READ_BUFFER mode=3 buf=0x{:02X} ({} bytes)", buf_id, hdr.dxfer_len));
        }
        // Mode 6: Vendor-specific (MTK register read)
        6 => {
            hdr.write_response(&[]);
            log(n, &format!("READ_BUFFER mode=6 buf=0x{:02X} ({} bytes)", buf_id, hdr.dxfer_len));
        }
        _ => {
            hdr.write_response(&[]);
            log(n, &format!("READ_BUFFER mode={} buf=0x{:02X} ({} bytes)", mode, buf_id, hdr.dxfer_len));
        }
    }
}

// ============================================================================
// 0x43 — READ TOC/PMA/ATIP (MMC-6 §6.26)
// ============================================================================

fn cmd_read_toc(hdr: &mut SgIoHdr, profile: &LoadedProfile, n: u32) {
    if !profile.has_disc() {
        hdr.set_check_condition(0x02, 0x3A, 0x00);
        save_sense(0x02, 0x3A, 0x00);
        log(n, "READ_TOC -> NOT READY");
        return;
    }

    if let Some(disc) = &profile.disc {
        if !disc.toc.is_empty() {
            hdr.write_response(&disc.toc);
            log(n, &format!("READ_TOC ({} bytes) from disc", hdr.dxfer_len));
            return;
        }
    }

    // Default minimal TOC
    let mut resp = [0u8; 12];
    resp[0] = 0x00; resp[1] = 0x0A; // data length
    resp[2] = 0x01; // first track
    resp[3] = 0x01; // last track
    resp[5] = 0x14; // ADR=1, CONTROL=4 (data)
    resp[6] = 0x01; // track 1
    hdr.write_response(&resp);
    log(n, &format!("READ_TOC ({} bytes) default", hdr.dxfer_len));
}

// ============================================================================
// 0x46 — GET CONFIGURATION (MMC-6 §6.6)
// ============================================================================
// CDB[1] bits 0-1: RT (requested type)
//   0 = all features starting from Starting Feature Number
//   1 = current features starting from Starting Feature Number
//   2 = single feature identified by Starting Feature Number
// CDB[2-3]: Starting Feature Number (big-endian)
// CDB[7-8]: Allocation Length (big-endian)
//
// Response header (8 bytes):
//   [0-3] Data Length (excluding these 4 bytes)
//   [4-5] Reserved
//   [6-7] Current Profile
//
// Feature Descriptor:
//   [0-1] Feature Code
//   [2]   Version[7:2] | Persistent[1] | Current[0]
//   [3]   Additional Length
//   [4+]  Feature-specific data

fn cmd_get_configuration(hdr: &mut SgIoHdr, profile: &LoadedProfile, n: u32) {
    let rt = hdr.cdb(1) & 0x03;
    let feat = u16::from_be_bytes([hdr.cdb(2), hdr.cdb(3)]);

    match rt {
        // RT=2: return single feature
        2 => {
            if let Some(feat_data) = profile.find_feature(feat) {
                let data_len = (4 + feat_data.len()) as u32;
                let mut resp = vec![0u8; 8 + feat_data.len()];
                resp[0..4].copy_from_slice(&data_len.to_be_bytes());
                resp[6..8].copy_from_slice(&profile.current_profile.to_be_bytes());
                resp[8..].copy_from_slice(feat_data);
                hdr.write_response(&resp);
                log(n, &format!("GET_CONFIG 0x{:04X} rt=2 ({} bytes)", feat, hdr.dxfer_len));
            } else {
                // Feature not present — return header only per MMC-6 §6.6.2
                let mut resp = [0u8; 8];
                resp[0..4].copy_from_slice(&4u32.to_be_bytes());
                resp[6..8].copy_from_slice(&profile.current_profile.to_be_bytes());
                hdr.write_response(&resp);
                log(n, &format!("GET_CONFIG 0x{:04X} rt=2 -> not present", feat));
            }
        }
        // RT=0 or RT=1: return features starting from 'feat'
        _ => {
            let mut body = Vec::new();
            for (code, data) in &profile.features {
                if *code >= feat {
                    // RT=1: only include "current" features (bit 0 of byte 2)
                    if rt == 1 && data.len() >= 3 && (data[2] & 0x01) == 0 {
                        continue;
                    }
                    body.extend_from_slice(data);
                }
            }
            let data_len = (4 + body.len()) as u32;
            let mut resp = vec![0u8; 8 + body.len()];
            resp[0..4].copy_from_slice(&data_len.to_be_bytes());
            resp[6..8].copy_from_slice(&profile.current_profile.to_be_bytes());
            if !body.is_empty() {
                resp[8..].copy_from_slice(&body);
            }
            hdr.write_response(&resp);
            log(n, &format!("GET_CONFIG 0x{:04X} rt={} ({} bytes, {} features)",
                            feat, rt, hdr.dxfer_len, profile.features.len()));
        }
    }
}

// ============================================================================
// 0x4A — GET EVENT STATUS NOTIFICATION (MMC-6 §6.5)
// ============================================================================
// Polled mode: host polls for media events.
// CDB[1] bit 0: Polled (1=polled, 0=async — async not supported)
// CDB[4]: Notification Class Request bitmap
//   bit 4: Media event
//   bit 2: Power Management
//   bit 1: Operational Change

fn cmd_get_event_status(hdr: &mut SgIoHdr, profile: &LoadedProfile, n: u32) {
    let polled = hdr.cdb(1) & 0x01;
    let class_req = hdr.cdb(4);

    if polled == 0 {
        // Async not supported
        hdr.set_check_condition(0x05, 0x24, 0x00);
        save_sense(0x05, 0x24, 0x00);
        log(n, "GET_EVENT_STATUS -> async not supported");
        return;
    }

    // Media event class (bit 4)
    if class_req & 0x10 != 0 {
        let mut resp = [0u8; 8];
        resp[0] = 0x00; resp[1] = 0x06; // event descriptor length
        resp[2] = 0x04;                  // notification class = media
        resp[3] = 0x10;                  // supported classes = media

        if profile.has_disc() {
            resp[4] = 0x02; // media event: media present
            resp[5] = 0x02; // media status: door closed, media present
        } else {
            resp[4] = 0x00; // no event
            resp[5] = 0x00; // door closed, no media
        }
        hdr.write_response(&resp);
        log(n, &format!("GET_EVENT_STATUS media (disc={})", profile.has_disc()));
    } else {
        // No supported event class
        let mut resp = [0u8; 4];
        resp[0] = 0x00; resp[1] = 0x02;
        resp[2] = 0x00; // no event
        resp[3] = 0x00; // no supported classes
        hdr.write_response(&resp);
        log(n, "GET_EVENT_STATUS -> no supported class");
    }
}

// ============================================================================
// 0x51 — READ DISC INFORMATION (MMC-6 §6.22)
// ============================================================================

fn cmd_read_disc_info(hdr: &mut SgIoHdr, profile: &LoadedProfile, n: u32) {
    if !profile.has_disc() {
        hdr.set_check_condition(0x02, 0x3A, 0x00);
        save_sense(0x02, 0x3A, 0x00);
        log(n, "READ_DISC_INFO -> NOT READY");
        return;
    }

    if let Some(disc) = &profile.disc {
        if !disc.disc_info.is_empty() {
            hdr.write_response(&disc.disc_info);
            log(n, &format!("READ_DISC_INFO ({} bytes) from disc", disc.disc_info.len()));
            return;
        }
    }

    // Default
    let mut resp = [0u8; 34];
    resp[0] = 0x00; resp[1] = 0x20;
    resp[2] = 0x0E;
    resp[3] = 0x01; resp[4] = 0x01;
    resp[5] = 0x01; resp[6] = 0x01;
    resp[7] = 0x20;
    hdr.write_response(&resp);
    log(n, &format!("READ_DISC_INFO ({} bytes) default", hdr.dxfer_len));
}

// ============================================================================
// 0x5A — MODE SENSE(10) (SPC-4 §6.11)
// ============================================================================
// CDB[2] bits 5-0: Page Code
// CDB[2] bits 7-6: PC (page control)
//
// Response: Mode Parameter Header(8) + Block Descriptor(s) + Mode Page(s)

fn cmd_mode_sense(hdr: &mut SgIoHdr, profile: &LoadedProfile, n: u32) {
    let page = hdr.cdb(2) & 0x3F;
    let _pc = (hdr.cdb(2) >> 6) & 0x03;

    match page {
        // Page 0x2A: CD/DVD Capabilities and Mechanical Status
        0x2A => {
            if !profile.mode_2a.is_empty() {
                hdr.write_response(&profile.mode_2a);
            } else {
                // Minimal capabilities page
                let mut resp = [0u8; 28];
                resp[0] = 0x00; resp[1] = 0x1A; // data length
                // Page 2A header
                resp[8] = 0x2A; resp[9] = 0x12; // page code, page length
                resp[10] = 0x3F; resp[11] = 0x37; // read capabilities
                hdr.write_response(&resp);
            }
            log(n, &format!("MODE_SENSE page 0x2A ({} bytes)", hdr.dxfer_len));
        }
        // Page 0x3F: All pages
        0x3F => {
            if !profile.mode_2a.is_empty() {
                hdr.write_response(&profile.mode_2a);
            } else {
                hdr.write_response(&[]);
            }
            log(n, &format!("MODE_SENSE page 0x3F (all) ({} bytes)", hdr.dxfer_len));
        }
        _ => {
            // Unsupported page
            hdr.set_check_condition(0x05, 0x24, 0x00);
            save_sense(0x05, 0x24, 0x00);
            log(n, &format!("MODE_SENSE page 0x{:02X} -> ILLEGAL REQUEST", page));
        }
    }
}

// ============================================================================
// 0xA3 — SEND KEY (MMC-6 §6.31)
// ============================================================================
// AACS authentication — just acknowledge for now

fn cmd_send_key(hdr: &mut SgIoHdr, n: u32) {
    let key_class = hdr.cdb(7);
    let key_format = hdr.cdb(10) & 0x3F;
    log(n, &format!("SEND_KEY class=0x{:02X} format={} ({} bytes)",
                     key_class, key_format, hdr.dxfer_len));
}

// ============================================================================
// 0xA4 — REPORT KEY (MMC-6 §6.25)
// ============================================================================
// Key class 0x00: DVD CSS/CPPM
// Key class 0x02: AACS
// Key class 0x08: RPC state

fn cmd_report_key(hdr: &mut SgIoHdr, profile: &LoadedProfile, n: u32) {
    let key_class = hdr.cdb(7);
    let key_format = hdr.cdb(10) & 0x3F;

    match key_class {
        // RPC state
        0x08 if key_format == 0x08 => {
            if !profile.rpc_state.is_empty() {
                hdr.write_response(&profile.rpc_state);
            } else {
                let resp = [0x00, 0x06, 0x00, 0x00, 0x25, 0xFF, 0x01, 0x00];
                hdr.write_response(&resp);
            }
            log(n, "REPORT_KEY RPC state");
        }
        _ => {
            hdr.write_response(&[]);
            log(n, &format!("REPORT_KEY class=0x{:02X} format={} ({} bytes)",
                             key_class, key_format, hdr.dxfer_len));
        }
    }
}

// ============================================================================
// 0xAD — READ DISC STRUCTURE (MMC-6 §6.23)
// ============================================================================
// Returns disc physical format information, DI, BCA, etc.

fn cmd_read_disc_structure(hdr: &mut SgIoHdr, profile: &LoadedProfile, n: u32) {
    if !profile.has_disc() {
        hdr.set_check_condition(0x02, 0x3A, 0x00);
        save_sense(0x02, 0x3A, 0x00);
        log(n, "READ_DISC_STRUCTURE -> NOT READY");
        return;
    }

    let format = hdr.cdb(7);

    if let Some(disc) = &profile.disc {
        if let Some(data) = disc.disc_structures.get(&format) {
            hdr.write_response(data);
            log(n, &format!("READ_DISC_STRUCTURE format={} ({} bytes) from disc", format, data.len()));
            return;
        }
    }

    // Format not available — return empty header (not an error, just no data)
    let mut resp = [0u8; 4];
    resp[0] = 0x00; resp[1] = 0x02;
    hdr.write_response(&resp);
    log(n, &format!("READ_DISC_STRUCTURE format={} -> empty", format));
}

// ============================================================================
// 0xBB — SET CD SPEED (MMC-6 §6.30)
// ============================================================================
// Pioneer uses this for speed control via vendor extension

fn cmd_set_cd_speed(hdr: &mut SgIoHdr, n: u32) {
    let read_speed = u16::from_be_bytes([hdr.cdb(2), hdr.cdb(3)]);
    let write_speed = u16::from_be_bytes([hdr.cdb(4), hdr.cdb(5)]);
    log(n, &format!("SET_CD_SPEED read={} write={}", read_speed, write_speed));
}
