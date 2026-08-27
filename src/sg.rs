// bdemu — Blu-ray Drive Emulator, MIT — freemkv project
// Linux SG_IO bindings

pub const SG_IO: libc::c_ulong = 0x2285;

/// sg_io_hdr_t from <scsi/sg.h>
#[repr(C)]
pub struct SgIoHdr {
    pub interface_id: i32,          // [i] 'S' for SCSI generic
    pub dxfer_direction: i32,       // [i] data transfer direction
    pub cmd_len: u8,                // [i] SCSI command length
    pub mx_sb_len: u8,              // [i] max sense buffer length
    pub iovec_count: u16,           // [i] 0 = no scatter gather
    pub dxfer_len: u32,             // [i] byte count of data transfer
    pub dxferp: *mut u8,            // [i] [o] points to data transfer memory
    pub cmdp: *const u8,            // [i] points to command to perform
    pub sbp: *mut u8,               // [i] points to sense_buffer
    pub timeout: u32,               // [i] MAX_UINT -> no timeout (unit: millisec)
    pub flags: u32,                 // [i] 0 -> default
    pub pack_id: i32,               // [i->o] unused internally
    pub usr_ptr: *mut libc::c_void, // [i->o] unused internally
    pub status: u8,                 // [o] SCSI status
    pub masked_status: u8,          // [o] shifted, masked scsi status
    pub msg_status: u8,             // [o] messaging level data (optional)
    pub sb_len_wr: u8,              // [o] byte count actually written to sbp
    pub host_status: u16,           // [o] host (adapter) status
    pub driver_status: u16,         // [o] driver status
    pub resid: i32,                 // [o] dxfer_len - actual_transferred
    pub duration: u32,              // [o] time taken by cmd (unit: millisec)
    pub info: u32,                  // [o] auxiliary information
}

// Cast to/from a raw kernel pointer via the LD_PRELOAD ioctl intercept, so this
// layout must match the kernel's sg_io_hdr_t exactly. libfreemkv's linux.rs
// client mirrors these assertions so drift fails at compile time, not silently.
#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<SgIoHdr>() == 88);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(std::mem::size_of::<SgIoHdr>() == 64);

// Safety contract for the raw-pointer methods below: when `dxferp`/`sbp`/`cmdp`
// is non-null it must point to at least `dxfer_len`/`mx_sb_len`/`cmd_len` valid
// bytes, guaranteed by our sole caller (the LD_PRELOAD SG_IO intercept).
impl SgIoHdr {
    /// Clear all output status fields (success)
    pub fn clear_status(&mut self) {
        self.status = 0;
        self.masked_status = 0;
        self.msg_status = 0;
        self.sb_len_wr = 0;
        self.host_status = 0;
        self.driver_status = 0;
        self.resid = 0;
    }

    /// Set CHECK CONDITION with sense data
    pub fn set_check_condition(&mut self, sense_key: u8, asc: u8, ascq: u8) {
        self.status = 0x02;
        self.masked_status = 0x01;
        // Only report sb_len_wr=18 when 18 sense bytes were actually written;
        // leave it at its reset value (0) otherwise so the host isn't told to
        // read sense data that doesn't exist.
        if !self.sbp.is_null() && self.mx_sb_len >= 18 {
            unsafe {
                std::ptr::write_bytes(self.sbp, 0, self.mx_sb_len as usize);
                *self.sbp.add(0) = 0x70; // error code: current, fixed
                *self.sbp.add(2) = sense_key;
                *self.sbp.add(7) = 10; // additional sense length
                *self.sbp.add(12) = asc;
                *self.sbp.add(13) = ascq;
            }
            self.sb_len_wr = 18;
        }
    }

    /// Get CDB opcode
    pub fn opcode(&self) -> u8 {
        if self.cmdp.is_null() || self.cmd_len == 0 {
            return 0;
        }
        unsafe { *self.cmdp }
    }

    /// Get CDB byte at index
    pub fn cdb(&self, idx: usize) -> u8 {
        if self.cmdp.is_null() || idx >= self.cmd_len as usize {
            return 0;
        }
        unsafe { *self.cmdp.add(idx) }
    }

    /// Write response data from slice
    pub fn write_response(&mut self, data: &[u8]) {
        if self.dxferp.is_null() || self.dxfer_len == 0 {
            // Nothing transferred: whole length is residual. Saturate the u32->i32
            // cast so dxfer_len above i32::MAX can't wrap negative (latent only).
            self.resid = self.dxfer_len.min(i32::MAX as u32) as i32;
            return;
        }
        let len = std::cmp::min(data.len(), self.dxfer_len as usize);
        let dxfer_len = self.dxfer_len as usize;
        unsafe {
            // Copy the response, then zero only the uncovered tail (was a full
            // write_bytes over the buffer first: ~2x slower). Zeroing is NOT
            // optional: a short response would leave stale host data visible.
            std::ptr::copy_nonoverlapping(data.as_ptr(), self.dxferp, len);
            if len < dxfer_len {
                std::ptr::write_bytes(self.dxferp.add(len), 0, dxfer_len - len);
            }
        }
        // Report the unfilled tail as residual (kernel sg driver semantics that
        // libfreemkv's linux.rs relies on) — without it, short responses leave
        // the client believing the full dxfer_len was transferred.
        self.resid = (self.dxfer_len.min(i32::MAX as u32) as i32).saturating_sub(len as i32);
    }
}

#[cfg(test)]
mod tests {
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

    /// A response shorter than the host's transfer length must leave the rest of
    /// the host buffer ZEROED, not holding whatever the host had there before.
    /// Catches the mutation that drops the tail `write_bytes` while keeping the
    /// copy — stale host memory past a short INQUIRY/READ_TOC response is
    /// indistinguishable, to the host, from bytes the drive returned. Also pins
    /// the residual the libfreemkv client uses to compute how much was
    /// transferred.
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
}
