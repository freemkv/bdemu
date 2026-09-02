// bdemu — Blu-ray Drive Emulator (MIT — freemkv project)
// MMC-6 / SPC-4 compliant SCSI command handlers
// Reference: MMC-6 (mmc6r02g.pdf), SPC-4, SBC-3

use crate::profile::{LoadedProfile, SECTOR_SIZE};
use crate::sg::SgIoHdr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

static CALL_NUM: AtomicU32 = AtomicU32::new(0);
static LAST_SENSE: Mutex<[u8; 3]> = Mutex::new([0, 0, 0]); // sense_key, asc, ascq
static MEDIA_CHANGED: AtomicBool = AtomicBool::new(false);
// One-shot "new media has arrived" edge for GET EVENT STATUS NOTIFICATION.
// MMC-6's NewMedia (0x02) is an edge event reported ONCE, not steady-state
// present; armed on media change, cleared after the first poll reports it.
static NEW_MEDIA_EVENT: AtomicBool = AtomicBool::new(false);

/// Called by the control socket to signal disc change.
pub fn set_media_changed(changed: bool) {
    MEDIA_CHANGED.store(changed, Ordering::SeqCst);
    // A media change also arms the GET EVENT one-shot NewMedia edge so a host
    // that polls GET EVENT (rather than observing the UNIT ATTENTION) re-enumerates
    // the disc exactly once instead of on every poll.
    if changed {
        NEW_MEDIA_EVENT.store(true, Ordering::SeqCst);
    }
}

fn call() -> u32 {
    // The value is only a human-readable log label, so wrap on overflow rather
    // than panic in debug builds after ~4 billion SCSI commands.
    CALL_NUM.fetch_add(1, Ordering::Relaxed).wrapping_add(1)
}

// Whether SCSI command logging is suppressed (BDEMU_QUIET). Cached in a
// OnceLock since std::env::var takes a process-wide lock and this runs on
// every SCSI command, including the hot READ(10)/READ(12) sector path.
fn quiet() -> bool {
    static QUIET: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *QUIET.get_or_init(|| std::env::var("BDEMU_QUIET").is_ok())
}

fn log(num: u32, msg: &str) {
    if !quiet() {
        eprintln!("  [{:3}] {}", num, msg);
    }
}

// Logging variant that only builds its message if it will be printed.
// `log(n, &format!(...))` formats eagerly even under BDEMU_QUIET; that's fine
// for one-shot commands but wasteful on the hot READ(10)/READ(12) path.
fn log_lazy(num: u32, msg: impl FnOnce() -> String) {
    if !quiet() {
        eprintln!("  [{:3}] {}", num, msg());
    }
}

/// Look up the unlock signature for this emulated drive using libfreemkv.
/// Matches the drive's INQUIRY fields + firmware date against the bundled profile database.
fn lookup_unlock_signature(profile: &LoadedProfile, n: u32) -> [u8; 4] {
    use libfreemkv::DriveId;

    // Extract firmware date from GET_CONFIG 010C feature data
    let firmware_date = profile
        .find_feature(0x010C)
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

    // Build DriveId from INQUIRY + firmware date via libfreemkv's `from_inquiry`
    // parser/Display; freemkv-unlock's catalog uses its own raw `DriveId`, so
    // map the four fields across (the two crates' DriveId are deliberately distinct).
    let lf_drive_id = DriveId::from_inquiry(&profile.inquiry, &firmware_date);
    let drive_id = freemkv_unlock::DriveId {
        vendor_id: lf_drive_id.vendor_id.clone(),
        product_id: lf_drive_id.product_id.clone(),
        product_revision: lf_drive_id.product_revision.clone(),
        vendor_specific: lf_drive_id.vendor_specific.clone(),
        firmware_date: lf_drive_id.firmware_date.clone(),
    };

    // Search the bundled drive-unlock profiles via freemkv-unlock's public catalog
    // API (the single freemkv-unlock crate, same crate libfreemkv depends on).
    if let Some(profiles) = freemkv_unlock::ld::profiles() {
        if let Some(m) = profiles.get(&drive_id)
            && m.profile.signature != [0; 4]
        {
            log(
                n,
                &format!(
                    "  Profile matched: {} {} {} (sig={:02x}{:02x}{:02x}{:02x})",
                    m.profile.identity.vendor_id.trim(),
                    m.profile.identity.vendor_specific.trim(),
                    m.profile.identity.product_revision.trim(),
                    m.profile.signature[0],
                    m.profile.signature[1],
                    m.profile.signature[2],
                    m.profile.signature[3]
                ),
            );
            return m.profile.signature;
        }
        // No match — log clearly
        log(
            n,
            &format!(
                "  No profile match for: {} (date={})",
                lf_drive_id, firmware_date
            ),
        );
    }

    [0; 4]
}

/// Serializes every test that touches the emulator's process-global SCSI state
/// (MEDIA_CHANGED, NEW_MEDIA_EVENT, LAST_SENSE). It lives OUTSIDE the test module
/// because the control-socket tests need it too: `cmd_load` / `cmd_eject` call
/// `set_media_changed(true)`, which a concurrently-running READ test would then
/// observe as a UNIT ATTENTION instead of the sense it was asserting on. One
/// shared guard is the only thing that actually serializes them.
#[cfg(test)]
pub static TEST_GUARD: Mutex<()> = Mutex::new(());

fn save_sense(key: u8, asc: u8, ascq: u8) {
    if let Ok(mut sense) = LAST_SENSE.lock() {
        *sense = [key, asc, ascq];
    }
}

pub fn handle_scsi(hdr: &mut SgIoHdr, profile: &LoadedProfile) {
    let n = call();
    hdr.clear_status();

    // UNIT ATTENTION (media changed) fires as CHECK CONDITION on the first
    // command after a change. INQUIRY/REQUEST SENSE are exempt from consuming
    // it (SPC) — `swap` must stay the right operand of `&&`, else it runs first.
    let op = hdr.opcode();
    if op != 0x12 && op != 0x03 && MEDIA_CHANGED.swap(false, Ordering::SeqCst) {
        hdr.set_check_condition(0x06, 0x28, 0x00); // UNIT ATTENTION, MEDIUM MAY HAVE CHANGED
        save_sense(0x06, 0x28, 0x00);
        log(
            n,
            &format!(
                "SCSI 0x{:02X} -> UNIT_ATTENTION (media changed)",
                hdr.opcode()
            ),
        );
        return;
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
            // Per SPC-4, an unsupported opcode must be CHECK CONDITION /
            // ILLEGAL REQUEST / INVALID COMMAND OPERATION CODE — not GOOD.
            hdr.set_check_condition(0x05, 0x20, 0x00);
            save_sense(0x05, 0x20, 0x00);
            log(
                n,
                &format!(
                    "SCSI 0x{:02X} ({} bytes) [unhandled -> ILLEGAL REQUEST]",
                    hdr.opcode(),
                    hdr.dxfer_len
                ),
            );
        }
    }
}

// ============================================================================
// 0x00 — TEST UNIT READY (SPC-4 §6.33)
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
// Returns the last sense data. Always succeeds.

fn cmd_request_sense(hdr: &mut SgIoHdr, n: u32) {
    let alloc = hdr.cdb(4) as usize;
    let mut sense = [0u8; 18];
    sense[0] = 0x70; // response code: current, fixed format

    // A pending media-change UNIT ATTENTION must be reported-and-cleared here too
    // (SPC-4 §6.27): handle_scsi exempts REQUEST SENSE from the UA-consuming swap,
    // so a bare REQUEST SENSE before any real command must consume/report it here.
    if MEDIA_CHANGED.swap(false, Ordering::SeqCst) {
        sense[2] = 0x06; // UNIT ATTENTION
        sense[7] = 10; // additional sense length
        sense[12] = 0x28; // ASC: MEDIUM MAY HAVE CHANGED
        sense[13] = 0x00; // ASCQ
        let len = std::cmp::min(alloc, 18);
        hdr.write_response(&sense[..len]);
        log(
            n,
            &format!("REQUEST_SENSE ({} bytes) -> UNIT ATTENTION", alloc),
        );
        return;
    }

    if let Ok(mut last) = LAST_SENSE.lock() {
        sense[2] = last[0]; // sense key
        sense[7] = 10; // additional sense length
        sense[12] = last[1]; // ASC
        sense[13] = last[2]; // ASCQ
        // Per SPC, REQUEST SENSE is read-and-clear: once reported, latched sense
        // must be cleared, or commands that don't reset sense on success (INQUIRY,
        // START_STOP_UNIT, etc.) leave a stale error replayed indefinitely.
        *last = [0, 0, 0];
    }
    let len = std::cmp::min(alloc, 18);
    hdr.write_response(&sense[..len]);
    log(n, &format!("REQUEST_SENSE ({} bytes)", alloc));
}

// ============================================================================
// 0x12 — INQUIRY (SPC-4 §6.4)
// Standard INQUIRY returns profile inquiry data; VPD (EVPD=1) returns VPD pages.

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
                    0x05, // peripheral qualifier + device type (CD/DVD)
                    0x00, // page code
                    0x00, 0x02, // page length = 2
                    0x00, // supported: page 0x00
                    0x80, // supported: page 0x80 (serial)
                ];
                hdr.write_response(&resp);
                log(n, "INQUIRY VPD page 0x00 (supported pages)");
            }
            // Page 0x80: Unit Serial Number
            0x80 => {
                // Extract serial from GET_CONFIG 0x0108 feature data
                let serial = profile
                    .find_feature(0x0108)
                    .map(|f| if f.len() > 4 { &f[4..] } else { &[] as &[u8] })
                    .unwrap_or(&[]);
                // The page-length byte is a u8; clamp the serial so a malformed
                // profile with a >255-byte serial can't advertise a wrong
                // length via a silent `as u8` truncation.
                let serial = &serial[..serial.len().min(255)];
                let mut resp = vec![0x05, 0x80, 0x00, serial.len() as u8];
                resp.extend_from_slice(serial);
                hdr.write_response(&resp);
                log(
                    n,
                    &format!("INQUIRY VPD page 0x80 (serial, {} bytes)", serial.len()),
                );
            }
            _ => {
                // Unsupported VPD page
                hdr.set_check_condition(0x05, 0x24, 0x00); // ILLEGAL REQUEST
                save_sense(0x05, 0x24, 0x00);
                log(
                    n,
                    &format!("INQUIRY VPD page 0x{:02X} -> ILLEGAL REQUEST", page_code),
                );
            }
        }
    }
}

