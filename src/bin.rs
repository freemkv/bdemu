// bdemu — Blu-ray Drive Emulator CLI (MIT, freemkv project)
// Usage: bdemu capture-disc /dev/sg4 profiles/bu40n/discs/my_disc/
//        bdemu validate profiles/bu40n/

mod capture;

// cdylib can't be linked as an rlib, so pull the shared control-socket path
// policy in via #[path] instead, keeping this connect side in sync with the
// emulator's bind side (control.rs includes the same file).
#[path = "socket_name.rs"]
mod socket_name;
use socket_name::socket_path;

// Same mechanism for the terminal-escape sanitiser applied to profile-derived
// text (see sanitize.rs for why profile strings are untrusted).
#[path = "sanitize.rs"]
mod sanitize;
use sanitize::sanitize_for_terminal;

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
            let RunArgs {
                profile,
                disc,
                cmd_start,
            } = match parse_run_args(&args) {
                Ok(parsed) => parsed,
                Err(flag) => {
                    eprintln!("Error: missing {} value", flag);
                    eprintln!();
                    eprintln!(
                        "Usage: bdemu run --profile <dir> [--disc <name>] -- <command> [args...]"
                    );
                    std::process::exit(1);
                }
            };

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
            // used to coerce to "." and mislead the "not found" error; surface
            // the real cause instead.
            let exe = std::env::current_exe().unwrap_or_else(|e| {
                eprintln!("Error: could not determine bdemu binary location: {}", e);
                std::process::exit(1);
            });
            // A `.` fallback would reintroduce the CWD lookup bug fixed above.
            // current_exe() is absolute on every real platform so this is
            // practically unreachable, but fail explicitly rather than fall back.
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
            if !validate_profile(&args[2]) {
                std::process::exit(1);
            }
        }

        "status" => send_control("status", false),
        "eject" => send_control("eject", false),
        "load" => {
            if args.len() < 3 {
                eprintln!("Usage: bdemu load <disc_name>");
                std::process::exit(1);
            }
            // Protocol is newline-delimited: a \n/\r in the name would truncate
            // on the wire or smuggle a second command. Reject up front.
            if let Some(bad) = args[2].chars().find(|c| c.is_control()) {
                eprintln!(
                    "Error: disc name contains an illegal control character (U+{:04X})",
                    bad as u32
                );
                std::process::exit(1);
            }
            // `load` is the slow verb: the emulator does a full synchronous
            // fs::read of sectors.bin (up to 16 GiB) before replying. A short
            // timeout would fire mid-load, so request the generous one here.
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

// See docs/bin-cli.md — flag_value: guards against a flag value that looks
// like another flag (e.g. `--profile --disc x`).
fn flag_value(args: &[String], idx: usize) -> Option<&String> {
    match args.get(idx) {
        Some(v) if !v.starts_with('-') => Some(v),
        _ => None,
    }
}

/// The parsed result of the `bdemu run` flag scan.
struct RunArgs {
    profile: Option<String>,
    disc: Option<String>,
    /// Index into `args` where the child command begins (0 = none found).
    cmd_start: usize,
}

// See docs/bin-cli.md — parse_run_args: pure scan (no print/exit) so the
// missing-value diagnostic is unit-testable.
fn parse_run_args(args: &[String]) -> Result<RunArgs, &'static str> {
    let mut profile: Option<String> = None;
    let mut disc: Option<String> = None;
    let mut cmd_start = 0;

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--profile" | "-p" => {
                i += 1;
                profile = Some(flag_value(args, i).ok_or("--profile")?.clone());
            }
            "--disc" | "-d" => {
                i += 1;
                disc = Some(flag_value(args, i).ok_or("--disc")?.clone());
            }
            "--" => {
                cmd_start = i + 1;
                break;
            }
            _ => {
                // First non-flag arg starts the command.
                cmd_start = i;
                break;
            }
        }
        i += 1;
    }

    Ok(RunArgs {
        profile,
        disc,
        cmd_start,
    })
}

// See docs/bin-cli.md — exit_code_for: signal exits map to 128+signum
// (shell convention) so CI can tell a crash from a normal failure.
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
    println!("  bdemu run -p profiles/bu40n -d sample -- ./freemkv info");
    println!("  bdemu validate profiles/bu40n/");
    println!();
    println!("https://github.com/freemkv/bdemu");
}

// See docs/bin-cli.md — BlobState: size (not mere existence) decides
// pass/fail, so a zero-byte blob from an interrupted capture fails; absent
// stays non-fatal.
#[derive(Debug, PartialEq, Eq)]
enum BlobState {
    Missing,
    Empty,
    Present(u64),
    /// Stat failed for a reason other than absence (permissions, I/O error): we
    /// cannot vouch for the blob, so it is not a pass.
    Unreadable,
}

