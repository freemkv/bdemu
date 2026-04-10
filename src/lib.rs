// bdemu — Blu-ray Drive Emulator
// AGPL-3.0 — freemkv project
//
// LD_PRELOAD entry point — intercepts ioctl(SG_IO) calls

mod profile;
mod scsi;
mod sg;
mod control;

use once_cell::sync::Lazy;
use profile::LoadedProfile;
use sg::{SgIoHdr, SG_IO};
use std::sync::{Arc, Mutex};

struct State {
    profile: Arc<Mutex<LoadedProfile>>,
    #[allow(dead_code)] // kept alive for the control socket listener thread
    emu_state: Arc<Mutex<control::EmulatorState>>,
}

static STATE: Lazy<Option<State>> = Lazy::new(|| {
    let path = match std::env::var("BDEMU_PROFILE") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("bdemu: BDEMU_PROFILE not set");
            return None;
        }
    };

    let loaded = match LoadedProfile::load(&path) {
        Some(p) => p,
        None => return None,
    };

    let has_disc = loaded.has_disc();
    let disc_name = std::env::var("BDEMU_DISC").ok();

    eprintln!(
        "bdemu: loaded '{}' ({} features, {} read_bufs, disc={})",
        loaded.name,
        loaded.features.len(),
        loaded.read_bufs.len(),
        if has_disc { "yes" } else { "no" }
    );

    let profile = Arc::new(Mutex::new(loaded));
    let emu_state = Arc::new(Mutex::new(control::EmulatorState {
        profile_dir: std::path::PathBuf::from(&path),
        disc_name: if has_disc { disc_name } else { None },
        disc_loaded: has_disc,
    }));

    // Start control socket listener
    control::start_listener(Arc::clone(&profile), Arc::clone(&emu_state));

    Some(State { profile, emu_state: emu_state })
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

    let state = match STATE.as_ref() {
        Some(s) => s,
        None => return (REAL_IOCTL)(fd, request, arg),
    };

    let guard = state.profile.lock().unwrap();
    let hdr = &mut *(arg as *mut SgIoHdr);
    scsi::handle_scsi(hdr, &*guard);
    0
}
