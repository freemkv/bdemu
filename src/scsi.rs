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

    // A READ of >=1 sector whose clamped transfer holds <1 whole sector skips
    // every miss loop below and would fall through to write_response with zeros
    // at GOOD — the zero-fill-at-GOOD defect. Flag the first LBA as missing.
    if count > 0 && out_sectors == 0 {
        missing = Some(u64::from(lba));
    }

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
/// only populates bytes `[0:4]` and `[12:16]` — never an OOM-sized alloc.
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
#[path = "scsi_tests.rs"]
mod tests;