impl BlobState {
    fn is_broken(&self) -> bool {
        matches!(self, BlobState::Empty | BlobState::Unreadable)
    }

    fn describe(&self) -> String {
        match self {
            BlobState::Missing => "missing".to_string(),
            BlobState::Empty => "EMPTY (0 bytes — interrupted capture?)".to_string(),
            BlobState::Present(n) => format!("{} bytes", n),
            BlobState::Unreadable => "UNREADABLE".to_string(),
        }
    }
}

/// Classify a blob from its `fs::metadata` result. Split out from the filesystem
/// access so the policy is unit-testable.
fn classify_blob(meta: Result<u64, std::io::ErrorKind>) -> BlobState {
    match meta {
        Ok(0) => BlobState::Empty,
        Ok(n) => BlobState::Present(n),
        Err(std::io::ErrorKind::NotFound) => BlobState::Missing,
        Err(_) => BlobState::Unreadable,
    }
}

fn blob_state(path: &std::path::Path) -> BlobState {
    classify_blob(
        std::fs::metadata(path)
            .map(|m| m.len())
            .map_err(|e| e.kind()),
    )
}

// See docs/bin-cli.md — validate_profile: returns the pass/fail verdict
// instead of exiting, so the CI profile gate logic is unit-testable.
fn validate_profile(dir: &str) -> bool {
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
            // INQUIRY strings come from profiles shared by strangers (GitHub
            // issues, per SCHEMA.md). `.trim()` strips spaces, not ESC, so a
            // product string like "BDR\x1b[2J" could clear the terminal.
            let vendor = std::str::from_utf8(&data[8..16]).unwrap_or("?").trim();
            let product = std::str::from_utf8(&data[16..32]).unwrap_or("?").trim();
            println!(
                "  ✓ inquiry.bin ({} bytes) — {} {}",
                data.len(),
                sanitize_for_terminal(vendor),
                sanitize_for_terminal(product)
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

    // Check key features. `annotated` is the single source of truth for whether
    // to decode a feature's payload into a date/serial annotation — branch on it
    // below instead of re-listing magic codes, which could drift from this slice.
    let features: &[(&str, u16, &str, bool)] = &[
        ("gc_0000.bin", 0x0000, "Profile List", false),
        ("gc_0108.bin", 0x0108, "Serial Number", true),
        ("gc_010c.bin", 0x010C, "Firmware Information", true),
    ];
    for (file, code, name, annotated) in features {
        let fp = p.join(file);
        if fp.exists() {
            // exists() alone is not "present and usable": a zero-byte blob
            // (interrupted capture write) or unreadable metadata is a BROKEN
            // required feature, not a pass — fail it loudly instead.
            match std::fs::metadata(&fp).map(|m| m.len()) {
                Ok(0) => {
                    println!("  ✗ {} (0x{:04X} {}) EMPTY", file, code, name);
                    ok = false;
                }
                Err(e) => {
                    println!("  ✗ {} (0x{:04X} {}) UNREADABLE: {}", file, code, name, e);
                    ok = false;
                }
                Ok(sz) => {
                    let mut extra = String::new();
                    if *annotated {
                        // The file passed exists() above; a read failure here (perms,
                        // truncation race) was previously swallowed by unwrap_or_default,
                        // silently dropping the date/serial annotation. Surface it.
                        match std::fs::read(&fp) {
                            Ok(data) if data.len() > 4 => {
                                // Same untrusted-profile reasoning as inquiry.bin above:
                                // the firmware date and serial are raw bytes from a
                                // third-party profile, printed straight to a terminal.
                                if *code == 0x010C {
                                    let date = std::str::from_utf8(&data[4..16.min(data.len())])
                                        .unwrap_or("?");
                                    extra = format!(" — date: {}", sanitize_for_terminal(date));
                                } else {
                                    let serial =
                                        std::str::from_utf8(&data[4..]).unwrap_or("?").trim();
                                    extra = format!(" — serial: {}", sanitize_for_terminal(serial));
                                }
                            }
                            Ok(_) => {}
                            Err(e) => {
                                eprintln!("  warning: failed to read {}: {}", fp.display(), e)
                            }
                        }
                    }
                    println!(
                        "  ✓ {} (0x{:04X} {}, {} bytes){}",
                        file, code, name, sz, extra
                    );
                }
            }
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
                        // Disc directory names come from the profile too, so they
                        // are sanitised like every other profile-derived string.
                        let name = sanitize_for_terminal(&entry.file_name().to_string_lossy());
                        let toc = blob_state(&entry.path().join("toc.bin"));
                        let sectors = blob_state(&entry.path().join("sectors.bin"));
                        println!(
                            "  {} disc: {} (toc={}, sectors={})",
                            if toc.is_broken() || sectors.is_broken() {
                                "✗"
                            } else {
                                "✓"
                            },
                            name,
                            toc.describe(),
                            sectors.describe()
                        );
                        // A zero-byte blob is an interrupted capture, not an
                        // absent one; the old `.exists()` check printed
                        // `sectors=true` and exited 0 despite serving nothing.
                        if toc.is_broken() || sectors.is_broken() {
                            ok = false;
                        }
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
    }
    // Return the verdict; the caller maps `false` to a non-zero exit so a CI step
    // gating on a complete profile fails on a broken one (the warnings-only OK
    // path still exits 0).
    ok
}

// See docs/bin-cli.md — send_control: `slow_read` picks a generous timeout
// for the gigabyte-scale `load` reply vs. a short one for instant commands.
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
            // Surface the OS error so ENOENT (no emulator), EACCES (permissions),
            // and ECONNREFUSED (stale socket) stay distinguishable, matching the
            // error-surfacing pattern used elsewhere in this file.
            eprintln!("Cannot connect to bdemu ({}). Is the emulator running?", e);
            eprintln!("Start with: bdemu run --profile <dir> -- <command>");
            std::process::exit(1);
        }
    };

    // Bound the write before sending: a backpressured emulator (e.g. blocked
    // draining a prior multi-GB disc read) can fill the kernel send buffer and
    // wedge writeln! indefinitely, so arm a timeout rather than risk that.
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

    // Bound the read: a hung emulator would otherwise stall the CLI forever.
    // `load` gets a 30-min ceiling (multi-GB read can legitimately take that
    // long); other commands keep the tight 5s timeout to fail fast instead.
    let read_timeout = if slow_read {
        Duration::from_secs(1800)
    } else {
        Duration::from_secs(5)
    };
    if let Err(e) = stream.set_read_timeout(Some(read_timeout)) {
        eprintln!("Failed to set bdemu read timeout: {}", e);
        std::process::exit(1);
    }

    // Read the reply line by line, stopping at the terminator sentinel. A read
    // error or EOF before the terminator leaves `terminated` false so the CLI
    // fails loudly, unlike the old map_while(Result::ok) which dropped the tail.
    let reader = BufReader::new(&stream);
    let mut lines: Vec<String> = Vec::new();
    let mut terminated = false;
    for item in reader.lines() {
        match item {
            Ok(l) if l == socket_name::CONTROL_TERMINATOR => {
                terminated = true;
                break;
            }
            Ok(l) => lines.push(l),
            Err(_) => break,
        }
    }

    match classify_response(&lines, terminated) {
        ControlOutcome::Truncated => {
            eprintln!(
                "bdemu: control response was truncated before its terminator \
                 (the emulator may have timed out or crashed mid-reply)"
            );
            for line in &lines {
                eprintln!("{}", line);
            }
            std::process::exit(1);
        }
        // The control protocol prefixes success with "OK" and failure with
        // "ERR ..." (see control.rs Response). `bdemu load <bad-name>` must exit
        // non-zero so script/CI gating works: print the reply to stderr and fail.
        ControlOutcome::Error => {
            for line in &lines {
                eprintln!("{}", line);
            }
            std::process::exit(1);
        }
        ControlOutcome::Ok => {
            for line in &lines {
                println!("{}", line);
            }
        }
    }
}

