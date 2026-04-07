// bdemu — Blu-ray Drive Emulator CLI
// AGPL-3.0 — freemkv project
//
// Usage:
//   bdemu capture-disc /dev/sg4 profiles/bu40n/discs/my_disc/
//   bdemu capture-disc /dev/sg4 profiles/bu40n/discs/my_disc/ --sectors 50000
//   bdemu validate profiles/bu40n/

mod scsi_probe;
mod capture;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        usage();
        return;
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
                        profile = args.get(i).cloned();
                    }
                    "--disc" | "-d" => {
                        i += 1;
                        disc = args.get(i).cloned();
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
                eprintln!("Usage: bdemu run --profile <dir> [--disc <name>] -- <command> [args...]");
                std::process::exit(1);
            });

            if cmd_start == 0 || cmd_start >= args.len() {
                eprintln!("Error: no command specified");
                eprintln!();
                eprintln!("Usage: bdemu run --profile <dir> [--disc <name>] -- <command> [args...]");
                eprintln!();
                eprintln!("Example:");
                eprintln!("  bdemu run --profile profiles/bu40n -- ./freemkv info");
                std::process::exit(1);
            }

            // Find libbdemu.so next to the bdemu binary
            let exe = std::env::current_exe().unwrap_or_default();
            let exe_dir = exe.parent().unwrap_or(std::path::Path::new("."));
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
                Ok(status) => std::process::exit(status.code().unwrap_or(1)),
                Err(e) => {
                    eprintln!("Failed to run {}: {}", cmd, e);
                    std::process::exit(1);
                }
            }
        }

        "capture-disc" => {
            if args.len() < 4 {
                eprintln!("Usage: bdemu capture-disc <device> <output_dir>");
                std::process::exit(1);
            }
            let device = &args[2];
            let output = &args[3];

            if let Err(e) = capture::capture_disc(device, output) {
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

        "status" => send_control("status"),
        "eject" => send_control("eject"),
        "load" => {
            if args.len() < 3 {
                eprintln!("Usage: bdemu load <disc_name>");
                std::process::exit(1);
            }
            send_control(&format!("load {}", args[2]));
        }
        "list-discs" => send_control("list-discs"),

        "--help" | "-h" | "help" => usage(),

        _ => {
            eprintln!("Unknown command: {}", args[1]);
            usage();
            std::process::exit(1);
        }
    }
}

fn usage() {
    println!("bdemu {}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("Commands:");
    println!("  run --profile <dir> [--disc <name>] -- <cmd>   Emulate a drive and run a command");
    println!("  capture-disc <device> <dir> [--sectors N]      Capture disc from real hardware");
    println!("  validate <profile_dir>                         Check profile completeness");
    println!();
    println!("Control (while emulator is running):");
    println!("  status                                         Show emulator state");
    println!("  eject                                          Eject the disc");
    println!("  load <disc_name>                               Load a disc");
    println!("  list-discs                                     List available discs");
    println!();
    println!("Examples:");
    println!("  bdemu run --profile profiles/bu40n -- ./freemkv info");
    println!("  bdemu run --profile profiles/bu40n --disc sample -- ./freemkv rip");
    println!("  bdemu eject                                    # while running");
    println!("  bdemu load sample2                             # swap disc");
    println!("  bdemu capture-disc /dev/sg4 profiles/my-drive/discs/my-disc/");
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
    if inq_path.exists() {
        let sz = std::fs::metadata(&inq_path).map(|m| m.len()).unwrap_or(0);
        if sz == 96 {
            let data = std::fs::read(&inq_path).unwrap_or_default();
            let vendor = std::str::from_utf8(&data[8..16]).unwrap_or("?").trim();
            let product = std::str::from_utf8(&data[16..32]).unwrap_or("?").trim();
            println!("  ✓ inquiry.bin ({} bytes) — {} {}", sz, vendor, product);
        } else {
            println!("  ⚠ inquiry.bin ({} bytes, expected 96)", sz);
            warnings += 1;
        }
    } else {
        println!("  ✗ inquiry.bin MISSING");
        ok = false;
    }

    // Check key features
    let features: &[(&str, u16, &str)] = &[
        ("gc_0000.bin", 0x0000, "Profile List"),
        ("gc_0108.bin", 0x0108, "Serial Number"),
        ("gc_010c.bin", 0x010C, "Firmware Information"),
    ];
    for (file, code, name) in features {
        let fp = p.join(file);
        if fp.exists() {
            let sz = std::fs::metadata(&fp).map(|m| m.len()).unwrap_or(0);
            let mut extra = String::new();
            if *code == 0x010C {
                let data = std::fs::read(&fp).unwrap_or_default();
                if data.len() > 4 {
                    let date = std::str::from_utf8(&data[4..16.min(data.len())]).unwrap_or("?");
                    extra = format!(" — date: {}", date);
                }
            }
            if *code == 0x0108 {
                let data = std::fs::read(&fp).unwrap_or_default();
                if data.len() > 4 {
                    let serial = std::str::from_utf8(&data[4..]).unwrap_or("?").trim();
                    extra = format!(" — serial: {}", serial);
                }
            }
            println!("  ✓ {} (0x{:04X} {}, {} bytes){}", file, code, name, sz, extra);
        } else {
            println!("  ✗ {} (0x{:04X} {}) MISSING", file, code, name);
            ok = false;
        }
    }

    // Count total features
    let feat_count = std::fs::read_dir(p)
        .map(|entries| entries.flatten().filter(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.starts_with("gc_") && n.ends_with(".bin")
        }).count())
        .unwrap_or(0);
    println!("  ✓ {} total features", feat_count);

    // Check optional files
    for (file, desc) in &[
        ("rpc_state.bin", "REPORT KEY RPC"),
        ("mode_2a.bin", "MODE SENSE 2A"),
        ("rb_f1.bin", "READ_BUFFER 0xF1 (Pioneer)"),
    ] {
        if p.join(file).exists() {
            let sz = std::fs::metadata(p.join(file)).map(|m| m.len()).unwrap_or(0);
            println!("  ✓ {} ({}, {} bytes)", file, desc, sz);
        } else {
            println!("  — {} ({}) not present", file, desc);
        }
    }

    // Check discs
    let discs_dir = p.join("discs");
    if discs_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&discs_dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let has_sectors = entry.path().join("sectors.bin").exists();
                    let has_toc = entry.path().join("toc.bin").exists();
                    println!("  ✓ disc: {} (toc={}, sectors={})", name, has_toc, has_sectors);
                }
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
}

fn send_control(cmd: &str) {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let mut stream = match UnixStream::connect("/tmp/bdemu.sock") {
        Ok(s) => s,
        Err(_) => {
            eprintln!("Cannot connect to bdemu. Is the emulator running?");
            eprintln!("Start with: bdemu run --profile <dir> -- <command>");
            std::process::exit(1);
        }
    };

    writeln!(stream, "{}", cmd).unwrap();

    let reader = BufReader::new(&stream);
    for line in reader.lines().flatten() {
        println!("{}", line);
    }
}
