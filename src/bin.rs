// bdemu — Blu-ray Drive Emulator CLI
// AGPL-3.0 — freemkv project
//
// Usage:
//   bdemu capture-disc /dev/sg4 profiles/bu40n/discs/my_disc/
//   bdemu validate profiles/bu40n/

mod capture;

// The library is a cdylib and cannot be linked as an rlib, so the binary cannot
// import from it. Pull the shared socket filename in directly via #[path] so it
// matches the emulator's bind side (control.rs uses the same file).
#[path = "socket_name.rs"]
mod socket_name;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        // A missing subcommand is a usage error; exit non-zero so scripts/CI
        // can detect it.
        usage();
        std::process::exit(1);
    }

    match args[1].as_str() {
        "run" => {
            // bdemu run --profile <dir> [--disc <name>] -- <command> [args...]
            let mut profile: Option<String> = None;
            let mut disc: Option<String> = None;
            let mut cmd_start = 0;

            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--profile" | "-p" => {
                        i += 1;
                        profile = Some(take_flag_value(&args, i, "--profile"));
                    }
                    "--disc" | "-d" => {
                        i += 1;
                        disc = Some(take_flag_value(&args, i, "--disc"));
                    }
                    "--" => {
                        cmd_start = i + 1;
                        break;
                    }
                    _ => {
                        // First non-flag arg starts the command
                        cmd_start = i;
                        break;
                    }
                }
                i += 1;
            }

            let profile = profile.unwrap_or_else(|| {
                eprintln!("Error: --profile <dir> is required");
                eprintln!();
                eprintln!(
                    "Usage: bdemu run --profile <dir> [--disc <name>] -- <command> [args...]"
                );
                std::process::exit(1);
            });

            if cmd_start == 0 || cmd_start >= args.len() {
                eprintln!("Error: no command specified");
                eprintln!();
                eprintln!(
                    "Usage: bdemu run --profile <dir> [--disc <name>] -- <command> [args...]"
                );
                eprintln!();
                eprintln!("Example:");
                eprintln!("  bdemu run --profile profiles/bu40n -- ./freemkv info");
                std::process::exit(1);
            }

            // Find libbdemu.so next to the bdemu binary. A failed current_exe()
            // previously coerced to an empty path, so the library was looked up
            // relative to "." and the resulting "not found" error pointed at the
            // wrong place; surface the real cause instead.
            let exe = std::env::current_exe().unwrap_or_else(|e| {
                eprintln!("Error: could not determine bdemu binary location: {}", e);
                std::process::exit(1);
            });
            // A `.` fallback here would silently reintroduce the CWD lookup the
            // comment above says was fixed (libbdemu.so resolved against the
            // current dir, not the binary's dir). current_exe() returns an
            // absolute path on every real platform so this is practically
            // unreachable, but error out explicitly rather than fall back.
            let exe_dir = exe.parent().unwrap_or_else(|| {
                eprintln!(
                    "Error: bdemu binary path has no parent directory: {}",
                    exe.display()
                );
                std::process::exit(1);
            });
            let lib_path = exe_dir.join("libbdemu.so");

            if !lib_path.exists() {
                eprintln!("Error: libbdemu.so not found at {}", lib_path.display());
                eprintln!("Place libbdemu.so next to the bdemu binary.");
                std::process::exit(1);
            }

            let cmd = &args[cmd_start];
            let cmd_args = &args[cmd_start + 1..];

            use std::process::Command;
            let mut child = Command::new(cmd);
            child.args(cmd_args);
            child.env("LD_PRELOAD", &lib_path);
            child.env("BDEMU_PROFILE", &profile);
            if let Some(d) = &disc {
                child.env("BDEMU_DISC", d);
            }

            match child.status() {
                Ok(status) => std::process::exit(exit_code_for(&status)),
                Err(e) => {
                    eprintln!("Failed to run {}: {}", cmd, e);
                    std::process::exit(1);
                }
            }
        }

        "capture-disc" => {
            // Collect positional args, allowing an optional `--eject` flag
            // anywhere after the subcommand.
            let mut eject = false;
            let mut positional: Vec<&String> = Vec::new();
            for a in &args[2..] {
                if a == "--eject" {
                    eject = true;
                } else {
                    positional.push(a);
                }
            }
            if positional.len() < 2 {
                eprintln!("Usage: bdemu capture-disc <device> <output_dir> [--eject]");
                std::process::exit(1);
            }
            let device = positional[0];
            let output = positional[1];

            if let Err(e) = capture::capture_disc(device, output, eject) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }

        "validate" => {
            if args.len() < 3 {
                eprintln!("Usage: bdemu validate <profile_dir>");
                std::process::exit(1);
            }
            validate_profile(&args[2]);
        }

        "status" => send_control("status", false),
        "eject" => send_control("eject", false),
        "load" => {
            if args.len() < 3 {
                eprintln!("Usage: bdemu load <disc_name>");
                std::process::exit(1);
            }
            // The control protocol is newline-delimited. A name containing a
            // control character (notably \n / \r) would be silently truncated on
            // the wire (or smuggle a second command), so the loaded disc would
            // not match what the user typed. Reject such names up front.
            if let Some(bad) = args[2].chars().find(|c| c.is_control()) {
                eprintln!(
                    "Error: disc name contains an illegal control character (U+{:04X})",
                    bad as u32
                );
                std::process::exit(1);
            }
            // `load` is the one slow verb: the emulator's control handler does a
            // full synchronous `fs::read` of sectors.bin (up to 16 GiB, multi-GB
            // typical for BD/UHD, far slower on NFS) before replying. A short read
            // timeout would fire mid-load and spuriously fail a successful rip, so
            // request the generous read timeout for this command only.
            send_control(&format!("load {}", args[2]), true);
        }
        "list-discs" => send_control("list-discs", false),

        "--help" | "-h" | "help" => usage(),

        _ => {
            eprintln!("Unknown command: {}", args[1]);
            usage();
            std::process::exit(1);
        }
    }
}