// 0x1B — START STOP UNIT (SPC-4 §6.30, MMC-6 §6.37)
// CDB[4] bit0=START (1=start,0=stop), bit1=LOEJ (1=load/eject,0=no):
// START=0,LOEJ=1 -> eject; START=1,LOEJ=1 -> load.

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

// 0x25 — READ CAPACITY (SBC-3 §5.16)
// Returns last LBA and block size.

fn cmd_read_capacity(hdr: &mut SgIoHdr, profile: &LoadedProfile, n: u32) {
    if let Some(disc) = &profile.disc
        && !disc.capacity.is_empty()
    {
        hdr.write_response(&disc.capacity);
        log(
            n,
            &format!("READ_CAPACITY ({} bytes) from disc", hdr.dxfer_len),
        );
        return;
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
    let blk: u32 = SECTOR_SIZE as u32;
    resp[0..4].copy_from_slice(&lba.to_be_bytes());
    resp[4..8].copy_from_slice(&blk.to_be_bytes());
    hdr.write_response(&resp);
    log(
        n,
        &format!("READ_CAPACITY ({} bytes) default", hdr.dxfer_len),
    );
}

// 0x28 — READ(10) (SBC-3 §5.8)
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

fn read_sectors(
    hdr: &mut SgIoHdr,
    profile: &LoadedProfile,
    lba: u32,
    count: u32,
    n: u32,
    cmd: &str,
) {
    if !profile.has_disc() {
        hdr.set_check_condition(0x02, 0x3A, 0x00);
        save_sense(0x02, 0x3A, 0x00);
        log(
            n,
            &format!("{} lba={} count={} -> NOT READY", cmd, lba, count),
        );
        return;
    }

    // `count` is untrusted (READ(12) allows ~4 billion sectors -> multi-TB alloc,
    // OOM-aborting the process). Clamp to the host's declared transfer length.
    let total = (count as usize)
        .saturating_mul(SECTOR_SIZE)
        .min(hdr.dxfer_len as usize);
    let mut data = vec![0u8; total];
    // Whole sectors the (clamped) buffer holds; every copy loop bounds to this
    // so the clamp can never be over-indexed.
    let out_sectors = data.len() / SECTOR_SIZE;

    // First LBA the fixture could not supply, if any. Must NOT be silently
    // zero-filled and returned at GOOD (this repo's worst defect class).
    // u64 so an LBA past u32::MAX logs at its true value, not truncated.
    let mut missing: Option<u64> = None;

    if let Some(disc) = &profile.disc {
        if !disc.sector_map.is_empty() {
            // Sparse sector map: look up each requested sector
            for i in 0..out_sectors {
                // `lba + i` in u64: near-top-of-range reads could otherwise wrap
                // in u32 and serve wrong low-LBA data as the requested high
                // sectors. Treat past-u32::MAX as a miss instead.
                let target_lba64 = lba as u64 + i as u64;
                if target_lba64 > u32::MAX as u64 {
                    missing = missing.or(Some(target_lba64));
                    continue;
                }
                let target_lba = target_lba64 as u32;
                // Guard the source slice: a range claiming a sector the file
                // doesn't actually hold is a corrupt capture, not zeros — miss.
                match lookup_sector(&disc.sector_map, target_lba) {
                    Some(offset) if offset + SECTOR_SIZE <= disc.sectors.len() => {
                        let dst = i * SECTOR_SIZE;
                        data[dst..dst + SECTOR_SIZE]
                            .copy_from_slice(&disc.sectors[offset..offset + SECTOR_SIZE]);
                    }
                    _ => missing = missing.or(Some(u64::from(target_lba))),
                }
            }
        } else if !disc.sectors.is_empty() {
            // Legacy flat dump (LBA = byte offset / SECTOR_SIZE)
            let max_sectors = disc.sectors.len() / SECTOR_SIZE;
            for i in 0..out_sectors {
                // Widen to u64 so `lba + i` cannot overflow, same as the sparse
                // path above (an LBA past u32::MAX is a miss, not a wraparound).
                let sector_lba = lba as u64 + i as u64;
                if sector_lba < max_sectors as u64 {
                    let src_start = sector_lba as usize * SECTOR_SIZE;
                    data[i * SECTOR_SIZE..(i + 1) * SECTOR_SIZE]
                        .copy_from_slice(&disc.sectors[src_start..src_start + SECTOR_SIZE]);
                } else {
                    missing = missing.or(Some(sector_lba));
                }
            }
        } else if !disc.sector_data.is_empty() {
            // Single-pattern fixture: every LBA is the same 2048-byte sector.
            // A sector_data.bin shorter than one sector is a truncated capture,
            // not a legitimately short sector — treat it as a miss, not padding.
            if disc.sector_data.len() < SECTOR_SIZE {
                missing = Some(u64::from(lba));
            } else {
                for i in 0..out_sectors {
                    data[i * SECTOR_SIZE..(i + 1) * SECTOR_SIZE]
                        .copy_from_slice(&disc.sector_data[..SECTOR_SIZE]);
                }
            }
        } else if out_sectors > 0 {
            // A disc directory with no sector source at all: sectors.bin absent,
            // unreadable (profile::read_bin logs why), or empty. Every requested
            // sector is unknown.
            missing = Some(u64::from(lba));
        }
    }

    if let Some(bad_lba) = missing {
        // MEDIUM ERROR / UNRECOVERED READ ERROR (0x03/0x11), not ILLEGAL REQUEST:
        // the LBA is valid, bdemu just can't recover the data — makes the host
        // retry/degrade/fail like against a real drive, instead of taking zeros.
        hdr.set_check_condition(0x03, 0x11, 0x00);
        save_sense(0x03, 0x11, 0x00);
        log_lazy(n, || {
            format!(
                "{} lba={} count={} -> UNRECOVERED READ ERROR (LBA {} not in capture)",
                cmd, lba, count, bad_lba
            )
        });
        return;
    }

    hdr.write_response(&data);
    log_lazy(n, || {
        format!(
            "{} lba={} count={} ({} bytes)",
            cmd, lba, count, hdr.dxfer_len
        )
    });
}

/// Look up a sector in the sparse sector map using binary search.
/// Returns the byte offset into the sectors data, or None if not captured.
fn lookup_sector(map: &[(u32, u32, usize)], lba: u32) -> Option<usize> {
    let idx = map
        .binary_search_by(|&(start, count, _)| {
            if lba < start {
                std::cmp::Ordering::Greater
            } else if lba as u64 >= start as u64 + count as u64 {
                // u64 to avoid wrapping when start+count is near u32::MAX.
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .ok()?;
    let (start, _, byte_offset) = map[idx];
    Some(byte_offset + (lba - start) as usize * SECTOR_SIZE)
}

// ============================================================================
// 0x3B — WRITE BUFFER (SPC-4 §6.38)
// ============================================================================

fn cmd_write_buffer(hdr: &mut SgIoHdr, n: u32) {
    let mode = hdr.cdb(1) & 0x1F;
    let buf_id = hdr.cdb(2);
    log(
        n,
        &format!(
            "WRITE_BUFFER mode={} buf=0x{:02X} ({} bytes)",
            mode, buf_id, hdr.dxfer_len
        ),
    );
}

// 0x3C — READ BUFFER (SPC-4 §6.7)
// Implements modes 2 (data), 3 (descriptor), 6 (vendor MTK reg, empty-GOOD),
// plus unlock shapes. Mode 0 deliberately NOT implemented (no captured payload).

/// Allocation size for the READ_BUFFER unlock response: host-requested
/// `dxfer_len` (untrusted u32) clamped to 64 bytes, since the unlock reply
/// only populates bytes [0:4] and [12:16] — never an OOM-sized alloc.
fn unlock_resp_len(dxfer_len: u32) -> usize {
    (dxfer_len as usize).min(64)
}

fn cmd_read_buffer(hdr: &mut SgIoHdr, profile: &LoadedProfile, n: u32) {
    let mode = hdr.cdb(1) & 0x1F;
    let buf_id = hdr.cdb(2);

    // The unlock-handshake CDB shapes are owned by the unlocker crate, not
    // open-coded here.
    let is_unlock = freemkv_unlock::ld::is_unlock_read_buffer(mode, buf_id);

    if is_unlock {
        // Look up drive signature from libfreemkv bundled profiles.
        // Match the emulated drive's INQUIRY fields against the profile database.
        let sig = lookup_unlock_signature(profile, n);
        // `dxfer_len` is untrusted; an unclamped alloc could OOM-abort the
        // emulator. The response only ever populates [0:4] and [12:16], so
        // clamp to 64 bytes (write_response truncates to dxfer_len anyway).
        let mut resp = vec![0u8; unlock_resp_len(hdr.dxfer_len)];
        if resp.len() >= 16 {
            // Signature at [0:4] from profile database
            resp[0..4].copy_from_slice(&sig);
            // 4-byte verification marker at [12:16] — owned by the unlocker.
            resp[12..16].copy_from_slice(freemkv_unlock::ld::UNLOCK_MARKER);
        }
        hdr.write_response(&resp);
        log(
            n,
            &format!(
                "READ_BUFFER mode={} buf=0x{:02X} -> UNLOCK (sig={:02x}{:02x}{:02x}{:02x})",
                mode, buf_id, sig[0], sig[1], sig[2], sig[3]
            ),
        );
        return;
    }

    match mode {
        // Mode 2: Data — look up by buffer ID from profile
        2 => {
            if let Some(data) = profile.find_read_buf(buf_id) {
                hdr.write_response(data);
                log(
                    n,
                    &format!(
                        "READ_BUFFER mode=2 buf=0x{:02X} ({} bytes)",
                        buf_id, hdr.dxfer_len
                    ),
                );
            } else {
                hdr.set_check_condition(0x05, 0x24, 0x00); // ILLEGAL REQUEST
                save_sense(0x05, 0x24, 0x00);
                log(
                    n,
                    &format!("READ_BUFFER mode=2 buf=0x{:02X} -> ILLEGAL REQUEST", buf_id),
                );
            }
        }
        // Mode 3: Descriptor — return buffer capacity
        3 => {
            let resp = [0u8; 4];
            hdr.write_response(&resp);
            log(
                n,
                &format!(
                    "READ_BUFFER mode=3 buf=0x{:02X} ({} bytes)",
                    buf_id, hdr.dxfer_len
                ),
            );
        }
        // Mode 6: Vendor-specific (MTK register read). Empty-GOOD is DELIBERATE
        // (unlocker's register-read flow, not an unimplemented mode) — pinned by
        // read_buffer_mode6_is_deliberate_empty_good so it isn't "fixed" away.
        6 => {
            hdr.write_response(&[]);
            log(
                n,
                &format!(
                    "READ_BUFFER mode=6 buf=0x{:02X} ({} bytes)",
                    buf_id, hdr.dxfer_len
                ),
            );
        }
        // Every other mode (including mode 0) is unimplemented. Empty-GOOD would
        // lie ("buffer is legitimately zero bytes"); ILLEGAL REQUEST says the
        // truth — bdemu does not implement this mode.
        _ => {
            hdr.set_check_condition(0x05, 0x24, 0x00);
            save_sense(0x05, 0x24, 0x00);
            log(
                n,
                &format!(
                    "READ_BUFFER mode={} buf=0x{:02X} -> ILLEGAL REQUEST (mode not implemented)",
                    mode, buf_id
                ),
            );
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

    if let Some(disc) = &profile.disc
        && !disc.toc.is_empty()
    {
        hdr.write_response(&disc.toc);
        log(n, &format!("READ_TOC ({} bytes) from disc", hdr.dxfer_len));
        return;
    }

    // Default minimal TOC
    let mut resp = [0u8; 12];
    resp[0] = 0x00;
    resp[1] = 0x0A; // data length
    resp[2] = 0x01; // first track
    resp[3] = 0x01; // last track
    resp[5] = 0x14; // ADR=1, CONTROL=4 (data)
    resp[6] = 0x01; // track 1
    hdr.write_response(&resp);
    log(n, &format!("READ_TOC ({} bytes) default", hdr.dxfer_len));
}

// 0x46 — GET CONFIGURATION (MMC-6 §6.6)
// CDB[1] bits0-1 RT: 0=all features, 1=current only, 2=single (CDB[2-3]).
// Response: 8-byte header (Data Length, Current Profile) + Feature Descriptors.

fn cmd_get_configuration(hdr: &mut SgIoHdr, profile: &LoadedProfile, n: u32) {
    let rt = hdr.cdb(1) & 0x03;
    let feat = u16::from_be_bytes([hdr.cdb(2), hdr.cdb(3)]);

    match rt {
        // RT=2: return single feature
        2 => {
            if let Some(feat_data) = profile.find_feature(feat) {
                // try_from instead of `as u32`: an oversized payload would
                // otherwise silently truncate the Data Length field. Bounded
                // profile files can't reach 4 GB, but be correct-by-construction.
                let data_len = u32::try_from(4 + feat_data.len()).unwrap_or(u32::MAX);
                let mut resp = vec![0u8; 8 + feat_data.len()];
                resp[0..4].copy_from_slice(&data_len.to_be_bytes());
                resp[6..8].copy_from_slice(&profile.current_profile.to_be_bytes());
                resp[8..].copy_from_slice(feat_data);
                hdr.write_response(&resp);
                log(
                    n,
                    &format!("GET_CONFIG 0x{:04X} rt=2 ({} bytes)", feat, hdr.dxfer_len),
                );
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
            // MMC-6 §6.6 requires ascending Feature Code order; profile.features
            // is sorted at load time, so iteration already satisfies this.
            // Assert the invariant so a future loader change can't regress it.
            debug_assert!(
                profile.features.windows(2).all(|w| w[0].0 <= w[1].0),
                "profile.features must be sorted ascending by feature code (MMC-6 §6.6)"
            );
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
            // try_from instead of `as u32`: avoid silently truncating the Data
            // Length field on an oversized body (unreachable for bounded
            // profiles, but correct-by-construction).
            let data_len = u32::try_from(4 + body.len()).unwrap_or(u32::MAX);
            let mut resp = vec![0u8; 8 + body.len()];
            resp[0..4].copy_from_slice(&data_len.to_be_bytes());
            resp[6..8].copy_from_slice(&profile.current_profile.to_be_bytes());
            if !body.is_empty() {
                resp[8..].copy_from_slice(&body);
            }
            hdr.write_response(&resp);
            log(
                n,
                &format!(
                    "GET_CONFIG 0x{:04X} rt={} ({} bytes, {} features)",
                    feat,
                    rt,
                    hdr.dxfer_len,
                    profile.features.len()
                ),
            );
        }
    }
}

// 0x4A — GET EVENT STATUS NOTIFICATION (MMC-6 §6.5)
// Polled mode only (CDB[1] bit0; async unsupported). CDB[4] class bitmap:
// bit4=Media event, bit2=Power Management, bit1=Operational Change.

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
        resp[0] = 0x00;
        resp[1] = 0x06; // event descriptor length
        resp[2] = 0x04; // notification class = media
        resp[3] = 0x10; // supported classes = media

        if profile.has_disc() {
            // NewMedia (0x02) is an edge event, not steady-state: report it ONCE
            // on the first poll after insertion, then NoChange — repeating it
            // makes hosts re-enumerate on every poll, risking re-mount loops.
            if NEW_MEDIA_EVENT.swap(false, Ordering::SeqCst) {
                resp[4] = 0x02; // event code: NewMedia (edge, one-shot)
            } else {
                resp[4] = 0x00; // event code: NoChange (steady state)
            }
            resp[5] = 0x02; // media status: door closed, media present
        } else {
            resp[4] = 0x00; // no event
            resp[5] = 0x00; // door closed, no media
        }
        hdr.write_response(&resp);
        log(
            n,
            &format!("GET_EVENT_STATUS media (disc={})", profile.has_disc()),
        );
    } else {
        // Host did not request the media class. Per MMC-6 the Supported Event
        // Classes field reflects device capability regardless of the request
        // bitmap, so still advertise media (bit 4) here.
        let mut resp = [0u8; 4];
        resp[0] = 0x00;
        resp[1] = 0x02;
        resp[2] = 0x00; // NEA=0, no event for the requested class
        resp[3] = 0x10; // supported classes = media
        hdr.write_response(&resp);
        log(
            n,
            "GET_EVENT_STATUS -> no requested class (media supported)",
        );
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

    if let Some(disc) = &profile.disc
        && !disc.disc_info.is_empty()
    {
        hdr.write_response(&disc.disc_info);
        log(
            n,
            &format!("READ_DISC_INFO ({} bytes) from disc", disc.disc_info.len()),
        );
        return;
    }

    // Default
    let mut resp = [0u8; 34];
    resp[0] = 0x00;
    resp[1] = 0x20;
    resp[2] = 0x0E;
    resp[3] = 0x01;
    resp[4] = 0x01;
    resp[5] = 0x01;
    resp[6] = 0x01;
    resp[7] = 0x20;
    hdr.write_response(&resp);
    log(
        n,
        &format!("READ_DISC_INFO ({} bytes) default", hdr.dxfer_len),
    );
}

// 0x5A — MODE SENSE(10) (SPC-4 §6.11)
// CDB[2] bits5-0=Page Code, bits7-6=PC (page control). Response: Mode
// Parameter Header(8) + Block Descriptor(s) + Mode Page(s).

fn cmd_mode_sense(hdr: &mut SgIoHdr, profile: &LoadedProfile, n: u32) {
    let page = hdr.cdb(2) & 0x3F;
    // PC (page control): only PC=0 (current) is modeled — the rip path never
    // issues PC=1/2/3. Surface non-zero PC in the trace so it's diagnosable
    // rather than silently treated as current values.
    let pc = (hdr.cdb(2) >> 6) & 0x03;
    if pc != 0 {
        log(
            n,
            &format!(
                "MODE_SENSE page 0x{page:02X} pc={pc} -> returning current values (only PC=0 emulated)"
            ),
        );
    }

    match page {
        // Page 0x2A: CD/DVD Capabilities and Mechanical Status
        0x2A => {
            if !profile.mode_2a.is_empty() {
                hdr.write_response(&profile.mode_2a);
            } else {
                // Minimal capabilities page
                let mut resp = [0u8; 28];
                resp[0] = 0x00;
                resp[1] = 0x1A; // data length
                // Page 2A header
                resp[8] = 0x2A;
                resp[9] = 0x12; // page code, page length
                resp[10] = 0x3F;
                resp[11] = 0x37; // read capabilities
                hdr.write_response(&resp);
            }
            log(
                n,
                &format!("MODE_SENSE page 0x2A ({} bytes)", hdr.dxfer_len),
            );
        }
        // Page 0x3F: All pages
        0x3F => {
            if !profile.mode_2a.is_empty() {
                hdr.write_response(&profile.mode_2a);
            } else {
                hdr.write_response(&[]);
            }
            log(
                n,
                &format!("MODE_SENSE page 0x3F (all) ({} bytes)", hdr.dxfer_len),
            );
        }
        _ => {
            // Unsupported page
            hdr.set_check_condition(0x05, 0x24, 0x00);
            save_sense(0x05, 0x24, 0x00);
            log(
                n,
                &format!("MODE_SENSE page 0x{:02X} -> ILLEGAL REQUEST", page),
            );
        }
    }
}

// 0xA3 — SEND KEY (MMC-6 §6.31)
// AACS authentication — just acknowledge for now

fn cmd_send_key(hdr: &mut SgIoHdr, n: u32) {
    let key_class = hdr.cdb(7);
    let key_format = hdr.cdb(10) & 0x3F;
    log(
        n,
        &format!(
            "SEND_KEY class=0x{:02X} format={} ({} bytes)",
            key_class, key_format, hdr.dxfer_len
        ),
    );
}

// 0xA4 — REPORT KEY (MMC-6 §6.25)
// Key class 0x00: DVD CSS/CPPM, 0x02: AACS, 0x08: RPC state

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
            // MMC-6 §6.25: unsupported key class/format must be ILLEGAL REQUEST,
            // not a successful zero-byte exchange — else an AACS probe reads a
            // bogus GOOD. Matches the unsupported-VPD-page / mode-page patterns.
            hdr.set_check_condition(0x05, 0x24, 0x00);
            save_sense(0x05, 0x24, 0x00);
            log(
                n,
                &format!(
                    "REPORT_KEY class=0x{:02X} format={} -> ILLEGAL REQUEST",
                    key_class, key_format
                ),
            );
        }
    }
}

// 0xAD — READ DISC STRUCTURE (MMC-6 §6.23)
// Returns disc physical format information, DI, BCA, etc.

fn cmd_read_disc_structure(hdr: &mut SgIoHdr, profile: &LoadedProfile, n: u32) {
    if !profile.has_disc() {
        hdr.set_check_condition(0x02, 0x3A, 0x00);
        save_sense(0x02, 0x3A, 0x00);
        log(n, "READ_DISC_STRUCTURE -> NOT READY");
        return;
    }

    let format = hdr.cdb(7);

    if let Some(disc) = &profile.disc
        && let Some(data) = disc.disc_structures.get(&format)
    {
        hdr.write_response(data);
        log(
            n,
            &format!(
                "READ_DISC_STRUCTURE format={} ({} bytes) from disc",
                format,
                data.len()
            ),
        );
        return;
    }

    // Format not available — return empty header (not an error, just no data)
    let mut resp = [0u8; 4];
    resp[0] = 0x00;
    resp[1] = 0x02;
    hdr.write_response(&resp);
    log(
        n,
        &format!("READ_DISC_STRUCTURE format={} -> empty", format),
    );
}

// 0xBB — SET CD SPEED (MMC-6 §6.29)
// Pioneer uses this for speed control via vendor extension

fn cmd_set_cd_speed(hdr: &mut SgIoHdr, n: u32) {
    let read_speed = u16::from_be_bytes([hdr.cdb(2), hdr.cdb(3)]);
    let write_speed = u16::from_be_bytes([hdr.cdb(4), hdr.cdb(5)]);
    log(
        n,
        &format!("SET_CD_SPEED read={} write={}", read_speed, write_speed),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::LoadedProfile;

    fn empty_profile() -> LoadedProfile {
        LoadedProfile {
            name: String::new(),
            inquiry: vec![0u8; 96],
            current_profile: 0x0043,
            features: Vec::new(),
            rpc_state: Vec::new(),
            read_bufs: Vec::new(),
            mode_2a: Vec::new(),
            disc: None,
        }
    }

    /// Build an SgIoHdr over caller-owned CDB + data buffers. Pointers stay
    /// valid for the duration of the borrow.
    fn hdr_for<'a>(cdb: &'a [u8], data: &'a mut [u8], sense: &'a mut [u8]) -> SgIoHdr {
        SgIoHdr {
            interface_id: b'S' as i32,
            dxfer_direction: -3, // SG_DXFER_FROM_DEV
            cmd_len: cdb.len() as u8,
            mx_sb_len: sense.len() as u8,
            iovec_count: 0,
            dxfer_len: data.len() as u32,
            dxferp: data.as_mut_ptr(),
            cmdp: cdb.as_ptr(),
            sbp: sense.as_mut_ptr(),
            timeout: 5000,
            flags: 0,
            pack_id: 0,
            usr_ptr: std::ptr::null_mut(),
            status: 0,
            masked_status: 0,
            msg_status: 0,
            sb_len_wr: 0,
            host_status: 0,
            driver_status: 0,
            resid: 0,
            duration: 0,
            info: 0,
        }
    }

    // A pending UA must survive an intervening INQUIRY (INQUIRY doesn't clear
    // UA per SPC) and be delivered as CHECK CONDITION to the next real
    // command. The old `swap(..) && ...` form consumed the flag on INQUIRY.
    #[test]
    fn unit_attention_survives_inquiry() {
        // Serialize against other tests touching the global flag.
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        let profile = empty_profile();

        // Signal a media change.
        set_media_changed(true);

        // 1. INQUIRY (0x12) must NOT consume the UA: GOOD status, flag stays set.
        let inq_cdb = [0x12u8, 0, 0, 0, 96, 0];
        let mut inq_data = vec![0u8; 96];
        let mut inq_sense = [0u8; 32];
        let mut inq = hdr_for(&inq_cdb, &mut inq_data, &mut inq_sense);
        handle_scsi(&mut inq, &profile);
        assert_eq!(
            inq.status, 0x00,
            "INQUIRY must return GOOD, not consume the UA as CHECK CONDITION"
        );

        // 2. The next non-exempt command (TEST UNIT READY, 0x00) must now see
        //    the UA as CHECK CONDITION / UNIT ATTENTION (0x06).
        let tur_cdb = [0x00u8, 0, 0, 0, 0, 0];
        let mut tur_data = vec![0u8; 0];
        let mut tur_sense = [0u8; 32];
        let mut tur = hdr_for(&tur_cdb, &mut tur_data, &mut tur_sense);
        handle_scsi(&mut tur, &profile);
        assert_eq!(
            tur.status, 0x02,
            "first real command after media change must be CHECK CONDITION"
        );
        // Sense key 0x06 = UNIT ATTENTION, ASC 0x28 = MEDIUM MAY HAVE CHANGED.
        assert_eq!(tur_sense[2], 0x06, "sense key must be UNIT ATTENTION");
        assert_eq!(
            tur_sense[12], 0x28,
            "ASC must be 0x28 (medium may have changed)"
        );

        // 3. The UA is one-shot: a following command sees normal status (here
        //    TEST UNIT READY with no disc -> NOT READY 0x3A, not UA 0x28).
        let mut tur2_data = vec![0u8; 0];
        let mut tur2_sense = [0u8; 32];
        let mut tur2 = hdr_for(&tur_cdb, &mut tur2_data, &mut tur2_sense);
        handle_scsi(&mut tur2, &profile);
        assert_ne!(
            tur2_sense[12], 0x28,
            "UA must be cleared after first delivery (one-shot)"
        );
    }

    /// A profile with a disc loaded, for the disc-present command paths.
    fn disc_profile() -> LoadedProfile {
        let mut p = empty_profile();
        p.disc = Some(crate::profile::DiscProfile {
            toc: Vec::new(),
            capacity: Vec::new(),
            disc_info: Vec::new(),
            disc_structures: std::collections::HashMap::new(),
            sector_data: Vec::new(),
            sectors: Vec::new(),
            sector_map: Vec::new(),
        });
        p
    }

    // GET EVENT must report NewMedia (0x02) only ONCE after a media change
    // (the MMC-6 edge event), then NoChange (0x00) on every later poll —
    // else hosts re-enumerate the disc each poll. resp[5] stays media-present.
    #[test]
    fn get_event_new_media_is_edge_triggered() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        let profile = disc_profile();

        // Arm the NewMedia edge (as a load/eject would via set_media_changed).
        set_media_changed(true);
        // Consume the UNIT ATTENTION that the same media change would surface so
        // it does not pre-empt the GET EVENT handler below.
        MEDIA_CHANGED.store(false, Ordering::SeqCst);

        // GET EVENT STATUS NOTIFICATION, polled, media class requested.
        let gesn_cdb = [0x4Au8, 0x01, 0, 0, 0x10, 0, 0, 0, 8, 0];

        // First poll: NewMedia edge.
        let mut d1 = vec![0u8; 8];
        let mut s1 = [0u8; 32];
        let mut h1 = hdr_for(&gesn_cdb, &mut d1, &mut s1);
        handle_scsi(&mut h1, &profile);
        assert_eq!(
            d1[4], 0x02,
            "first poll after media change must be NewMedia"
        );
        assert_eq!(
            d1[5], 0x02,
            "media status must be door-closed/media-present"
        );

        // Second poll: NoChange — the edge is one-shot.
        let mut d2 = vec![0u8; 8];
        let mut s2 = [0u8; 32];
        let mut h2 = hdr_for(&gesn_cdb, &mut d2, &mut s2);
        handle_scsi(&mut h2, &profile);
        assert_eq!(
            d2[4], 0x00,
            "second poll must report NoChange (edge already delivered)"
        );
        assert_eq!(d2[5], 0x02, "media status must remain media-present");
    }

    /// Per SPC, REQUEST SENSE is read-and-clear: after reporting the latched
    /// sense it must reset it, so a second REQUEST SENSE returns no error rather
    /// than replaying the stale one.
    #[test]
    fn request_sense_clears_after_reporting() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        // Latch a known sense (UNIT ATTENTION / medium may have changed).
        save_sense(0x06, 0x28, 0x00);

        let rs_cdb = [0x03u8, 0, 0, 0, 18, 0];

        // First REQUEST SENSE reports the latched sense.
        let mut d1 = vec![0u8; 18];
        let mut s1 = [0u8; 32];
        let mut h1 = hdr_for(&rs_cdb, &mut d1, &mut s1);
        handle_scsi(&mut h1, &empty_profile());
        assert_eq!(d1[2], 0x06, "sense key must be reported");
        assert_eq!(d1[12], 0x28, "ASC must be reported");

        // Second REQUEST SENSE must report cleared sense (0/0/0).
        let mut d2 = vec![0u8; 18];
        let mut s2 = [0u8; 32];
        let mut h2 = hdr_for(&rs_cdb, &mut d2, &mut s2);
        handle_scsi(&mut h2, &empty_profile());
        assert_eq!(d2[2], 0x00, "sense key must be cleared after first report");
        assert_eq!(d2[12], 0x00, "ASC must be cleared after first report");
        assert_eq!(d2[13], 0x00, "ASCQ must be cleared after first report");
    }

    // A bare REQUEST SENSE (before any other command) must report and clear a
    // pending media-change UA. handle_scsi exempts REQUEST SENSE from the UA-
    // consuming swap, so cmd_request_sense needs its own MEDIA_CHANGED check.
    #[test]
    fn request_sense_reports_pending_media_change_ua() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        // Arm a media change but DON'T issue any non-exempt command first, so the
        // UA was never latched into LAST_SENSE — only MEDIA_CHANGED is set.
        // Also clear any stale latched sense.
        save_sense(0, 0, 0);
        set_media_changed(true);

        let rs_cdb = [0x03u8, 0, 0, 0, 18, 0];

        // First (bare) REQUEST SENSE must report UNIT ATTENTION / 0x28.
        let mut d1 = vec![0u8; 18];
        let mut s1 = [0u8; 32];
        let mut h1 = hdr_for(&rs_cdb, &mut d1, &mut s1);
        handle_scsi(&mut h1, &empty_profile());
        assert_eq!(d1[2], 0x06, "bare REQUEST SENSE must report UNIT ATTENTION");
        assert_eq!(d1[12], 0x28, "ASC must be MEDIUM MAY HAVE CHANGED");

        // The UA is one-shot: a second REQUEST SENSE reports cleared sense.
        let mut d2 = vec![0u8; 18];
        let mut s2 = [0u8; 32];
        let mut h2 = hdr_for(&rs_cdb, &mut d2, &mut s2);
        handle_scsi(&mut h2, &empty_profile());
        assert_eq!(d2[2], 0x00, "UA must be cleared after first report");
        assert_eq!(d2[12], 0x00, "ASC must be cleared after first report");

        // And the MEDIA_CHANGED flag was consumed: a following non-exempt
        // command (TEST UNIT READY) must NOT see a UA.
        let tur_cdb = [0x00u8, 0, 0, 0, 0, 0];
        let mut td = vec![0u8; 0];
        let mut ts = [0u8; 32];
        let mut tur = hdr_for(&tur_cdb, &mut td, &mut ts);
        handle_scsi(&mut tur, &empty_profile());
        assert_ne!(
            ts[12], 0x28,
            "UA consumed by REQUEST SENSE must not resurface on next command"
        );
    }

    // lookup_sector binary-searches the map, which depends on it being sorted
    // ascending by start_lba (parse_sector_file guarantees this). An unsorted,
    // file-order map made binary_search miss present sectors, zero-filling them.
    #[test]
    fn lookup_sector_resolves_out_of_order_capture() {
        let bdsm = {
            let mut v = Vec::new();
            v.extend_from_slice(b"BDSM");
            v.extend_from_slice(&1u32.to_le_bytes());
            v.extend_from_slice(&2u32.to_le_bytes());
            // Range 0 (file order): LBA 1000, 2 sectors.
            v.extend_from_slice(&1000u32.to_le_bytes());
            v.extend_from_slice(&2u32.to_le_bytes());
            // Range 1 (file order): LBA 100, 3 sectors — out of order.
            v.extend_from_slice(&100u32.to_le_bytes());
            v.extend_from_slice(&3u32.to_le_bytes());
            // Payload in file order: 2 sectors for LBA-1000 range, then 3 for LBA-100.
            v.extend_from_slice(&vec![0u8; (2 + 3) * 2048]);
            v
        };
        let (_, map) = crate::profile::parse_sector_file(bdsm);

        // Sorted ascending: LBA-100 range first.
        assert_eq!(map[0].0, 100);
        assert_eq!(map[1].0, 1000);
        let off_100 = map[0].2;
        let off_1000 = map[1].2;

        // Every sector in each present range resolves to the right offset.
        assert_eq!(lookup_sector(&map, 100), Some(off_100));
        assert_eq!(lookup_sector(&map, 101), Some(off_100 + 2048));
        assert_eq!(lookup_sector(&map, 102), Some(off_100 + 2 * 2048));
        assert_eq!(lookup_sector(&map, 1000), Some(off_1000));
        assert_eq!(lookup_sector(&map, 1001), Some(off_1000 + 2048));

        // Sectors outside any range are absent (zero-filled by READ).
        assert_eq!(lookup_sector(&map, 99), None);
        assert_eq!(lookup_sector(&map, 103), None);
        assert_eq!(lookup_sector(&map, 500), None);
        assert_eq!(lookup_sector(&map, 1002), None);
    }

    // The unlock response must not size its allocation from raw, untrusted
    // `dxfer_len` — a hostile huge value would OOM-abort. Clamped to 64 bytes
    // (the reply only touches [0:4]/[12:16]), while small requests pass through.
    #[test]
    fn unlock_resp_len_clamps_hostile_dxfer_len() {
        // A hostile multi-GB transfer length must be clamped to 64 bytes —
        // never a ~4 GiB allocation.
        assert_eq!(unlock_resp_len(0xFFFF_FFFF), 64);
        assert_eq!(unlock_resp_len(1 << 30), 64); // 1 GiB request -> 64
        // The clamp boundary.
        assert_eq!(unlock_resp_len(64), 64);
        assert_eq!(unlock_resp_len(65), 64);
        // Smaller-than-clamp requests pass through unchanged (so a host that
        // asks for fewer than 16 bytes still gets the marker-guard semantics).
        assert_eq!(unlock_resp_len(16), 16);
        assert_eq!(unlock_resp_len(0), 0);
    }

    /// Find an unlock (mode, buf_id) pair the unlocker recognises, without
    /// open-coding the variant CDB bytes here — those internals live in the
    /// freemkv-unlock-ld crate.
    fn an_unlock_mode_and_buf() -> (u8, u8) {
        for mode in 0u8..=0x1F {
            for buf_id in 0u8..=0xFF {
                if freemkv_unlock::ld::is_unlock_read_buffer(mode, buf_id) {
                    return (mode, buf_id);
                }
            }
        }
        panic!("the unlocker must recognise at least one unlock READ_BUFFER CDB");
    }

    // End-to-end: an unlock READ_BUFFER with a normal 64-byte transfer
    // produces the unlock marker at [12:16], confirming the clamped path
    // still serves a real request (dxfer_len here equals the host buffer).
    #[test]
    fn read_buffer_unlock_writes_marker() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        let profile = empty_profile();
        // Unlock READ_BUFFER (0x3C); mode = cdb[1]&0x1F, buf = cdb[2], both
        // sourced from the unlocker's public seam rather than hardcoded.
        let (mode, buf_id) = an_unlock_mode_and_buf();
        let cdb = [0x3Cu8, mode, buf_id, 0, 0, 0, 0, 0, 0, 0];
        let mut data = vec![0u8; 64];
        let mut sense = [0u8; 32];
        let mut hdr = hdr_for(&cdb, &mut data, &mut sense);

        handle_scsi(&mut hdr, &profile);

        assert_eq!(
            &data[12..16],
            freemkv_unlock::ld::UNLOCK_MARKER,
            "unlock marker must be written"
        );
    }
    // ------------------------------------------------------------------
    // READ(10) / READ(12) — the miss-vs-hit distinction
    // ------------------------------------------------------------------

    /// Build a profile whose disc carries a BDSM sparse capture of `ranges`,
    /// all bytes set to `fill`. Goes through the real `parse_sector_file` so
    /// the map matches what an actual captured `sectors.bin` would produce.
    fn sparse_disc_profile(ranges: &[(u32, u32)], fill: u8) -> LoadedProfile {
        let mut file = Vec::new();
        file.extend_from_slice(b"BDSM");
        file.extend_from_slice(&1u32.to_le_bytes());
        file.extend_from_slice(&(ranges.len() as u32).to_le_bytes());
        for (start, count) in ranges {
            file.extend_from_slice(&start.to_le_bytes());
            file.extend_from_slice(&count.to_le_bytes());
        }
        let sectors: u32 = ranges.iter().map(|(_, c)| c).sum();
        file.extend(std::iter::repeat_n(fill, sectors as usize * 2048));

        let (sectors, sector_map) = crate::profile::parse_sector_file(file);
        assert!(!sector_map.is_empty(), "fixture must parse as sparse BDSM");
        let mut p = disc_profile();
        if let Some(d) = p.disc.as_mut() {
            d.sectors = sectors;
            d.sector_map = sector_map;
        }
        p
    }

    /// A profile whose disc holds a legacy flat dump of `sector_count` sectors.
    fn flat_disc_profile(sector_count: usize, fill: u8) -> LoadedProfile {
        let mut p = disc_profile();
        if let Some(d) = p.disc.as_mut() {
            d.sectors = vec![fill; sector_count * 2048];
        }
        p
    }

    fn read10_cdb(lba: u32, count: u16) -> [u8; 10] {
        let l = lba.to_be_bytes();
        let c = count.to_be_bytes();
        [0x28, 0, l[0], l[1], l[2], l[3], 0, c[0], c[1], 0]
    }

    fn read12_cdb(lba: u32, count: u32) -> [u8; 12] {
        let l = lba.to_be_bytes();
        let c = count.to_be_bytes();
        [
            0xA8, 0, l[0], l[1], l[2], l[3], c[0], c[1], c[2], c[3], 0, 0,
        ]
    }

    /// Issue one READ through the real dispatcher and return (status, data, sense).
    fn run_read(profile: &LoadedProfile, cdb: &[u8], sectors: usize) -> (u8, Vec<u8>, [u8; 32]) {
        let mut data = vec![0xEEu8; sectors * 2048];
        let mut sense = [0u8; 32];
        let mut hdr = hdr_for(cdb, &mut data, &mut sense);
        handle_scsi(&mut hdr, profile);
        let status = hdr.status;
        (status, data, sense)
    }

    // A READ the fixture cannot satisfy must not return GOOD with zero-filled
    // data. Catches the mutation that restores a single fall-through
    // `write_response(&data)` for all branches, masking a missing sectors.bin as real disc content.
    #[test]
    fn read10_with_no_captured_sectors_is_check_condition_not_zeros() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        set_media_changed(false);

        // A disc directory with no sectors.bin at all: every sector source empty.
        let profile = disc_profile();
        let (status, data, sense) = run_read(&profile, &read10_cdb(0, 1), 1);

        assert_eq!(
            status, 0x02,
            "a read with nothing captured must be CHECK CONDITION, not GOOD"
        );
        assert_eq!(sense[2], 0x03, "sense key must be MEDIUM ERROR");
        assert_eq!(sense[12], 0x11, "ASC must be UNRECOVERED READ ERROR");
        // The host buffer must not have been overwritten with a plausible-looking
        // all-zero sector.
        assert!(
            data.iter().all(|&b| b == 0xEE),
            "no data may be presented for a failed read"
        );
    }

    // A sector inside a captured BDSM range is authoritative and served at
    // GOOD even when its contents are genuinely zero. Catches an over-broad
    // fix that failed any all-zero read, breaking legitimately blank disc regions.
    #[test]
    fn read10_captured_zero_sector_is_served_as_good() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        set_media_changed(false);

        // Range [100, 102] captured, and the captured bytes are zeros.
        let profile = sparse_disc_profile(&[(100, 3)], 0x00);
        let (status, data, _) = run_read(&profile, &read10_cdb(101, 1), 1);

        assert_eq!(status, 0x00, "a captured sector must be GOOD");
        assert!(
            data.iter().all(|&b| b == 0),
            "captured zeros must be served verbatim"
        );
    }

    /// A hit must still serve the captured bytes byte-for-byte. Catches a fix
    /// that failed reads indiscriminately (the opposite over-correction).
    #[test]
    fn read10_hit_serves_captured_bytes() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        set_media_changed(false);

        let profile = sparse_disc_profile(&[(100, 3)], 0xA5);
        let (status, data, _) = run_read(&profile, &read10_cdb(100, 2), 2);

        assert_eq!(status, 0x00);
        assert!(
            data.iter().all(|&b| b == 0xA5),
            "captured bytes must appear"
        );
    }

    // A sector outside every captured range was never read off the medium,
    // so it must fail even though the disc is loaded and other sectors are
    // present — the everyday case of a sparse map that doesn't cover the LBA.
    #[test]
    fn read10_outside_the_captured_map_is_check_condition() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        set_media_changed(false);

        let profile = sparse_disc_profile(&[(100, 3)], 0xA5);
        let (status, data, sense) = run_read(&profile, &read10_cdb(500, 1), 1);

        assert_eq!(status, 0x02, "uncaptured LBA must be CHECK CONDITION");
        assert_eq!(sense[2], 0x03);
        assert_eq!(sense[12], 0x11);
        assert!(data.iter().all(|&b| b == 0xEE), "no data for a failed read");
    }

    /// A multi-sector read that straddles the end of a captured range fails as a
    /// whole: fixed-format sense cannot express "the first sector was fine", and
    /// silently zero-filling the tail is exactly the defect.
    #[test]
    fn read10_partially_captured_span_fails() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        set_media_changed(false);

        let profile = sparse_disc_profile(&[(100, 2)], 0xA5);
        // LBAs 100,101 are captured; 102 is not.
        let (status, _, sense) = run_read(&profile, &read10_cdb(100, 3), 3);

        assert_eq!(status, 0x02, "a partially-captured span must fail");
        assert_eq!(sense[12], 0x11);
    }

    /// READ(12) shares the miss path with READ(10) — its CDB parses count from a
    /// different field, so it gets its own coverage. Catches a fix applied to only
    /// one of the two opcodes.
    #[test]
    fn read12_miss_and_hit_match_read10() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        set_media_changed(false);

        let profile = sparse_disc_profile(&[(7, 1)], 0x5A);

        let (status, data, _) = run_read(&profile, &read12_cdb(7, 1), 1);
        assert_eq!(status, 0x00, "READ(12) hit must be GOOD");
        assert!(data.iter().all(|&b| b == 0x5A));

        let (status, _, sense) = run_read(&profile, &read12_cdb(8, 1), 1);
        assert_eq!(status, 0x02, "READ(12) miss must be CHECK CONDITION");
        assert_eq!(sense[12], 0x11);
    }

    /// The legacy flat dump has the same two cases: inside the file is captured
    /// data, past the end was never captured.
    #[test]
    fn read10_flat_dump_past_end_is_check_condition() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        set_media_changed(false);

        let profile = flat_disc_profile(2, 0x11);

        let (status, data, _) = run_read(&profile, &read10_cdb(1, 1), 1);
        assert_eq!(status, 0x00, "an in-range flat sector must be GOOD");
        assert!(data.iter().all(|&b| b == 0x11));

        let (status, _, sense) = run_read(&profile, &read10_cdb(2, 1), 1);
        assert_eq!(status, 0x02, "past the end of a flat dump must fail");
        assert_eq!(sense[12], 0x11);
    }

    // A failed read must also be reportable through REQUEST SENSE: the miss
    // path uses the same `save_sense` seam as every other error path.
    // Catches a fix that set the header status but forgot to latch the sense.
    #[test]
    fn read_miss_is_visible_to_request_sense() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        set_media_changed(false);

        let profile = sparse_disc_profile(&[(100, 1)], 0xA5);
        let _ = run_read(&profile, &read10_cdb(999, 1), 1);

        let rs_cdb = [0x03u8, 0, 0, 0, 18, 0];
        let mut rs_data = vec![0u8; 18];
        let mut rs_sense = [0u8; 32];
        let mut rs = hdr_for(&rs_cdb, &mut rs_data, &mut rs_sense);
        handle_scsi(&mut rs, &profile);
        assert_eq!(rs_data[2], 0x03, "REQUEST SENSE must report MEDIUM ERROR");
        assert_eq!(rs_data[12], 0x11, "…with UNRECOVERED READ ERROR");
    }

    // An unimplemented READ BUFFER mode (e.g. mode 0) must be ILLEGAL
    // REQUEST, not empty GOOD. Catches the mutation that restores
    // `write_response(&[])` in the catch-all arm, falsely claiming a zero-byte buffer.
    #[test]
    fn read_buffer_unimplemented_mode_is_illegal_request() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        set_media_changed(false);

        let profile = empty_profile();
        // Mode 0, with a buffer id the unlocker does not claim.
        let mut cdb = [0x3Cu8, 0, 0x00, 0, 0, 0, 0, 0, 64, 0];
        let mut buf_id = 0u8;
        while freemkv_unlock::ld::is_unlock_read_buffer(0, buf_id) {
            buf_id += 1;
        }
        cdb[2] = buf_id;

        let mut data = vec![0u8; 64];
        let mut sense = [0u8; 32];
        let mut hdr = hdr_for(&cdb, &mut data, &mut sense);
        handle_scsi(&mut hdr, &profile);

        assert_eq!(
            hdr.status, 0x02,
            "unimplemented mode must be CHECK CONDITION"
        );
        assert_eq!(sense[2], 0x05, "sense key must be ILLEGAL REQUEST");
        assert_eq!(sense[12], 0x24, "ASC must be INVALID FIELD IN CDB");
    }

    // The sparse READ path computed `lba.wrapping_add(i)`, so a READ(12) near
    // the top of u32 LBA space wrapped its out-of-range tail back to LBA 0/1
    // and served that as GOOD. LBAs past u32::MAX must miss. Catches wrapping_add.
    #[test]
    fn read12_lba_past_u32_max_is_a_miss_not_a_wrapped_hit() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        set_media_changed(false);

        let profile = sparse_disc_profile(&[(0, 2), (0xFFFF_FFFE, 2)], 0xA5);
        let (status, data, sense) = run_read(&profile, &read12_cdb(0xFFFF_FFFE, 4), 4);

        assert_eq!(
            status, 0x02,
            "an LBA past u32::MAX must be a miss, not wrapped to a low captured sector"
        );
        assert_eq!(sense[2], 0x03, "sense key must be MEDIUM ERROR");
        assert_eq!(sense[12], 0x11, "ASC must be UNRECOVERED READ ERROR");
        assert!(
            data.iter().all(|&b| b == 0xEE),
            "no data may be served for a read whose tail wraps past u32::MAX"
        );
    }

    // A `sector_data.bin` shorter than one 2048-byte sector is a truncated
    // capture. The single-pattern branch used to copy the short bytes and
    // zero-fill the tail, returning GOOD like a genuine hit; it must now miss.
    #[test]
    fn read10_short_sector_data_pattern_is_a_miss_not_zero_padded() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        set_media_changed(false);

        let mut profile = disc_profile();
        if let Some(d) = profile.disc.as_mut() {
            d.sector_data = vec![0xC3u8; 100]; // shorter than a full sector
        }
        let (status, data, sense) = run_read(&profile, &read10_cdb(0, 1), 1);

        assert_eq!(
            status, 0x02,
            "a truncated single-pattern sector must fail, not serve real+zero bytes at GOOD"
        );
        assert_eq!(sense[12], 0x11, "ASC must be UNRECOVERED READ ERROR");
        assert!(
            data.iter().all(|&b| b == 0xEE),
            "no partial-then-zero data may be served for the short pattern"
        );
    }

    /// The other half: a `sector_data.bin` that IS a whole sector is a legitimate
    /// single-pattern fixture and must still be served at GOOD, byte-for-byte, on
    /// every LBA. Catches an over-correction that failed the pattern branch outright.
    #[test]
    fn read10_full_sector_data_pattern_is_served_as_good() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        set_media_changed(false);

        let mut profile = disc_profile();
        if let Some(d) = profile.disc.as_mut() {
            d.sector_data = vec![0x7Eu8; 2048];
        }
        let (status, data, _) = run_read(&profile, &read10_cdb(5, 1), 1);

        assert_eq!(status, 0x00, "a whole-sector pattern must be GOOD");
        assert!(
            data.iter().all(|&b| b == 0x7E),
            "the captured pattern must be served on every LBA"
        );
    }

    // A hostile sectors.bin with an overlapping BDSM range table must serve
    // NOTHING (miss), never the discarded file's header bytes as sector 0 at
    // GOOD. Catches the mutation that kept file bytes instead of discarding them.
    #[test]
    fn read10_overlapping_capture_serves_nothing_not_header() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        set_media_changed(false);

        // An overlapping BDSM built exactly as a hostile sectors.bin would be.
        let mut file = Vec::new();
        file.extend_from_slice(b"BDSM");
        file.extend_from_slice(&1u32.to_le_bytes()); // version
        file.extend_from_slice(&2u32.to_le_bytes()); // 2 ranges
        file.extend_from_slice(&100u32.to_le_bytes());
        file.extend_from_slice(&5u32.to_le_bytes()); // LBA 100..105
        file.extend_from_slice(&103u32.to_le_bytes());
        file.extend_from_slice(&3u32.to_le_bytes()); // LBA 103..106 — overlap
        file.extend_from_slice(&vec![0xEEu8; (5 + 3) * 2048]);

        let (sectors, sector_map) = crate::profile::parse_sector_file(file);
        let mut profile = disc_profile();
        if let Some(d) = profile.disc.as_mut() {
            d.sectors = sectors;
            d.sector_map = sector_map;
        }

        let (status, data, sense) = run_read(&profile, &read10_cdb(0, 1), 1);
        assert_eq!(
            status, 0x02,
            "an overlapping capture must miss, not serve the BDSM header at GOOD"
        );
        assert_eq!(sense[12], 0x11, "ASC must be UNRECOVERED READ ERROR");
        assert!(
            data.iter().all(|&b| b == 0xEE),
            "no header bytes may reach the host for a discarded overlapping capture"
        );
    }

    // READ BUFFER mode 6 (MTK register read) is a deliberate empty-GOOD
    // (status GOOD, no payload) and must not be turned into ILLEGAL REQUEST,
    // which would break the MTK handshake. Pins the carve-out against a catch-all mutation.
    #[test]
    fn read_buffer_mode6_is_deliberate_empty_good() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        set_media_changed(false);

        let profile = empty_profile();
        // Mode 6 with a buffer id the unlocker does not claim (so it reaches the
        // mode-6 arm, not the unlock fast-path).
        let mut buf_id = 0u8;
        while freemkv_unlock::ld::is_unlock_read_buffer(6, buf_id) {
            buf_id += 1;
        }
        let cdb = [0x3Cu8, 6, buf_id, 0, 0, 0, 0, 0, 64, 0];
        let mut data = vec![0xEEu8; 64];
        let mut sense = [0u8; 32];
        let mut hdr = hdr_for(&cdb, &mut data, &mut sense);
        handle_scsi(&mut hdr, &profile);

        assert_eq!(
            hdr.status, 0x00,
            "mode 6 is a deliberate empty-GOOD, not CHECK CONDITION"
        );
        assert_eq!(
            hdr.resid, 64,
            "an empty response leaves the whole buffer residual"
        );
        assert!(
            data.iter().all(|&b| b == 0),
            "an empty response zero-fills the host buffer"
        );
    }

    // Opcode dispatch coverage — one command per handler through handle_scsi,
    // asserting status and response bytes. All lock TEST_GUARD and clear
    // MEDIA_CHANGED so a stray UNIT ATTENTION can't pre-empt the handler.

    /// Lock the global SCSI state and clear MEDIA_CHANGED for a handler test.
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        let g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        MEDIA_CHANGED.store(false, Ordering::SeqCst);
        NEW_MEDIA_EVENT.store(false, Ordering::SeqCst);
        save_sense(0, 0, 0);
        g
    }

    /// Run one command through the dispatcher; return (status, data, sense).
    fn run(profile: &LoadedProfile, cdb: &[u8], dxfer: usize) -> (u8, Vec<u8>, [u8; 32]) {
        let mut data = vec![0u8; dxfer];
        let mut sense = [0u8; 32];
        let mut hdr = hdr_for(cdb, &mut data, &mut sense);
        handle_scsi(&mut hdr, profile);
        (hdr.status, data, sense)
    }

    // --- 0x12 INQUIRY ---

    #[test]
    fn inquiry_standard_returns_profile_inquiry() {
        let _g = guard();
        let (status, _, _) = run(&empty_profile(), &[0x12, 0x00, 0x00, 0, 96, 0], 96);
        assert_eq!(status, 0x00, "standard INQUIRY must be GOOD");
    }

    #[test]
    fn inquiry_vpd_page_00_lists_supported_pages() {
        let _g = guard();
        let (status, data, _) = run(&empty_profile(), &[0x12, 0x01, 0x00, 0, 8, 0], 8);
        assert_eq!(status, 0x00);
        assert_eq!(data[1], 0x00, "page code echoed");
        assert!(data[..6].contains(&0x80), "page 0x80 must be advertised");
    }

    #[test]
    fn inquiry_vpd_page_80_returns_serial() {
        let _g = guard();
        let mut p = empty_profile();
        // Feature 0x0108 carries the serial after its 4-byte descriptor header.
        p.features
            .push((0x0108, vec![0x01, 0x08, 0x00, 0x00, b'S', b'N', b'9']));
        let (status, data, _) = run(&p, &[0x12, 0x01, 0x80, 0, 16, 0], 16);
        assert_eq!(status, 0x00);
        assert_eq!(data[1], 0x80, "page code");
        assert_eq!(data[3], 3, "serial length");
        assert_eq!(&data[4..7], b"SN9");
    }

    #[test]
    fn inquiry_vpd_unsupported_page_is_illegal_request() {
        let _g = guard();
        let (status, _, sense) = run(&empty_profile(), &[0x12, 0x01, 0x83, 0, 8, 0], 8);
        assert_eq!(status, 0x02);
        assert_eq!(sense[2], 0x05, "ILLEGAL REQUEST");
        assert_eq!(sense[12], 0x24);
    }

    // --- 0x25 READ CAPACITY ---

    #[test]
    fn read_capacity_from_disc() {
        let _g = guard();
        let mut p = disc_profile();
        if let Some(d) = p.disc.as_mut() {
            d.capacity = vec![0xDE, 0xAD, 0xBE, 0xEF, 0, 0, 8, 0];
        }
        let (status, data, _) = run(&p, &[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], 8);
        assert_eq!(status, 0x00);
        assert_eq!(&data[..4], &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn read_capacity_no_medium_is_not_ready() {
        let _g = guard();
        let (status, _, sense) = run(&empty_profile(), &[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], 8);
        assert_eq!(status, 0x02);
        assert_eq!(sense[2], 0x02, "NOT READY");
        assert_eq!(sense[12], 0x3A);
    }

    #[test]
    fn read_capacity_default_when_no_capacity_captured() {
        let _g = guard();
        let (status, data, _) = run(&disc_profile(), &[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], 8);
        assert_eq!(status, 0x00);
        assert_eq!(
            u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
            12219391,
            "default BD-SL last LBA"
        );
        assert_eq!(
            u32::from_be_bytes([data[4], data[5], data[6], data[7]]),
            SECTOR_SIZE as u32
        );
    }

    // --- 0x46 GET CONFIGURATION ---

    fn profile_with_features() -> LoadedProfile {
        let mut p = empty_profile();
        // 0x0001: current (bit0 of byte 2 set); 0x0002: not current.
        p.features.push((0x0001, vec![0x00, 0x01, 0x01, 0x00]));
        p.features.push((0x0002, vec![0x00, 0x02, 0x00, 0x00]));
        p
    }

    #[test]
    fn get_config_rt0_returns_all_features() {
        let _g = guard();
        let (status, data, _) = run(
            &profile_with_features(),
            &[0x46, 0x00, 0, 0, 0, 0, 0, 0, 64, 0],
            64,
        );
        assert_eq!(status, 0x00);
        // header(8) + both 4-byte feature descriptors = 16 payload bytes.
        let data_len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        assert_eq!(data_len, 4 + 8, "data length = 4 + body(8)");
    }

    #[test]
    fn get_config_rt1_returns_only_current_features() {
        let _g = guard();
        let (status, data, _) = run(
            &profile_with_features(),
            &[0x46, 0x01, 0, 0, 0, 0, 0, 0, 64, 0],
            64,
        );
        assert_eq!(status, 0x00);
        // Only the current feature (4 bytes) survives the rt=1 filter.
        let data_len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        assert_eq!(data_len, 4 + 4, "only current feature in body");
    }

    #[test]
    fn get_config_rt2_single_feature_present() {
        let _g = guard();
        let (status, data, _) = run(
            &profile_with_features(),
            &[0x46, 0x02, 0x00, 0x01, 0, 0, 0, 0, 64, 0],
            64,
        );
        assert_eq!(status, 0x00);
        assert_eq!(
            &data[8..12],
            &[0x00, 0x01, 0x01, 0x00],
            "the single feature"
        );
    }

    #[test]
    fn get_config_rt2_single_feature_absent_returns_header_only() {
        let _g = guard();
        let (status, data, _) = run(
            &profile_with_features(),
            &[0x46, 0x02, 0x00, 0xFF, 0, 0, 0, 0, 64, 0],
            64,
        );
        assert_eq!(status, 0x00);
        let data_len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        assert_eq!(data_len, 4, "absent feature -> header only");
    }

    // --- 0x4A GET EVENT STATUS NOTIFICATION ---

    #[test]
    fn get_event_status_async_unsupported() {
        let _g = guard();
        // polled bit clear.
        let (status, _, sense) = run(&disc_profile(), &[0x4A, 0x00, 0, 0, 0x10, 0, 0, 0, 8, 0], 8);
        assert_eq!(status, 0x02);
        assert_eq!(sense[2], 0x05, "ILLEGAL REQUEST");
        assert_eq!(sense[12], 0x24);
    }

    #[test]
    fn get_event_status_no_media_class_requested() {
        let _g = guard();
        // polled, but class request bitmap does not include media (bit4).
        let (status, data, _) = run(&disc_profile(), &[0x4A, 0x01, 0, 0, 0x00, 0, 0, 0, 4, 0], 4);
        assert_eq!(status, 0x00);
        assert_eq!(data[3], 0x10, "media class still advertised as supported");
    }

    #[test]
    fn get_event_status_no_disc_reports_no_media() {
        let _g = guard();
        let (status, data, _) = run(
            &empty_profile(),
            &[0x4A, 0x01, 0, 0, 0x10, 0, 0, 0, 8, 0],
            8,
        );
        assert_eq!(status, 0x00);
        assert_eq!(data[5], 0x00, "no media present");
    }

    // --- 0x51 READ DISC INFORMATION ---

    #[test]
    fn read_disc_info_not_ready_without_disc() {
        let _g = guard();
        let (status, _, sense) = run(&empty_profile(), &[0x51, 0, 0, 0, 0, 0, 0, 0, 34, 0], 34);
        assert_eq!(status, 0x02);
        assert_eq!(sense[12], 0x3A);
    }

    #[test]
    fn read_disc_info_default_when_disc_present() {
        let _g = guard();
        let (status, data, _) = run(&disc_profile(), &[0x51, 0, 0, 0, 0, 0, 0, 0, 34, 0], 34);
        assert_eq!(status, 0x00);
        assert_eq!(data[1], 0x20, "default disc-info data length");
    }

    #[test]
    fn read_disc_info_from_disc_when_captured() {
        let _g = guard();
        let mut p = disc_profile();
        if let Some(d) = p.disc.as_mut() {
            d.disc_info = vec![0x00, 0x99, 0x0E, 0x01];
        }
        let (status, data, _) = run(&p, &[0x51, 0, 0, 0, 0, 0, 0, 0, 34, 0], 34);
        assert_eq!(status, 0x00);
        assert_eq!(data[1], 0x99, "captured disc info served");
    }

    // --- 0x5A MODE SENSE(10) ---

    #[test]
    fn mode_sense_page_2a_default() {
        let _g = guard();
        let (status, data, _) = run(&empty_profile(), &[0x5A, 0, 0x2A, 0, 0, 0, 0, 0, 28, 0], 28);
        assert_eq!(status, 0x00);
        assert_eq!(data[8], 0x2A, "page 2A code in default page");
    }

    #[test]
    fn mode_sense_page_2a_from_profile() {
        let _g = guard();
        let mut p = empty_profile();
        p.mode_2a = vec![0x00, 0x1A, 0, 0, 0, 0, 0, 0, 0x2A, 0x12];
        let (status, data, _) = run(&p, &[0x5A, 0, 0x2A, 0, 0, 0, 0, 0, 10, 0], 10);
        assert_eq!(status, 0x00);
        assert_eq!(data[9], 0x12, "profile mode_2a served verbatim");
    }

    #[test]
    fn mode_sense_page_3f_all_pages() {
        let _g = guard();
        let mut p = empty_profile();
        p.mode_2a = vec![0x00, 0x1A, 0, 0, 0, 0, 0, 0];
        let (status, _, _) = run(&p, &[0x5A, 0, 0x3F, 0, 0, 0, 0, 0, 8, 0], 8);
        assert_eq!(status, 0x00, "MODE SENSE all-pages must be GOOD");
    }

    #[test]
    fn mode_sense_unsupported_page_is_illegal_request() {
        let _g = guard();
        let (status, _, sense) = run(&empty_profile(), &[0x5A, 0, 0x10, 0, 0, 0, 0, 0, 8, 0], 8);
        assert_eq!(status, 0x02);
        assert_eq!(sense[2], 0x05);
        assert_eq!(sense[12], 0x24);
    }

    // --- 0xA4 REPORT KEY ---

    #[test]
    fn report_key_rpc_state_default() {
        let _g = guard();
        let cdb = [0xA4, 0, 0, 0, 0, 0, 0, 0x08, 0, 0, 0x08, 0];
        let (status, data, _) = run(&empty_profile(), &cdb, 8);
        assert_eq!(status, 0x00);
        assert_eq!(data[4], 0x25, "default RPC state byte");
    }

    #[test]
    fn report_key_unsupported_class_is_illegal_request() {
        let _g = guard();
        // AACS (class 0x02) is not modelled.
        let cdb = [0xA4, 0, 0, 0, 0, 0, 0, 0x02, 0, 0, 0x00, 0];
        let (status, _, sense) = run(&empty_profile(), &cdb, 8);
        assert_eq!(status, 0x02);
        assert_eq!(sense[2], 0x05);
        assert_eq!(sense[12], 0x24);
    }

    // --- 0xAD READ DISC STRUCTURE ---

    #[test]
    fn read_disc_structure_not_ready_without_disc() {
        let _g = guard();
        let cdb = [0xAD, 0, 0, 0, 0, 0, 0, 0x00, 0, 0, 0, 0];
        let (status, _, sense) = run(&empty_profile(), &cdb, 8);
        assert_eq!(status, 0x02);
        assert_eq!(sense[12], 0x3A);
    }

    #[test]
    fn read_disc_structure_from_disc() {
        let _g = guard();
        let mut p = disc_profile();
        if let Some(d) = p.disc.as_mut() {
            d.disc_structures.insert(0x00, vec![0x11, 0x22, 0x33, 0x44]);
        }
        let cdb = [0xAD, 0, 0, 0, 0, 0, 0, 0x00, 0, 0, 0, 0];
        let (status, data, _) = run(&p, &cdb, 4);
        assert_eq!(status, 0x00);
        assert_eq!(&data[..4], &[0x11, 0x22, 0x33, 0x44]);
    }

    #[test]
    fn read_disc_structure_absent_format_returns_empty_header() {
        let _g = guard();
        let cdb = [0xAD, 0, 0, 0, 0, 0, 0, 0x07, 0, 0, 0, 0];
        let (status, data, _) = run(&disc_profile(), &cdb, 4);
        assert_eq!(status, 0x00, "absent format is empty header, not an error");
        assert_eq!(data[1], 0x02, "empty-header length field");
    }

    // --- 0x43 READ TOC ---

    #[test]
    fn read_toc_not_ready_without_disc() {
        let _g = guard();
        let (status, _, sense) = run(&empty_profile(), &[0x43, 0, 0, 0, 0, 0, 0, 0, 12, 0], 12);
        assert_eq!(status, 0x02);
        assert_eq!(sense[12], 0x3A);
    }

    #[test]
    fn read_toc_default_when_disc_present() {
        let _g = guard();
        let (status, data, _) = run(&disc_profile(), &[0x43, 0, 0, 0, 0, 0, 0, 0, 12, 0], 12);
        assert_eq!(status, 0x00);
        assert_eq!(data[2], 0x01, "default first track");
        assert_eq!(data[3], 0x01, "default last track");
    }

    #[test]
    fn read_toc_from_disc_when_captured() {
        let _g = guard();
        let mut p = disc_profile();
        if let Some(d) = p.disc.as_mut() {
            d.toc = vec![0x00, 0x0A, 0x01, 0x05];
        }
        let (status, data, _) = run(&p, &[0x43, 0, 0, 0, 0, 0, 0, 0, 4, 0], 4);
        assert_eq!(status, 0x00);
        assert_eq!(data[3], 0x05, "captured TOC served");
    }

    // --- 0x00 TEST UNIT READY ---

    #[test]
    fn test_unit_ready_good_with_disc() {
        let _g = guard();
        let (status, _, _) = run(&disc_profile(), &[0x00, 0, 0, 0, 0, 0], 0);
        assert_eq!(status, 0x00, "TEST UNIT READY with a disc is GOOD");
    }

    #[test]
    fn test_unit_ready_not_ready_without_disc() {
        let _g = guard();
        let (status, _, sense) = run(&empty_profile(), &[0x00, 0, 0, 0, 0, 0], 0);
        assert_eq!(status, 0x02);
        assert_eq!(sense[12], 0x3A);
    }

    // --- 0x1B START STOP UNIT ---

    #[test]
    fn start_stop_unit_variants_are_good() {
        let _g = guard();
        for cdb4 in [0x00u8, 0x01, 0x02, 0x03] {
            let (status, _, _) = run(&empty_profile(), &[0x1B, 0, 0, 0, cdb4, 0], 0);
            assert_eq!(
                status, 0x00,
                "START STOP UNIT cdb[4]={cdb4:#x} must be GOOD"
            );
        }
    }

    // --- 0x1E PREVENT ALLOW MEDIUM REMOVAL ---

    #[test]
    fn prevent_allow_removal_is_good() {
        let _g = guard();
        let (status, _, _) = run(&empty_profile(), &[0x1E, 0, 0, 0, 0x01, 0], 0);
        assert_eq!(status, 0x00);
    }

    // --- 0x3C READ BUFFER data/descriptor modes ---

    #[test]
    fn read_buffer_mode2_serves_profile_buffer() {
        let _g = guard();
        let mut p = empty_profile();
        p.read_bufs.push((0x02, vec![0xAB, 0xCD]));
        // buf id 0x02, ensure it is not an unlock shape.
        assert!(!freemkv_unlock::ld::is_unlock_read_buffer(2, 0x02));
        let cdb = [0x3C, 0x02, 0x02, 0, 0, 0, 0, 0, 2, 0];
        let (status, data, _) = run(&p, &cdb, 2);
        assert_eq!(status, 0x00);
        assert_eq!(&data[..2], &[0xAB, 0xCD]);
    }

    #[test]
    fn read_buffer_mode2_missing_buffer_is_illegal_request() {
        let _g = guard();
        let cdb = [0x3C, 0x02, 0x7E, 0, 0, 0, 0, 0, 8, 0];
        let (status, _, sense) = run(&empty_profile(), &cdb, 8);
        assert_eq!(status, 0x02);
        assert_eq!(sense[12], 0x24);
    }

    #[test]
    fn read_buffer_mode3_returns_descriptor() {
        let _g = guard();
        let cdb = [0x3C, 0x03, 0x00, 0, 0, 0, 0, 0, 4, 0];
        let (status, _, _) = run(&empty_profile(), &cdb, 4);
        assert_eq!(status, 0x00, "descriptor mode is GOOD");
    }

    // --- Ancillary GOOD-status opcodes and the unhandled arm ---

    #[test]
    fn write_buffer_is_good() {
        let _g = guard();
        let (status, _, _) = run(&empty_profile(), &[0x3B, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0);
        assert_eq!(status, 0x00);
    }

    #[test]
    fn send_key_is_good() {
        let _g = guard();
        let cdb = [0xA3, 0, 0, 0, 0, 0, 0, 0x02, 0, 0, 0x01, 0];
        let (status, _, _) = run(&empty_profile(), &cdb, 0);
        assert_eq!(status, 0x00);
    }

    #[test]
    fn set_cd_speed_is_good() {
        let _g = guard();
        let cdb = [0xBB, 0, 0x10, 0x00, 0x10, 0x00, 0, 0, 0, 0, 0, 0];
        let (status, _, _) = run(&empty_profile(), &cdb, 0);
        assert_eq!(status, 0x00);
    }

    #[test]
    fn unhandled_opcode_is_illegal_request() {
        let _g = guard();
        // 0xFF is not dispatched anywhere.
        let (status, _, sense) = run(&empty_profile(), &[0xFF, 0, 0, 0, 0, 0], 0);
        assert_eq!(status, 0x02);
        assert_eq!(sense[2], 0x05, "ILLEGAL REQUEST");
        assert_eq!(sense[12], 0x20, "INVALID COMMAND OPERATION CODE");
    }
}
