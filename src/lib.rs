// bdemu — Blu-ray Drive Emulator
// AGPL-3.0 — freemkv project
//
// LD_PRELOAD entry point — intercepts ioctl(SG_IO) calls

mod control;
mod profile;
mod scsi;
mod sg;

use once_cell::sync::Lazy;
use profile::LoadedProfile;
use sg::{SG_IO, SgIoHdr};
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

    // LoadedProfile::load accepts both a directory and a `.json` file. The
    // control socket treats profile_dir as a base directory it joins "discs"
    // onto, so when the profile is a JSON file we store its parent directory
    // (otherwise `load`/`list-discs` would build paths like
    // `/path/profile.json/discs/<name>` and silently fail).
    let profile_base = {
        let p = std::path::PathBuf::from(&path);
        if p.is_dir() {
            p
        } else {
            // A bare filename like "profile.json" yields parent ""
            // (Path::parent returns Some(Path::new("")), never None), so the
            // old `.unwrap_or_else(|| ".")` fallback was dead and profile_base
            // became "" — joining "discs" onto "" gives the cwd-relative
            // "discs", which diverges from "./discs" if the process chdir's.
            // Map an empty parent explicitly to "." so the fallback fires.
            match p.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
                _ => std::path::PathBuf::from("."),
            }
        }
    };

    let profile = Arc::new(Mutex::new(loaded));
    let emu_state = Arc::new(Mutex::new(control::EmulatorState {
        profile_dir: profile_base,
        // A single DiscState enum keeps "loaded" and the name in sync by type.
        // Startup with a disc present but BDEMU_DISC unset is Loaded(None),
        // which cmd_status renders as "loaded (unknown)".
        disc: if has_disc {
            control::DiscState::Loaded(disc_name)
        } else {
            control::DiscState::Empty
        },
    }));

    // Start control socket listener
    control::start_listener(Arc::clone(&profile), Arc::clone(&emu_state));

    Some(State { profile, emu_state })
});

type RealIoctl = unsafe extern "C" fn(libc::c_int, libc::c_ulong, ...) -> libc::c_int;

// Hold an Option so a failed dlsym does not panic! out of a Lazy that is
// forced from inside the exported `extern "C" fn ioctl` (which would unwind
// across the C boundary — UB under the default panic=unwind).
static REAL_IOCTL: Lazy<Option<RealIoctl>> = Lazy::new(|| unsafe {
    let ptr = libc::dlsym(libc::RTLD_NEXT, c"ioctl".as_ptr());
    if ptr.is_null() {
        eprintln!("bdemu: cannot find real ioctl");
        return None;
    }
    Some(std::mem::transmute::<*mut libc::c_void, RealIoctl>(ptr))
});

/// Forward to the real libc `ioctl`, or fail with EINVAL if dlsym never found
/// it. Never panics.
///
/// # Safety
/// Same contract as libc `ioctl`: `arg` must be valid for `request`.
unsafe fn forward_ioctl(
    fd: libc::c_int,
    request: libc::c_ulong,
    arg: *mut libc::c_void,
) -> libc::c_int {
    match *REAL_IOCTL {
        Some(real) => unsafe { real(fd, request, arg) },
        // dlsym never found the real ioctl: signal failure. POSIX requires a
        // failing ioctl() to leave a meaningful errno, so set EINVAL rather than
        // leaving a stale value before returning -1. The shim ships as a
        // Linux .so; the cfg below keeps the non-Linux precommit build (macOS)
        // compiling with the platform's errno accessor.
        None => {
            unsafe {
                #[cfg(target_os = "linux")]
                {
                    *libc::__errno_location() = libc::EINVAL;
                }
                #[cfg(not(target_os = "linux"))]
                {
                    *libc::__error() = libc::EINVAL;
                }
            }
            -1
        }
    }
}

/// # Safety
/// Called by the dynamic linker as an LD_PRELOAD ioctl intercept.
/// `arg` must be a valid pointer to an `SgIoHdr` when `request` is `SG_IO`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ioctl(
    fd: libc::c_int,
    request: libc::c_ulong,
    arg: *mut libc::c_void,
) -> libc::c_int {
    // Rust 2024: unsafe fn body is not implicitly unsafe; each call-site
    // needs its own `unsafe { }` block.
    if request != SG_IO || arg.is_null() {
        return unsafe { forward_ioctl(fd, request, arg) };
    }

    let state = match STATE.as_ref() {
        Some(s) => s,
        None => return unsafe { forward_ioctl(fd, request, arg) },
    };

    // This cdylib builds under the default panic=unwind — no panic=abort is
    // configured anywhere (not in Cargo.toml, not in .cargo/config.toml, not in
    // CI). So this catch_unwind is the SOLE guard against UB from a panic
    // unwinding across this `extern "C"` boundary — do NOT remove it as a
    // "redundant" backstop. Contain any panic from the SCSI handler (e.g. a
    // poisoned lock, an out-of-bounds slice on a crafted profile) here and
    // report CHECK CONDITION / HARDWARE ERROR instead of unwinding.
    //
    // Fail the build if a future profile flips us to panic=abort: catch_unwind is
    // a no-op under panic=abort, so the guard would silently stop guarding (a
    // handler panic would abort the host process mid-ioctl instead of returning a
    // SCSI error). Better a compile error than a silent loss of the only UB guard.
    #[cfg(panic = "abort")]
    compile_error!(
        "bdemu requires panic=unwind: the extern \"C\" ioctl catch_unwind is the sole UB guard"
    );
    let arg_addr = arg as usize;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Recover the poisoned guard rather than unwrapping: a panic in any
        // other lock holder (control socket, a prior handler) must not wedge
        // every subsequent ioctl.
        let guard = state
            .profile
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let hdr = unsafe { &mut *(arg_addr as *mut SgIoHdr) };
        scsi::handle_scsi(hdr, &guard);
    }));

    if result.is_err() {
        // Best-effort: surface a CHECK CONDITION / HARDWARE ERROR to the host
        // so the caller sees a SCSI failure rather than corrupt/stale data.
        let hdr = unsafe { &mut *(arg as *mut SgIoHdr) };
        // Clear first: set_check_condition only writes status/masked_status and
        // (conditionally) the sense buffer, leaving resid/host_status/
        // driver_status/sb_len_wr untouched. If the panic fired before
        // handle_scsi reached its own clear_status() (e.g. in the
        // UNIT_ATTENTION branch or while computing the opcode), those fields
        // would still hold caller-supplied values, presenting inconsistent
        // metadata alongside the recovered SCSI status.
        hdr.clear_status();
        hdr.set_check_condition(0x04, 0x44, 0x00); // HARDWARE ERROR / INTERNAL TARGET FAILURE
    }
    0
}