/// Take the value at `idx` for a flag that requires one, exiting with a clear
/// error if it is missing or looks like another flag. Without this, the old
/// `args.get(i).cloned()` silently swallowed the following token, so
/// `bdemu run --profile --disc x` set profile="--disc" and then complained about
/// a missing command — a confusing diagnostic for a simple typo.
fn take_flag_value(args: &[String], idx: usize, flag: &str) -> String {
    match args.get(idx) {
        Some(v) if !v.starts_with('-') => v.clone(),
        _ => {
            eprintln!("Error: missing {} value", flag);
            eprintln!();
            eprintln!("Usage: bdemu run --profile <dir> [--disc <name>] -- <command> [args...]");
            std::process::exit(1);
        }
    }
}

/// Map a finished child's status to a process exit code. A child killed by a
/// signal has no exit code (`status.code()` is None); reporting that as a plain
/// `1` makes a crash (SIGSEGV, SIGABRT) indistinguishable from an ordinary
/// non-zero exit. Follow the shell convention and return `128 + signum` so CI can
/// tell a crash from a normal failure, falling back to 1 only if neither is set.
fn exit_code_for(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(1)
}

fn usage() {
    println!("bdemu {}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("Commands:");
    println!("  run --profile <dir> [--disc <name>] -- <cmd>   Emulate drive, run command");
    println!("  capture-disc <device> <output_dir> [--eject]   Smart capture from hardware");
    println!("  validate <profile_dir>                         Check profile completeness");
    println!();
    println!("Control (while emulator is running):");
    println!("  status                                         Show emulator state");
    println!("  eject                                          Eject the disc");
    println!("  load <disc_name>                               Load a disc");
    println!("  list-discs                                     List available discs");
    println!();
    println!("Examples:");
    println!(
        "  bdemu capture-disc /dev/sr0 ./testbed/disc     Capture + auto-name (add --eject to eject)"
    );
    println!("  bdemu run -p profiles/bu40n -d sample -- ./freemkv disc-info");
    println!("  bdemu validate profiles/bu40n/");
    println!();
    println!("https://github.com/freemkv/bdemu");
}

fn validate_profile(dir: &str) {
    use std::path::Path;
    let p = Path::new(dir);

    println!("Validating profile: {}", dir);
    println!();

    let mut ok = true;
    let mut warnings = 0;

    // Check drive.toml
    let toml_path = p.join("drive.toml");
    if toml_path.exists() {
        println!("  ✓ drive.toml");
    } else {
        println!("  ✗ drive.toml MISSING");
        ok = false;
    }

    // Check inquiry
    let inq_path = p.join("inquiry.bin");
    match std::fs::read(&inq_path) {
        // Read the file once, then validate its length before slicing. The
        // metadata-then-read split previously raced: a truncation between the
        // size check and the read left an empty Vec, and &data[8..16] panicked.
        Ok(data) if data.len() == 96 => {
            let vendor = std::str::from_utf8(&data[8..16]).unwrap_or("?").trim();
            let product = std::str::from_utf8(&data[16..32]).unwrap_or("?").trim();
            println!(
                "  ✓ inquiry.bin ({} bytes) — {} {}",
                data.len(),
                vendor,
                product
            );
        }
        Ok(data) => {
            println!("  ⚠ inquiry.bin ({} bytes, expected 96)", data.len());
            warnings += 1;
        }
        Err(_) => {
            println!("  ✗ inquiry.bin MISSING");
            ok = false;
        }
    }

    // Check key features. The `annotated` bool is the single source of truth for
    // "decode this feature's payload into a date/serial annotation" — branch on
    // it below instead of re-listing the magic feature codes (which would drift
    // from this slice if a code were added/changed in one place but not the
    // other).
    let features: &[(&str, u16, &str, bool)] = &[
        ("gc_0000.bin", 0x0000, "Profile List", false),
        ("gc_0108.bin", 0x0108, "Serial Number", true),
        ("gc_010c.bin", 0x010C, "Firmware Information", true),
    ];
    for (file, code, name, annotated) in features {
        let fp = p.join(file);
        if fp.exists() {
            let sz = std::fs::metadata(&fp).map(|m| m.len()).unwrap_or(0);
            let mut extra = String::new();
            if *annotated {
                // The file passed exists() above; a read failure here (perms,
                // truncation race) was previously swallowed by unwrap_or_default,
                // silently dropping the date/serial annotation. Surface it.
                match std::fs::read(&fp) {
                    Ok(data) if data.len() > 4 => {
                        if *code == 0x010C {
                            let date =
                                std::str::from_utf8(&data[4..16.min(data.len())]).unwrap_or("?");
                            extra = format!(" — date: {}", date);
                        } else {
                            let serial = std::str::from_utf8(&data[4..]).unwrap_or("?").trim();
                            extra = format!(" — serial: {}", serial);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("  warning: failed to read {}: {}", fp.display(), e),
                }
            }
            println!(
                "  ✓ {} (0x{:04X} {}, {} bytes){}",
                file, code, name, sz, extra
            );
        } else {
            println!("  ✗ {} (0x{:04X} {}) MISSING", file, code, name);
            ok = false;
        }
    }

    // Count total features. A read_dir failure here previously coerced to
    // `unwrap_or(0)` and printed "✓ 0 total features" — a fake success that hid an
    // unreadable profile dir (perms, vanished). Surface the error and fail.
    match std::fs::read_dir(p) {
        Ok(entries) => {
            let feat_count = entries
                .flatten()
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().to_string();
                    n.starts_with("gc_") && n.ends_with(".bin")
                })
                .count();
            println!("  ✓ {} total features", feat_count);
        }
        Err(e) => {
            println!("  ✗ could not enumerate features: {}", e);
            ok = false;
        }
    }

    // Check optional files
    for (file, desc) in &[
        ("rpc_state.bin", "REPORT KEY RPC"),
        ("mode_2a.bin", "MODE SENSE 2A"),
        ("rb_f1.bin", "READ_BUFFER 0xF1 (Pioneer)"),
    ] {
        let fp = p.join(file);
        if fp.exists() {
            let sz = std::fs::metadata(&fp).map(|m| m.len()).unwrap_or(0);
            println!("  ✓ {} ({}, {} bytes)", file, desc, sz);
        } else {
            println!("  — {} ({}) not present", file, desc);
        }
    }

    // Check discs
    let discs_dir = p.join("discs");
    if discs_dir.exists() {
        // A read_dir failure on an existing discs/ dir was silently ignored (the
        // `if let Ok` simply skipped the loop), so a broken/unreadable directory
        // looked like "no discs" instead of an error. Report it and fail.
        match std::fs::read_dir(&discs_dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        let name = entry.file_name().to_string_lossy().to_string();
                        let has_sectors = entry.path().join("sectors.bin").exists();
                        let has_toc = entry.path().join("toc.bin").exists();
                        println!(
                            "  ✓ disc: {} (toc={}, sectors={})",
                            name, has_toc, has_sectors
                        );
                    }
                }
            }
            Err(e) => {
                println!("  ✗ could not enumerate discs: {}", e);
                ok = false;
            }
        }
    } else {
        println!("  — No disc profiles");
        warnings += 1;
    }

    println!();
    if ok {
        println!("Profile OK ({} warnings)", warnings);
    } else {
        println!("Profile INCOMPLETE — missing required files");
        // Signal failure so a CI step gating on a complete profile fails on a
        // broken one (the warnings-only OK path still exits 0).
        std::process::exit(1);
    }
}

