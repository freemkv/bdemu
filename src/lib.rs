// bdemu — Blu-ray Drive Emulator
// AGPL-3.0 — freemkv project
//
// LD_PRELOAD entry point — intercepts ioctl(SG_IO) calls

mod profile;
mod scsi;
mod sg;

use once_cell::sync::Lazy;
use profile::LoadedProfile;
use sg::{SgIoHdr, SG_IO};
use std::sync::Mutex;

static PROFILE: Lazy<Mutex<Option<LoadedProfile>>> = Lazy::new(|| {
    let path = match std::env::var("BDEMU_PROFILE") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("bdemu: BDEMU_PROFILE not set");
            return Mutex::new(None);
        }
    };

    let loaded = match LoadedProfile::load(&path) {
        Some(p) => p,
        None => return Mutex::new(None),
    };

    eprintln!(
        "bdemu: loaded '{}' ({} features, {} read_bufs, disc={})",
        loaded.name,
        loaded.features.len(),
        loaded.read_bufs.len(),
        if loaded.has_disc() { "yes" } else { "no" }
    );

    Mutex::new(Some(loaded))
});

type RealIoctl = unsafe extern "C" fn(libc::c_int, libc::c_ulong, ...) -> libc::c_int;

static REAL_IOCTL: Lazy<RealIoctl> = Lazy::new(|| unsafe {
    let ptr = libc::dlsym(libc::RTLD_NEXT, b"ioctl\0".as_ptr() as *const _);
    if ptr.is_null() {
        panic!("bdemu: cannot find real ioctl");
    }
    std::mem::transmute(ptr)
});

#[no_mangle]
pub unsafe extern "C" fn ioctl(
    fd: libc::c_int,
    request: libc::c_ulong,
    arg: *mut libc::c_void,
) -> libc::c_int {
    if request != SG_IO || arg.is_null() {
        return (REAL_IOCTL)(fd, request, arg);
    }

    let guard = PROFILE.lock().unwrap();
    let profile = match guard.as_ref() {
        Some(p) => p,
        None => return (REAL_IOCTL)(fd, request, arg),
    };

    let hdr = &mut *(arg as *mut SgIoHdr);
    scsi::handle_scsi(hdr, profile);
    0
}
