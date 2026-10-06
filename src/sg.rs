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
#[path = "sg_tests.rs"]
mod tests;