/// Path to the control socket: the per-user runtime dir (mode 0700 on Linux) so
/// the socket is never world-accessible. The control socket accepts load/eject/
/// status commands, so a world-writable `/tmp/bdemu.sock` would let any local
/// user drive the emulator; we therefore REFUSE the insecure /tmp fallback and
/// return an error if $XDG_RUNTIME_DIR is unset/empty.
///
/// This MUST match the path the LD_PRELOAD library binds in `control.rs` (the
/// cdylib cannot be linked as an rlib, so the logic is duplicated rather than
/// shared).
fn socket_path() -> Result<std::path::PathBuf, String> {
    socket_path_from(std::env::var("XDG_RUNTIME_DIR").ok().as_deref())
}

/// Pure decision split out of `socket_path` so the fallback-refusal policy is
/// unit-testable without mutating the process environment.
fn socket_path_from(xdg_runtime_dir: Option<&str>) -> Result<std::path::PathBuf, String> {
    match xdg_runtime_dir {
        Some(dir) if !dir.is_empty() => {
            Ok(std::path::PathBuf::from(dir).join(crate::socket_name::SOCKET_FILENAME))
        }
        _ => Err(
            "XDG_RUNTIME_DIR is unset or empty; refusing to use a world-accessible \
             control socket in /tmp. Set XDG_RUNTIME_DIR to a private per-user runtime \
             directory (e.g. /run/user/$(id -u)) and retry."
                .to_string(),
        ),
    }
}

