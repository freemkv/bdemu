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

// A READ of count>=1 but a sub-sector dxfer_len clamps `total` below one
// sector, so out_sectors==0 and every miss loop is skipped. Without the
// guard this returned zeros at GOOD; it must be UNRECOVERED READ ERROR.
#[test]
fn read10_subsector_dxfer_len_is_check_condition_not_zeros() {
    let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    set_media_changed(false);

    // Even a captured LBA must fail here: fewer than 2048 bytes can't carry
    // a whole sector, so there is nothing legitimate to return.
    let profile = sparse_disc_profile(&[(100, 3)], 0xA5);
    let cdb = read10_cdb(100, 1);
    let mut data = vec![0xEEu8; 1024]; // sub-sector transfer length
    let mut sense = [0u8; 32];
    let mut hdr = hdr_for(&cdb, &mut data, &mut sense);
    handle_scsi(&mut hdr, &profile);

    assert_eq!(
        hdr.status, 0x02,
        "a sub-sector-length read must be CHECK CONDITION, not GOOD"
    );
    assert_eq!(sense[2], 0x03, "sense key must be MEDIUM ERROR");
    assert_eq!(sense[12], 0x11, "ASC must be UNRECOVERED READ ERROR");
    assert!(
        data.iter().all(|&b| b == 0xEE),
        "no zero-filled data may be presented at GOOD"
    );
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
fn inquiry_vpd_page_80_clamps_serial_over_255_bytes() {
    let _g = guard();
    let mut p = empty_profile();
    // A malformed profile carrying a 300-byte serial: the page-length byte
    // is a u8, so the response must clamp to 255 rather than truncate via
    // `as u8` and advertise a wrong (wrapped) length.
    let mut feat = vec![0x01, 0x08, 0x00, 0x00];
    feat.extend(std::iter::repeat_n(b'X', 300));
    p.features.push((0x0108, feat));
    let (status, data, _) = run(&p, &[0x12, 0x01, 0x80, 0, 0, 0], 260);
    assert_eq!(status, 0x00);
    assert_eq!(data[3], 255, "page length clamped to 255");
    assert_eq!(&data[4..259], &[b'X'; 255][..]);
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
