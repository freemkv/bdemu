use super::SgIoHdr;

fn hdr_over(data: &mut [u8]) -> SgIoHdr {
    SgIoHdr {
        interface_id: b'S' as i32,
        dxfer_direction: -3, // SG_DXFER_FROM_DEV
        cmd_len: 0,
        mx_sb_len: 0,
        iovec_count: 0,
        dxfer_len: data.len() as u32,
        dxferp: data.as_mut_ptr(),
        cmdp: std::ptr::null(),
        sbp: std::ptr::null_mut(),
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

// Short responses must zero the untouched tail, not leave stale host bytes.
#[test]
fn short_response_zeroes_the_untouched_tail() {
    let mut buf = vec![0xEEu8; 64];
    let mut hdr = hdr_over(&mut buf);
    hdr.write_response(&[1, 2, 3, 4]);

    assert_eq!(hdr.resid, 60, "residual must report the unfilled tail");
    assert_eq!(&buf[..4], &[1, 2, 3, 4]);
    assert!(
        buf[4..].iter().all(|&b| b == 0),
        "stale host bytes must not survive past the response: {:?}",
        &buf[4..]
    );
}

/// The full-length case is the one the tail-only zeroing optimises: the copy
/// covers the whole buffer, so nothing needs zeroing and residual is 0.
#[test]
fn full_response_fills_the_buffer_exactly() {
    let mut buf = vec![0xEEu8; 8];
    let mut hdr = hdr_over(&mut buf);
    hdr.write_response(&[9u8; 8]);

    assert_eq!(hdr.resid, 0);
    assert!(buf.iter().all(|&b| b == 9));
}

/// An over-long response is truncated to the host's buffer — the host told us
/// how much room it has and we may never write past it.
#[test]
fn oversized_response_is_truncated_to_the_host_buffer() {
    let mut buf = vec![0xEEu8; 4];
    let mut hdr = hdr_over(&mut buf);
    hdr.write_response(&[7u8; 16]);

    assert_eq!(hdr.resid, 0);
    assert_eq!(buf, vec![7u8; 4]);
}

/// A null/zero-length transfer transfers nothing, and the WHOLE declared
/// length is residual.
#[test]
fn empty_transfer_reports_full_residual() {
    let mut buf: Vec<u8> = Vec::new();
    let mut hdr = hdr_over(&mut buf);
    hdr.dxfer_len = 512; // host declared a length but gave no buffer
    hdr.dxferp = std::ptr::null_mut();
    hdr.write_response(&[1, 2, 3]);
    assert_eq!(hdr.resid, 512);
}

#[test]
fn clear_status_resets_every_output_field() {
    let mut buf = [0u8; 4];
    let mut hdr = hdr_over(&mut buf);
    hdr.status = 2;
    hdr.masked_status = 1;
    hdr.msg_status = 3;
    hdr.sb_len_wr = 18;
    hdr.host_status = 7;
    hdr.driver_status = 8;
    hdr.resid = 9;
    hdr.clear_status();
    assert_eq!(
        (
            hdr.status,
            hdr.masked_status,
            hdr.msg_status,
            hdr.sb_len_wr,
            hdr.host_status,
            hdr.driver_status,
            hdr.resid
        ),
        (0, 0, 0, 0, 0, 0, 0)
    );
}

// Too small a declared sense buffer: status is set but no sense is written.
#[test]
fn check_condition_skips_sense_when_buffer_is_too_small() {
    let mut buf = [0u8; 4];
    let mut sense = [0xAAu8; 32];
    let mut hdr = hdr_over(&mut buf);
    hdr.sbp = sense.as_mut_ptr();
    hdr.mx_sb_len = 8;
    hdr.set_check_condition(0x05, 0x24, 0x00);
    assert_eq!(hdr.status, 0x02);
    assert_eq!(hdr.sb_len_wr, 0);
    assert!(sense.iter().all(|&b| b == 0xAA), "sense must be untouched");
}

#[test]
fn opcode_is_zero_when_cmd_len_is_zero() {
    let mut buf = [0u8; 4];
    let cdb = [0x28u8, 1, 2, 3];
    let mut hdr = hdr_over(&mut buf);
    hdr.cmdp = cdb.as_ptr();
    hdr.cmd_len = 0;
    assert_eq!(hdr.opcode(), 0);
    hdr.cmd_len = 4;
    assert_eq!(hdr.opcode(), 0x28);
}

#[test]
fn cdb_past_cmd_len_reads_as_zero() {
    let mut buf = [0u8; 4];
    let cdb = [0x28u8, 1, 2, 3];
    let mut hdr = hdr_over(&mut buf);
    hdr.cmdp = cdb.as_ptr();
    hdr.cmd_len = 2;
    assert_eq!(hdr.cdb(1), 1);
    assert_eq!(hdr.cdb(3), 0, "bytes past cmd_len are not part of the CDB");
}