/// How the CLI should treat a control-socket reply.
#[derive(Debug, PartialEq, Eq)]
enum ControlOutcome {
    Ok,
    Error,
    /// The terminator never arrived: the reply was cut short (emulator timeout or
    /// crash), so it must NOT be acted on as if complete.
    Truncated,
}

// Truncation takes precedence over content: a reply missing its terminator
// is untrustworthy even if the bytes that arrived start with "OK". See
// docs/bin-cli.md for why this is split out of send_control.
fn classify_response(lines: &[String], terminated: bool) -> ControlOutcome {
    if !terminated {
        ControlOutcome::Truncated
    } else if response_is_error(lines) {
        ControlOutcome::Error
    } else {
        ControlOutcome::Ok
    }
}

// True when a response is a failure: any "ERR " line, or a first line that
// doesn't start with "OK" (including empty/closed). See docs/bin-cli.md.
fn response_is_error(lines: &[String]) -> bool {
    lines.iter().any(|l| l.starts_with("ERR "))
        || !lines.first().map(|l| l.starts_with("OK")).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::{
        ControlOutcome, classify_response, exit_code_for, flag_value, parse_run_args,
        response_is_error, validate_profile,
    };

    /// A per-test scratch directory under the crate's `target/` (never /tmp, per
    /// the project no-/tmp scratch rule); `cargo clean` reclaims it.
    fn test_scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-scratch")
            .join(tag);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test scratch dir");
        dir
    }

    fn s(v: &str) -> String {
        v.to_string()
    }

    // Pins that the CLI derives its socket path from the shared
    // `socket_name` policy (see docs/bin-cli.md), not a local computation.
    #[test]
    fn cli_socket_path_uses_the_shared_policy() {
        let p = crate::socket_name::socket_path_from(Some("/run/user/1000"), None)
            .expect("must accept a runtime dir");
        assert_eq!(
            p,
            std::path::PathBuf::from("/run/user/1000").join(crate::socket_name::SOCKET_FILENAME)
        );
        assert!(
            crate::socket_name::socket_path_from(None, None).is_err(),
            "unset XDG_RUNTIME_DIR must be refused, not silently /tmp"
        );
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
    // Catches a regression to existence-only checking: a zero-length blob
    // must be BROKEN, non-empty must pass, absent stays non-fatal. See
    // docs/bin-cli.md.
    #[test]
    fn zero_byte_blob_is_a_failure_not_a_pass() {
        use super::{BlobState, classify_blob};
        use std::io::ErrorKind;

        assert_eq!(classify_blob(Ok(0)), BlobState::Empty);
        assert!(
            classify_blob(Ok(0)).is_broken(),
            "an interrupted capture must fail validate, not pass it"
        );
        assert!(classify_blob(Ok(0)).describe().contains("0 bytes"));

        assert_eq!(classify_blob(Ok(204800)), BlobState::Present(204800));
        assert!(!classify_blob(Ok(204800)).is_broken());
        assert!(
            classify_blob(Ok(204800)).describe().contains("204800"),
            "the size must be reported, not a bare true/false"
        );

        // Absent is legitimate (a metadata-only disc fixture) and stays non-fatal.
        assert_eq!(classify_blob(Err(ErrorKind::NotFound)), BlobState::Missing);
        assert!(!classify_blob(Err(ErrorKind::NotFound)).is_broken());

        // Anything else (permissions, I/O error) cannot be vouched for.
        assert_eq!(
            classify_blob(Err(ErrorKind::PermissionDenied)),
            BlobState::Unreadable
        );
        assert!(classify_blob(Err(ErrorKind::PermissionDenied)).is_broken());
    }

    // Catches the mutation that ignores `terminated`: a reply cut short must
    // be truncation even if it starts with "OK". See docs/bin-cli.md.
    #[test]
    fn missing_terminator_is_truncation_even_if_it_starts_ok() {
        // Terminator seen: ordinary OK / ERR handling.
        assert_eq!(
            classify_response(&[s("OK"), s("  disc-a (sectors=true)")], true),
            ControlOutcome::Ok
        );
        assert_eq!(
            classify_response(&[s("ERR disc not found")], true),
            ControlOutcome::Error
        );
        // Terminator NOT seen: truncated, even though the bytes that arrived look
        // like a valid OK response. This is the whole point of the fix.
        assert_eq!(
            classify_response(&[s("OK"), s("  disc-a (sectors=true)")], false),
            ControlOutcome::Truncated
        );
        // Nothing at all, unterminated: also truncated (not a bare error).
        assert_eq!(classify_response(&[], false), ControlOutcome::Truncated);
        // Empty but TERMINATED reply (no OK line) is a genuine error, not truncation.
        assert_eq!(classify_response(&[], true), ControlOutcome::Error);
    }

    /// The `!v.starts_with('-')` guard in flag_value: a flag's value that looks
    /// like another flag is a MISSING value, not the value. Catches the regression
    /// where `bdemu run --profile --disc x` silently set profile="--disc".
    #[test]
    fn flag_value_rejects_a_following_flag_and_missing_value() {
        let args = vec![s("bdemu"), s("run"), s("--profile"), s("mydir")];
        assert_eq!(
            flag_value(&args, 3),
            Some(&s("mydir")),
            "a plain value is taken"
        );

        let swallow = vec![s("bdemu"), s("run"), s("--profile"), s("--disc")];
        assert_eq!(
            flag_value(&swallow, 3),
            None,
            "a value that looks like a flag must be refused, not swallowed"
        );

        // Past the end of args -> None (missing value).
        assert_eq!(flag_value(&args, 99), None);
    }

    /// The `bdemu run` flag scan: flags before `--`, the first positional starting
    /// the command, and a missing flag value surfacing as Err(flag). Catches
    /// mutations to the loop's stop conditions or index handling.
    #[test]
    fn parse_run_args_covers_the_flag_scan() {
        // Full form with an explicit `--` separator.
        let a = [
            "bdemu",
            "run",
            "--profile",
            "p",
            "--disc",
            "d",
            "--",
            "cmd",
            "arg",
        ]
        .map(s)
        .to_vec();
        let r = parse_run_args(&a).expect("well-formed");
        assert_eq!(r.profile.as_deref(), Some("p"));
        assert_eq!(r.disc.as_deref(), Some("d"));
        assert_eq!(r.cmd_start, 7, "cmd_start points just past `--`");

        // Short flags, no `--`: the first non-flag token starts the command.
        let b = ["bdemu", "run", "-p", "p", "cmd", "arg"].map(s).to_vec();
        let r = parse_run_args(&b).expect("well-formed");
        assert_eq!(r.profile.as_deref(), Some("p"));
        assert!(r.disc.is_none());
        assert_eq!(r.cmd_start, 4);

        // No flags at all: the command starts immediately.
        let c = ["bdemu", "run", "cmd"].map(s).to_vec();
        let r = parse_run_args(&c).expect("well-formed");
        assert!(r.profile.is_none());
        assert_eq!(r.cmd_start, 2);

        // A flag missing its value (next token looks like a flag) is an error
        // naming that flag.
        let d = ["bdemu", "run", "--profile", "--disc", "x"].map(s).to_vec();
        assert!(
            matches!(parse_run_args(&d), Err("--profile")),
            "a flag whose value looks like another flag is a missing-value error"
        );
    }

    /// exit_code_for maps a signal-killed child to 128+signum (so CI can tell a
    /// crash from an ordinary non-zero exit) and a normal exit to its code. Catches
    /// a mutation that collapses signals to a bare 1.
    #[test]
    fn exit_code_for_distinguishes_signals_from_normal_exit() {
        use std::os::unix::process::ExitStatusExt;
        use std::process::ExitStatus;

        // Wait-status encoding: exit code n is (n << 8); a signal is the low 7 bits.
        assert_eq!(exit_code_for(&ExitStatus::from_raw(0)), 0, "clean exit");
        assert_eq!(
            exit_code_for(&ExitStatus::from_raw(5 << 8)),
            5,
            "exit code 5"
        );
        assert_eq!(
            exit_code_for(&ExitStatus::from_raw(9)),
            128 + 9,
            "SIGKILL must be 137, not a bare 1"
        );
        assert_eq!(
            exit_code_for(&ExitStatus::from_raw(11)),
            128 + 11,
            "SIGSEGV must be 139"
        );
    }

    // Catches a mutation that drops an `ok = false` assignment (a broken
    // profile must not pass validate). See docs/bin-cli.md.
    #[test]
    fn validate_returns_true_only_for_a_complete_profile() {
        let root = test_scratch_dir("validate_exit");

        // A complete-enough profile: drive.toml + 96-byte inquiry + the three
        // required feature blobs. No discs dir is a warning, not a failure.
        let good = root.join("good");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::write(good.join("drive.toml"), "[drive]\nproduct = \"X\"\n").unwrap();
        std::fs::write(good.join("inquiry.bin"), vec![0u8; 96]).unwrap();
        for f in ["gc_0000.bin", "gc_0108.bin", "gc_010c.bin"] {
            std::fs::write(good.join(f), vec![1u8; 8]).unwrap();
        }
        assert!(
            validate_profile(good.to_str().unwrap()),
            "a complete profile must validate clean"
        );

        // Break it with an interrupted-capture disc: a zero-byte sectors.bin.
        let disc = good.join("discs").join("d1");
        std::fs::create_dir_all(&disc).unwrap();
        std::fs::write(disc.join("toc.bin"), vec![1u8; 4]).unwrap();
        std::fs::write(disc.join("sectors.bin"), Vec::<u8>::new()).unwrap();
        assert!(
            !validate_profile(good.to_str().unwrap()),
            "a zero-byte sectors.bin (interrupted capture) must fail the gate"
        );

        // A profile missing the required inquiry/features fails too.
        let bad = root.join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("drive.toml"), "[drive]\nproduct = \"X\"\n").unwrap();
        assert!(
            !validate_profile(bad.to_str().unwrap()),
            "a profile missing inquiry/features must fail the gate"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