/// Send one control-socket command and relay the reply.
///
/// `slow_read` selects the read timeout: fast commands (status/eject/list-discs)
/// reply instantly and use a short read timeout so a hung emulator can't stall
/// the CLI forever. `load` is synchronous and gigabyte-scale on the emulator
/// side (a full `fs::read` of sectors.bin before it replies), so it needs a
/// generous read timeout — a short one would fire mid-load and spuriously fail a
/// successful load. The write timeout (tiny request payload) is short either way.
fn send_control(cmd: &str, slow_read: bool) {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let path = match socket_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("bdemu: {}", e);
            std::process::exit(1);
        }
    };
    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            // Surface the OS error so the distinct failure modes are
            // distinguishable: ENOENT (no emulator) vs EACCES (permissions) vs
            // ECONNREFUSED (stale socket). Matches the error-surfacing pattern
            // used elsewhere in this file (current_exe, writeln, etc.).
            eprintln!("Cannot connect to bdemu ({}). Is the emulator running?", e);
            eprintln!("Start with: bdemu run --profile <dir> -- <command>");
            std::process::exit(1);
        }
    };

    // Bound the write before sending: a backpressured emulator (e.g. blocked
    // draining its recv buffer during a prior multi-GB disc read) can fill the
    // kernel send buffer and wedge writeln! indefinitely. Surface a failure to
    // arm the timeout rather than swallowing it, consistent with the connect/
    // writeln error-surfacing in this function.
    if let Err(e) = stream.set_write_timeout(Some(Duration::from_secs(5))) {
        eprintln!("Failed to set bdemu write timeout: {}", e);
        std::process::exit(1);
    }

    // Don't unwrap: the emulator may close the socket between connect and
    // write (EPIPE). Report cleanly and exit non-zero, mirroring the
    // connect-error handling above.
    if let Err(e) = writeln!(stream, "{}", cmd) {
        eprintln!("Failed to send command to bdemu: {}", e);
        std::process::exit(1);
    }

    // Bound the read: a hung emulator (connected but never replies) would
    // otherwise stall the CLI forever. On timeout the read errors out, lines()
    // ends (map_while stops on Err), and response_is_error treats the
    // partial/empty result as a failure so the CLI exits non-zero. `load` is the
    // exception: it can legitimately take many minutes (multi-GB synchronous
    // sectors.bin read on the emulator, slower still on NFS), so it gets a 30-min
    // ceiling that still bounds a truly dead emulator. Fast commands keep the
    // tight 5s timeout.
    let read_timeout = if slow_read {
        Duration::from_secs(1800)
    } else {
        Duration::from_secs(5)
    };
    if let Err(e) = stream.set_read_timeout(Some(read_timeout)) {
        eprintln!("Failed to set bdemu read timeout: {}", e);
        std::process::exit(1);
    }

    let reader = BufReader::new(&stream);
    let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();

    // The control protocol prefixes success with "OK" and failure with
    // "ERR ..." (see control.rs Response). send_control previously printed every
    // line and returned, so `bdemu load <bad-name>` reported the error but still
    // exited 0 — defeating script/CI gating. Treat any "ERR " line (or a missing
    // leading "OK") as a failure: print to stderr and exit non-zero.
    if response_is_error(&lines) {
        for line in &lines {
            eprintln!("{}", line);
        }
        std::process::exit(1);
    }

    for line in &lines {
        println!("{}", line);
    }
}

/// True when a control-socket response should be treated as a failure: any
/// "ERR " line, or a response whose first line does not start with "OK"
/// (including an empty/closed response). Pulled out of `send_control` so the
/// exit-code policy is unit-testable without spawning a socket.
fn response_is_error(lines: &[String]) -> bool {
    lines.iter().any(|l| l.starts_with("ERR "))
        || !lines.first().map(|l| l.starts_with("OK")).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::{response_is_error, socket_path_from};

    #[test]
    fn socket_path_refuses_insecure_tmp_fallback() {
        // With a private per-user runtime dir, the socket lives there.
        let p = socket_path_from(Some("/run/user/1000")).expect("must accept a runtime dir");
        assert_eq!(
            p,
            std::path::PathBuf::from("/run/user/1000").join(crate::socket_name::SOCKET_FILENAME)
        );

        // Unset or empty XDG_RUNTIME_DIR must be REFUSED, not silently fall back
        // to a world-accessible /tmp/bdemu.sock.
        let err = socket_path_from(None).expect_err("None must be refused");
        assert!(err.contains("XDG_RUNTIME_DIR"), "got: {err}");
        assert!(socket_path_from(Some("")).is_err(), "empty must be refused");
    }

    #[test]
    fn ok_response_is_success() {
        assert!(!response_is_error(&["OK ejected".to_string()]));
        assert!(!response_is_error(&[
            "OK".to_string(),
            "profile: /x".to_string(),
            "disc: empty".to_string(),
        ]));
    }

    #[test]
    fn err_response_is_failure() {
        // `bdemu load <bad-name>` -> "ERR disc not found": must be non-zero.
        assert!(response_is_error(&["ERR disc not found".to_string()]));
        assert!(response_is_error(&["ERR invalid disc name".to_string()]));
        // ERR appearing on a later line still fails.
        assert!(response_is_error(&[
            "OK".to_string(),
            "ERR something".to_string()
        ]));
    }

    #[test]
    fn empty_or_garbage_response_is_failure() {
        // Closed socket / no reply: not an OK, so treat as failure.
        assert!(response_is_error(&[]));
        // First line lacks the OK prefix.
        assert!(response_is_error(&["unexpected".to_string()]));
    }
}
